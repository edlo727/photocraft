//! Live projection Gradient and Paint Bucket over the whole latlong: the gradient's position is
//! measured on the sphere (so it continues past the view and across the ±180° seam), and the
//! bucket floods the equirect itself, carried across the seam. Both respect the selection.

use photocraft_algo::paint::GradientShape;
use photocraft_algo::selection::Region;
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::equirect::ViewProjection;
use crate::{EngineError, Result, Session};

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn sub(a: [f64; 3], b: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] - k * b[0], a[1] - k * b[1], a[2] - k * b[2]]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: [f64; 3]) -> Option<[f64; 3]> {
    let l = dot(a, a).sqrt();
    (l > 1e-12).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

/// The live projection's Gradient over the whole sphere, measured by angle so it wraps around
/// consistently. The axis is the great circle through the start and end directions; `c` is the
/// unit tangent of that circle at their midpoint, so bands of equal `t` are the small circles
/// `asin(d·c) = const` crossing the axis at right angles (a vertical drag at the horizon gives
/// horizontal bands all the way round). `e` is the across direction (the view's right-hand
/// perpendicular of the drag, for Diamond and Angle).
pub(crate) struct SphereGradient {
    a: [f64; 3],
    c: [f64; 3],
    e: [f64; 3],
    th_a: f64,
    len: f64,
    radial_len: f64,
    shape: GradientShape,
}

impl SphereGradient {
    pub(crate) fn new(vp: &ViewProjection, from: [f64; 2], to: [f64; 2], shape: GradientShape) -> Option<Self> {
        let a = vp.view_to_dir(from[0], from[1]);
        let b = vp.view_to_dir(to[0], to[1]);
        // The axis great circle's normal; `fwd` (the view's forward direction in that circle's
        // plane) is where the bands are centred, so `c` is the circle's tangent there: a drag
        // through the view centre's vertical gives bands that are exactly latitudes.
        let n = norm(cross(a, b))?;
        let f = vp.view_to_dir(vp.w / 2.0, vp.h / 2.0);
        let fwd = norm(sub(f, n, dot(f, n))).or_else(|| norm([a[0] + b[0], a[1] + b[1], a[2] + b[2]]))?;
        // Level bands: where the axis circle crosses the horizon, its tangent is the steepest
        // climb (world up within the circle's plane), so a vertical drag gives latitudes at any
        // view pitch. A drag along the horizon itself centres on the view's forward instead.
        let up = [0.0, 1.0, 0.0];
        let up_in = sub(up, n, dot(up, n));
        let mut c = if dot(up_in, up_in) > 0.04 { norm(up_in)? } else { norm(cross(n, fwd))? };
        if dot(c, sub(b, a, 1.0)) < 0.0 {
            c = [-c[0], -c[1], -c[2]];
        }
        let (th_a, th_b) = (dot(a, c).clamp(-1.0, 1.0).asin(), dot(b, c).clamp(-1.0, 1.0).asin());
        let len = (th_b - th_a).max(1e-9);
        let radial_len = dot(a, b).clamp(-1.0, 1.0).acos().max(1e-9);
        // Across: the axis circle's normal, signed like the flat tool's (−uy, ux).
        let mut e = n;
        let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
        let l = (dx * dx + dy * dy).sqrt().max(1e-9);
        let p = vp.view_to_dir(from[0] - dy / l, from[1] + dx / l);
        if dot(sub(p, a, 1.0), e) < 0.0 {
            e = [-e[0], -e[1], -e[2]];
        }
        Some(Self { a, c, e, th_a, len, radial_len, shape })
    }

    pub(crate) fn t(&self, d: [f64; 3]) -> f32 {
        let along = dot(d, self.c).clamp(-1.0, 1.0).asin() - self.th_a;
        let across = dot(d, self.e).clamp(-1.0, 1.0).asin();
        let t = match self.shape {
            GradientShape::Linear => along / self.len,
            GradientShape::Radial => dot(d, self.a).clamp(-1.0, 1.0).acos() / self.radial_len,
            GradientShape::Reflected => along.abs() / self.len,
            GradientShape::Diamond => (along.abs() + across.abs()) / self.len,
            GradientShape::Angle => (across.atan2(along) / std::f64::consts::TAU).rem_euclid(1.0),
        };
        t.clamp(0.0, 1.0) as f32
    }
}

fn bad(msg: &str) -> EngineError {
    EngineError::BadParams { cmd: "paint.projectedTool".into(), msg: msg.into() }
}

/// Gradient from view `from` to `to` over the whole document (the selection's bounds if any).
pub(crate) fn gradient(s: &mut Session, params: &Value, vp: &ViewProjection, from: [f64; 2], to: [f64; 2]) -> Result<Rect> {
    let g = crate::paint_cmds::gradient_setup(s, params)?;
    let sg = SphereGradient::new(vp, from, to, g.shape).ok_or_else(|| bad("the gradient needs two different points"))?;
    s.edit("Gradient", |doc, active| {
        let (iw, ih) = (doc.size.width as f64, doc.size.height as f64);
        let sel = doc.selection.clone();
        let area = sel.as_ref().map(|m| m.content_bounds()).filter(|r| !r.is_empty()).unwrap_or_else(|| doc.bounds()).intersect(&doc.bounds());
        // Per-column/row trig tables: one sin/cos pair per column and row, not per pixel.
        let cols: Vec<(f64, f64)> = (0..doc.size.width).map(|x| ((x as f64 + 0.5) / iw - 0.5) * std::f64::consts::TAU).map(|l| l.sin_cos()).collect();
        let rows: Vec<(f64, f64)> = (0..doc.size.height).map(|y| ((0.5 - (y as f64 + 0.5) / ih) * std::f64::consts::PI).sin_cos()).collect();
        let t_at = |x: i32, y: i32| {
            let (sl, cl) = rows[y.clamp(0, rows.len() as i32 - 1) as usize];
            let (so, co) = cols[x.clamp(0, cols.len() as i32 - 1) as usize];
            sg.t([cl * so, sl, cl * co])
        };
        let (surf, lock) = crate::channel_cmds::target_surface(doc, *active, params)?;
        photocraft_algo::paint::paint_gradient_by(surf, area, t_at, &g.stops, g.reverse, g.opacity, g.blend, g.dither, sel.as_ref(), lock);
        Ok(area)
    })
}

/// The Gradient's live preview on the view-sized scratch: the same sphere formula through each
/// view pixel's direction, so the preview is the committed result.
pub(crate) fn gradient_view(s: &mut Session, params: &Value, vp: &ViewProjection, from: [f64; 2], to: [f64; 2]) -> Result<()> {
    let g = crate::paint_cmds::gradient_setup(s, params)?;
    let Some(sg) = SphereGradient::new(vp, from, to, g.shape) else { return Ok(()) };
    let vp = *vp;
    s.edit("Gradient", |doc, active| {
        let sel = doc.selection.clone();
        let area = sel.as_ref().map(|m| m.content_bounds()).filter(|r| !r.is_empty()).unwrap_or_else(|| doc.bounds()).intersect(&doc.bounds());
        let t_at = |x: i32, y: i32| sg.t(vp.view_to_dir(f64::from(x) + 0.5, f64::from(y) + 0.5));
        let (surf, lock) = crate::channel_cmds::target_surface(doc, *active, params)?;
        photocraft_algo::paint::paint_gradient_by(surf, area, t_at, &g.stops, g.reverse, g.opacity, g.blend, g.dither, sel.as_ref(), lock);
        Ok(())
    })
}

/// Paint Bucket at equirect pixel `seed`, flooding across the ±180° seam.
pub(crate) fn bucket(s: &mut Session, params: &Value, seed: (i32, i32)) -> Result<Rect> {
    let Some(b) = crate::paint_cmds::bucket_setup(s, params)? else {
        // Pattern fills: the regular bucket on the latlong (no seam carry).
        let mut p = params.clone();
        p["x"] = json!(seed.0);
        p["y"] = json!(seed.1);
        s.execute("paint.bucket", p)?;
        return Ok(s.active().map(|d| d.doc.bounds()).unwrap_or(Rect::EMPTY));
    };
    let sampled = if b.all { Some(crate::selection_cmds::sample_rgba8(s, true)?) } else { None };
    s.edit("Paint Bucket", |doc, active| {
        let area = doc.bounds();
        let sel = doc.selection.clone();
        let (surf, lock) = crate::channel_cmds::target_surface(doc, *active, params)?;
        let (rarea, mut img) = match sampled {
            Some((a, img)) => (a, img),
            None => (area, photocraft_algo::selection::rgba8_image(surf, area)),
        };
        let Some(region) = seam_region(&mut img, rarea, seed, b.tolerance, b.contiguous, b.anti_alias) else { return Ok(Rect::EMPTY) };
        let bbox = region.bbox;
        photocraft_algo::paint::bucket_fill_region_inks(surf, &region, b.opacity, sel.as_ref(), lock, |_, _| b.color, b.inks, b.mode);
        surf.prune();
        Ok(bbox)
    })
}

/// The wand region from `seed`, continued across the left/right edges (the ±180° seam): a
/// pixel on one edge next to a filled pixel on the other, similar to the seed colour, seeds
/// another flood (compared against the original seed colour). `img` is modified in place.
pub(crate) fn seam_region(img: &mut [[u8; 4]], area: Rect, seed: (i32, i32), tol: f32, contiguous: bool, aa: bool) -> Option<Region> {
    let first = photocraft_algo::selection::wand_region(img, area, seed, tol, contiguous, aa)?;
    if !contiguous || area.width() < 2 {
        return Some(first);
    }
    let (w, h) = (area.width() as usize, area.height() as usize);
    let target = img[(seed.1 - area.y0) as usize * w + (seed.0 - area.x0) as usize];
    let t = (tol + 1e-3).floor().max(0.0) as i32;
    let similar = |p: [u8; 4]| (target[3] == 0 && p[3] == 0) || (0..4).all(|c| (p[c] as i32 - target[c] as i32).abs() <= t);
    // The union, full-size, built only once a second flood is needed.
    let mut union: Option<Vec<u8>> = None;
    let mut regions = vec![first];
    let covered = |regions: &[Region], union: &Option<Vec<u8>>, x: i32, y: i32| -> bool {
        match union {
            Some(u) => u[(y - area.y0) as usize * w + (x - area.x0) as usize] > 0,
            None => regions.iter().any(|r| r.at(x, y) > 0.0),
        }
    };
    let (xl, xr) = (area.x0, area.x1 - 1);
    for _ in 0..64 {
        let mut next = None;
        'scan: for yy in 0..h {
            let y = area.y0 + yy as i32;
            for (from, to) in [(xl, xr), (xr, xl)] {
                if covered(&regions, &union, from, y) && !covered(&regions, &union, to, y) && similar(img[yy * w + (to - area.x0) as usize]) {
                    next = Some((to, y));
                    break 'scan;
                }
            }
        }
        let Some((x, y)) = next else { break };
        let i = (y - area.y0) as usize * w + (x - area.x0) as usize;
        let keep = img[i];
        img[i] = target;
        let r = photocraft_algo::selection::wand_region(img, area, (x, y), tol, contiguous, aa);
        img[i] = keep;
        let Some(r) = r else { break };
        let u = union.get_or_insert_with(|| {
            let mut u = vec![0u8; w * h];
            merge(&mut u, area, &regions[0]);
            u
        });
        merge(u, area, &r);
        regions.push(r);
    }
    match union {
        None => regions.pop(),
        Some(mask) => {
            let bbox = regions.iter().fold(Rect::EMPTY, |acc, r| if acc.is_empty() { r.bbox } else { acc.union(&r.bbox) });
            let bw = bbox.width() as usize;
            let mut m = vec![0u8; bw * bbox.height() as usize];
            for y in 0..bbox.height() as usize {
                let src = (bbox.y0 - area.y0) as usize + y;
                let s0 = src * w + (bbox.x0 - area.x0) as usize;
                m[y * bw..(y + 1) * bw].copy_from_slice(&mask[s0..s0 + bw]);
            }
            Some(Region { bbox, mask: m })
        }
    }
}

fn merge(u: &mut [u8], area: Rect, r: &Region) {
    let w = area.width() as usize;
    let bw = r.bbox.width() as usize;
    for y in 0..r.bbox.height() as usize {
        let row = ((r.bbox.y0 - area.y0) as usize + y) * w + (r.bbox.x0 - area.x0) as usize;
        for x in 0..bw {
            let v = r.mask[y * bw + x];
            if v > u[row + x] {
                u[row + x] = v;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equirect::Camera;

    /// A 2048×1024 F32 latlong, black; a red (selection-free) gradient covers the whole sphere.
    fn f32_doc() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 2048, "height": 1024, "depth": 32})).unwrap();
        s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        s
    }

    fn px(s: &Session, x: i32, y: i32) -> Vec<f32> {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().pixel(x, y)
    }

    const SHAPES: [GradientShape; 5] = [GradientShape::Linear, GradientShape::Radial, GradientShape::Reflected, GradientShape::Diamond, GradientShape::Angle];

    #[test]
    fn sphere_gradient_is_close_to_the_flat_preview_near_the_drag() {
        let vp = ViewProjection::new(Camera { yaw: 30.0, pitch: 20.0, fov: 60.0 }, 800.0, 600.0);
        let g = SphereGradient::new(&vp, [400.0, 400.0], [400.0, 200.0], GradientShape::Linear).unwrap();
        for (x, y) in [(400.5, 300.5), (400.5, 380.5), (400.5, 220.5)] {
            let flat = photocraft_algo::paint::tool_gradient_t(GradientShape::Linear, (400.0, 400.0), (400.0, 200.0), x as f32, y as f32);
            let t = g.t(vp.view_to_dir(x, y));
            assert!((t - flat).abs() < 0.03, "({x},{y}): {t} vs flat {flat}");
        }
    }

    /// Ed's case: a vertical drag at yaw 0 from pitch −10° to +10° is level: every latlong row
    /// is constant (the value depends only on latitude), also from a view pitched down 5°.
    #[test]
    fn a_vertical_drag_at_the_horizon_is_level_all_round() {
        for pitch in [0.0, -5.0] {
            let vp = ViewProjection::new(Camera { yaw: 0.0, pitch, fov: 90.0 }, 800.0, 600.0);
            let at = |deg: f64| {
                let r = deg.to_radians();
                vp.dir_to_view([0.0, r.sin(), r.cos()]).unwrap()
            };
            let g = SphereGradient::new(&vp, at(-10.0), at(10.0), GradientShape::Linear).unwrap();
            let mut last = -1.0;
            for v in (0..512).step_by(8) {
                let v = v as f64 + 0.5;
                let t0 = g.t(crate::equirect::equirect_to_dir(0.5, v, 1024.0, 512.0));
                for u in (0..1024).step_by(16) {
                    let t = g.t(crate::equirect::equirect_to_dir(u as f64 + 0.5, v, 1024.0, 512.0));
                    assert!((t - t0).abs() < 1e-4, "pitch {pitch}, row {v}: {t} at u={u} vs {t0}");
                }
                // Brighter going up: decreasing with the row.
                if last >= 0.0 {
                    assert!(t0 <= last + 1e-6);
                }
                last = t0;
            }
            // ±10° are the ends; the horizon is the middle.
            let lat = |deg: f64| g.t([0.7 * deg.to_radians().cos(), deg.to_radians().sin(), -0.71 * deg.to_radians().cos()]);
            assert!(lat(-10.0) < 0.01 && lat(10.0) > 0.99 && (lat(0.0) - 0.5).abs() < 0.01, "{} {} {}", lat(-10.0), lat(0.0), lat(10.0));
        }
    }

    /// The whole latlong has no cliffs: neighbouring pixels differ by little, except at the
    /// Angle gradient's own wrap and the two horizon points where the flat gradient converges.
    #[test]
    fn sphere_gradient_is_continuous_over_the_whole_latlong() {
        let vp = ViewProjection::new(Camera { yaw: 0.0, pitch: 0.0, fov: 90.0 }, 800.0, 600.0);
        let (w, h) = (1024usize, 512usize);
        for shape in SHAPES {
            let g = SphereGradient::new(&vp, [400.0, 550.0], [400.0, 100.0], shape).unwrap();
            let img: Vec<f32> =
                (0..w * h).map(|i| g.t(crate::equirect::equirect_to_dir((i % w) as f64 + 0.5, (i / w) as f64 + 0.5, w as f64, h as f64))).collect();
            if let Ok(out) = std::env::var("GRADIENT_PGM") {
                let mut bytes = format!("P5 {w} {h} 255\n").into_bytes();
                bytes.extend(img.iter().map(|t| (t * 255.0).round() as u8));
                std::fs::write(format!("{out}-{shape:?}.pgm"), bytes).unwrap();
            }
            let mut big = 0;
            for y in 0..h {
                for x in 0..w {
                    let a = img[y * w + x];
                    for b in [img[y * w + (x + 1) % w], img[(y + 1).min(h - 1) * w + x]] {
                        let mut j = (a - b).abs();
                        if shape == GradientShape::Angle {
                            j = j.min(1.0 - j);
                        }
                        if j > 0.25 {
                            big += 1;
                        }
                    }
                }
            }
            assert!(big < 40, "{shape:?}: {big} jumps over 0.25");
        }
    }

    #[test]
    fn projected_gradient_fills_the_whole_f32_latlong_including_behind() {
        let mut s = f32_doc();
        let view = json!({"yaw": 0.0, "pitch": 0.0, "fov": 90.0, "width": 800, "height": 600});
        let p = json!({"command": "paint.gradient", "params": {"target": "pixels", "colors": ["#ff0000", "#0000ff"]}, "projection": view, "points": [[300, 300, 1.0], [500, 300, 1.0]]});
        s.execute("paint.projectedTool", p).unwrap();
        // Directly behind the camera (the seam column) is painted, finite, and at an end colour.
        for (x, y) in [(0, 512), (2047, 512), (1024, 10), (1024, 1010), (1500, 512)] {
            let c = px(&s, x, y);
            assert!(c.iter().all(|v| v.is_finite()) && c[0] + c[2] > 0.9, "({x},{y}) {c:?}");
        }
    }

    #[test]
    fn projected_gradient_respects_the_selection() {
        let mut s = f32_doc();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 100, "height": 1024})).unwrap();
        let view = json!({"yaw": 0.0, "pitch": 0.0, "fov": 90.0, "width": 800, "height": 600});
        let p = json!({"command": "paint.gradient", "params": {"target": "pixels", "colors": ["#ff0000", "#ff0000"]}, "projection": view, "points": [[300, 300, 1.0], [500, 300, 1.0]]});
        s.execute("paint.projectedTool", p).unwrap();
        assert!(px(&s, 50, 512)[0] > 0.9, "inside the selection (behind the view)");
        assert!(px(&s, 1024, 512)[0] < 0.01, "outside the selection");
    }

    #[test]
    fn projected_bucket_floods_the_whole_latlong_and_across_the_seam() {
        let mut s = f32_doc();
        // A white vertical wall at x = 1000..1010 except the seam is open: the area left of the
        // wall (0..1000) and right of it (1010..2048) connect only through the ±180° seam.
        s.execute("select.rect", json!({"x": 1000, "y": 0, "width": 10, "height": 1024})).unwrap();
        s.execute("edit.fill", json!({"color": "#ffffff"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        s.tools.foreground = [1.0, 0.0, 0.0, 1.0];
        // Click at yaw −90° (u ≈ 512), far from the right half.
        let view = json!({"yaw": -90.0, "pitch": 0.0, "fov": 90.0, "width": 800, "height": 600});
        let p = json!({"command": "paint.bucket", "params": {"target": "pixels", "tolerance": 10, "contiguous": true, "contents": "foreground"}, "projection": view, "points": [[400, 300, 1.0]]});
        s.execute("paint.projectedTool", p).unwrap();
        let fg = px(&s, 512, 512);
        assert!(fg != vec![0.0, 0.0, 0.0, 1.0], "filled at the click: {fg:?}");
        assert_eq!(px(&s, 1500, 512), fg, "the right half filled across the seam");
        assert_eq!(px(&s, 1005, 512), vec![1.0, 1.0, 1.0, 1.0], "the wall is not filled");
    }
}
