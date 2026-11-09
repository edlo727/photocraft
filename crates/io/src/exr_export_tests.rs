use super::*;
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Adjustment, Layer as DocLayer, LayerMask};
use photocraft_raster::Surface;

fn doc() -> Document {
    Document::new("t", photocraft_geom::Size::new(8, 6), ColorMode::Rgb, SampleType::F32)
}

fn raster(doc: &Document, name: &str, rgba: [f32; 4]) -> DocLayer {
    let mut s = Surface::new(doc.pixel_format());
    s.fill_rect(Rect::new(1, 1, 7, 5), &rgba);
    DocLayer::new(name, LayerContent::Raster(s))
}

fn grade(name: &str) -> DocLayer {
    DocLayer::new(
        name,
        LayerContent::Adjustment(Adjustment::Grade {
            blackpoint: [0.0; 3],
            whitepoint: [1.0; 3],
            lift: [0.0; 3],
            gain: [1.5, 1.0, 1.0],
            multiply: [1.0; 3],
            offset: [0.0; 3],
            gamma: [1.0; 3],
            black_clamp: true,
            white_clamp: false,
        }),
    )
}

/// A mask hiding the left half of the canvas.
fn half_mask() -> LayerMask {
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(0, 0, 4, 6), &[0.0]);
    m
}

fn collect(doc: &Document, groups: GroupExport) -> (Vec<ExportLayer>, Vec<String>, Vec<String>) {
    let mut c = Collector::new(doc, groups, NO_PROGRESS, 0.6);
    let tree = c.list(&doc.layers, true, 0);
    (tree, c.pixels.into_iter().map(|(n, _)| n).collect(), c.masks.into_iter().map(|(n, _)| n).collect())
}

fn script(doc: &Document, groups: GroupExport) -> String {
    let (tree, _, _) = collect(doc, groups);
    generate_nuke_script("t.exr", &tree, 8, 6)
}

/// The text of the node named `name`, from its inputs to its closing brace.
fn node<'a>(nk: &'a str, name: &str) -> &'a str {
    let at = nk.find(&format!(" name {name}\n")).unwrap_or_else(|| panic!("no node {name} in:\n{nk}"));
    // The node's pushes start after the previous node's closing line.
    let start = [nk[..at].rfind("[stack 0]\n").map(|i| i + 10), nk[..at].rfind("\n}\n").map(|i| i + 3)].into_iter().flatten().max().unwrap_or(0);
    let end = at + nk[at..].find('}').unwrap_or(0);
    &nk[start..end]
}

#[test]
fn a_clipped_grade_grades_only_its_base() {
    let mut d = doc();
    let base = raster(&d, "Base", [0.5, 0.5, 0.5, 1.0]);
    let mut g = grade("Warm");
    g.clipped = true;
    let top = raster(&d, "Top", [0.2, 0.4, 0.6, 1.0]);
    let under = raster(&d, "Under", [0.1, 0.1, 0.1, 1.0]);
    d.layers = vec![under, base, g, top];

    let (tree, pixels, _) = collect(&d, GroupExport::Branches);
    assert_eq!(tree.len(), 3, "the Grade rides on its base");
    assert_eq!(tree[1].clipped.len(), 1);
    assert_eq!(pixels, ["Under", "Base", "Top"]);

    let nk = generate_nuke_script("t.exr", &tree, 8, 6);
    // The Grade's input is the base's own stream, not the comp.
    assert!(node(&nk, "Grade_Warm").starts_with("push $NPremult_Base\n"), "{nk}");
    // The graded base then merges onto the comp.
    assert!(node(&nk, "Merge_Base").contains("push $NGrade_Warm\npush $NDot_Comp_Under"), "{nk}");
    assert!(node(&nk, "Merge_Top").contains("push $NPremult_Top\npush $NMerge_Base"), "{nk}");
}

#[test]
fn a_layer_clipped_in_normal_goes_atop_its_base() {
    let mut d = doc();
    let base = raster(&d, "Base", [0.5, 0.5, 0.5, 1.0]);
    let mut paint = raster(&d, "Paint", [1.0, 0.0, 0.0, 1.0]);
    paint.clipped = true;
    paint.opacity = 0.5;
    d.layers = vec![base, paint];
    let nk = script(&d, GroupExport::Branches);
    let clip = node(&nk, "Clip_Paint");
    assert!(clip.contains("operation atop") && clip.contains("mix 0.5"), "{clip}");
    assert!(clip.contains("push $NPremult_Paint\npush $NPremult_Base"), "{clip}");
}

#[test]
fn a_layer_mask_is_an_exr_layer_on_the_merge_mask_input() {
    let mut d = doc();
    let base = raster(&d, "Base", [0.5, 0.5, 0.5, 1.0]);
    let mut top = raster(&d, "Top", [1.0, 0.0, 0.0, 1.0]);
    top.mask = Some(half_mask());
    d.layers = vec![base, top];

    let (tree, _, masks) = collect(&d, GroupExport::Branches);
    assert_eq!(masks, ["Top_mask"]);
    assert_eq!(tree[1].mask.as_deref(), Some("Top_mask"));

    let nk = generate_nuke_script("t.exr", &tree, 8, 6);
    assert!(nk.contains("add_layer {Top_mask Top_mask.red Top_mask.green Top_mask.blue Top_mask.alpha}"), "{nk}");
    // The mask is Shuffled off the layer's bus Dot…
    let shuffle = node(&nk, "ShuffleMask_Top");
    assert!(shuffle.starts_with("push $NDot_Top\n") && shuffle.contains("in Top_mask"), "{shuffle}");
    // …and multiplies the layer with a Merge mask (A = mask, B = layer)…
    let mask = node(&nk, "Mask_Top");
    assert!(mask.contains("push $NShuffleMask_Top\npush $NPremult_Top") && mask.contains("operation mask"), "{mask}");
    // …before the layer merges onto the comp.
    let merge = node(&nk, "Merge_Top");
    assert!(merge.contains("push $NMask_Top\npush $NDot_Comp_Base") && !merge.contains("maskChannel"), "{merge}");
}

#[test]
fn a_mask_values_match_the_canvas() {
    let mut d = doc();
    let mut top = raster(&d, "Top", [1.0, 0.0, 0.0, 1.0]);
    let mut m = half_mask();
    m.density = 0.5;
    top.mask = Some(m);
    d.layers = vec![top];
    let mut c = Collector::new(&d, GroupExport::Branches, NO_PROGRESS, 0.6);
    let _ = c.list(&d.layers, true, 0);
    let (_, v) = &c.masks[0];
    // Density 50%: the hidden half shows at 0.5.
    assert_eq!(v[0], 0.5);
    assert_eq!(v[7], 1.0);
}

#[test]
fn the_base_layer_mask_goes_into_its_coverage() {
    let mut d = doc();
    let mut base = raster(&d, "Base", [0.5, 0.5, 0.5, 1.0]);
    base.mask = Some(half_mask());
    d.layers = vec![base];
    let nk = script(&d, GroupExport::Branches);
    let m = node(&nk, "Mask_Base");
    assert!(m.contains("operation mask"), "{m}");
    assert!(nk.contains("push $NMask_Base\nDot {\n name Dot_Comp_Base"), "{nk}");
}

#[test]
fn a_masked_layer_in_any_mode_is_masked_before_its_merge() {
    let mut d = doc();
    d.layers = vec![raster(&d, "Base", [0.5, 0.5, 0.5, 1.0])];
    let mut top = raster(&d, "Top", [0.9, 0.2, 0.1, 0.8]);
    top.blend = BlendMode::Multiply;
    top.mask = Some(half_mask());
    d.layers.push(top);
    let nk = script(&d, GroupExport::Branches);
    let merge = node(&nk, "Merge_Top");
    assert!(merge.contains("push $NMask_Top\n") && merge.contains("operation multiply"), "{merge}");
    // Nuke's multiply blacks out B where A is transparent; alpha masking keeps it, as the canvas.
    assert!(merge.contains("screen_alpha true"), "{merge}");
}

#[test]
fn a_masked_adjustment_takes_its_mask_on_its_mask_input() {
    let mut d = doc();
    let mut g = grade("Warm");
    g.mask = Some(half_mask());
    d.layers = vec![raster(&d, "Base", [0.5, 0.5, 0.5, 1.0]), g];
    let nk = script(&d, GroupExport::Branches);
    // The Grade grades the comp…
    let grade = node(&nk, "Grade_Warm");
    assert!(grade.starts_with("push $NDot_Comp_Base\nGrade") && !grade.contains("inputs"), "{grade}");
    // …its mask is Shuffled off the bus beside it, like a layer's…
    let shuffle = node(&nk, "ShuffleMask_Warm");
    assert!(shuffle.starts_with("push $NDot_Warm_mask\n") && shuffle.contains("in Warm_mask"), "{shuffle}");
    // …and a Keymix mixes the graded comp over the ungraded one by it (mask, A, B pushed).
    let mix = node(&nk, "MaskMix_Warm");
    assert!(mix.contains("push $NShuffleMask_Warm\npush $NGrade_Warm\npush $NDot_Comp_Base\nKeymix"), "{mix}");
    assert!(mix.contains("inputs 3") && mix.contains("maskChannel rgba.alpha"), "{mix}");
    assert!(nk.contains("add_layer {Warm_mask Warm_mask.red"), "{nk}");
}

#[test]
fn clipped_nodes_stack_under_their_base_column() {
    let mut d = doc();
    let base = raster(&d, "Base", [0.5, 0.5, 0.5, 1.0]);
    let mut g = grade("Warm");
    g.clipped = true;
    d.layers = vec![raster(&d, "BG", [0.2; 4]), base, g];
    let nk = script(&d, GroupExport::Branches);
    let x = |name: &str| node(&nk, name).split("xpos ").nth(1).and_then(|t| t.lines().next()).map(str::to_string);
    assert_eq!(x("Grade_Warm"), x("Premult_Base"), "{nk}");
    assert_eq!(x("Merge_Base"), x("Premult_Base"), "the base merges below its own column");
}

#[test]
fn an_isolated_group_composites_its_children_then_merges_in_its_mode() {
    let mut d = doc();
    let bg = raster(&d, "BG", [0.5, 0.5, 0.5, 1.0]);
    let a = raster(&d, "A", [1.0, 0.0, 0.0, 1.0]);
    let b = raster(&d, "B", [0.0, 1.0, 0.0, 0.5]);
    let mut g = DocLayer::group("Shade", vec![a, b]);
    g.blend = BlendMode::Multiply;
    g.opacity = 0.75;
    g.mask = Some(half_mask());
    d.layers = vec![bg, g];

    let nk = script(&d, GroupExport::Branches);
    // The children start their own comp…
    assert!(nk.contains(" name Dot_Comp_A\n"), "{nk}");
    assert!(node(&nk, "Merge_B").contains("push $NPremult_B\npush $NDot_Comp_A"), "{nk}");
    // …which merges onto the background in the group's mode, mix and mask.
    let merge = node(&nk, "Merge_Shade");
    assert!(merge.contains("push $NMask_Shade\npush $NDot_Comp_BG"), "{merge}");
    assert!(node(&nk, "Mask_Shade").contains("push $NShuffleMask_Shade\npush $NMerge_B"), "{nk}");
    assert!(merge.contains("operation multiply") && merge.contains("mix 0.75") && merge.contains("screen_alpha true"), "{merge}");
    assert!(nk.contains("Shade_mask.alpha"), "{nk}");
    assert!(nk.contains(" name BD_Shade\n"), "a backdrop frames the group");
}

#[test]
fn a_pass_through_group_continues_the_comp() {
    let mut d = doc();
    let bg = raster(&d, "BG", [0.5, 0.5, 0.5, 1.0]);
    let a = raster(&d, "A", [1.0, 0.0, 0.0, 1.0]);
    d.layers = vec![bg, DocLayer::group("Folder", vec![a])];
    let nk = script(&d, GroupExport::Branches);
    assert!(node(&nk, "Merge_A").contains("push $NPremult_A\npush $NDot_Comp_BG"), "{nk}");
    assert!(!nk.contains(" name Merge_Folder\n"));
}

#[test]
fn a_pass_through_group_at_partial_opacity_mixes_with_the_comp_beneath() {
    let mut d = doc();
    let bg = raster(&d, "BG", [0.5, 0.5, 0.5, 1.0]);
    let a = raster(&d, "A", [1.0, 0.0, 0.0, 1.0]);
    let mut g = DocLayer::group("Folder", vec![a]);
    g.opacity = 0.4;
    d.layers = vec![bg, g];
    let nk = script(&d, GroupExport::Branches);
    let mix = node(&nk, "Merge_Folder");
    assert!(mix.contains("operation copy") && mix.contains("mix 0.4"), "{mix}");
    assert!(mix.contains("push $NMerge_A\npush $NDot_Comp_BG"), "{mix}");
}

#[test]
fn merged_groups_are_one_exr_layer_in_the_groups_mode() {
    let mut d = doc();
    let bg = raster(&d, "BG", [0.5, 0.5, 0.5, 1.0]);
    let a = raster(&d, "A", [1.0, 0.0, 0.0, 1.0]);
    let b = raster(&d, "B", [0.0, 1.0, 0.0, 1.0]);
    let mut g = DocLayer::group("Shade", vec![a, b]);
    g.blend = BlendMode::Multiply;
    d.layers = vec![bg, g];

    let (tree, pixels, _) = collect(&d, GroupExport::Merged);
    assert_eq!(pixels, ["BG", "Shade"]);
    assert!(tree[1].children.is_none());
    assert_eq!(tree[1].blend_mode, BlendMode::Multiply);
    let nk = generate_nuke_script("t.exr", &tree, 8, 6);
    assert!(node(&nk, "Merge_Shade").contains("operation multiply"), "{nk}");
    assert!(!nk.contains("Shuffle_A"));
}

#[test]
fn a_merged_pass_through_group_is_over() {
    let mut d = doc();
    let a = raster(&d, "A", [1.0, 0.0, 0.0, 1.0]);
    let mut b = raster(&d, "B", [0.0, 1.0, 0.0, 1.0]);
    b.blend = BlendMode::Screen;
    d.layers = vec![raster(&d, "BG", [0.5; 4]), DocLayer::group("Folder", vec![a, b])];
    let (tree, _, _) = collect(&d, GroupExport::Merged);
    assert_eq!(tree[1].blend_mode, BlendMode::Normal);
    assert!(tree[1].approx, "a Screen child blended with the layers beneath; merged alone it can't");
}

#[test]
fn hidden_layers_are_written_but_disabled() {
    let mut d = doc();
    let mut top = raster(&d, "Top", [1.0, 0.0, 0.0, 1.0]);
    top.visible = false;
    d.layers = vec![raster(&d, "BG", [0.5; 4]), top];
    let (_, pixels, _) = collect(&d, GroupExport::Branches);
    assert_eq!(pixels, ["BG", "Top"]);
    assert!(node(&script(&d, GroupExport::Branches), "Merge_Top").contains("disable true"));
}

#[test]
fn names_stay_unique_across_layers_and_masks() {
    let mut d = doc();
    let mut a = raster(&d, "Top", [1.0, 0.0, 0.0, 1.0]);
    a.mask = Some(half_mask());
    let b = raster(&d, "Top mask", [0.0, 1.0, 0.0, 1.0]);
    let c = raster(&d, "rgba", [0.0, 0.0, 1.0, 1.0]);
    d.layers = vec![a, b, c];
    let (_, pixels, masks) = collect(&d, GroupExport::Branches);
    assert_eq!(masks, ["Top_mask"]);
    assert_eq!(pixels, ["Top", "Top_mask_2", "rgba_layer"]);
}

#[test]
fn export_writes_mask_channels_and_the_script() {
    let mut d = doc();
    let mut top = raster(&d, "Top", [1.0, 0.0, 0.0, 1.0]);
    top.mask = Some(half_mask());
    d.layers = vec![raster(&d, "BG", [0.5; 4]), top];
    let dir = std::env::temp_dir().join(format!("photocraft-exr-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("comp.exr");
    export_multichannel_exr_with_options(&d, &path, NukeCompression::ZipScanline, true, true, true, GroupExport::Branches, NO_PROGRESS).expect("export");

    let image = exr::prelude::read_all_flat_layers_from_file(&path).expect("read back");
    let names: Vec<String> = image.layer_data[0].channel_data.list.iter().map(|c| c.name.to_string()).collect();
    for want in ["Top_mask.R", "Top_mask.A", "Top.R", "BG.A", "R", "A"] {
        assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
    }
    let nk = std::fs::read_to_string(dir.join("comp.nk")).expect("script written");
    assert!(nk.contains("operation mask") && nk.contains("in Top_mask"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_reports_progress_and_can_be_cancelled_before_writing() {
    let mut d = doc();
    d.layers = vec![raster(&d, "BG", [0.5; 4]), raster(&d, "Top", [1.0, 0.0, 0.0, 1.0])];
    let dir = std::env::temp_dir().join(format!("photocraft-exr-progress-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");

    let seen = std::sync::Mutex::new(Vec::<f32>::new());
    let record = |f: f32, _: &str| {
        seen.lock().expect("lock").push(f);
        true
    };
    let path = dir.join("ok.exr");
    export_multichannel_exr_with_options(&d, &path, NukeCompression::ZipScanline, true, true, true, GroupExport::Branches, &record).expect("export");
    let seen = seen.into_inner().expect("lock");
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progress only goes forward: {seen:?}");
    assert_eq!(seen.last().copied(), Some(1.0));

    // Cancelled on the second layer: an error, and no file.
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let cancel = |_: f32, _: &str| calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 1;
    let path = dir.join("cancelled.exr");
    let r = export_multichannel_exr_with_options(&d, &path, NukeCompression::ZipScanline, true, true, true, GroupExport::Branches, &cancel);
    assert_eq!(r, Err("cancelled".to_string()));
    assert!(!path.exists() && !path.with_extension("nk").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn alpha_masking_is_on_for_modes_that_ignore_alpha() {
    // Nuke's multiply and average change B where A is empty; the canvas doesn't.
    for mode in [BlendMode::Multiply, BlendMode::Reflect] {
        assert_eq!(merge_knobs(mode), " screen_alpha true\n", "{mode:?} ({})", mode.label());
    }
    // Porter-Duff operations use alpha themselves; plus, min, max and difference already leave B.
    for mode in [BlendMode::Normal, BlendMode::LinearBurn, BlendMode::LinearDodge, BlendMode::Darken, BlendMode::Lighten, BlendMode::Difference] {
        assert_eq!(merge_knobs(mode), "", "{mode:?} ({})", mode.label());
    }
}

#[test]
fn a_clipped_masked_curves_takes_its_mask_beside_its_base_column() {
    let mut d = doc();
    let mut g = grade("Warm");
    g.clipped = true;
    g.mask = Some(half_mask());
    d.layers = vec![raster(&d, "Base", [0.5, 0.5, 0.5, 1.0]), g];
    let nk = script(&d, GroupExport::Branches);
    let mix = node(&nk, "MaskMix_Warm");
    assert!(mix.contains("push $NShuffleMask_Warm\npush $NGrade_Warm\npush $NPremult_Base\nKeymix"), "{mix}");
    // The base (the bottom layer here) starts the comp from the Keymix.
    assert!(nk.contains("push $NMaskMix_Warm\nDot {\n name Dot_Comp_Base"), "{nk}");
}

#[test]
fn no_node_uses_a_mask_input_slot() {
    // Nuke didn't keep `inputs 1+1` / `2+1` mask inputs from the script; Keymix carries masks.
    let mut d = doc();
    let mut base = raster(&d, "Base", [0.5, 0.5, 0.5, 1.0]);
    base.mask = Some(half_mask());
    let mut g = grade("Warm");
    g.clipped = true;
    g.mask = Some(half_mask());
    let mut m = raster(&d, "Mult", [0.9, 0.2, 0.1, 1.0]);
    m.clipped = true;
    m.blend = BlendMode::Multiply;
    let a = raster(&d, "A", [1.0, 0.0, 0.0, 1.0]);
    let mut folder = DocLayer::group("Folder", vec![a]);
    folder.mask = Some(half_mask());
    d.layers = vec![raster(&d, "BG", [0.2; 4]), base, g, m, folder];
    let nk = script(&d, GroupExport::Branches);
    assert!(!nk.contains("inputs 1+1") && !nk.contains("inputs 2+1") && !nk.contains("maskChannelInput"), "{nk}");
    // A clipped Multiply: merged, then kept inside the base by a Keymix on the base's alpha.
    let clip = node(&nk, "Clip_Mult");
    assert!(clip.contains("push $NMaskMix_Warm\npush $NClipOp_Mult\npush $NMaskMix_Warm\nKeymix"), "{clip}");
    assert!(node(&nk, "Merge_Folder").contains("Keymix"), "{nk}");
}
