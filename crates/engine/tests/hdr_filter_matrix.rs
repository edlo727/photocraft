//! Measures how every registered `filter.*` command treats 32-bit ACEScg pixels: values above
//! 1.0 (a 4.0 highlight) and small negatives (−0.05, normal in ACEScg for saturated colours).
//! Report only — it never fails on a filter's behaviour, so it can run before any fix.
//! Opt-in: `cargo test -p photocraft-engine --test hdr_filter_matrix -- --ignored --nocapture`
//! The table is printed and written to `target/hdr_filter_matrix.md`.
use photocraft_doc::Rect;
use photocraft_engine::{Session, command_specs};
use serde_json::json;
use std::sync::mpsc;
use std::time::Duration;

const W: i32 = 64;
const H: i32 = 64;
/// The HDR patch (x/y 16..48) and the value written there.
const HOT: [f32; 4] = [4.0, 2.0, 0.5, 1.0];
/// The negative patch (rows 52..64, x 16..48).
const NEG: [f32; 4] = [-0.05, 0.3, -0.05, 1.0];
const DEADLINE: Duration = Duration::from_secs(60);

/// The test image: a 0..1 ramp on top, the HDR patch in the middle, a negative patch at the
/// bottom, 18 % grey elsewhere.
fn source_pixel(x: i32, y: i32) -> [f32; 4] {
    if y < 12 {
        let v = x as f32 / (W - 1) as f32;
        [v, v * 0.5, 1.0 - v, 1.0]
    } else if (16..48).contains(&x) && (16..48).contains(&y) {
        HOT
    } else if (16..48).contains(&x) && y >= 52 {
        NEG
    } else {
        [0.18, 0.18, 0.18, 1.0]
    }
}

fn fixture() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": W, "height": H, "depth": 32})).unwrap();
    s.edit("hdr fixture", |doc, active| {
        doc.icc_profile = Some(craft_cms::Builtin::AcesCg.profile().to_bytes());
        let surface = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let format = surface.format();
        let bytes: Vec<f32> =
            (0..H).flat_map(|y| (0..W).map(move |x| (x, y))).flat_map(|(x, y)| photocraft_raster::from_rgba(&format, source_pixel(x, y))).collect();
        surface.write_region(Rect::from_xywh(0, 0, W as u32, H as u32), &bytes);
        Ok(())
    })
    .unwrap();
    s
}

/// The active layer's pixels as straight RGBA; the flattened image if the filter replaced the
/// layer with something that has no surface (a smart object, say).
fn pixels(s: &Session) -> Vec<[f32; 4]> {
    let st = s.active().unwrap();
    if let Some(surface) = st.active_layer.and_then(|id| st.doc.layer(id)).and_then(|l| l.surface()) {
        let mut out = vec![[0.0; 4]; (W * H) as usize];
        surface.read_rgba_into(Rect::from_xywh(0, 0, W as u32, H as u32), &mut out);
        return out;
    }
    photocraft_compose::flatten(&st.doc).into_px()
}

struct Measure {
    max: f32,
    min: f32,
    hot_core: [f32; 3],
    neg_core: [f32; 3],
    non_finite: usize,
    changed: bool,
}

fn mean(px: &[[f32; 4]], xs: std::ops::Range<i32>, ys: std::ops::Range<i32>) -> [f32; 3] {
    let mut sum = [0.0f64; 3];
    let mut n = 0.0;
    for y in ys {
        for x in xs.clone() {
            let p = px[(y * W + x) as usize];
            for c in 0..3 {
                sum[c] += f64::from(p[c]);
            }
            n += 1.0;
        }
    }
    sum.map(|v| (v / n) as f32)
}

fn measure(before: &[[f32; 4]], after: &[[f32; 4]]) -> Measure {
    let rgb = after.iter().flat_map(|p| p[..3].iter().copied());
    let finite: Vec<f32> = rgb.clone().filter(|v| v.is_finite()).collect();
    Measure {
        max: finite.iter().copied().fold(f32::MIN, f32::max),
        min: finite.iter().copied().fold(f32::MAX, f32::min),
        hot_core: mean(after, 28..36, 28..36),
        neg_core: mean(after, 28..36, 56..62),
        non_finite: rgb.filter(|v| !v.is_finite()).count(),
        changed: before.len() != after.len() || before.iter().zip(after).any(|(a, b)| a.iter().zip(b).any(|(x, y)| (x - y).abs() > 1e-6)),
    }
}

fn verdict(m: &Measure) -> &'static str {
    if m.non_finite > 0 {
        "NaN/Inf"
    } else if !m.changed {
        "no change at defaults"
    } else if m.max <= 1.0 + 1e-4 && m.min >= -1e-6 {
        "CLAMPS to 0..1"
    } else if m.max <= 1.0 + 1e-4 {
        "CLIPS above 1"
    } else if m.hot_core[0] < 1.5 && m.hot_core[0] > -1.5 {
        "highlight crushed (max survives elsewhere)"
    } else if m.min >= -1e-6 {
        "keeps >1, clamps negatives"
    } else {
        "keeps HDR + negatives"
    }
}

fn fmt3(v: [f32; 3]) -> String {
    format!("{:.3}, {:.3}, {:.3}", v[0], v[1], v[2])
}

#[test]
#[ignore = "report: run with --ignored --nocapture"]
fn hdr_filter_matrix() {
    let before = pixels(&fixture());
    let src = measure(&before, &before);
    let mut rows = vec![
        "| Filter | Verdict | max | min | HDR core (in 4, 2, 0.5) | negative core (in −0.05, 0.3, −0.05) |".to_string(),
        "|---|---|---|---|---|---|".to_string(),
        format!("| *(source)* | – | {:.3} | {:.3} | {} | {} |", src.max, src.min, fmt3(src.hot_core), fmt3(src.neg_core)),
    ];
    let mut ids: Vec<&str> = command_specs().iter().map(|c| c.id).filter(|id| id.starts_with("filter.")).collect();
    ids.sort_unstable();
    ids.dedup();
    let mut tally = std::collections::BTreeMap::<String, usize>::new();
    for id in ids {
        let (tx, rx) = mpsc::channel();
        let owned = id.to_string();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(move || {
                let mut s = fixture();
                s.execute(&owned, json!({})).map(|_| pixels(&s)).map_err(|e| e.to_string())
            });
            let _ = tx.send(r.unwrap_or_else(|_| Err("panicked".into())));
        });
        let row = match rx.recv_timeout(DEADLINE) {
            Ok(Ok(after)) => {
                let m = measure(&before, &after);
                let v = verdict(&m);
                *tally.entry(v.to_string()).or_default() += 1;
                let nf = if m.non_finite > 0 { format!(" ({} non-finite)", m.non_finite) } else { String::new() };
                format!("| `{id}` | {v}{nf} | {:.3} | {:.3} | {} | {} |", m.max, m.min, fmt3(m.hot_core), fmt3(m.neg_core))
            }
            Ok(Err(e)) => {
                *tally.entry("error".into()).or_default() += 1;
                let e: String = e.replace('|', "/").replace('\n', " ").chars().take(90).collect();
                format!("| `{id}` | error: {e} | | | | |")
            }
            Err(_) => {
                *tally.entry("hung".into()).or_default() += 1;
                format!("| `{id}` | hung (>{}s) | | | | |", DEADLINE.as_secs())
            }
        };
        println!("{row}");
        rows.push(row);
    }
    let summary: Vec<String> = tally.iter().map(|(k, n)| format!("- {k}: {n}")).collect();
    let report = format!("# 32-bit ACEScg filter matrix\n\nDefault parameters, 64×64 32-bit ACEScg layer.\n\n{}\n\n{}\n", summary.join("\n"), rows.join("\n"));
    println!("\n{report}");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/hdr_filter_matrix.md");
    let _ = std::fs::write(&path, &report);
    println!("written to {}", path.display());
}

/// One filter at its defaults on the fixture.
fn run(id: &str) -> Measure {
    let mut s = fixture();
    let before = pixels(&s);
    s.execute(id, json!({})).unwrap();
    measure(&before, &pixels(&s))
}

/// The filters fixed after the first report keep 32-bit values (unlike the report, this one fails).
#[test]
fn fixed_filters_keep_32_bit_values() {
    // A warp moves pixels: the 4.0 highlight survives.
    let m = run("filter.adaptiveWideAngle");
    assert!(m.max > 1.5, "Adaptive Wide Angle clipped the highlight: max {}", m.max);
    // Relief across the edge of the highlight is brighter than 1.0, and never negative.
    let m = run("filter.stylize.emboss");
    assert!(m.max > 1.0 + 1e-3 && m.min >= 0.0, "Emboss: max {}, min {}", m.max, m.min);
    // Dots take the colour under them, highlight included.
    let m = run("filter.pixelate.pointillize");
    assert!(m.max > 1.5, "Pointillize clipped the highlight: max {}", m.max);
    // Highlights invert to black, not below it.
    let m = run("filter.stylize.solarize");
    assert!(m.hot_core.iter().all(|&v| v >= 0.0), "Solarize went negative: {:?}", m.hot_core);
    // Wide-gamut negatives survive being lit.
    let m = run("filter.render.lightingEffects");
    assert!(m.min < 0.0, "Lighting Effects clamped negatives: min {}", m.min);
}
