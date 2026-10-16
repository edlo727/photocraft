//! Paths in View › Equirectangular Live Projection.
//!
//! `path.projectedSet` takes a path drawn in the perspective view (Pen, Freeform Pen,
//! Curvature Pen: view px) and stores it on the document in equirect pixels, so it displays and
//! rasterizes where it was drawn: each view segment is split into pieces short enough that a
//! cubic in equirect space follows the view curve, and x is unwrapped along the path (it may run
//! past the image's left or right edge across the ±180° seam; `"wrap": true` on
//! `path.toSelection` / `path.fill` counts those parts on the other side).
//! [`view_polylines`] draws any document path back in the view.

use photocraft_doc::{Knot, Path, Subpath};
use photocraft_geom::Point;
use serde_json::Value;

use crate::equirect::{ViewProjection, equirect_to_dir};
use crate::{EngineError, Result, Session};

const CMD: &str = "path.projectedSet";

fn lerp(a: Point, b: Point, t: f64) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// `[p0, c1, c2, p1]` split into `k` equal-parameter pieces.
fn split(seg: [Point; 4], k: usize) -> Vec<[Point; 4]> {
    let mut out = Vec::with_capacity(k);
    let mut rest = seg;
    for i in 0..k {
        let t = 1.0 / (k - i) as f64;
        if i + 1 == k {
            out.push(rest);
            break;
        }
        let [p0, c1, c2, p1] = rest;
        let (a, b, c) = (lerp(p0, c1, t), lerp(c1, c2, t), lerp(c2, p1, t));
        let (d, e) = (lerp(a, b, t), lerp(b, c, t));
        let m = lerp(d, e, t);
        out.push([p0, a, d, m]);
        rest = [m, e, c, p1];
    }
    out
}

/// View px → equirect px, x unwrapped to lie within half an image of `near`.
fn to_eq(proj: &ViewProjection, q: Point, iw: f64, ih: f64, near: Option<f64>) -> Point {
    let [mut u, v] = proj.view_to_equirect(q.x, q.y, iw, ih);
    if let Some(n) = near {
        u += iw * ((n - u) / iw).round();
    }
    Point::new(u, v)
}

/// A view-space path in equirect pixels (see the module docs).
pub fn view_path_to_equirect(path: &Path, proj: &ViewProjection, iw: f64, ih: f64) -> Path {
    let mut subs = Vec::new();
    for sp in &path.subpaths {
        let segs = sp.segments();
        if segs.is_empty() {
            let knots = sp.knots.iter().map(|k| {
                let a = to_eq(proj, k.anchor, iw, ih, None);
                Knot { anchor: a, in_ctrl: a, out_ctrl: a, smooth: k.smooth }
            });
            subs.push(Subpath { closed: sp.closed, knots: knots.collect(), op: sp.op });
            continue;
        }
        // Pieces of at most ~48 view px, each remembering whether it starts at an original knot.
        let mut pieces: Vec<([Point; 4], Option<usize>)> = Vec::new();
        for (i, s) in segs.iter().enumerate() {
            let len = (s[0].x - s[1].x).hypot(s[0].y - s[1].y) + (s[1].x - s[2].x).hypot(s[1].y - s[2].y) + (s[2].x - s[3].x).hypot(s[2].y - s[3].y);
            let k = ((len / 48.0).ceil() as usize).clamp(1, 64);
            for (j, piece) in split(*s, k).into_iter().enumerate() {
                pieces.push((piece, (j == 0).then_some(i)));
            }
        }
        let mut knots: Vec<Knot> = Vec::with_capacity(pieces.len() + 1);
        let mut near: Option<f64> = None;
        let mut pending_in: Option<Point> = None;
        let _ = &pending_in;
        for (piece, orig) in &pieces {
            let a = to_eq(proj, piece[0], iw, ih, near);
            near = Some(a.x);
            let smooth = orig.map_or(true, |i| sp.knots[i].smooth);
            let in_ctrl = pending_in.take().unwrap_or_else(|| if sp.closed { a } else { to_eq(proj, sp.knots[0].in_ctrl, iw, ih, near) });
            knots.push(Knot { anchor: a, in_ctrl, out_ctrl: to_eq(proj, piece[1], iw, ih, near), smooth });
            pending_in = Some(to_eq(proj, piece[2], iw, ih, near));
        }
        let last = pieces.last().map(|p| p.0[3]).unwrap_or_default();
        if sp.closed {
            // The closing piece's handle arrives at the first knot: unwrapped next to it.
            if let (Some(k0), Some((piece, _))) = (knots.first().map(|k| k.anchor.x), pieces.last()) {
                knots[0].in_ctrl = to_eq(proj, piece[2], iw, ih, Some(k0));
            }
        } else {
            let a = to_eq(proj, last, iw, ih, near);
            let nk = sp.knots.last().copied().unwrap_or_else(|| Knot::corner(last.x, last.y));
            near = Some(a.x);
            knots.push(Knot { anchor: a, in_ctrl: pending_in.unwrap_or(a), out_ctrl: to_eq(proj, nk.out_ctrl, iw, ih, near), smooth: nk.smooth });
        }
        subs.push(Subpath { closed: sp.closed, knots, op: sp.op });
    }
    Path { subpaths: subs, fill_rule: path.fill_rule, inverted: path.inverted }
}

/// `path.projectedSet` — `path.set` with the path in view px and the view's `projection`.
pub fn projected_set(s: &mut Session, p: &Value) -> Result<Value> {
    let proj = crate::projected_transform::projection_of(p, CMD)?;
    let path = crate::vector_cmds::parse_path(p.get("path").ok_or_else(|| EngineError::BadParams { cmd: CMD.into(), msg: "missing `path`".into() })?)
        .map_err(|e| EngineError::BadParams { cmd: CMD.into(), msg: e })?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let (iw, ih) = (f64::from(d.doc.size.width), f64::from(d.doc.size.height));
    let eq = view_path_to_equirect(&path, &proj, iw, ih);
    let mut q = p.clone();
    if let Some(m) = q.as_object_mut() {
        m.remove("projection");
    }
    q["path"] = crate::vector_cmds::path_json(&eq);
    crate::vector_cmds::path_set(s, &q)
}

/// Document path `path` (equirect px) as view-space polylines (view px), split where it goes
/// behind the camera; `true` when a piece closes on itself.
pub fn view_polylines(path: &Path, proj: &ViewProjection, iw: f64, ih: f64) -> Vec<(Vec<[f64; 2]>, bool)> {
    let mut out = Vec::new();
    for sp in &path.subpaths {
        let segs = sp.segments();
        let mut line: Vec<[f64; 2]> = Vec::new();
        let mut whole = true;
        let mut flush = |line: &mut Vec<[f64; 2]>, out: &mut Vec<(Vec<[f64; 2]>, bool)>| {
            if line.len() >= 2 {
                out.push((std::mem::take(line), false));
            } else {
                line.clear();
            }
        };
        if segs.is_empty() {
            for k in &sp.knots {
                if let Some(v) = proj.dir_to_view(equirect_to_dir(k.anchor.x, k.anchor.y, iw, ih)) {
                    out.push((vec![v], false));
                }
            }
            continue;
        }
        for s in &segs {
            let n = 24;
            for i in 0..=n {
                if i == 0 && !line.is_empty() {
                    continue;
                }
                let t = i as f64 / n as f64;
                let mt = 1.0 - t;
                let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
                let (x, y) = (a * s[0].x + b * s[1].x + c * s[2].x + d * s[3].x, a * s[0].y + b * s[1].y + c * s[2].y + d * s[3].y);
                match proj.dir_to_view(equirect_to_dir(x, y, iw, ih)) {
                    Some(v) => line.push(v),
                    None => {
                        whole = false;
                        flush(&mut line, &mut out);
                    }
                }
            }
        }
        let closed = sp.closed && whole;
        if line.len() >= 2 {
            out.push((line, closed));
        }
        let _ = &mut flush;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::equirect::Camera;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 4096, "height": 2048, "depth": 32})).unwrap();
        s
    }

    #[test]
    fn a_view_square_becomes_a_selection_where_it_was_drawn_even_across_the_seam() {
        let mut s = session();
        // Looking at the seam (yaw 180°): the square straddles x = 0 / 4096.
        let view = json!({"yaw": 180.0, "pitch": 10.0, "fov": 60.0, "width": 800, "height": 600});
        let vp = ViewProjection::new(Camera { yaw: 180.0, pitch: 10.0, fov: 60.0 }, 800.0, 600.0);
        let sq = json!({"subpaths": [{"closed": true, "knots": [[300, 200], [500, 200], [500, 400], [300, 400]]}]});
        s.execute(CMD, json!({"projection": view, "path": sq})).unwrap();
        let path = s.active().unwrap().doc.work_path.clone().expect("a work path");
        assert!(path.subpaths[0].knots.len() > 4, "subdivided");
        // Displayed back in the view: the same square.
        let lines = view_polylines(&path, &vp, 4096.0, 2048.0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].1, "closed");
        for v in &lines[0].0 {
            let on_edge = (v[0] - 300.0).abs() < 1.0 || (v[0] - 500.0).abs() < 1.0 || (v[1] - 200.0).abs() < 1.0 || (v[1] - 400.0).abs() < 1.0;
            assert!(on_edge, "the view square's outline: {v:?}");
        }
        // Make Selection (wrapped) selects both sides of the seam, as in the view.
        s.execute("path.toSelection", json!({"wrap": true})).unwrap();
        let sel = s.active().unwrap().doc.selection.clone().unwrap();
        let mut m = [0f32; 1];
        let mut at = |vx: f64, vy: f64| {
            let [u, v] = vp.view_to_equirect(vx, vy, 4096.0, 2048.0);
            sel.read_pixel(u as i32, v as i32, &mut m);
            m[0]
        };
        assert!(at(350.0, 300.0) > 0.9 && at(450.0, 300.0) > 0.9, "both sides of the seam");
        assert!(at(250.0, 300.0) < 0.1 && at(550.0, 300.0) < 0.1 && at(400.0, 150.0) < 0.1, "only inside");
    }
}

/// `path` (equirect px) turned across the sphere by `q` (the Path Selection drag in projection):
/// every anchor and handle moves as a direction, x unwrapped along each subpath.
pub fn rotate_path(path: &Path, q: &crate::equirect::Rot, iw: f64, ih: f64) -> Path {
    let turn = |p: Point, near: Option<f64>| -> Point {
        let [mut u, v] = crate::equirect::dir_to_equirect(crate::equirect::rot(q, equirect_to_dir(p.x, p.y, iw, ih)), iw, ih);
        if let Some(n) = near {
            u += iw * ((n - u) / iw).round();
        }
        Point::new(u, v)
    };
    let mut out = path.clone();
    for sp in &mut out.subpaths {
        let mut near = None;
        for k in &mut sp.knots {
            let a = turn(k.anchor, near);
            near = Some(a.x);
            *k = Knot { anchor: a, in_ctrl: turn(k.in_ctrl, near), out_ctrl: turn(k.out_ctrl, near), smooth: k.smooth };
        }
    }
    out
}

/// View px → equirect px with x unwrapped next to `near` (Direct Selection drags).
pub fn view_to_equirect_near(proj: &ViewProjection, v: [f64; 2], iw: f64, ih: f64, near: f64) -> [f64; 2] {
    let p = to_eq(proj, Point::new(v[0], v[1]), iw, ih, Some(near));
    [p.x, p.y]
}
