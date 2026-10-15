//! `paint.projectedStroke`: a Brush or Eraser stroke drawn in the View › Equirectangular Live
//! Projection perspective view, written back to the equirect (latlong) pixels of the target.
//!
//! The stroke is rendered by the ordinary brush engine in *view* space, over the target's pixels
//! resampled into that view (`pre`); the result is `post`. Every target pixel whose direction
//! falls inside the stroke's view-space damage is then moved by `post − pre` sampled at its own
//! view position, so untouched detail is kept exactly and every blend mode, opacity and the
//! eraser work as they do on the flat canvas. One `edit` makes it one History step.

use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

use super::{bad, damage_json, erase_locked, parse_points, resolve_brush, stroke_target, with_blend_mode};
use crate::equirect::{Camera, ViewProjection, view_rect_to_equirect};
use crate::{EngineError, Result, Session};

/// The view the stroke was drawn in, from `"projection": {"yaw", "pitch", "fov", "width", "height"}`.
fn projection(p: &Value) -> Result<ViewProjection> {
    let q = p.get("projection").ok_or_else(|| bad("paint.projectedStroke", "missing `projection`"))?;
    let g = |k: &str, d: f64| q.get(k).and_then(Value::as_f64).unwrap_or(d);
    let (w, h) = (g("width", 0.0), g("height", 0.0));
    if !(w >= 1.0 && h >= 1.0 && w <= 16384.0 && h <= 16384.0) {
        return Err(bad("paint.projectedStroke", "`projection.width` and `height` must be 1..=16384"));
    }
    Ok(ViewProjection::new(Camera { yaw: g("yaw", 0.0), pitch: g("pitch", 0.0), fov: g("fov", 90.0) }, w, h))
}

/// Bilinear sample of `s` at pixel-centre position (x, y) (pixel i's centre is i + 0.5), wrapping
/// x around `w` and clamping y to the image rows.
fn sample_wrapped(s: &Surface, x: f64, y: f64, w: i32, h: i32, out: &mut [f32], tmp: &mut [f32]) {
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
    let (x0, y0) = (x0 as i64, y0 as i64);
    out.iter_mut().for_each(|o| *o = 0.0);
    for (dy, wy) in [(0, 1.0 - ty), (1, ty)] {
        let yy = (y0 + dy).clamp(0, i64::from(h) - 1) as i32;
        for (dx, wx) in [(0, 1.0 - tx), (1, tx)] {
            let xx = (x0 + dx).rem_euclid(i64::from(w)) as i32;
            s.read_pixel(xx, yy, tmp);
            let k = wx * wy;
            for (o, v) in out.iter_mut().zip(tmp.iter()) {
                *o += v * k;
            }
        }
    }
}

/// Bilinear sample of a row-major `n`-channel buffer covering `r`, at a pixel-centre position;
/// edges clamp.
fn sample_buf(buf: &[f32], r: Rect, n: usize, x: f64, y: f64, out: &mut [f32]) {
    let (w, h) = (r.width() as i64, r.height() as i64);
    let (fx, fy) = (x - 0.5 - f64::from(r.x0), y - 0.5 - f64::from(r.y0));
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
    let (x0, y0) = (x0 as i64, y0 as i64);
    out.iter_mut().for_each(|o| *o = 0.0);
    for (dy, wy) in [(0, 1.0 - ty), (1, ty)] {
        let yy = (y0 + dy).clamp(0, h - 1) as usize;
        for (dx, wx) in [(0, 1.0 - tx), (1, tx)] {
            let xx = (x0 + dx).clamp(0, w - 1) as usize;
            let i = (yy * w as usize + xx) * n;
            let k = wx * wy;
            for (c, o) in out.iter_mut().enumerate() {
                *o += buf[i + c] * k;
            }
        }
    }
}

/// Resample the equirect target into view space over `r`.
pub(crate) fn to_view(target: &Surface, proj: &ViewProjection, r: Rect, iw: i32, ih: i32) -> Surface {
    use rayon::prelude::*;
    let n = target.channels();
    let row = r.width() as usize * n;
    let mut data = vec![0f32; row * r.height() as usize];
    if row > 0 {
        data.par_chunks_mut(row).enumerate().for_each(|(j, line)| {
            let y = r.y0 + j as i32;
            let (mut px, mut tmp) = (vec![0f32; n], vec![0f32; n]);
            for (i, x) in (r.x0..r.x1).enumerate() {
                let [u, v] = proj.view_to_equirect(f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(iw), f64::from(ih));
                sample_wrapped(target, u, v, iw, ih, &mut px, &mut tmp);
                line[i * n..(i + 1) * n].copy_from_slice(&px);
            }
        });
    }
    let mut s = Surface::new(target.format());
    s.write_region(r, &data);
    s
}

/// Write `post − pre` (view space, over `damage`) back into the equirect `target`, scaled by the
/// equirect `selection` coverage when there is one. Returns the equirect damage.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_back(target: &mut Surface, proj: &ViewProjection, pre: &Surface, post: &Surface, damage: Rect, iw: i32, ih: i32, selection: Option<&Surface>) -> Rect {
    let n = target.channels();
    let (pre_b, post_b) = (pre.read_region(damage), post.read_region(damage));
    let alpha = target.format().alpha.then(|| n - 1);
    let r = [f64::from(damage.x0), f64::from(damage.y0), f64::from(damage.x1), f64::from(damage.y1)];
    let mut total = Rect::EMPTY;
    for q in view_rect_to_equirect(proj, r, iw as u32, ih as u32, 1.0) {
        let rect = Rect::new(q[0] as i32, q[1] as i32, q[2] as i32, q[3] as i32);
        if rect.is_empty() {
            continue;
        }
        let mut buf = target.read_region(rect);
        // Rows in parallel: a stroke near a pole touches the full image width.
        use rayon::prelude::*;
        let row = rect.width() as usize * n;
        let touched = buf
            .par_chunks_mut(row)
            .enumerate()
            .map(|(j, line)| {
                let y = rect.y0 + j as i32;
                let (mut a, mut b) = (vec![0f32; n], vec![0f32; n]);
                let mut touched = false;
                let mut m = vec![0f32; selection.map_or(1, Surface::channels)];
                for (i, x) in (rect.x0..rect.x1).enumerate() {
                    let Some([vx, vy]) = proj.equirect_to_view(f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(iw), f64::from(ih)) else { continue };
                    if !(vx >= r[0] && vx < r[2] && vy >= r[1] && vy < r[3]) {
                        continue;
                    }
                    let k = match selection {
                        Some(sel) => {
                            sel.read_pixel(x, y, &mut m);
                            m[0].clamp(0.0, 1.0)
                        }
                        None => 1.0,
                    };
                    if k <= 0.0 {
                        continue;
                    }
                    sample_buf(&pre_b, damage, n, vx, vy, &mut a);
                    sample_buf(&post_b, damage, n, vx, vy, &mut b);
                    for c in 0..n {
                        let d = (b[c] - a[c]) * k;
                        if d != 0.0 && d.is_finite() {
                            touched = true;
                            let v = line[i * n + c] + d;
                            line[i * n + c] = if Some(c) == alpha { v.clamp(0.0, 1.0) } else { v.max(0.0) };
                        }
                    }
                }
                touched
            })
            .reduce(|| false, |x, y| x || y);
        if touched {
            target.write_region(rect, &buf);
            total = if total.is_empty() { rect } else { total.union(&rect) };
        }
    }
    total
}

/// What a live projected stroke paints with.
pub(crate) enum PreviewKind {
    Brush { brush: Box<photocraft_paint::BrushSettings>, lock: bool },
    /// The Clone Stamp: the rotated source in view space, painted through the stroke coverage.
    Clone { source: Surface, brush: Box<photocraft_paint::BrushSettings>, mode: photocraft_color::BlendMode, lock: bool },
}

/// A projected stroke shown while it is drawn (View › Equirectangular Live Projection): the
/// target resampled into the whole view once, then every update renders the stroke so far with
/// the real brush engine (or the Clone Stamp's coverage and source) on a copy, and hands back an
/// RGBA8 overlay of what changed. Only view-sized buffers are touched, whatever the document
/// size; the commit (`paint.projectedStroke` / `paint.projectedClone`) reprojects on release.
pub struct ProjectedPreview {
    pre: Surface,
    view: Rect,
    /// The active selection resampled into the view: strokes are clipped by it.
    sel: Option<Surface>,
    pub(crate) kind: PreviewKind,
}

/// One preview frame: `px` (unmultiplied sRGB RGBA8, row-major) covers view pixels `rect`.
pub struct PreviewOverlay {
    pub rect: Rect,
    pub px: Vec<[u8; 4]>,
}

impl ProjectedPreview {
    /// A Brush or Eraser preview from `paint.projectedStroke` params (points unused).
    pub fn brush(s: &Session, p: &Value) -> Result<Self> {
        let proj = projection(p)?;
        let brush = with_blend_mode(resolve_brush(s, p, "paint.projectedStroke")?, p);
        let bg = s.tools.background;
        let (id, mut brush, _) = stroke_target(s, p, brush)?;
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let mut doc = (*st.doc).clone();
        let (iw, ih) = (doc.size.width as i32, doc.size.height as i32);
        let view = Rect::new(0, 0, proj.w as i32, proj.h as i32);
        let sel = doc.selection.clone().map(|m| to_view(&m, &proj, view, iw, ih));
        let (surf, lock) = crate::channel_cmds::target_surface(&mut doc, id, p)?;
        erase_locked(&mut brush, lock, bg);
        Ok(Self { pre: to_view(surf, &proj, view, iw, ih), view, sel, kind: PreviewKind::Brush { brush: Box::new(brush), lock } })
    }

    pub(crate) fn with(pre: Surface, view: Rect, sel: Option<Surface>, kind: PreviewKind) -> Self {
        Self { pre, view, sel, kind }
    }

    /// Render `points` (view pixels, `[x, y, pressure]`). `None` when nothing shows yet or the
    /// target isn't RGB/gray.
    pub fn render(&self, points: &[[f64; 3]]) -> Option<PreviewOverlay> {
        let pts: Vec<photocraft_paint::StrokePoint> = points.iter().map(|q| photocraft_paint::StrokePoint::new(q[0], q[1], q[2] as f32)).collect();
        if pts.is_empty() {
            return None;
        }
        let mut post = self.pre.clone();
        let damage = match &self.kind {
            PreviewKind::Brush { brush, lock } => photocraft_paint::render_stroke(&mut post, brush, &pts, self.sel.as_ref(), *lock, 1.0),
            PreviewKind::Clone { source, brush, mode, lock } => {
                let stroke = photocraft_paint::Stroke { brush: (**brush).clone(), points: pts };
                let (bounds, cov) = photocraft_paint::retouch::stroke_coverage(&stroke);
                let work = bounds.intersect(&self.view);
                if work.is_empty() {
                    return None;
                }
                let cov = crop_cov(&cov, bounds, work);
                let paint = photocraft_paint::retouch::Region::read(source, work);
                photocraft_paint::retouch::apply_coverage(&mut post, work, &cov, brush.opacity, self.sel.as_ref(), *lock, &paint, *mode)
            }
        }
        .intersect(&self.view);
        if damage.is_empty() {
            return None;
        }
        overlay(&self.pre, &post, damage)
    }
}

/// `cov` (row-major over `from`) cut to `to` (inside `from`).
pub(crate) fn crop_cov(cov: &[f32], from: Rect, to: Rect) -> Vec<f32> {
    if from == to {
        return cov.to_vec();
    }
    let fw = from.width() as usize;
    let mut out = Vec::with_capacity(to.width() as usize * to.height() as usize);
    for y in to.y0..to.y1 {
        let row = (y - from.y0) as usize * fw;
        out.extend_from_slice(&cov[row + (to.x0 - from.x0) as usize..row + (to.x1 - from.x0) as usize]);
    }
    out
}

/// What the stroke changed over `r`, as an overlay: painted pixels as they now are (their own
/// alpha), erased ones as a translucent grey veil by how much was removed, unchanged ones clear.
pub(crate) fn overlay(pre: &Surface, post: &Surface, r: Rect) -> Option<PreviewOverlay> {
    use photocraft_color::{ColorMode, SampleType};
    let f = pre.format();
    let n = f.channels();
    let gray = match f.mode {
        ColorMode::Rgb => false,
        ColorMode::Grayscale => true,
        _ => return None,
    };
    let linear = f.sample == SampleType::F32;
    let (a, b) = (pre.read_region(r), post.read_region(r));
    let enc = |v: f32| -> u8 {
        let v = v.max(0.0);
        let v = if linear { if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.min(1.0).powf(1.0 / 2.4) - 0.055 } } else { v };
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    };
    let px = a
        .chunks_exact(n)
        .zip(b.chunks_exact(n))
        .map(|(p, q)| {
            if p == q {
                return [0, 0, 0, 0];
            }
            let (pa, qa) = if f.alpha { (p[n - 1], q[n - 1]) } else { (1.0, 1.0) };
            if qa + 1e-4 < pa {
                let lost = ((pa - qa) * 0.6 * 255.0) as u8;
                return [150, 150, 150, lost];
            }
            let (r, g, b) = if gray { (q[0], q[0], q[0]) } else { (q[0], q[1], q[2]) };
            [enc(r), enc(g), enc(b), (qa.clamp(0.0, 1.0) * 255.0 + 0.5) as u8]
        })
        .collect();
    Some(PreviewOverlay { rect: r, px })
}

/// `paint.projectedStroke`: `paint.stroke` params with `points` in view pixels and the view's
/// `projection` camera; the brush size is in view pixels.
pub fn paint_projected_stroke(s: &mut Session, p: &Value) -> Result<Value> {
    let proj = projection(p)?;
    let pts = parse_points(p, "paint.projectedStroke")?;
    if pts.is_empty() {
        return Err(bad("paint.projectedStroke", "no points"));
    }
    let brush = with_blend_mode(resolve_brush(s, p, "paint.projectedStroke")?, p);
    let label = if brush.erase { "Eraser" } else { "Brush Tool" };
    let bg = s.tools.background;
    let (id, brush, _) = stroke_target(s, p, brush)?;
    let dmg = s.edit(label, |doc, _| {
        let (iw, ih) = (doc.size.width as i32, doc.size.height as i32);
        let selection = doc.selection.clone();
        if iw < 2 || ih < 1 {
            return Err(EngineError::Other("document too small for a projected stroke".into()));
        }
        let (surf, lock) = crate::channel_cmds::target_surface(doc, id, p)?;
        let mut brush = brush;
        erase_locked(&mut brush, lock, bg);
        // The view-space area the stroke can reach: its points, padded by the brush.
        let reach = (f64::from(brush.size) * 0.75 + 4.0).ceil();
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for q in &pts {
            (x0, y0, x1, y1) = (x0.min(q.x), y0.min(q.y), x1.max(q.x), y1.max(q.y));
        }
        let work = Rect::new((x0 - reach).floor() as i32, (y0 - reach).floor() as i32, (x1 + reach).ceil() as i32, (y1 + reach).ceil() as i32)
            .intersect(&Rect::new(0, 0, proj.w as i32, proj.h as i32));
        if work.is_empty() {
            return Ok(Rect::EMPTY);
        }
        let pre = to_view(surf, &proj, work, iw, ih);
        let mut post = pre.clone();
        let damage = photocraft_paint::render_stroke(&mut post, &brush, &pts, None, lock, 1.0).intersect(&work);
        if damage.is_empty() {
            return Ok(Rect::EMPTY);
        }
        Ok(write_back(surf, &proj, &pre, &post, damage, iw, ih, selection.as_ref()))
    })?;
    Ok(damage_json(s, dmg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(s: &Session, x: i32, y: i32) -> [f32; 4] {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
    }

    #[test]
    fn projected_stroke_paints_where_the_view_looks_and_undoes() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 720, "height": 360, "background": "transparent"})).unwrap();
        // Look at yaw 90° (u = 540), straight ahead; dab the view centre in red.
        let p = json!({
            "points": [[200, 150], [200, 150]],
            "size": 20, "hardness": 1.0, "color": "#ff0000",
            "projection": {"yaw": 90.0, "pitch": 0.0, "fov": 90.0, "width": 400, "height": 300}
        });
        let r = s.execute("paint.projectedStroke", p).unwrap();
        let d = r["damage"].as_array().unwrap();
        assert!(d[2].as_u64().unwrap() > 0, "{r}");
        let c = rgba(&s, 540, 180);
        assert!(c[0] > 0.9 && c[3] > 0.9, "centre painted red: {c:?}");
        assert_eq!(rgba(&s, 180, 180)[3], 0.0, "the opposite side is untouched");
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(rgba(&s, 540, 180)[3], 0.0, "undone");
    }

    #[test]
    fn live_preview_shows_the_brush_colour_with_its_tip_alpha() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 720, "height": 360, "background": "transparent"})).unwrap();
        let p = json!({
            "size": 30, "hardness": 0.0, "color": "#ff0000",
            "projection": {"yaw": 0.0, "pitch": 0.0, "fov": 90.0, "width": 400, "height": 300}
        });
        let live = ProjectedPreview::brush(&s, &p).unwrap();
        let o = live.render(&[[200.0, 150.0, 1.0]]).expect("a dab shows");
        let at = |x: i32, y: i32| o.px[((y - o.rect.y0) * o.rect.width() as i32 + (x - o.rect.x0)) as usize];
        let c = at(200, 150);
        assert!(c[0] > 240 && c[1] < 10 && c[3] > 200, "centre is opaque red, not black: {c:?}");
        // A soft tip: the edge is fainter, and the corners of the dab's box stay clear.
        assert!(at(212, 150)[3] < c[3]);
        assert_eq!(at(o.rect.x0, o.rect.y0)[3], 0, "not a square stamp");
        // Nothing was written to the document.
        assert_eq!(rgba(&s, 360, 180)[3], 0.0);
    }

    #[test]
    fn projected_stroke_across_the_seam_paints_both_edges() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 720, "height": 360, "background": "transparent"})).unwrap();
        let p = json!({
            "points": [[200, 150]], "size": 40, "hardness": 1.0, "color": "#0000ff",
            "projection": {"yaw": 180.0, "pitch": 0.0, "fov": 90.0, "width": 400, "height": 300}
        });
        s.execute("paint.projectedStroke", p).unwrap();
        assert!(rgba(&s, 1, 180)[3] > 0.9, "left edge painted: {:?}", rgba(&s, 1, 180));
        assert!(rgba(&s, 718, 180)[3] > 0.9, "right edge painted: {:?}", rgba(&s, 718, 180));
    }
}
