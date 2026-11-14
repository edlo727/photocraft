//! Beyond not clipping (see `hdr_filter_matrix`): is a filter *correct* on 32-bit scene-linear
//! (ACEScg) data? Three measurable properties, each a thing a compositor relies on:
//!
//! 1. **Exposure invariance** — `f(k·I) == k·f(I)`. Grading before or after the filter must give
//!    the same picture. A filter that gamma-encodes internally, or thresholds on absolute values,
//!    breaks this; in Nuke a Blur does not.
//! 2. **Linear-light averaging** — across a `0 → V` step, any symmetric blur leaves exactly `V/2`
//!    at the edge. An sRGB round-trip inside the filter lands near `0.73·V` or `0.21·V` instead,
//!    and a tone-map shows up as a `V = 4` reading far from `2.0`.
//! 3. **Premultiplied edges** — blurring an opaque patch against transparency must not drag its
//!    colour towards black, which is the classic dark-fringe bug.
//!
//! `cargo test -p photocraft-engine --test linear_filter_behaviour -- --include-ignored --nocapture`
//! runs the strict checks and prints the full table.
use photocraft_doc::Rect;
use photocraft_engine::Session;
use serde_json::{Value, json};

const W: i32 = 64;
const H: i32 = 64;
/// The gain applied for the exposure-invariance comparison (3 stops).
const GAIN: f32 = 8.0;
/// Relative tolerance for a property that should hold exactly, bar float reordering.
const TOL: f32 = 1e-3;

/// A 32-bit ACEScg document with an empty layer on top, which the filters then run on.
fn doc32() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": W, "height": H, "depth": 32})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s
}

/// Fills the active layer from `px`, in the surface's own model.
fn write(s: &mut Session, px: impl Fn(i32, i32) -> [f32; 4]) {
    s.edit("fixture", |doc, active| {
        doc.icc_profile = Some(craft_cms::Builtin::AcesCg.profile().to_bytes());
        let surface = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let format = surface.format();
        let bytes: Vec<f32> = (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).flat_map(|(x, y)| photocraft_raster::from_rgba(&format, px(x, y))).collect();
        surface.write_region(Rect::from_xywh(0, 0, W as u32, H as u32), &bytes);
        Ok(())
    })
    .unwrap();
}

/// The active layer as straight RGBA.
fn read(s: &Session) -> Option<Vec<[f32; 4]>> {
    let st = s.active()?;
    let surface = st.active_layer.and_then(|id| st.doc.layer(id)).and_then(photocraft_doc::Layer::surface)?;
    let mut out = vec![[0.0; 4]; (W * H) as usize];
    surface.read_rgba_into(Rect::from_xywh(0, 0, W as u32, H as u32), &mut out);
    Some(out)
}

/// `id` with `p` over a layer filled from `px`; `None` if the command refused it.
fn run(id: &str, p: &Value, px: impl Fn(i32, i32) -> [f32; 4]) -> Option<Vec<[f32; 4]>> {
    let mut s = doc32();
    write(&mut s, px);
    s.execute(id, p.clone()).ok()?;
    read(&s)
}

/// The exposure-invariance fixture: a two-axis ramp, a 4.0 highlight patch and a small negative
/// patch, all opaque. Nothing is uniform, so no filter is a no-op on it.
fn pattern(x: i32, y: i32) -> [f32; 4] {
    let (v, w) = (x as f32 / (W - 1) as f32 * 0.8 + 0.02, y as f32 / (H - 1) as f32 * 0.5 + 0.05);
    if (16..32).contains(&x) && (16..32).contains(&y) {
        [4.0, 2.0, 0.5, 1.0]
    } else if (40..56).contains(&x) && (40..56).contains(&y) {
        [-0.05, 0.3, -0.02, 1.0]
    } else {
        [v, w, 0.5 * (v + w), 1.0]
    }
}

/// Property 1: the largest difference between `f(GAIN·I)` and `GAIN·f(I)` over RGB, relative to
/// the result's own range. 0 is exact invariance; `None` if the filter refused or went non-finite.
fn exposure_error(id: &str, p: &Value) -> Option<f32> {
    let base = run(id, p, pattern)?;
    let up = run(id, p, |x, y| {
        let q = pattern(x, y);
        [q[0] * GAIN, q[1] * GAIN, q[2] * GAIN, q[3]]
    })?;
    let range = base.iter().flat_map(|q| q[..3].iter()).fold(0.0f32, |m, v| m.max(v.abs())).max(1e-6) * GAIN;
    let mut worst = 0.0f32;
    for (b, u) in base.iter().zip(&up) {
        for (b, u) in b[..3].iter().zip(&u[..3]) {
            if !b.is_finite() || !u.is_finite() {
                return None;
            }
            worst = worst.max((*u - GAIN * *b).abs() / range);
        }
    }
    Some(worst)
}

/// Property 2: the mean of the two pixels straddling a `0 → v` step at x = 32, which any
/// symmetric blur that averages in linear light leaves at exactly `v/2`.
fn edge_mean(id: &str, p: &Value, v: f32) -> Option<f32> {
    let out = run(id, p, move |x, _| {
        let c = if x >= 32 { v } else { 0.0 };
        [c, c, c, 1.0]
    })?;
    let at = |x: i32| out[(H / 2 * W + x) as usize][0];
    Some((at(31) + at(32)) / 2.0)
}

/// Property 3: the worst surviving share of an opaque patch's colour at the soft edge the filter
/// created, as straight colour over the original. 1.0 keeps the colour; blurring straight colour
/// against transparent black lands near the pixel's own alpha. `None` if no edge was made.
fn edge_colour(id: &str, p: &Value) -> Option<f32> {
    const C: [f32; 4] = [4.0, 2.0, 0.5, 1.0];
    let out = run(id, p, |x, y| if (16..48).contains(&x) && (16..48).contains(&y) { C } else { [0.0; 4] })?;
    out.iter().filter(|q| (0.2..0.8).contains(&q[3]) && q[0].is_finite()).map(|q| q[0] / C[0]).fold(None, |w: Option<f32>, r| Some(w.map_or(r, |w| w.min(r))))
}

/// Every filter the `hdr_filter_matrix` report found to keep 32-bit values, with settings that
/// actually change the image (defaults leave several of them a no-op).
fn filters() -> Vec<(&'static str, Value)> {
    vec![
        ("filter.blur.gaussianBlur", json!({"radius": 3})),
        ("filter.blur.boxBlur", json!({"radius": 3})),
        ("filter.blur.blur", json!({})),
        ("filter.blur.blurMore", json!({})),
        ("filter.blur.motionBlur", json!({"angle": 0, "distance": 9})),
        ("filter.blur.radialBlur", json!({"amount": 10, "method": "spin", "quality": "best"})),
        ("filter.blur.lensBlur", json!({"radius": 8})),
        ("filter.blur.shapeBlur", json!({"radius": 8, "shape": "circle"})),
        ("filter.blur.smartBlur", json!({"radius": 5, "threshold": 25})),
        ("filter.blur.surfaceBlur", json!({"radius": 5, "threshold": 15})),
        ("filter.blurGallery.fieldBlur", json!({"blur": 15})),
        ("filter.blurGallery.irisBlur", json!({"blur": 15})),
        ("filter.blurGallery.tiltShift", json!({"blur": 15})),
        ("filter.blurGallery.spinBlur", json!({"blurAngle": 15})),
        ("filter.blurGallery.pathBlur", json!({"speed": 50})),
        ("filter.sharpen.sharpen", json!({})),
        ("filter.sharpen.sharpenMore", json!({})),
        ("filter.sharpen.sharpenEdges", json!({})),
        ("filter.sharpen.unsharpMask", json!({"amount": 100, "radius": 2, "threshold": 0})),
        ("filter.sharpen.smartSharpen", json!({"amount": 100, "radius": 2, "reduceNoise": 0})),
        ("filter.noise.median", json!({"radius": 2})),
        ("filter.noise.despeckle", json!({})),
        ("filter.noise.dustAndScratches", json!({"radius": 2, "threshold": 0})),
        ("filter.noise.reduceNoise", json!({"strength": 6})),
        ("filter.noise.addNoise", json!({"amount": 12.5, "seed": 1})),
        ("filter.other.maximum", json!({"radius": 2})),
        ("filter.other.minimum", json!({"radius": 2})),
        ("filter.other.offset", json!({"horizontal": 5, "vertical": 3, "undefinedAreas": "wrap"})),
        ("filter.other.colorToAlpha", json!({})),
        ("filter.pixelate.mosaic", json!({"cellSize": 8})),
        ("filter.pixelate.crystallize", json!({"cellSize": 10, "seed": 1})),
        ("filter.pixelate.facet", json!({})),
        ("filter.pixelate.fragment", json!({})),
        ("filter.pixelate.pointillize", json!({"cellSize": 6, "seed": 1})),
        ("filter.distort.twirl", json!({"angle": 50})),
        ("filter.distort.pinch", json!({"amount": 50})),
        ("filter.distort.spherize", json!({"amount": 100})),
        ("filter.distort.ripple", json!({"amount": 100, "size": "medium"})),
        ("filter.distort.wave", json!({"seed": 1})),
        ("filter.distort.zigZag", json!({"amount": 10, "ridges": 5})),
        ("filter.distort.shear", json!({"amount": 20})),
        ("filter.distort.polarCoordinates", json!({"mode": "rectangularToPolar"})),
        ("filter.stylize.emboss", json!({"angle": 135, "height": 3, "amount": 100})),
        ("filter.stylize.diffuse", json!({"mode": "normal", "seed": 1})),
        ("filter.stylize.wind", json!({"method": "wind", "direction": "fromRight", "seed": 1})),
        ("filter.stylize.tiles", json!({"count": 10, "maxOffset": 10, "fill": "unaltered", "seed": 1})),
        ("filter.stylize.extrude", json!({"type": "blocks", "size": 16, "depth": 30, "seed": 1})),
        ("filter.stylize.oilPaint", json!({})),
        ("filter.render.lensFlare", json!({"brightness": 100})),
        ("filter.render.lightingEffects", json!({})),
        ("filter.render.flame", json!({})),
        ("filter.render.tree", json!({})),
        ("filter.adaptiveWideAngle", json!({"model": "perspective", "scale": 90})),
        ("filter.video.deInterlace", json!({})),
    ]
}

/// Resamples and linear averages with no absolute threshold: these must be exposure invariant,
/// or a grade's position in the stack changes the picture.
const MUST_BE_INVARIANT: &[&str] = &[
    "filter.blur.gaussianBlur",
    "filter.blur.boxBlur",
    "filter.blur.blur",
    "filter.blur.blurMore",
    "filter.blur.motionBlur",
    "filter.blur.lensBlur",
    "filter.blur.shapeBlur",
    "filter.blurGallery.fieldBlur",
    "filter.blurGallery.irisBlur",
    "filter.blurGallery.tiltShift",
    "filter.blurGallery.spinBlur",
    "filter.blurGallery.pathBlur",
    "filter.sharpen.sharpen",
    "filter.sharpen.sharpenMore",
    "filter.sharpen.unsharpMask",
    "filter.noise.median",
    "filter.other.maximum",
    "filter.other.minimum",
    "filter.other.offset",
    "filter.pixelate.mosaic",
    "filter.pixelate.facet",
    "filter.pixelate.fragment",
    "filter.distort.twirl",
    "filter.distort.pinch",
    "filter.distort.spherize",
    "filter.distort.ripple",
    "filter.distort.zigZag",
    "filter.distort.shear",
    "filter.distort.polarCoordinates",
    "filter.video.deInterlace",
    "filter.adaptiveWideAngle",
];

/// Edge-preserving filters whose strength is an absolute difference, so they cannot be exposure
/// invariant: the threshold means "this many levels of 255" wherever it is used, which on a
/// scene-linear plate smooths the shadows and leaves the highlights alone. Despeckle and Sharpen
/// Edges are presets of this kind (Surface Blur threshold 12, Unsharp Mask threshold 6), not the
/// plain averages their names suggest. Rescaling the control against local brightness would make
/// them exposure invariant, and would change their 8- and 16-bit behaviour, so it is a decision
/// rather than a fix.
const THRESHOLD_BASED: &[&str] = &[
    "filter.noise.despeckle",
    "filter.sharpen.sharpenEdges",
    "filter.blur.smartBlur",
    "filter.blur.surfaceBlur",
    "filter.noise.dustAndScratches",
    "filter.noise.reduceNoise",
];

/// Symmetric blurs, whose step edge lands on exactly half the step in linear light.
const SYMMETRIC_BLURS: &[&str] =
    &["filter.blur.gaussianBlur", "filter.blur.boxBlur", "filter.blur.blur", "filter.blur.blurMore", "filter.blur.motionBlur", "filter.blur.shapeBlur"];

/// Blurs that spread an opaque patch into transparency, where its colour must survive.
const SOFTENING_BLURS: &[&str] =
    &["filter.blur.gaussianBlur", "filter.blur.boxBlur", "filter.blur.motionBlur", "filter.blur.shapeBlur", "filter.blur.lensBlur"];

/// Looks up a filter's settings from [`filters`].
fn params(id: &str) -> Value {
    filters().into_iter().find(|(f, _)| *f == id).map(|(_, p)| p).unwrap_or_else(|| panic!("{id} is not in filters()"))
}

#[test]
fn linear_filters_are_exposure_invariant() {
    let mut bad = Vec::new();
    for id in MUST_BE_INVARIANT {
        match exposure_error(id, &params(id)) {
            Some(e) if e <= TOL => {}
            Some(e) => bad.push(format!("{id}: f(8x) differs from 8*f(x) by {:.2}% of range", e * 100.0)),
            None => bad.push(format!("{id}: refused the fixture or went non-finite")),
        }
    }
    assert!(bad.is_empty(), "{} filters are not exposure invariant:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn symmetric_blurs_average_in_linear_light() {
    let mut bad = Vec::new();
    for id in SYMMETRIC_BLURS {
        for v in [1.0f32, 4.0] {
            let Some(got) = edge_mean(id, &params(id), v) else {
                bad.push(format!("{id}: refused the step fixture"));
                continue;
            };
            // sRGB-encoded averaging of 0 and v lands near 0.73*v for v = 1; a tone-map pulls the
            // v = 4 reading far below 2.0. Either is well outside 2 % of v.
            if (got - v / 2.0).abs() > 0.02 * v {
                bad.push(format!("{id}: a 0 -> {v} step reads {got:.4} at the edge, not {:.4}", v / 2.0));
            }
        }
    }
    assert!(bad.is_empty(), "{} readings are not a linear-light average:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn blurs_keep_colour_at_transparent_edges() {
    let mut bad = Vec::new();
    for id in SOFTENING_BLURS {
        match edge_colour(id, &params(id)) {
            Some(r) if r >= 0.9 => {}
            Some(r) => bad.push(format!("{id}: colour at a soft edge fell to {:.0}% of the patch's", r * 100.0)),
            None => bad.push(format!("{id}: made no partly transparent edge to measure")),
        }
    }
    assert!(bad.is_empty(), "{} filters darken colour against transparency:\n{}", bad.len(), bad.join("\n"));
}

#[test]
#[ignore = "report: run with --include-ignored --nocapture"]
fn linear_behaviour_report() {
    let mut rows = vec![
        "| Filter | exposure invariant | 0→1 edge (want 0.500) | 0→4 edge (want 2.000) | colour at soft edge |".to_string(),
        "|---|---|---|---|---|".to_string(),
    ];
    for (id, p) in filters() {
        let inv = match exposure_error(id, &p) {
            Some(e) if e <= TOL => "yes".to_string(),
            Some(e) if THRESHOLD_BASED.contains(&id) => format!("no, absolute threshold ({:.1}%)", e * 100.0),
            Some(e) => format!("**no** ({:.1}% of range)", e * 100.0),
            None => "refused".to_string(),
        };
        // A step edge only means something for a symmetric blur; a distort moves the edge instead.
        let edge = |v: f32| {
            if SYMMETRIC_BLURS.contains(&id) { edge_mean(id, &p, v).map_or_else(|| "–".to_string(), |g| format!("{g:.4}")) } else { "n/a".to_string() }
        };
        let col = edge_colour(id, &p).map_or_else(|| "–".to_string(), |r| format!("{:.0}%", r * 100.0));
        let row = format!("| `{id}` | {inv} | {} | {} | {col} |", edge(1.0), edge(4.0));
        println!("{row}");
        rows.push(row);
    }
    let report = format!(
        "# Linear-light filter behaviour (32-bit ACEScg)\n\n\
         `exposure invariant`: f(8x) == 8*f(x), so a grade's position in the stack does not change \
         the picture. `edge`: a symmetric blur across a 0→V step reads V/2 in linear light, which an \
         sRGB round-trip inside the filter (≈0.73·V) or a tone-map (a 0→4 reading far from 2.0) would \
         break; `n/a` is a filter that is not a symmetric blur, where the reading says nothing. \
         `colour at soft edge`: the share of an opaque patch's colour surviving where the filter made \
         it partly transparent, so less than 100% is a dark fringe against transparency; `–` means it \
         made no soft edge to measure.\n\n{}\n",
        rows.join("\n")
    );
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/linear_filter_behaviour.md");
    let _ = std::fs::write(&path, &report);
    println!("\nwritten to {}", path.display());
}
