use super::*;

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

#[test]
fn nuke_hard_light_matches_nuke_merge_math() {
    // A = source (top), B = backdrop. Multiply below A = 0.5, screen above, continuous at 0.5.
    let hl = |a: f32, b: f32| blend_channel_hdr(BlendMode::NukeHardLight, b, a);
    assert!(close(hl(0.25, 0.4), 0.2));
    // A > 0.5 over a dark backdrop: 1 - 2(1-A)(1-B), not A (the old max() fallback bug).
    assert!(close(hl(0.6, 0.1), 0.28));
    assert!(close(hl(0.9, 0.2), 0.84));
    for b in [0.0f32, 0.1, 0.4, 0.5, 0.9, 1.0] {
        assert!(close(hl(0.5, b), b), "neutral at A = 0.5 for B = {b}");
        assert!((hl(0.5 - 1e-4, b) - hl(0.5 + 1e-4, b)).abs() < 1e-3, "continuous at 0.5 for B = {b}");
    }
    // Measured in Nuke 17: hard-light and overlay are display-range operations. The deciding
    // input is clamped to 0..1, the other to 0 and above, and the result to 1 and below.
    assert!(close(hl(2.0, 0.2), 1.0), "{}", hl(2.0, 0.2));
    assert!(close(hl(0.2, 2.0), 0.8), "{}", hl(0.2, 2.0));
    assert!(close(hl(2.0, 2.0), 1.0), "{}", hl(2.0, 2.0));
    assert!(close(hl(0.8, -0.25), 0.6), "{}", hl(0.8, -0.25));
    assert!(close(hl(-0.25, 0.8), 0.0), "{}", hl(-0.25, 0.8));
    // Overlay is hard light with A and B swapped (decided by B).
    assert!(close(blend_channel(BlendMode::NukeOverlay, 0.6, 0.1), hl(0.6, 0.1)));
}

#[test]
fn nuke_soft_light_matches_nuke_merge_math_and_keeps_float() {
    // Nuke's soft-light (measured in Nuke 17), A = source (top), B = backdrop:
    // B(2A + B(1 - AB)) where AB < 1, else 2AB.
    let sl = |a: f32, b: f32| blend_channel(BlendMode::NukeSoftLight, b, a);
    // Unlike Photoshop's soft light it lightens at A = 0.5 too.
    assert!(close(sl(0.5, 0.5), 0.6875), "{}", sl(0.5, 0.5));
    // The B(1 - AB) term, not B(1 - 2A): an earlier version had 0.896 here.
    assert!(close(sl(0.8, 0.8), 1.5104), "{}", sl(0.8, 0.8));
    assert!(close(sl(0.0, 0.6), 0.36));
    // Continuous where it switches to 2AB.
    assert!(close(sl(1.0, 1.0), 2.0));
    // Above 1 nothing is clipped first: a hot A over a dark B stays in the AB < 1 branch...
    assert!(close(sl(4.0, 0.1), 0.806), "{}", sl(4.0, 0.1));
    // ...and a bright plate takes 2AB.
    assert!(close(sl(2.0, 1.0), 4.0));
    // Negative inputs count as 0 (measured in Nuke 17): B² where A < 0, 0 where B < 0.
    assert!(close(sl(-0.25, 0.6), 0.36));
    assert!(close(sl(0.8, -0.25), 0.0));
}

#[test]
fn photoshop_modes_keep_highlights_in_32_bit() {
    // Screen of a 50.0 sun over itself is the sun, not 100 - 2500.
    for mode in [BlendMode::Screen, BlendMode::Overlay, BlendMode::HardLight, BlendMode::SoftLight, BlendMode::ColorDodge, BlendMode::Exclusion] {
        assert!(close(blend_channel_hdr(mode, 50.0, 50.0), 50.0), "{mode:?}");
        assert!(blend_channel_hdr(mode, 50.0, 0.3) >= 50.0, "{mode:?} keeps a bright backdrop");
        // Inside 0..1 the formula is untouched, and so is every integer depth.
        assert_eq!(blend_channel_hdr(mode, 0.4, 0.7), blend_channel(mode, 0.4, 0.7), "{mode:?}");
    }
    assert!(close(blend_channel(BlendMode::Screen, 0.5, 0.5), 0.75));
}

#[test]
fn photoshop_modes_are_photoshops_and_nuke_modes_are_nukes() {
    // Photoshop's Subtract is backdrop − source, clipped at 0; Nuke's minus is A − B.
    assert!(close(blend_channel(BlendMode::Subtract, 0.75, 0.25), 0.5));
    assert!(close(blend_channel(BlendMode::Subtract, 0.25, 0.75), 0.0));
    assert!(close(blend_channel(BlendMode::NukeMinus, 0.25, 0.75), 0.5));
    assert!(close(blend_channel(BlendMode::NukeFrom, 0.75, 0.25), 0.5));
    // Photoshop's Linear Light; Nuke's hypot.
    assert!(close(blend_channel(BlendMode::LinearLight, 0.25, 0.5), 0.25));
    assert!(close(blend_channel(BlendMode::NukeHypot, 0.3, 0.4), 0.5));
    // Multiply leaves the backdrop where the source is transparent in both.
    let bg = [0.2, 0.4, 0.6, 1.0];
    for m in [BlendMode::Multiply, BlendMode::NukeMultiply] {
        assert_eq!(composite(m, bg, [1.0, 0.0, 0.0, 0.0], 1.0), bg, "{m:?}");
    }
    // Nuke's min takes the backdrop to black where the source is transparent; Darken doesn't.
    let min = composite(BlendMode::NukeMin, bg, [1.0, 1.0, 1.0, 0.0], 1.0);
    assert!(close(min[3], 0.0), "{min:?}");
    assert_eq!(composite(BlendMode::Darken, bg, [1.0, 1.0, 1.0, 0.0], 1.0), bg);
}

#[test]
fn menus_list_photoshops_modes_and_nuke_ones_only_when_asked() {
    set_nuke_modes_in_menus(false);
    assert_eq!(BlendMode::menu_modes().collect::<Vec<_>>(), BlendMode::LAYER_MODES.to_vec());
    assert!(!BlendMode::NukeMultiply.is_menu_mode());
    set_nuke_modes_in_menus(true);
    let menu: Vec<_> = BlendMode::menu_modes().collect();
    assert_eq!(menu.len(), 27 + 18);
    assert!(BlendMode::NukeMultiply.is_menu_mode() && !BlendMode::NukeAtop.is_menu_mode());
    set_nuke_modes_in_menus(false);
    // Every mode is in exactly one list, and the Nuke ones all have an operation.
    let all: Vec<_> = BlendMode::all_modes().collect();
    assert_eq!(all.len(), 33 + 28);
    for (i, m) in all.iter().enumerate() {
        assert!(!all[..i].contains(m), "{m:?} twice");
        assert_eq!(m.is_nuke(), m.nuke_op().is_some() && *m != BlendMode::Normal, "{m:?}");
        assert!(!m.is_nuke() || m.label().ends_with("(Nuke)"), "{m:?}");
    }
    // Labels are unique, so a name finds one mode.
    let mut labels: Vec<_> = all.iter().map(|m| m.label().to_ascii_lowercase()).collect();
    labels.sort();
    labels.dedup();
    assert_eq!(labels.len(), all.len());
}

#[test]
fn psd_keys_read_as_photoshop_modes_and_nuke_ones_write_their_nearest() {
    for key in [b"mul ", b"scrn", b"over", b"lbrn", b"fsub", b"fdiv", b"hue ", b"vLit"] {
        let m = BlendMode::from_psd_key(*key).expect("a Photoshop mode");
        assert!(!m.is_nuke(), "{m:?}");
    }
    assert_eq!(BlendMode::NukeMultiply.psd_key(), *b"mul ");
    assert_eq!(BlendMode::NukeFrom.psd_key(), *b"fsub");
    assert_eq!(BlendMode::NukeHypot.psd_key(), *b"norm");
    assert!(!BlendMode::NukeMultiply.has_psd_equivalent());
    // Nuke scripts: Photoshop modes map to the closest Merge operation, inexactly.
    assert_eq!(BlendMode::Multiply.nearest_nuke_op(), ("multiply", false));
    assert_eq!(BlendMode::Subtract.nearest_nuke_op(), ("from", false));
    assert_eq!(BlendMode::NukeMultiply.nearest_nuke_op(), ("multiply", true));
    assert_eq!(BlendMode::Normal.nearest_nuke_op(), ("over", true));
    assert_eq!(BlendMode::Hue.nearest_nuke_op(), ("over", false));
}

/// Nuke 17's Merge2, measured: `nuke17_merge.csv` (see its header). Each row is one operation on
/// one pair of colours and alphas, in the alpha-masking setting the .nk export writes for it.
const NUKE17: &str = include_str!("nuke17_merge.csv");

fn nuke_op_mode(name: &str) -> Option<BlendMode> {
    BlendMode::all_modes().find(|m| m.is_nuke() && m.nuke_op() == Some(name))
}

#[test]
fn every_nuke_operation_matches_nuke_17() {
    let mut rows = 0;
    let mut ops = std::collections::BTreeSet::new();
    let mut bad = Vec::new();
    for line in NUKE17.lines().filter(|l| !l.starts_with('#') && !l.is_empty()) {
        let f: Vec<&str> = line.split(',').collect();
        assert_eq!(f.len(), 8, "{line}");
        let mode = nuke_op_mode(f[0]).unwrap_or_else(|| panic!("no mode for {}", f[0]));
        let v: Vec<f32> = f[2..].iter().map(|s| s.parse().unwrap()).collect();
        let (ca, a, cb, b, out, out_a) = (v[0], v[1], v[2], v[3], v[4], v[5]);
        let got = composite_hdr(mode, [cb, cb, cb, b], [ca, ca, ca, a], 1.0);
        let alpha = out_a.clamp(0.0, 1.0);
        rows += 1;
        ops.insert(f[0]);
        if (got[3] - alpha).abs() > 1e-4 {
            bad.push(format!("{line}: alpha {}", got[3]));
        } else if alpha > 1e-6 {
            // Nuke's premultiplied colour, as the straight colour the canvas keeps.
            let want = out / alpha;
            if (got[0] - want).abs() > 1e-3 * want.abs().max(1.0) {
                bad.push(format!("{line}: colour {} for {want}", got[0]));
            }
        }
        // alpha 0 with colour left over (min of two negatives): straight alpha keeps nothing there.
    }
    assert_eq!(ops.len(), 27, "{ops:?}");
    assert!(rows > 2500, "{rows}");
    assert!(bad.is_empty(), "{} of {rows} differ from Nuke 17:\n{}", bad.len(), bad[..bad.len().min(12)].join("\n"));
}

#[test]
fn opaque_layers_never_go_transparent_under_the_subtracting_operations() {
    // Their own formulas on the alphas give 0 for two opaque inputs (|1 - 1|, 1 - 1), which a
    // plain Merge leaves as colour with no alpha and straight alpha cannot hold at all. With
    // alpha masking, as the export writes them, the result covers.
    for op in [BlendMode::NukeDifference, BlendMode::NukeExclusion, BlendMode::NukeMinus, BlendMode::NukeFrom] {
        let got = composite_hdr(op, [0.2, 0.2, 0.2, 1.0], [0.7, 0.7, 0.7, 1.0], 1.0);
        assert!((got[3] - 1.0).abs() < 1e-6, "{op:?} went transparent: {got:?}");
    }
}

#[test]
fn masked_operations_leave_each_input_alone_where_the_other_is_empty() {
    // Merge's alpha masking: each input is unchanged where the other has zero alpha, and the
    // output alpha is a + b - ab. Multiply used to black the source out where the backdrop was
    // empty.
    use BlendMode::*;
    for op in [NukeMultiply, NukeDivide, NukeMinus, NukeFrom, NukeDifference, NukeExclusion, NukeOverlay, NukeSoftLight, NukeAverage] {
        let over_empty = composite_hdr(op, [0.9, 0.9, 0.9, 0.0], [0.3, 0.6, 2.0, 0.7], 1.0);
        assert!((over_empty[0] - 0.3).abs() < 1e-5 && (over_empty[2] - 2.0).abs() < 1e-4, "{op:?} over nothing: {over_empty:?}");
        assert!((over_empty[3] - 0.7).abs() < 1e-6);
        let empty = composite_hdr(op, [0.3, 0.6, 2.0, 0.6], [0.9, 0.9, 0.9, 0.0], 1.0);
        assert!((empty[0] - 0.3).abs() < 1e-5 && (empty[3] - 0.6).abs() < 1e-6, "{op:?} from nothing: {empty:?}");
        let both = composite_hdr(op, [0.5, 0.5, 0.5, 0.4], [0.5, 0.5, 0.5, 0.5], 1.0);
        assert!((both[3] - 0.7).abs() < 1e-6, "{op:?}: alpha {} for a + b - ab = 0.7", both[3]);
    }
}

#[test]
fn nuke_screen_divide_exclusion_and_geometric_switch_on_their_inputs() {
    let ch = |op: BlendMode, a: f32, b: f32| blend_channel_hdr(op, b, a);
    // Screen takes the formula only where both inputs are within 0..1, else max(A, B): a hot
    // highlight or a negative ACEScg value is never pushed further (measured in Nuke 17).
    assert!(close(ch(BlendMode::NukeScreen, 0.5, 0.5), 0.75));
    assert!(close(ch(BlendMode::NukeScreen, 2.0, 0.5), 2.0));
    assert!(close(ch(BlendMode::NukeScreen, -0.25, 0.2), 0.2));
    assert!(close(ch(BlendMode::NukeScreen, 4.0, 3.0), 4.0));
    // Two negatives don't make a positive.
    assert!(close(ch(BlendMode::NukeMultiply, -0.25, -0.5), -0.25));
    // Divide: A / B where B is positive, else A.
    assert!(close(ch(BlendMode::NukeDivide, -0.25, 0.5), -0.5));
    assert!(close(ch(BlendMode::NukeDivide, -0.25, -0.5), -0.25));
    assert!(close(ch(BlendMode::NukeDivide, 0.5, -0.25), 0.5));
    assert!(close(ch(BlendMode::NukeDivide, 0.5, 0.0), 0.5));
    // Exclusion drops its 2AB term, and geometric gives 0, where either input is negative.
    assert!(close(ch(BlendMode::NukeExclusion, -0.25, 0.8), 0.55));
    assert!(close(ch(BlendMode::NukeExclusion, 0.5, 0.5), 0.5));
    assert!(close(ch(BlendMode::NukeGeometric, -0.25, 0.8), 0.0));
    assert!(close(ch(BlendMode::NukeGeometric, 0.5, 0.5), 0.5));
    // Color dodge and burn clamp: dodge to 1 and below, burn to 0 and above.
    assert!(close(ch(BlendMode::NukeColorDodge, 0.8, 0.6), 1.0));
    assert!(close(ch(BlendMode::NukeColorDodge, -0.25, 0.6), 0.6));
    assert!(close(ch(BlendMode::NukeColorBurn, 0.2, 0.6), 0.0));
    assert!(close(ch(BlendMode::NukeColorBurn, 2.0, -0.25), 0.5));
}

/// Matte is "Aa + B(1-a)" with alpha a + b(1 - a) (measured in Nuke 17), the over for an
/// *unpremultiplied* A: PhotoCraft's layer colour is straight, so A·a is the premultiplied colour
/// and the result is an over. The .nk export Unpremults a matte layer's stream so Nuke gets the
/// straight colour too; fed premultiplied, Nuke would premultiply it twice.
#[test]
fn nuke_matte_is_an_over_of_the_straight_colour() {
    let got = composite_hdr(BlendMode::NukeMatte, [0.8, 0.8, 0.8, 0.5], [0.2, 0.2, 0.2, 0.3], 1.0);
    let alpha = 0.3 + 0.5 * 0.7;
    assert!((got[3] - alpha).abs() < 1e-6);
    assert!((got[0] - (0.2 * 0.3 + 0.8 * 0.5 * 0.7) / alpha).abs() < 1e-5, "{got:?}");
}
