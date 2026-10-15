//! `select.projected`: the selection tools in View › Equirectangular Live Projection.
//!
//! The shape (marquee, ellipse, lasso) is rasterised, or the pixel-based tool (Magic Wand, Quick
//! Selection, Object Selection) runs, in *view* space over the image resampled into the view.
//! The view-space mask (feathered there when asked) is then reprojected onto the equirect
//! selection: only the equirect rectangles the mask can reach are read and written (split at the
//! ±180° seam, full width at a pole), and it combines with the current selection by `mode` as one
//! History step.

use photocraft_algo::segment::{SurfaceSampler, grabcut, quick};
use photocraft_algo::selection::{self as sel, Region, SelectionMode};
use photocraft_geom::Rect;
use photocraft_color::PixelFormat;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::brush_cmds::projected::to_view;
use crate::equirect::{Camera, ViewProjection, view_rect_to_equirect};
use crate::{EngineError, Result, Session};

const CMD: &str = "select.projected";

fn bad(msg: &str) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

fn projection(p: &Value) -> Result<ViewProjection> {
    let q = p.get("projection").ok_or_else(|| bad("missing `projection`"))?;
    let g = |k: &str, d: f64| q.get(k).and_then(Value::as_f64).unwrap_or(d);
    let (w, h) = (g("width", 0.0), g("height", 0.0));
    if !(w >= 1.0 && h >= 1.0 && w <= 16384.0 && h <= 16384.0) {
        return Err(bad("`projection.width` and `height` must be 1..=16384"));
    }
    Ok(ViewProjection::new(Camera { yaw: g("yaw", 0.0), pitch: g("pitch", 0.0), fov: g("fov", 90.0) }, w, h))
}

fn f(p: &Value, k: &str, d: f64) -> f64 {
    p.get(k).and_then(Value::as_f64).unwrap_or(d)
}
fn b(p: &Value, k: &str, d: bool) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(d)
}

fn points(p: &Value) -> Vec<(f32, f32)> {
    p.get("points")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|q| {
                    let q = q.as_array()?;
                    Some((q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32))
                })
                .filter(|(x, y)| x.is_finite() && y.is_finite())
                .collect()
        })
        .unwrap_or_default()
}

/// `"rect": [x0, y0, x1, y1]` in view pixels, normalised.
fn rect4(p: &Value) -> Option<[f32; 4]> {
    let v: Vec<f32> = p.get("rect")?.as_array()?.iter().filter_map(Value::as_f64).map(|v| v as f32).collect();
    if v.len() < 4 || v.iter().any(|v| !v.is_finite()) {
        return None;
    }
    Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

fn region_to_mask(r: &Region, view: Rect) -> Vec<f32> {
    let w = view.width() as usize;
    let mut m = vec![0f32; w * view.height() as usize];
    let bb = r.bbox.intersect(&view);
    for y in bb.y0..bb.y1 {
        for x in bb.x0..bb.x1 {
            m[(y - view.y0) as usize * w + (x - view.x0) as usize] = r.at(x, y);
        }
    }
    m
}

/// The pixels the wand / quick / object tools look at, resampled into the whole view: the active
/// layer, or the composite (only where the view looks) with `sampleAllLayers` or no pixel layer.
fn view_pixels(s: &Session, proj: &ViewProjection, view: Rect, all_layers: bool) -> Result<Surface> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = &st.doc;
    let (iw, ih) = (doc.size.width as i32, doc.size.height as i32);
    let layer = st.active_layer.and_then(|id| doc.layer(id)).and_then(|l| l.surface()).filter(|_| !all_layers);
    if let Some(surf) = layer {
        return Ok(to_view(surf, proj, view, iw, ih));
    }
    let mut flat = Surface::new(PixelFormat::RGBA32F);
    let r = [f64::from(view.x0), f64::from(view.y0), f64::from(view.x1), f64::from(view.y1)];
    for e in view_rect_to_equirect(proj, r, iw as u32, ih as u32, 2.0) {
        let rect = Rect::new(e[0] as i32, e[1] as i32, e[2] as i32, e[3] as i32).intersect(&doc.bounds());
        if !rect.is_empty() {
            let px = photocraft_compose::render(doc, rect).px;
            flat.write_region(rect, px.as_flattened());
        }
    }
    Ok(to_view(&flat, proj, view, iw, ih))
}

/// The view-space coverage mask (row-major over `view`) the params describe.
fn view_mask(s: &Session, p: &Value, proj: &ViewProjection, view: Rect) -> Result<(&'static str, Vec<f32>)> {
    let aa = b(p, "antiAlias", true);
    let kind = p.get("kind").and_then(Value::as_str).unwrap_or("rect");
    let all = b(p, "sampleAllLayers", false);
    Ok(match kind {
        "rect" | "ellipse" => {
            let [x0, y0, x1, y1] = rect4(p).ok_or_else(|| bad("`rect` must be [x0, y0, x1, y1]"))?;
            let poly: Vec<(f32, f32)> = if kind == "rect" {
                vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            } else {
                let (cx, cy, rx, ry) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0, (y1 - y0) / 2.0);
                let n = 256;
                (0..n).map(|i| std::f32::consts::TAU * i as f32 / n as f32).map(|a| (cx + rx * a.cos(), cy + ry * a.sin())).collect()
            };
            (if kind == "rect" { "Rectangular Marquee" } else { "Elliptical Marquee" }, sel::polygon(&poly, view, aa && kind == "ellipse"))
        }
        "lasso" => {
            let pts = points(p);
            if pts.len() < 3 {
                return Err(bad("a lasso needs at least 3 `points`"));
            }
            ("Lasso", sel::polygon(&pts, view, aa))
        }
        "wand" => {
            let pts = points(p);
            let &(x, y) = pts.first().ok_or_else(|| bad("the Magic Wand needs `points`: [[x, y]]"))?;
            let px = view_pixels(s, proj, view, all)?;
            let img = sel::rgba8_image(&px, view);
            let r = sel::wand_region(&img, view, (x.floor() as i32, y.floor() as i32), f(p, "tolerance", 32.0) as f32, b(p, "contiguous", true), aa);
            ("Magic Wand", r.map(|r| region_to_mask(&r, view)).unwrap_or_else(|| vec![0.0; view.size().area() as usize]))
        }
        "quick" => {
            let pts = points(p);
            if pts.is_empty() {
                return Err(bad("Quick Selection needs `points`"));
            }
            let px = view_pixels(s, proj, view, all)?;
            let r = quick::quick_select(&SurfaceSampler(&px), view, &pts, f(p, "size", 30.0).clamp(1.0, 5000.0) as f32);
            ("Quick Selection", r.map(|r| region_to_mask(&r, view)).unwrap_or_else(|| vec![0.0; view.size().area() as usize]))
        }
        "object" => {
            let [x0, y0, x1, y1] = rect4(p).ok_or_else(|| bad("`rect` must be [x0, y0, x1, y1]"))?;
            let px = view_pixels(s, proj, view, all)?;
            let rect = Rect::new(x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32, y1.ceil() as i32).intersect(&view);
            let r = grabcut::object_select(&SurfaceSampler(&px), view, rect, 160_000);
            ("Object Selection", r.map(|r| region_to_mask(&r, view)).unwrap_or_else(|| vec![0.0; view.size().area() as usize]))
        }
        _ => return Err(bad("`kind` must be rect, ellipse, lasso, wand, quick, object, row or column")),
    })
}

/// Bounding box of the nonzero mask samples.
fn mask_bounds(m: &[f32], view: Rect) -> Rect {
    let w = view.width() as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (i, v) in m.iter().enumerate() {
        if *v > 0.0 {
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
        }
    }
    if x0 > x1 { Rect::EMPTY } else { Rect::new(x0 + view.x0, y0 + view.y0, x1 + view.x0, y1 + view.y0) }
}

/// The view mask reprojected onto the equirect: one 8-bit region per equirect rectangle it
/// reaches (disjoint: the seam splits them).
fn reproject(mask: &[f32], view: Rect, proj: &ViewProjection, iw: i32, ih: i32) -> Vec<Region> {
    use rayon::prelude::*;
    let reach = mask_bounds(mask, view);
    if reach.is_empty() {
        return Vec::new();
    }
    let vw = view.width() as i64;
    let vh = view.height() as i64;
    let sample = |x: f64, y: f64| -> f32 {
        // Bilinear, zero outside the view.
        let (fx, fy) = (x - 0.5 - f64::from(view.x0), y - 0.5 - f64::from(view.y0));
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let mut v = 0.0;
        for (dy, wy) in [(0, 1.0 - ty), (1, ty)] {
            for (dx, wx) in [(0, 1.0 - tx), (1, tx)] {
                let (xx, yy) = (x0 + dx, y0 + dy);
                if xx >= 0 && yy >= 0 && xx < vw && yy < vh {
                    v += mask[(yy * vw + xx) as usize] * wx * wy;
                }
            }
        }
        v
    };
    let r = [f64::from(reach.x0) - 1.0, f64::from(reach.y0) - 1.0, f64::from(reach.x1) + 1.0, f64::from(reach.y1) + 1.0];
    let mut out = Vec::new();
    for e in view_rect_to_equirect(proj, r, iw as u32, ih as u32, 2.0) {
        let rect = Rect::new(e[0] as i32, e[1] as i32, e[2] as i32, e[3] as i32).intersect(&Rect::new(0, 0, iw, ih));
        if rect.is_empty() {
            continue;
        }
        let w = rect.width() as usize;
        let mut bytes = vec![0u8; w * rect.height() as usize];
        bytes.par_chunks_mut(w).enumerate().for_each(|(j, line)| {
            let y = rect.y0 + j as i32;
            for (i, o) in line.iter_mut().enumerate() {
                let x = rect.x0 + i as i32;
                if let Some([vx, vy]) = proj.equirect_to_view(f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(iw), f64::from(ih))
                    && vx >= r[0]
                    && vy >= r[1]
                    && vx < r[2]
                    && vy < r[3]
                {
                    *o = (sample(vx, vy).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        });
        out.push(Region { bbox: rect, mask: bytes });
    }
    out
}

/// Combine the reprojected `regions` with `old` by `mode`.
fn combine(old: Option<&Surface>, regions: &[Region], mode: SelectionMode) -> Option<Surface> {
    match mode {
        SelectionMode::Replace => regions.iter().fold(None, |acc, r| sel::combine_region(acc.as_ref(), Some(r), SelectionMode::Add)),
        SelectionMode::Add | SelectionMode::Subtract => regions.iter().fold(old.cloned(), |acc, r| sel::combine_region(acc.as_ref(), Some(r), mode)),
        SelectionMode::Intersect => {
            let old = old?;
            // Only where the new shape reaches can anything stay.
            regions.iter().fold(None, |acc, r| {
                let kept = sel::combine_region(Some(old), Some(r), SelectionMode::Intersect)?;
                let mut v = Vec::new();
                kept.read_region_into(r.bbox, &mut v);
                let ch = kept.channels();
                let mask = v.chunks_exact(ch).map(|p| (p[0].clamp(0.0, 1.0) * 255.0).round() as u8).collect();
                sel::combine_region(acc.as_ref(), Some(&Region { bbox: r.bbox, mask }), SelectionMode::Add)
            })
        }
    }
}

/// `select.projected` — see the module docs. Params: `kind` (rect | ellipse | lasso | wand |
/// quick | object), `rect` [x0, y0, x1, y1] or `points` [[x, y], …] in view pixels, `mode`,
/// `feather` (view pixels), `antiAlias`, `tolerance`, `contiguous`, `size`, `sampleAllLayers`,
/// and the view's `projection`.
pub fn select_projected(s: &mut Session, p: &Value) -> Result<Value> {
    let proj = projection(p)?;
    if let Some(kind @ ("row" | "column")) = p.get("kind").and_then(Value::as_str) {
        return single_line(s, p, &proj, kind == "row");
    }
    let view = Rect::new(0, 0, proj.w as i32, proj.h as i32);
    let (label, mut mask) = view_mask(s, p, &proj, view)?;
    let feather = f(p, "feather", 0.0) as f32;
    if feather > 0.0 {
        mask = sel::feather(&mask, view.width() as usize, view.height() as usize, feather);
    }
    let mode = SelectionMode::parse(p.get("mode").and_then(Value::as_str).unwrap_or("replace"));
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (iw, ih) = (st.doc.size.width as i32, st.doc.size.height as i32);
    if iw < 2 || ih < 1 {
        return Err(EngineError::Other("document too small for a projected selection".into()));
    }
    let regions = reproject(&mask, view, &proj, iw, ih);
    let label = if regions.is_empty() && mode == SelectionMode::Replace { "Deselect" } else { label };
    let selected = s.edit(label, |doc, _| {
        doc.selection = combine(doc.selection.as_ref(), &regions, mode);
        Ok(doc.selection.is_some())
    })?;
    Ok(json!({ "selected": selected }))
}

/// Single Row / Single Column Marquee: the equirect pixel row (a line of latitude) or column (a
/// meridian) through the clicked direction.
fn single_line(s: &mut Session, p: &Value, proj: &ViewProjection, row: bool) -> Result<Value> {
    let &(x, y) = points(p).first().ok_or_else(|| bad("Single Row/Column needs `points`: [[x, y]]"))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (iw, ih) = (st.doc.size.width as i32, st.doc.size.height as i32);
    let [u, v] = proj.view_to_equirect(f64::from(x), f64::from(y), f64::from(iw), f64::from(ih));
    let (u, v) = ((u.floor() as i32).rem_euclid(iw.max(1)), (v.floor() as i32).clamp(0, ih - 1));
    let bbox = if row { Rect::new(0, v, iw, v + 1) } else { Rect::new(u, 0, u + 1, ih) };
    let region = Region { bbox, mask: vec![255; bbox.size().area() as usize] };
    let mode = SelectionMode::parse(p.get("mode").and_then(Value::as_str).unwrap_or("replace"));
    let label = if row { "Single Row Marquee" } else { "Single Column Marquee" };
    let selected = s.edit(label, |doc, _| {
        doc.selection = combine(doc.selection.as_ref(), std::slice::from_ref(&region), mode);
        Ok(doc.selection.is_some())
    })?;
    Ok(json!({ "selected": selected }))
}

/// The pixels the selection tools see, resampled into one view once: the live Quick Selection
/// preview and the Magnetic Lasso's edge tracing work on this, never on the full equirect.
pub struct ViewPixels {
    px: Surface,
    view: Rect,
    tracer: photocraft_algo::magnetic::Tracer,
}

impl ViewPixels {
    /// From `select.projected`-style params (`projection`, `sampleAllLayers`).
    pub fn new(s: &Session, p: &Value) -> Result<Self> {
        let proj = projection(p)?;
        let view = Rect::new(0, 0, proj.w as i32, proj.h as i32);
        let px = view_pixels(s, &proj, view, b(p, "sampleAllLayers", false))?;
        Ok(Self { px, view, tracer: photocraft_algo::magnetic::Tracer::new(view) })
    }

    /// Quick Selection over `points` (view px) at brush `size`: coverage bytes over the view.
    pub fn quick(&self, points: &[(f32, f32)], size: f32) -> Option<Region> {
        quick::quick_select(&SurfaceSampler(&self.px), self.view, points, size.clamp(1.0, 5000.0))
    }

    /// Magnetic Lasso: snap a fastening point to the nearest edge.
    pub fn snap(&mut self, p: [f64; 2], st: photocraft_algo::magnetic::Settings) -> Option<[f64; 2]> {
        let px = &self.px;
        self.tracer.snap(&mut |r| sel::rgba8_image(px, r), p, st)
    }

    /// Magnetic Lasso: the border from `from` to `to` along the edges near `guide`.
    pub fn trace(&mut self, from: [f64; 2], to: [f64; 2], guide: &[[f64; 2]], st: photocraft_algo::magnetic::Settings) -> Vec<[f64; 2]> {
        let px = &self.px;
        self.tracer.trace(&mut |r| sel::rgba8_image(px, r), from, to, guide, st)
    }
}

/// The active selection resampled into a view (`None` without one): one coverage byte per view
/// pixel, row-major. The UI draws the marching ants from its edges.
pub fn projected_selection_mask(doc: &photocraft_doc::Document, cam: Camera, w: u32, h: u32) -> Option<Vec<u8>> {
    let m = doc.selection.as_ref()?;
    let proj = ViewProjection::new(cam, f64::from(w), f64::from(h));
    let view = Rect::new(0, 0, w as i32, h as i32);
    let v = to_view(m, &proj, view, doc.size.width as i32, doc.size.height as i32);
    let mut out = Vec::new();
    v.read_region_into(view, &mut out);
    Some(out.iter().map(|c| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 360, "height": 180, "background": "white"})).unwrap();
        s
    }
    fn view(yaw: f64) -> Value {
        json!({"yaw": yaw, "pitch": 0.0, "fov": 60.0, "width": 200, "height": 200})
    }
    fn cov(s: &Session, x: i32, y: i32) -> f32 {
        let d = s.active().unwrap();
        d.doc.selection.as_ref().map_or(0.0, |m| {
            let mut v = [0f32];
            m.read_pixel(x, y, &mut v);
            v[0]
        })
    }

    #[test]
    fn marquee_across_the_seam_selects_both_edges() {
        // Looking at ±180° (yaw 180): the seam runs down the middle of the view.
        let mut s = doc();
        s.execute(CMD, json!({"kind": "rect", "rect": [80, 80, 120, 120], "projection": view(180.0)})).unwrap();
        assert!(cov(&s, 1, 90) > 0.99, "right of the seam (left image edge)");
        assert!(cov(&s, 358, 90) > 0.99, "left of the seam (right image edge)");
        assert!(cov(&s, 180, 90) == 0.0 && cov(&s, 20, 90) == 0.0, "nothing else");
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.selection.is_none(), "one undo step");
    }

    #[test]
    fn add_subtract_and_intersect_combine_with_the_selection() {
        let mut s = doc();
        // Yaw 0 looks at the image centre (x 180).
        s.execute(CMD, json!({"kind": "rect", "rect": [60, 60, 100, 140], "projection": view(0.0)})).unwrap();
        s.execute(CMD, json!({"kind": "rect", "rect": [100, 60, 140, 140], "mode": "add", "projection": view(0.0)})).unwrap();
        assert!(cov(&s, 172, 90) > 0.99 && cov(&s, 188, 90) > 0.99, "added");
        s.execute(CMD, json!({"kind": "ellipse", "rect": [90, 90, 110, 110], "mode": "subtract", "projection": view(0.0)})).unwrap();
        assert!(cov(&s, 180, 90) < 0.01, "subtracted the middle");
        assert!(cov(&s, 172, 82) > 0.99, "kept the rest");
        s.execute(CMD, json!({"kind": "rect", "rect": [60, 60, 100, 140], "mode": "intersect", "projection": view(0.0)})).unwrap();
        assert!(cov(&s, 172, 82) > 0.99 && cov(&s, 188, 82) < 0.01, "intersected");
        s.execute("edit.undo", json!({})).unwrap();
        assert!(cov(&s, 188, 82) > 0.99, "undo restores");
    }

    #[test]
    fn feathered_lasso_is_soft_and_wand_selects_the_region() {
        let mut s = doc();
        s.execute(CMD, json!({"kind": "lasso", "points": [[60, 60], [140, 60], [140, 140], [60, 140]], "feather": 6, "projection": view(0.0)})).unwrap();
        assert!(cov(&s, 180, 90) > 0.95);
        // The lasso's right edge (view x 140) lands near equirect x 193.
        let soft = (186..200).filter(|x| (0.05..0.95).contains(&cov(&s, *x, 90))).count();
        assert!(soft >= 3, "a feathered edge spans pixels, not one step ({soft})");
        // A white canvas: the wand takes the whole view's worth.
        s.execute(CMD, json!({"kind": "wand", "points": [[100, 100]], "projection": view(90.0)})).unwrap();
        assert!(cov(&s, 270, 90) > 0.99 && cov(&s, 180, 90) < 0.01, "replaced by the wand");
    }

    #[test]
    fn single_row_is_a_line_of_latitude() {
        let mut s = doc();
        s.execute(CMD, json!({"kind": "row", "points": [[100, 100]], "projection": view(0.0)})).unwrap();
        assert!(cov(&s, 0, 90) > 0.99 && cov(&s, 359, 90) > 0.99, "the whole row, all the way round");
        assert!(cov(&s, 180, 88) == 0.0);
        s.execute(CMD, json!({"kind": "column", "points": [[100, 100]], "mode": "add", "projection": view(0.0)})).unwrap();
        assert!(cov(&s, 180, 0) > 0.99 && cov(&s, 180, 179) > 0.99, "pole to pole");
    }

    #[test]
    fn view_pixels_quick_selection_and_trace() {
        let mut s = doc();
        let v = ViewPixels::new(&s, &json!({"projection": view(0.0)})).unwrap();
        let r = v.quick(&[(100.0, 100.0)], 10.0).expect("white canvas: a region");
        assert!(r.at(100, 100) > 0.5);
        let mut v = v;
        let t = v.trace([20.0, 20.0], [80.0, 20.0], &[], photocraft_algo::magnetic::Settings::new(10.0, 0.1));
        assert!(t.len() >= 2);
        let _ = &mut s;
    }

    #[test]
    fn projected_brush_is_clipped_by_the_selection() {
        let mut s = doc();
        s.execute("tools.setColors", json!({"foreground": [1.0, 0.0, 0.0, 1.0]})).unwrap();
        s.execute(CMD, json!({"kind": "rect", "rect": [0, 0, 100, 200], "projection": view(0.0)})).unwrap();
        s.execute("paint.projectedStroke", json!({"points": [[60, 100], [140, 100]], "size": 20, "hardness": 100, "projection": view(0.0)})).unwrap();
        let px = |x: i32, y: i32| {
            let d = s.active().unwrap();
            d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
        };
        assert!(px(170, 90)[1] < 0.1, "painted inside the selection: {:?}", px(170, 90));
        assert!(px(190, 90)[1] > 0.99, "untouched outside: {:?}", px(190, 90));
    }
}
