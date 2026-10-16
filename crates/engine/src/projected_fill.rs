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
fn norm(a: [f64; 3]) -> Option<[f64; 3]> {
    let l = dot(a, a).sqrt();
    (l > 1e-12).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

/// The sphere gradient frame: start direction `a`, the tangent towards the end `ta`, the
/// across tangent `tc` (the view's right-hand perpendicular) and the a→b angle.
pub(crate) struct SphereGradient {
    a: [f64; 3],
    ta: [f64; 3],
    tc: [f64; 3],
    len: f64,
    shape: GradientShape,
}

impl SphereGradient {
    pub(crate) fn new(vp: &ViewProjection, from: [f64; 2], to: [f64; 2], shape: GradientShape) -> Option<Self> {
        let a = vp.view_to_dir(from[0], from[1]);
        let b = vp.view_to_dir(to[0], to[1]);
        let ta = norm(sub(b, a, dot(a, b)))?;
        let len = dot(a, b).clamp(-1.0, 1.0).acos().max(1e-9);
        // Screen perpendicular (−uy, ux) of the drag, as the flat tool measures `across`.
        let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
        let l = (dx * dx + dy * dy).sqrt().max(1e-9);
        let p = vp.view_to_dir(from[0] - dy / l, from[1] + dx / l);
        let tc0 = sub(sub(p, a, dot(p, a)), ta, dot(p, ta));
        let tc = norm(tc0)?;
        Some(Self { a, ta, tc, len, shape })
    }

    pub(crate) fn t(&self, d: [f64; 3]) -> f32 {
        let along = dot(d, self.ta).atan2(dot(d, self.a));
        let across = dot(d, self.tc).clamp(-1.0, 1.0).asin();
        let t = match self.shape {
            GradientShape::Linear => along / self.len,
            GradientShape::Radial => dot(d, self.a).clamp(-1.0, 1.0).acos() / self.len,
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
        photocraft_algo::paint::bucket_fill_region_inks(surf, &region, b.opacity, sel.as_ref(), lock, |_, _| b.color, b.inks);
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

    #[test]
    fn sphere_gradient_matches_the_flat_one_near_the_start() {
        let vp = ViewProjection::new(Camera { yaw: 0.0, pitch: 0.0, fov: 90.0 }, 800.0, 600.0);
        let g = SphereGradient::new(&vp, [300.0, 300.0], [500.0, 300.0], GradientShape::Linear).unwrap();
        assert!(g.t(vp.view_to_dir(300.0, 300.0)) < 0.01);
        assert!(g.t(vp.view_to_dir(500.0, 300.0)) > 0.99);
        let mid = g.t(vp.view_to_dir(400.0, 300.0));
        assert!((mid - 0.5).abs() < 0.05, "{mid}");
        // Perpendicular to the drag: the same t.
        let (a, b) = (g.t(vp.view_to_dir(400.0, 250.0)), g.t(vp.view_to_dir(400.0, 350.0)));
        assert!((a - b).abs() < 0.02, "{a} {b}");
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
