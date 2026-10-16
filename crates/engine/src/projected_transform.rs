//! Move, Free Transform and Paste in View › Equirectangular Live Projection.
//!
//! - `layer.projectedRotate`: the Move tool. The active layer (or the selected part of it, with
//!   the selection) turns across the sphere by `rotation` (row-major 3×3, taking where the drag
//!   started to where it ended); seams and poles need nothing special, every pixel is a
//!   direction.
//! - `layer.projectedPlace`: Free Transform and Paste. A view-space source rect (`rect`, view
//!   px; or the clipboard image) is mapped onto a view-space quad (`quad`, four corners tl, tr,
//!   br, bl: scale, rotate, skew, distort and perspective are all one homography), and every
//!   equirect pixel the quad covers looks up through the view and the inverse homography.
//!   `source: "layer"` moves the active layer's content inside `rect` (clipped by the selection)
//!   and clears where it was; `source: "clipboard"` pastes the clipboard as a new layer.
//!
//! The samplers keep `inf` / `NaN` pixels from spreading: a bilinear tap set holding one falls
//! back to the nearest pixel, so a moved HDR sun stays one bright pixel.

use photocraft_color::PixelFormat;
use photocraft_doc::Layer;
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::equirect::{Camera, Rot, ViewProjection, dir_to_equirect, equirect_to_dir, rot, rot_from_slice, view_rect_to_equirect};
use crate::{EngineError, Result, Session};

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// The view projection in `p["projection"]`.
pub fn projection_of(p: &Value, cmd: &str) -> Result<ViewProjection> {
    let q = p.get("projection").ok_or_else(|| bad(cmd, "missing `projection`"))?;
    let g = |k: &str, d: f64| q.get(k).and_then(Value::as_f64).unwrap_or(d);
    let (w, h) = (g("width", 0.0), g("height", 0.0));
    if !(w >= 1.0 && h >= 1.0 && w <= 16384.0 && h <= 16384.0) {
        return Err(bad(cmd, "`projection.width` and `height` must be 1..=16384"));
    }
    Ok(ViewProjection::new(Camera { yaw: g("yaw", 0.0), pitch: g("pitch", 0.0), fov: g("fov", 90.0) }, w, h))
}

/// A row band `[y0, y1)` of a surface, full width, sampled with wrap-around in x.
struct Band {
    buf: Vec<f32>,
    w: i64,
    y0: i64,
    y1: i64,
    n: usize,
}

impl Band {
    fn read(s: &Surface, w: i32, y0: i32, y1: i32) -> Band {
        Band { buf: s.read_region(Rect::new(0, y0, w, y1)), w: i64::from(w), y0: i64::from(y0), y1: i64::from(y1), n: s.channels() }
    }
    fn tap(&self, x: i64, y: i64) -> usize {
        let (x, y) = (x.rem_euclid(self.w), y.clamp(self.y0, self.y1 - 1) - self.y0);
        ((y * self.w + x) as usize) * self.n
    }
    /// Bilinear at pixel-centre position (x, y); a non-finite tap makes that channel nearest.
    fn sample(&self, x: f64, y: f64, out: &mut [f32]) {
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let t = [self.tap(x0, y0), self.tap(x0 + 1, y0), self.tap(x0, y0 + 1), self.tap(x0 + 1, y0 + 1)];
        let k = [(1.0 - tx) * (1.0 - ty), tx * (1.0 - ty), (1.0 - tx) * ty, tx * ty];
        let near = t[usize::from(tx >= 0.5) + 2 * usize::from(ty >= 0.5)];
        for (c, o) in out.iter_mut().enumerate().take(self.n) {
            let v = [self.buf[t[0] + c], self.buf[t[1] + c], self.buf[t[2] + c], self.buf[t[3] + c]];
            *o = if v.iter().all(|v| v.is_finite()) { v[0] * k[0] + v[1] * k[1] + v[2] * k[2] + v[3] * k[3] } else { self.buf[near + c] };
        }
    }
}

/// `top` (straight colour, coverage `ka` on top of its own alpha) over `base`, in place.
fn over(base: &mut [f32], top: &[f32], ka: f32, alpha: Option<usize>) {
    if ka <= 0.0 {
        return;
    }
    match alpha {
        Some(a) => {
            let ta = (top[a] * ka).clamp(0.0, 1.0);
            let ba = base[a].clamp(0.0, 1.0);
            let oa = ta + ba * (1.0 - ta);
            for c in 0..base.len() {
                if c == a {
                    continue;
                }
                base[c] = if oa > 0.0 { (top[c] * ta + base[c] * ba * (1.0 - ta)) / oa } else { 0.0 };
            }
            base[a] = oa;
        }
        None => {
            let k = ka.clamp(0.0, 1.0);
            for c in 0..base.len() {
                base[c] = if k >= 1.0 { top[c] } else { base[c] + (top[c] - base[c]) * k };
            }
        }
    }
}

/// Clear coverage `k` of `px` (alpha layers lose it; opaque ones keep their pixels, Photoshop
/// fills the Background with the background colour, here they stay under the moved content).
fn clear(px: &mut [f32], k: f32, alpha: Option<usize>) {
    if let Some(a) = alpha {
        px[a] *= 1.0 - k.clamp(0.0, 1.0);
    }
}

fn transpose(q: &Rot) -> Rot {
    [[q[0][0], q[1][0], q[2][0]], [q[0][1], q[1][1], q[2][1]], [q[0][2], q[1][2], q[2][2]]]
}

/// `layer.projectedRotate` — see the module docs.
/// Pixel-centre directions of an equirect grid from per-column and per-row sines and cosines:
/// the same values as `equirect_to_dir`, without two of its four trig calls per pixel.
struct Trig {
    lon: Vec<(f64, f64)>,
    lat: Vec<(f64, f64)>,
}

impl Trig {
    fn new(iw: i32, ih: i32) -> Self {
        let (fw, fh) = (f64::from(iw), f64::from(ih));
        let lon = (0..iw).map(|x| ((f64::from(x) + 0.5) / fw - 0.5) * std::f64::consts::TAU).map(f64::sin_cos).collect();
        let lat = (0..ih).map(|y| ((0.5 - (f64::from(y) + 0.5) / fh) * std::f64::consts::PI).clamp(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2)).map(f64::sin_cos).collect();
        Trig { lon, lat }
    }
    fn dir(&self, x: i32, y: i32) -> [f64; 3] {
        let ((so, co), (sl, cl)) = (self.lon[x as usize], self.lat[y as usize]);
        [cl * so, sl, cl * co]
    }
}

/// The pixels a move can change: a set of columns over rows `y0..y1`.
struct MoveRegion {
    cols: Vec<bool>,
    y0: i32,
    y1: i32,
}

impl MoveRegion {
    fn all(iw: i32, ih: i32) -> Self {
        MoveRegion { cols: vec![true; iw as usize], y0: 0, y1: ih }
    }
}

/// Where a selection with bounds `r` is and where sphere rotation `q` takes it, padded.
fn move_region(r: Rect, q: &Rot, iw: i32, ih: i32) -> MoveRegion {
    let (fw, fh) = (f64::from(iw), f64::from(ih));
    let pad = 4.0 + fw / 256.0;
    let inv = transpose(q);
    let inside = |d: [f64; 3]| {
        let [u, v] = dir_to_equirect(d, fw, fh);
        let (x0, x1) = (f64::from(r.x0) - pad, f64::from(r.x1) + pad);
        let du = if u < x0 { u + fw } else if u > x1 { u - fw } else { u };
        du >= x0 && du <= x1 && v >= f64::from(r.y0) - pad && v <= f64::from(r.y1) + pad
    };
    // Sample the source rect's edges densely and its inside coarsely.
    let mut pts = Vec::new();
    let (ax, ay, bx, by) = (f64::from(r.x0), f64::from(r.y0), f64::from(r.x1), f64::from(r.y1));
    for i in 0..=512 {
        let t = f64::from(i) / 512.0;
        let (x, y) = (ax + (bx - ax) * t, ay + (by - ay) * t);
        pts.extend([[x, ay], [x, by], [ax, y], [bx, y]]);
    }
    for i in 0..=32 {
        for j in 0..=32 {
            pts.push([ax + (bx - ax) * f64::from(i) / 32.0, ay + (by - ay) * f64::from(j) / 32.0]);
        }
    }
    let mut us = Vec::with_capacity(pts.len());
    let (mut vmin, mut vmax) = (f64::from(r.y0), f64::from(r.y1));
    for [x, y] in pts {
        let [u, v] = dir_to_equirect(rot(q, equirect_to_dir(x, y, fw, fh)), fw, fh);
        us.push(u);
        vmin = vmin.min(v);
        vmax = vmax.max(v);
    }
    let north = inside(rot(&inv, [0.0, 1.0, 0.0]));
    let south = inside(rot(&inv, [0.0, -1.0, 0.0]));
    if north {
        vmin = 0.0;
    }
    if south {
        vmax = fh;
    }
    let y0 = ((vmin - pad).floor() as i32).clamp(0, ih);
    let y1 = ((vmax + pad).ceil() as i32).clamp(0, ih);
    let mut cols = vec![false; iw as usize];
    let mut mark = |a: f64, b: f64| {
        if b - a >= fw {
            cols.iter_mut().for_each(|c| *c = true);
            return;
        }
        let mut x = a.floor() as i64;
        while (x as f64) < b.ceil() {
            cols[x.rem_euclid(i64::from(iw)) as usize] = true;
            x += 1;
        }
    };
    // Where it is.
    mark(ax - pad, bx + pad);
    // Where it lands: the shortest arc holding every mapped u (the complement of the widest gap).
    us.sort_by(f64::total_cmp);
    let mut gap = (us[0] + fw - us[us.len() - 1], us[us.len() - 1]);
    for w in us.windows(2) {
        if w[1] - w[0] > gap.0 {
            gap = (w[1] - w[0], w[0]);
        }
    }
    if north || south || gap.0 < fw * 0.25 {
        mark(0.0, fw);
    } else {
        let start = gap.1 + gap.0;
        mark(start - pad, start + (fw - gap.0) + pad);
    }
    MoveRegion { cols, y0, y1 }
}

pub fn projected_rotate(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.projectedRotate";
    let a = p.get("rotation").and_then(Value::as_array).ok_or_else(|| bad(CMD, "missing `rotation`"))?;
    let q = rot_from_slice(&a.iter().filter_map(Value::as_f64).collect::<Vec<_>>()).ok_or_else(|| bad(CMD, "`rotation` must be 9 numbers"))?;
    let inv = transpose(&q);
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let id = st.active_layer.ok_or_else(|| bad(CMD, "no active layer"))?;
    let label = if st.doc.selection.is_some() { "Move Selection Contents" } else { "Move" };
    // Tests: the whole sphere, to check the region-limited move against.
    let full = p.get("_fullSphere").and_then(Value::as_bool).unwrap_or(false);
    s.edit(label, |doc, _| {
        let (iw, ih) = (doc.size.width as i32, doc.size.height as i32);
        let sel = doc.selection.clone();
        let layer = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let surf = layer.surface_mut().ok_or_else(|| bad(CMD, "the active layer has no pixels (rasterize it first)"))?;
        let fmt = surf.format();
        let n = surf.channels();
        let alpha = fmt.alpha.then(|| n - 1);
        let (fw, fh) = (f64::from(iw), f64::from(ih));
        // With a selection only where it is (cleared) and where it lands change: a 20k latlong
        // moves a small selection in a fraction of the full-sphere time.
        let sel_rect = sel.as_ref().map(|m| m.content_bounds()).filter(|r| !r.is_empty());
        let region = match (&sel, sel_rect) {
            (Some(_), Some(r)) if !full => move_region(r, &q, iw, ih),
            (Some(_), Some(_)) => MoveRegion::all(iw, ih),
            (Some(_), None) => return Ok(()),
            _ => MoveRegion::all(iw, ih),
        };
        let (sy0, sy1) = sel_rect.map_or((0, ih), |r| ((r.y0 - 2).max(0), (r.y1 + 2).min(ih)));
        let src = Band::read(surf, iw, sy0, sy1);
        let sb = sel.as_ref().map(|m| Band::read(m, iw, sy0, sy1));
        let cols: Vec<i32> = (0..iw).filter(|&x| region.cols[x as usize]).collect();
        let trig = Trig::new(iw, ih);
        // Rows in bands: one full output copy of a 20k F32 latlong is 3 GB.
        let band = 256;
        let mut y0 = region.y0;
        while y0 < region.y1 {
            let y1 = (y0 + band).min(region.y1);
            let r = Rect::new(0, y0, iw, y1);
            let mut out = surf.read_region(r);
            out.par_chunks_mut(iw as usize * n).enumerate().for_each(|(j, line)| {
                let y = y0 + j as i32;
                let (mut c, mut m) = (vec![0f32; n], [0f32; 4]);
                for &x in &cols {
                    let px = &mut line[x as usize * n..(x as usize + 1) * n];
                    let [u, v] = dir_to_equirect(rot(&inv, trig.dir(x, y)), fw, fh);
                    match &sb {
                        Some(sb) => {
                            // What stays: the unselected part here; on top, the selected part from there.
                            let here = if y >= sy0 && y < sy1 { sb.buf[sb.tap(i64::from(x), i64::from(y))] } else { 0.0 };
                            clear(px, here, alpha);
                            if v + 1.0 < f64::from(sy0) || v - 1.0 > f64::from(sy1) {
                                continue;
                            }
                            sb.sample(u, v, &mut m);
                            if m[0] > 0.0 {
                                src.sample(u, v, &mut c);
                                over(px, &c, m[0], alpha);
                            }
                        }
                        None => {
                            src.sample(u, v, &mut c);
                            px.copy_from_slice(&c);
                        }
                    }
                }
            });
            surf.write_region(r, &out);
            y0 = y1;
        }
        // The selection moves with its contents.
        if let (Some(m), Some(sb)) = (doc.selection.as_mut(), sb) {
            let mn = m.channels();
            let r = Rect::new(0, region.y0, iw, region.y1);
            let mut out = m.read_region(r);
            out.par_chunks_mut(iw as usize * mn).enumerate().for_each(|(j, line)| {
                let y = region.y0 + j as i32;
                let mut v4 = [0f32; 4];
                for &x in &cols {
                    let [u, v] = dir_to_equirect(rot(&inv, trig.dir(x, y)), fw, fh);
                    v4[0] = 0.0;
                    if v + 1.0 >= f64::from(sy0) && v - 1.0 <= f64::from(sy1) {
                        sb.sample(u, v, &mut v4);
                    }
                    line[x as usize * mn] = v4[0];
                }
            });
            m.write_region(r, &out);
        }
        Ok(())
    })?;
    if let Some(st) = s.active_mut() {
        st.last_damage = None;
    }
    Ok(json!({}))
}

/// The homography taking the four `from` points to the four `to` points (row-major 3×3).
pub fn homography(from: [[f64; 2]; 4], to: [[f64; 2]; 4]) -> Option<[f64; 9]> {
    let mut a = [[0f64; 9]; 8];
    for i in 0..4 {
        let ([x, y], [u, v]) = (from[i], to[i]);
        a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    for c in 0..8 {
        let p = (c..8).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[p][c].abs() < 1e-12 {
            return None;
        }
        a.swap(c, p);
        for r in 0..8 {
            if r != c {
                let f = a[r][c] / a[c][c];
                for k in c..9 {
                    a[r][k] -= f * a[c][k];
                }
            }
        }
    }
    let h: Vec<f64> = (0..8).map(|i| a[i][8] / a[i][i]).collect();
    Some([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0])
}

fn invert3(m: &[f64; 9]) -> Option<[f64; 9]> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-15 {
        return None;
    }
    Some([(e * i - f * h) / det, (c * h - b * i) / det, (b * f - c * e) / det, (f * g - d * i) / det, (a * i - c * g) / det, (c * d - a * f) / det, (d * h - e * g) / det, (b * g - a * h) / det, (a * e - b * d) / det])
}

/// `m · (x, y, 1)`, `None` past the horizon of the plane (w ≤ 0).
pub fn apply_h(m: &[f64; 9], x: f64, y: f64) -> Option<[f64; 2]> {
    let w = m[6] * x + m[7] * y + m[8];
    if w <= 1e-12 {
        return None;
    }
    Some([(m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w])
}

fn quad_of(p: &Value, k: &str, cmd: &str) -> Result<[[f64; 2]; 4]> {
    let a = p.get(k).and_then(Value::as_array).filter(|a| a.len() == 4).ok_or_else(|| bad(cmd, format!("`{k}` must be four [x, y] corners")))?;
    let mut q = [[0.0; 2]; 4];
    for (i, c) in a.iter().enumerate() {
        let c = c.as_array().filter(|c| c.len() >= 2).ok_or_else(|| bad(cmd, format!("`{k}` must be four [x, y] corners")))?;
        q[i] = [c[0].as_f64().unwrap_or(0.0), c[1].as_f64().unwrap_or(0.0)];
    }
    Ok(q)
}

/// A transform in progress: the source content in view space, warped onto a quad.
pub struct Warp {
    pub src: [f64; 4],
    pub quad: [[f64; 2]; 4],
    /// Quad → source.
    inv: [f64; 9],
}

impl Warp {
    pub fn new(src: [f64; 4], quad: [[f64; 2]; 4]) -> Option<Warp> {
        let corners = [[src[0], src[1]], [src[2], src[1]], [src[2], src[3]], [src[0], src[3]]];
        let h = homography(corners, quad)?;
        Some(Warp { src, quad, inv: invert3(&h)? })
    }
    /// The source position (in `src` space) shown at view position `v`, if inside the source.
    pub fn back(&self, v: [f64; 2]) -> Option<[f64; 2]> {
        let s = apply_h(&self.inv, v[0], v[1])?;
        (s[0] >= self.src[0] && s[0] < self.src[2] && s[1] >= self.src[1] && s[1] < self.src[3]).then_some(s)
    }
    fn quad_bbox(&self) -> [f64; 4] {
        let xs = self.quad.map(|q| q[0]);
        let ys = self.quad.map(|q| q[1]);
        [xs.iter().copied().fold(f64::MAX, f64::min), ys.iter().copied().fold(f64::MAX, f64::min), xs.iter().copied().fold(f64::MIN, f64::max), ys.iter().copied().fold(f64::MIN, f64::max)]
    }
}

/// The equirect row band the view-space rects touch (full width, so the seam needs nothing).
fn rows_of(proj: &ViewProjection, rects: &[[f64; 4]], iw: u32, ih: u32) -> Option<(i32, i32)> {
    let mut v = (i64::MAX, i64::MIN);
    for r in rects {
        for q in view_rect_to_equirect(proj, *r, iw, ih, 2.0) {
            v = (v.0.min(q[1]), v.1.max(q[3]));
        }
    }
    (v.0 < v.1).then(|| (v.0.max(0) as i32, v.1.min(i64::from(ih)) as i32))
}

/// A new layer above the active one holding `src` over `cb` mapped onto the view `quad`
/// (Paste, and the Type and shape tools' rendered layers).
pub(crate) fn place_new_layer(s: &mut Session, src: &Surface, cb: Rect, quad: [[f64; 2]; 4], proj: &ViewProjection, label: &str, name: Option<String>) -> Result<photocraft_doc::LayerId> {
    const CMD: &str = "layer.projectedPlace";
    let proj = *proj;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (iw, ih) = (st.doc.size.width as i32, st.doc.size.height as i32);
    let (fw, fh) = (f64::from(iw), f64::from(ih));
    {
        if cb.is_empty() {
            return Err(EngineError::Other("the clipboard is empty".into()));
        }
        let warp = Warp::new([f64::from(cb.x0), f64::from(cb.y0), f64::from(cb.x1), f64::from(cb.y1)], quad).ok_or_else(|| bad(CMD, "the quad is degenerate"))?;
        let fmt = st.doc.pixel_format();
        let target = PixelFormat::new(fmt.mode, fmt.sample, true);
        let img = if src.format() == target { src.clone() } else { src.convert(target) };
        let n = img.channels();
        let ibuf = img.read_region(cb);
        let (cw, ch) = (i64::from(cb.width()), i64::from(cb.height()));
        let rows = rows_of(&proj, &[warp.quad_bbox()], iw as u32, ih as u32);
        let mut out = Surface::new(target);
        if let Some((y0, y1)) = rows {
            let r = Rect::new(0, y0, iw, y1);
            let mut buf = vec![0f32; iw as usize * (y1 - y0) as usize * n];
            buf.par_chunks_mut(iw as usize * n).enumerate().for_each(|(j, line)| {
                let y = f64::from(y0 + j as i32) + 0.5;
                for x in 0..iw as usize {
                    let Some(v) = proj.dir_to_view(equirect_to_dir(x as f64 + 0.5, y, fw, fh)) else { continue };
                    let Some([sx, sy]) = warp.back(v) else { continue };
                    // Bilinear inside the image, transparent past its edge.
                    let (fx, fy) = (sx - f64::from(cb.x0) - 0.5, sy - f64::from(cb.y0) - 0.5);
                    let (x0, y0i) = (fx.floor() as i64, fy.floor() as i64);
                    let (tx, ty) = ((fx - fx.floor()) as f32, (fy - fy.floor()) as f32);
                    let px = &mut line[x * n..(x + 1) * n];
                    let mut wsum = 0.0;
                    for (dx, dy, k) in [(0, 0, (1.0 - tx) * (1.0 - ty)), (1, 0, tx * (1.0 - ty)), (0, 1, (1.0 - tx) * ty), (1, 1, tx * ty)] {
                        let (xx, yy) = (x0 + dx, y0i + dy);
                        if k <= 0.0 || xx < 0 || yy < 0 || xx >= cw || yy >= ch {
                            continue;
                        }
                        let i = ((yy * cw + xx) as usize) * n;
                        let a = ibuf[i + n - 1] * k;
                        for c in 0..n - 1 {
                            if ibuf[i + c].is_finite() {
                                px[c] += ibuf[i + c] * a;
                            }
                        }
                        px[n - 1] += a;
                        wsum += k;
                    }
                    let _ = wsum;
                    if px[n - 1] > 0.0 {
                        for c in 0..n - 1 {
                            px[c] /= px[n - 1];
                        }
                    }
                }
            });
            out.write_region(r, &buf);
        }
        let before = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
        let id = s.edit(label, |doc, active| {
            let mut l = Layer::raster(name.clone().unwrap_or_else(|| doc.next_layer_name("Layer")), target);
            *crate::pixels_mut(&mut l)? = out;
            let id = doc.insert_above(*active, l);
            *active = Some(id);
            doc.selection = None;
            Ok(id)
        })?;
        crate::layer_multi_cmds::note_insert(s, &before);
        Ok(id)
    }
}

/// `layer.projectedPlace` — see the module docs.
pub fn projected_place(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.projectedPlace";
    let proj = projection_of(p, CMD)?;
    let quad = quad_of(p, "quad", CMD)?;
    let from_clip = p.get("source").and_then(Value::as_str) == Some("clipboard");
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (iw, ih) = (st.doc.size.width as i32, st.doc.size.height as i32);
    let (fw, fh) = (f64::from(iw), f64::from(ih));
    if from_clip {
        let clip = s.clipboard.clone().ok_or_else(|| EngineError::Other("the clipboard is empty".into()))?;
        let cb = clip.surface.content_bounds();
        let cb = if cb.is_empty() { clip.bounds } else { cb };
        let id = place_new_layer(s, &clip.surface, cb, quad, &proj, "Paste", None)?;
        return Ok(json!({"layer": id.0}));
    }
    // The active layer's content inside `rect` (view px).
    let r = p.get("rect").and_then(Value::as_array).filter(|a| a.len() == 4).ok_or_else(|| bad(CMD, "missing `rect` [x0, y0, x1, y1]"))?;
    let src = [0, 1, 2, 3].map(|i| r[i].as_f64().unwrap_or(0.0));
    let warp = Warp::new(src, quad).ok_or_else(|| bad(CMD, "the quad is degenerate"))?;
    let id = st.active_layer.ok_or_else(|| bad(CMD, "no active layer"))?;
    let dmg = s.edit("Free Transform", |doc, _| {
        let sel = doc.selection.clone();
        let Some((y0, y1)) = rows_of(&proj, &[src, warp.quad_bbox()], iw as u32, ih as u32) else { return Ok(Rect::EMPTY) };
        let layer = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let surf = layer.surface_mut().ok_or_else(|| bad(CMD, "the active layer has no pixels (rasterize it first)"))?;
        let n = surf.channels();
        let alpha = surf.format().alpha.then(|| n - 1);
        // Every source position maps back into the same view: its rows are the source rows.
        let (sy0, sy1) = rows_of(&proj, &[src], iw as u32, ih as u32).unwrap_or((y0, y1));
        let (sy0, sy1) = (sy0.min(y0), sy1.max(y1));
        let sband = Band::read(surf, iw, sy0, sy1);
        let mband = sel.as_ref().map(|m| Band::read(m, iw, sy0, sy1));
        let inside = |v: [f64; 2]| v[0] >= src[0] && v[0] < src[2] && v[1] >= src[1] && v[1] < src[3];
        let rr = Rect::new(0, y0, iw, y1);
        let mut out = surf.read_region(rr);
        let mut new_sel = mband.as_ref().map(|m| m.buf[((y0 - sy0) as usize * iw as usize) * m.n..((y1 - sy0) as usize * iw as usize) * m.n].to_vec());
        out.par_chunks_mut(iw as usize * n).enumerate().for_each(|(j, line)| {
            let y = y0 + j as i32;
            let (mut c, mut m) = (vec![0f32; n], [0f32; 4]);
            for x in 0..iw {
                let Some(v) = proj.dir_to_view(equirect_to_dir(f64::from(x) + 0.5, f64::from(y) + 0.5, fw, fh)) else { continue };
                let px = &mut line[x as usize * n..(x as usize + 1) * n];
                if inside(v) {
                    let k = mband.as_ref().map_or(1.0, |mb| mb.buf[mb.tap(i64::from(x), i64::from(y))]);
                    clear(px, k, alpha);
                }
                if let Some(sv) = warp.back(v) {
                    let [u, vv] = proj.view_to_equirect(sv[0], sv[1], fw, fh);
                    let k = match &mband {
                        Some(mb) => {
                            mb.sample(u, vv, &mut m);
                            m[0]
                        }
                        None => 1.0,
                    };
                    if k > 0.0 {
                        sband.sample(u, vv, &mut c);
                        over(px, &c, k, alpha);
                    }
                }
            }
        });
        surf.write_region(rr, &out);
        // The selection follows its contents.
        if let (Some(ns), Some(mb), Some(m)) = (new_sel.as_mut(), mband.as_ref(), doc.selection.as_mut()) {
            let mn = mb.n;
            ns.par_chunks_mut(iw as usize * mn).enumerate().for_each(|(j, line)| {
                let y = y0 + j as i32;
                let mut q = [0f32; 4];
                for x in 0..iw as usize {
                    let Some(v) = proj.dir_to_view(equirect_to_dir(x as f64 + 0.5, f64::from(y) + 0.5, fw, fh)) else { continue };
                    let mut k = if inside(v) { 0.0 } else { line[x * mn] };
                    if let Some(sv) = warp.back(v) {
                        let [u, vv] = proj.view_to_equirect(sv[0], sv[1], fw, fh);
                        mb.sample(u, vv, &mut q);
                        k = k.max(q[0]);
                    }
                    line[x * mn] = k;
                }
            });
            m.write_region(rr, ns);
        }
        Ok(rr)
    })?;
    if let Some(st) = s.active_mut() {
        st.last_damage = Some(dmg);
    }
    Ok(json!({"damage": [dmg.x0, dmg.y0, dmg.width(), dmg.height()]}))
}

/// `s` box-averaged by `f` (finite samples only), as a full band, for previews.
fn reduced(s: &Surface, iw: i32, ih: i32, f: i32) -> Band {
    if f <= 1 {
        return Band::read(s, iw, 0, ih);
    }
    let n = s.channels();
    let (w, h) = ((iw + f - 1) / f, (ih + f - 1) / f);
    let mut buf = vec![0f32; (w * h) as usize * n];
    buf.par_chunks_mut(w as usize * n).enumerate().for_each(|(y, line)| {
        let y0 = y as i32 * f;
        let rows = s.read_region(Rect::new(0, y0, iw, (y0 + f).min(ih)));
        let rh = rows.len() / (iw as usize * n);
        for x in 0..w as usize {
            for c in 0..n {
                let (mut sum, mut cnt) = (0f32, 0f32);
                for yy in 0..rh {
                    for xx in x * f as usize..((x + 1) * f as usize).min(iw as usize) {
                        let v = rows[(yy * iw as usize + xx) * n + c];
                        if v.is_finite() {
                            sum += v;
                            cnt += 1.0;
                        }
                    }
                }
                line[x * n + c] = if cnt > 0.0 { sum / cnt } else { 0.0 };
            }
        }
    });
    Band { buf, w: i64::from(w), y0: 0, y1: i64::from(h), n }
}

/// View-space live previews for the Move and Free Transform drags: the active layer and the
/// selection resampled into the view once (and the whole layer reduced to at most 4096 px
/// wide for what comes into the view from elsewhere), then each frame the result in view space
/// as an overlay (changed pixels; cleared ones as the grey veil).
pub struct XformPreview {
    proj: ViewProjection,
    view: Rect,
    pre: Surface,
    sel: Option<Surface>,
    lb: Band,
    mb: Option<Band>,
    /// Full-resolution pixels per reduced pixel.
    f: f64,
    iw: i32,
    ih: i32,
}

impl XformPreview {
    /// From `{"projection": …}` for the active layer of the active document.
    pub fn new(s: &Session, p: &Value) -> Result<Self> {
        let proj = projection_of(p, "projection preview")?;
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let id = st.active_layer.ok_or_else(|| EngineError::Other("no active layer".into()))?;
        let layer = st.doc.layer(id).and_then(|l| l.surface()).ok_or_else(|| EngineError::Other("the active layer has no pixels (rasterize it first)".into()))?;
        let (iw, ih) = (st.doc.size.width as i32, st.doc.size.height as i32);
        let view = Rect::new(0, 0, proj.w as i32, proj.h as i32);
        let pre = crate::brush_cmds::projected::to_view(layer, &proj, view, iw, ih);
        let sel = st.doc.selection.as_ref().map(|m| crate::brush_cmds::projected::to_view(m, &proj, view, iw, ih));
        let f = ((iw + 4095) / 4096).max(1);
        let lb = reduced(layer, iw, ih, f);
        let mb = st.doc.selection.as_ref().map(|m| reduced(m, iw, ih, f));
        Ok(Self { proj, view, pre, sel, lb, mb, f: f64::from(f), iw, ih })
    }

    /// The view-space bounds of what a transform would take: the selection, else the layer's
    /// opaque pixels, within the view.
    pub fn content_rect(&self) -> Option<[f64; 4]> {
        let n = self.pre.channels();
        let (src, ch, idx) = match &self.sel {
            Some(m) => (m.read_region(self.view), m.channels(), 0),
            None if self.pre.format().alpha => (self.pre.read_region(self.view), n, n - 1),
            None => return Some([0.0, 0.0, f64::from(self.view.x1), f64::from(self.view.y1)]),
        };
        let w = self.view.width() as usize;
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for (i, px) in src.chunks_exact(ch).enumerate() {
            if px[idx] > 0.004 {
                let (x, y) = (i % w, i / w);
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
        (x0 < x1).then(|| [x0 as f64, y0 as f64, x1 as f64, y1 as f64])
    }

    /// Each view pixel: whether its own content is cleared, and the direction it now shows.
    fn render(&self, at: impl Fn([f64; 2]) -> (bool, Option<[f64; 3]>) + Sync) -> Option<crate::brush_cmds::projected::PreviewOverlay> {
        let n = self.pre.channels();
        let alpha = self.pre.format().alpha.then(|| n - 1);
        let w = self.view.width() as usize;
        let pre = self.pre.read_region(self.view);
        let selv = self.sel.as_ref().map(|m| (m.read_region(self.view), m.channels()));
        let (rw, rh) = (self.lb.w as f64, self.lb.y1 as f64);
        let _ = (self.iw, self.ih, self.f);
        let mut post = pre.clone();
        post.par_chunks_mut(w * n).enumerate().for_each(|(y, line)| {
            let (mut c, mut m) = (vec![0f32; n], [0f32; 4]);
            for x in 0..w {
                let px = &mut line[x * n..(x + 1) * n];
                let (cleared, from) = at([x as f64 + 0.5, y as f64 + 0.5]);
                if cleared {
                    let k = selv.as_ref().map_or(1.0, |(s, ch)| s[(y * w + x) * ch]);
                    clear(px, k, alpha);
                }
                let Some(d) = from else { continue };
                let [u, v] = dir_to_equirect(d, rw, rh);
                let k = match &self.mb {
                    Some(mb) => {
                        mb.sample(u, v, &mut m);
                        m[0]
                    }
                    None => 1.0,
                };
                if k > 0.0 {
                    self.lb.sample(u, v, &mut c);
                    over(px, &c, k, alpha);
                }
            }
        });
        let mut ps = Surface::new(self.pre.format());
        ps.write_region(self.view, &post);
        crate::brush_cmds::projected::overlay(&self.pre, &ps, self.view)
    }

    /// The Move drag: the layer (or the selected part) turned by `q`.
    pub fn rotate(&self, q: &Rot) -> Option<crate::brush_cmds::projected::PreviewOverlay> {
        let inv = transpose(q);
        self.render(|v| (true, Some(rot(&inv, self.proj.view_to_dir(v[0], v[1])))))
    }

    /// The Free Transform: `rect` (view px) onto `quad`.
    pub fn place(&self, rect: [f64; 4], quad: [[f64; 2]; 4]) -> Option<crate::brush_cmds::projected::PreviewOverlay> {
        let warp = Warp::new(rect, quad)?;
        let inside = |v: [f64; 2]| v[0] >= rect[0] && v[0] < rect[2] && v[1] >= rect[1] && v[1] < rect[3];
        self.render(|v| (inside(v), warp.back(v).map(|s| self.proj.view_to_dir(s[0], s[1]))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equirect::rotation_between;

    /// A 2048×1024 F32 latlong: gradient, HDR, an `inf` and a `NaN`, and a red square at
    /// (1000..1040, 500..540).
    fn doc() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 2048, "height": 1024, "depth": 32})).unwrap();
        let id = s.active().unwrap().doc.walk().iter().find(|(_, _, l)| l.surface().is_some()).map(|(_, _, l)| l.id).unwrap();
        s.select_layer(id).unwrap();
        s.edit("fill", |doc, _| {
            let surf = doc.layer_mut(id).unwrap().surface_mut().unwrap();
            let n = surf.channels();
            let (w, h) = (2048usize, 1024usize);
            let mut d = vec![0f32; w * h * n];
            for y in 0..h {
                for x in 0..w {
                    let i = (y * w + x) * n;
                    d[i] = 0.1 + x as f32 / w as f32 + if y < 300 { 20.0 } else { 0.0 };
                    d[i + 1] = 0.2;
                    d[i + 2] = 0.3;
                    if n == 4 {
                        d[i + 3] = 1.0;
                    }
                    if (1000..1040).contains(&x) && (500..540).contains(&y) {
                        d[i] = 5.0;
                        d[i + 1] = 0.0;
                        d[i + 2] = 0.0;
                    }
                }
            }
            d[(100 * w + 50) * n] = f32::INFINITY;
            d[(700 * w + 900) * n] = f32::NAN;
            surf.write_region(Rect::new(0, 0, w as i32, h as i32), &d);
            Ok(())
        })
        .unwrap();
        s
    }
    fn px(s: &Session, x: i32, y: i32) -> [f32; 4] {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
    }

    #[test]
    fn homography_maps_corners() {
        let h = homography([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], [[10.0, 10.0], [30.0, 12.0], [28.0, 40.0], [8.0, 35.0]]).unwrap();
        let p = apply_h(&h, 1.0, 1.0).unwrap();
        assert!((p[0] - 28.0).abs() < 1e-9 && (p[1] - 40.0).abs() < 1e-9);
    }

    #[test]
    fn move_rotates_across_the_seam_and_keeps_inf_nan_contained() {
        let mut s = doc();
        // Turn the sphere by 180° of longitude: the square crosses the seam to x ≈ 0..16 / 2032..2048.
        let a = equirect_to_dir(1020.0, 520.0, 2048.0, 1024.0);
        let b = equirect_to_dir(1020.0 + 1024.0 - 2048.0 + 2048.0, 520.0, 2048.0, 1024.0);
        let b = if (b[0] - a[0]).abs() < 1e-9 { equirect_to_dir(4.0, 520.0, 2048.0, 1024.0) } else { b };
        let q = rotation_between(a, b);
        s.execute("layer.projectedRotate", json!({"rotation": q.concat()})).unwrap();
        let moved = dir_to_equirect(rot(&q, a), 2048.0, 1024.0);
        let c = px(&s, moved[0] as i32, moved[1] as i32);
        assert!(c[0] > 4.0 && c[1] < 0.05, "the square moved: {c:?} at {moved:?}");
        let d = s.active().unwrap();
        let all = d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().read_region(Rect::new(0, 0, 2048, 1024));
        let bad = all.iter().filter(|v| !v.is_finite()).count();
        assert!(bad <= 2, "inf / NaN stay single pixels: {bad}");
        s.execute("edit.undo", json!({})).unwrap();
        assert!(px(&s, 1020, 520)[0] > 4.0);
    }

    #[test]
    fn move_with_a_selection_moves_only_it_near_the_pole() {
        let mut s = doc();
        s.execute("select.rect", json!({"x": 1000, "y": 500, "width": 40, "height": 40})).unwrap();
        let a = equirect_to_dir(1020.0, 520.0, 2048.0, 1024.0);
        let b = equirect_to_dir(1020.0, 60.0, 2048.0, 1024.0);
        let q = rotation_between(a, b);
        s.execute("layer.projectedRotate", json!({"rotation": q.concat()})).unwrap();
        let c = px(&s, 1020, 60);
        assert!(c[0] > 4.0 && c[1] < 0.05, "selected square went north: {c:?}");
        // The rest of the layer did not move, and the selection went with it.
        assert!((px(&s, 1500, 800)[0] - (0.1 + 1500.0 / 2048.0)).abs() < 1e-3);
        let sel = s.active().unwrap().doc.selection.as_ref().unwrap().content_bounds();
        assert!(sel.contains(1020, 60) && !sel.contains(1020, 520), "{sel:?}");
    }

    #[test]
    fn free_transform_scales_in_view_and_paste_places_undistorted() {
        let mut s = doc();
        let view = json!({"yaw": 0.0, "pitch": 0.0, "fov": 60.0, "width": 800, "height": 600});
        let vp = ViewProjection::new(Camera { yaw: 0.0, pitch: 0.0, fov: 60.0 }, 800.0, 600.0);
        let c = vp.equirect_to_view(1020.0, 520.0, 2048.0, 1024.0).unwrap();
        s.execute("select.rect", json!({"x": 1000, "y": 500, "width": 40, "height": 40})).unwrap();
        let r = [c[0] - 40.0, c[1] - 40.0, c[0] + 40.0, c[1] + 40.0];
        // Twice the size, shifted right by 150 px.
        let q = [[c[0] - 80.0 + 150.0, c[1] - 80.0], [c[0] + 80.0 + 150.0, c[1] - 80.0], [c[0] + 80.0 + 150.0, c[1] + 80.0], [c[0] - 80.0 + 150.0, c[1] + 80.0]];
        let pv = XformPreview::new(&s, &json!({"projection": view})).unwrap();
        assert!(pv.place(r, q).is_some(), "a live preview");
        s.execute("layer.projectedPlace", json!({"projection": view, "rect": r, "quad": q, "source": "layer"})).unwrap();
        let at = vp.view_to_equirect(c[0] + 150.0 + 60.0, c[1], 2048.0, 1024.0);
        let p = px(&s, at[0] as i32, at[1] as i32);
        assert!(p[0] > 4.0 && p[1] < 0.05, "scaled square reaches further: {p:?}");
        // Paste: the square (from before the transform), centred and upright in the view.
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("select.rect", json!({"x": 1000, "y": 500, "width": 40, "height": 40})).unwrap();
        s.execute("edit.copy", json!({})).unwrap();
        let layers = s.active().unwrap().doc.walk().len();
        let q = [[350.0, 250.0], [450.0, 250.0], [450.0, 350.0], [350.0, 350.0]];
        s.execute("layer.projectedPlace", json!({"projection": view, "quad": q, "source": "clipboard"})).unwrap();
        assert_eq!(s.active().unwrap().doc.walk().len(), layers + 1, "a new layer");
        let mid = vp.view_to_equirect(400.0, 300.0, 2048.0, 1024.0);
        let p = px(&s, mid[0] as i32, mid[1] as i32);
        assert!(p[0] > 4.0 && p[3] > 0.99, "the paste is in the middle of the view: {p:?}");
        let off = vp.view_to_equirect(470.0, 300.0, 2048.0, 1024.0);
        assert!(px(&s, off[0] as i32, off[1] as i32)[3] < 0.01, "and only inside its quad");
    }
}

/// `layer.projectedRender`: the Type and shape tools in projection. `command` (`type.create` or
/// `shape.create`) runs with its `params` in view px on a transparent view-sized scratch
/// document; the layer it makes is rasterized there and placed upright in the view as a new
/// pixel layer named after it (the equirect can't hold an editable flat text or shape layer
/// that looks right on the sphere).
pub fn projected_render(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "layer.projectedRender";
    let proj = projection_of(p, CMD)?;
    let cmd = p.get("command").and_then(Value::as_str).ok_or_else(|| bad(CMD, "missing `command`"))?;
    let raster = match cmd {
        "type.create" => "type.rasterize",
        "shape.create" => "shape.rasterize",
        _ => return Err(bad(CMD, "`command` must be type.create or shape.create")),
    };
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let fmt = st.doc.pixel_format();
    let (w, h) = (proj.w as i32, proj.h as i32);
    let mut temp = Session::new();
    temp.tools = s.tools.clone();
    temp.presets = s.presets.clone();
    let depth = match fmt.sample {
        photocraft_color::SampleType::F32 => 32,
        photocraft_color::SampleType::U16 => 16,
        _ => 8,
    };
    temp.execute("file.new", json!({"width": w, "height": h, "depth": depth, "background": "transparent"}))?;
    let r = temp.execute(cmd, p.get("params").cloned().unwrap_or_else(|| json!({})))?;
    let id = r.get("layer").and_then(Value::as_u64).ok_or_else(|| EngineError::Other(format!("{cmd} made no layer")))?;
    temp.execute(raster, json!({"layer": id}))?;
    let td = temp.active().ok_or(EngineError::NoDocument)?;
    let lid = td.active_layer.ok_or_else(|| EngineError::Other("no layer".into()))?;
    let layer = td.doc.layer(lid).ok_or(EngineError::NoLayer(lid))?;
    let name = layer.name.clone();
    let surf = layer.surface().ok_or_else(|| EngineError::Other("nothing rendered".into()))?.clone();
    let cb = surf.content_bounds();
    if cb.is_empty() {
        return Err(EngineError::Other("nothing to place (empty text or shape)".into()));
    }
    let quad = [[f64::from(cb.x0), f64::from(cb.y0)], [f64::from(cb.x1), f64::from(cb.y0)], [f64::from(cb.x1), f64::from(cb.y1)], [f64::from(cb.x0), f64::from(cb.y1)]];
    let label = if cmd == "type.create" { "Type" } else { "Shape" };
    let keep = s.active().and_then(|d| d.doc.selection.clone());
    let id = place_new_layer(s, &surf, cb, quad, &proj, label, Some(name))?;
    if keep.is_some()
        && let Some(d) = s.active_mut()
    {
        let mut doc = (*d.doc).clone();
        doc.selection = keep;
        d.doc = std::sync::Arc::new(doc);
    }
    Ok(json!({"layer": id.0}))
}

#[cfg(test)]
mod render_tests {
    use super::*;

    #[test]
    fn a_shape_drawn_in_the_view_lands_upright_on_an_f32_latlong() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 2048, "height": 1024, "depth": 32})).unwrap();
        let view = json!({"yaw": 30.0, "pitch": 40.0, "fov": 60.0, "width": 800, "height": 600});
        let vp = ViewProjection::new(Camera { yaw: 30.0, pitch: 40.0, fov: 60.0 }, 800.0, 600.0);
        let n = s.active().unwrap().doc.walk().len();
        s.execute("layer.projectedRender", json!({"command": "shape.create", "params": {"kind": "rect", "rect": [300, 250, 200, 100], "fill": "#ff0000"}, "projection": view})).unwrap();
        assert_eq!(s.active().unwrap().doc.walk().len(), n + 1);
        let d = s.active().unwrap();
        let surf = d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap();
        let at = |x: f64, y: f64| {
            let [u, v] = vp.view_to_equirect(x, y, 2048.0, 1024.0);
            surf.rgba(u as i32, v as i32)
        };
        let (inside, corner, out) = (at(400.0, 300.0), at(305.0, 255.0), at(520.0, 300.0));
        assert!(inside[0] > 0.9 && inside[3] > 0.99, "{inside:?}");
        assert!(corner[3] > 0.9, "the view rect's corner is filled: {corner:?}");
        assert!(out[3] < 0.01, "{out:?}");
    }
}

#[cfg(test)]
mod move_region_tests {
    use super::*;
    use crate::equirect::rotation_between;

    fn run(rotation: &Rot, sel: [i32; 4], full: bool) -> (Vec<f32>, Vec<f32>) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 1024, "height": 512, "depth": 32})).unwrap();
        {
            let st = s.active_mut().unwrap();
            let id = st.active_layer.unwrap();
            let surf = std::sync::Arc::make_mut(&mut st.doc).layer_mut(id).unwrap().surface_mut().unwrap();
            let n = surf.channels();
            let mut buf = vec![0f32; 1024 * 512 * n];
            for (i, px) in buf.chunks_mut(n).enumerate() {
                let (x, y) = ((i % 1024) as f32, (i / 1024) as f32);
                px[0] = (x * 0.37).sin() * 4.0 + 4.0; // HDR
                px[1] = (y * 0.11).cos();
                if n > 2 { px[2] = ((x + y) * 0.05).sin(); }
                px[n - 1] = 1.0;
            }
            buf[(300 * 1024 + 520) * n] = f32::INFINITY;
            buf[(310 * 1024 + 530) * n + 1] = f32::NAN;
            surf.write_region(Rect::new(0, 0, 1024, 512), &buf);
        }
        s.execute("select.rect", json!({"x": sel[0], "y": sel[1], "width": sel[2], "height": sel[3]})).unwrap();
        s.execute("layer.projectedRotate", json!({"rotation": rotation.concat(), "_fullSphere": full})).unwrap();
        let st = s.active().unwrap();
        let surf = st.doc.layer(st.active_layer.unwrap()).unwrap().surface().unwrap();
        (surf.read_region(Rect::new(0, 0, 1024, 512)), st.doc.selection.as_ref().unwrap().read_region(Rect::new(0, 0, 1024, 512)))
    }

    #[test]
    fn trig_tables_match_equirect_to_dir() {
        let t = Trig::new(1000, 500);
        for (x, y) in [(0, 0), (999, 499), (517, 233), (250, 1)] {
            let a = t.dir(x, y);
            let b = equirect_to_dir(f64::from(x) + 0.5, f64::from(y) + 0.5, 1000.0, 500.0);
            assert!((0..3).all(|i| (a[i] - b[i]).abs() < 1e-12), "{a:?} {b:?}");
        }
    }

    #[test]
    fn a_selection_move_changes_the_same_pixels_as_the_full_sphere_move_on_f32() {
        let cases = [
            // A small nudge, across the seam, and over the north pole.
            (equirect_to_dir(500.0, 300.0, 1024.0, 512.0), equirect_to_dir(540.0, 280.0, 1024.0, 512.0), [480, 280, 80, 60]),
            (equirect_to_dir(980.0, 250.0, 1024.0, 512.0), equirect_to_dir(40.0, 260.0, 1024.0, 512.0), [940, 220, 70, 70]),
            (equirect_to_dir(500.0, 120.0, 1024.0, 512.0), equirect_to_dir(700.0, 10.0, 1024.0, 512.0), [450, 80, 120, 80]),
        ];
        for (a, b, sel) in cases {
            let q = rotation_between(a, b);
            let (pa, ma) = run(&q, sel, false);
            let (pb, mb) = run(&q, sel, true);
            let diff = pa.iter().zip(&pb).filter(|(x, y)| !(x == y || (x.is_nan() && y.is_nan()))).count();
            let mdiff = ma.iter().zip(&mb).filter(|(x, y)| (*x - *y).abs() > 1e-6).count();
            assert_eq!((diff, mdiff), (0, 0), "{sel:?}");
        }
    }
}
