//! `paint.projectedClone`: the Clone Stamp in View › Equirectangular Live Projection.
//!
//! The source is a *rotation of the sphere* (`"rotation"`, row-major 3×3): destination direction
//! `d` samples the image at direction `rotation · d`. Both sides are resampled into the view the
//! stroke is drawn in, so the clone looks undistorted there, the ordinary Clone Stamp coverage and
//! blend run in view space, and the change is written back onto the equirect pixels as the
//! projected Brush does (`brush_cmds::projected`). One History step per stroke.

use photocraft_geom::Rect;
use photocraft_paint::retouch::{Region, apply_coverage, stroke_coverage};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use super::{SampleLayers, bad, blend_param, composite_region, damage_json, parse_brush, run_stroke, sample_layers};
use crate::brush_cmds::projected::{to_view, write_back};
use crate::equirect::{Camera, ViewProjection, rot_from_slice, view_rect_to_equirect};
use crate::{Result, Session};

const CMD: &str = "paint.projectedClone";

pub(super) fn projected_clone(s: &mut Session, p: &Value) -> Result<Value> {
    let q = p.get("projection").ok_or_else(|| bad(CMD, "missing `projection`"))?;
    let g = |k: &str, d: f64| q.get(k).and_then(Value::as_f64).unwrap_or(d);
    let (w, h) = (g("width", 0.0), g("height", 0.0));
    if !(w >= 1.0 && h >= 1.0 && w <= 16384.0 && h <= 16384.0) {
        return Err(bad(CMD, "`projection.width` and `height` must be 1..=16384"));
    }
    let proj = ViewProjection::new(Camera { yaw: g("yaw", 0.0), pitch: g("pitch", 0.0), fov: g("fov", 90.0) }, w, h);
    let rotation: Vec<f64> = p.get("rotation").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    let rotation = rot_from_slice(&rotation).ok_or_else(|| bad(CMD, "`rotation` must be a 3×3 rotation (9 numbers, row-major)"))?;
    let src_proj = proj.rotated(&rotation);
    let (stroke, id) = parse_brush(s, p, CMD)?;
    let which = sample_layers(p, CMD)?;
    let mode = blend_param(p, CMD)?;
    let dmg = run_stroke(s, "Clone Stamp", id, p, |pre, surf, sel, lock| {
        let (iw, ih) = (pre.size.width as i32, pre.size.height as i32);
        if iw < 2 || ih < 1 {
            return Err(bad(CMD, "document too small for a projected stroke"));
        }
        let (bounds, cov) = stroke_coverage(&stroke);
        let view = Rect::new(0, 0, w as i32, h as i32);
        let work = bounds.intersect(&view);
        if work.is_empty() {
            return Ok(Rect::EMPTY);
        }
        // Coverage only over the part of the stroke inside the view.
        let cov = crop(&cov, bounds, work);
        let pre_view = to_view(surf, &proj, work, iw, ih);
        let source = match which {
            SampleLayers::Current => to_view(surf, &src_proj, work, iw, ih),
            _ => {
                // The composite only where the rotated view looks.
                let fmt = surf.format();
                let mut flat = Surface::new(fmt);
                let r = [f64::from(work.x0), f64::from(work.y0), f64::from(work.x1), f64::from(work.y1)];
                for e in view_rect_to_equirect(&src_proj, r, iw as u32, ih as u32, 2.0) {
                    let rect = Rect::new(e[0] as i32, e[1] as i32, e[2] as i32, e[3] as i32);
                    if !rect.is_empty() {
                        flat.write_region(rect, &composite_region(pre, id, which, rect, fmt).data);
                    }
                }
                to_view(&flat, &src_proj, work, iw, ih)
            }
        };
        let paint = Region::read(&source, work);
        let mut post = pre_view.clone();
        let damage = apply_coverage(&mut post, work, &cov, stroke.brush.opacity, None, lock, &paint, mode).intersect(&work);
        if damage.is_empty() {
            return Ok(Rect::EMPTY);
        }
        Ok(write_back(surf, &proj, &pre_view, &post, damage, iw, ih, sel))
    })?;
    Ok(json!({ "damage": damage_json(dmg) }))
}

/// A live Clone Stamp preview from `paint.projectedClone` params (points unused): the target
/// and the rotated source resampled into the whole view once.
pub fn projected_clone_preview(s: &Session, p: &Value) -> Result<crate::brush_cmds::projected::ProjectedPreview> {
    use crate::brush_cmds::projected::{PreviewKind, ProjectedPreview};
    let q = p.get("projection").ok_or_else(|| bad(CMD, "missing `projection`"))?;
    let g = |k: &str, d: f64| q.get(k).and_then(Value::as_f64).unwrap_or(d);
    let (w, h) = (g("width", 0.0), g("height", 0.0));
    if !(w >= 1.0 && h >= 1.0 && w <= 16384.0 && h <= 16384.0) {
        return Err(bad(CMD, "`projection.width` and `height` must be 1..=16384"));
    }
    let proj = ViewProjection::new(Camera { yaw: g("yaw", 0.0), pitch: g("pitch", 0.0), fov: g("fov", 90.0) }, w, h);
    let rotation: Vec<f64> = p.get("rotation").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    let rotation = rot_from_slice(&rotation).ok_or_else(|| bad(CMD, "`rotation` must be a 3×3 rotation"))?;
    let src_proj = proj.rotated(&rotation);
    let mut pp = p.clone();
    pp["points"] = json!([[0, 0]]);
    let (stroke, id) = parse_brush(s, &pp, CMD)?;
    let which = sample_layers(p, CMD)?;
    let mode = blend_param(p, CMD)?;
    let st = s.active().ok_or(crate::EngineError::NoDocument)?;
    let pre = st.doc.clone();
    let mut doc = (*pre).clone();
    let (iw, ih) = (doc.size.width as i32, doc.size.height as i32);
    let (surf, lock) = crate::channel_cmds::target_surface(&mut doc, id, p)?;
    let view = Rect::new(0, 0, w as i32, h as i32);
    let source = match which {
        SampleLayers::Current => to_view(surf, &src_proj, view, iw, ih),
        _ => {
            let fmt = surf.format();
            let mut flat = Surface::new(fmt);
            for e in view_rect_to_equirect(&src_proj, [0.0, 0.0, w, h], iw as u32, ih as u32, 2.0) {
                let rect = Rect::new(e[0] as i32, e[1] as i32, e[2] as i32, e[3] as i32);
                if !rect.is_empty() {
                    flat.write_region(rect, &composite_region(&pre, id, which, rect, fmt).data);
                }
            }
            to_view(&flat, &src_proj, view, iw, ih)
        }
    };
    let pre_view = to_view(surf, &proj, view, iw, ih);
    let sel = pre.selection.as_ref().map(|m| to_view(m, &proj, view, iw, ih));
    Ok(ProjectedPreview::with(pre_view, view, sel, PreviewKind::Clone { source, brush: Box::new(stroke.brush), mode, lock }))
}

/// `cov` (row-major over `from`) cut to `to` (inside `from`).
fn crop(cov: &[f32], from: Rect, to: Rect) -> Vec<f32> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equirect::{rot, rotation_between};

    fn rgba(s: &Session, x: i32, y: i32) -> [f32; 4] {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
    }

    fn doc() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 720, "height": 360, "background": "transparent"})).unwrap();
        s.execute("layer.new.layer", json!({"name": "paint"})).unwrap();
        s
    }

    fn view(yaw: f64) -> Value {
        json!({"yaw": yaw, "pitch": 0.0, "fov": 90.0, "width": 400, "height": 300})
    }

    fn clone(s: &mut Session, yaw: f64, at: [f64; 2], q: &crate::equirect::Rot) {
        s.execute(CMD, json!({"points": [at], "size": 30, "hardness": 100, "rotation": q.concat(), "projection": view(yaw)})).unwrap();
    }

    #[test]
    fn clones_a_patch_across_the_seam() {
        let mut s = doc();
        // A red patch around longitude 0 (the image centre, u = 360).
        s.execute("select.rect", json!({"x": 340, "y": 160, "width": 40, "height": 40})).unwrap();
        s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        // Looking at 180° (the seam), clone from the patch: the source is half a turn round.
        let p = ViewProjection::new(Camera { yaw: 180.0, pitch: 0.0, fov: 90.0 }, 400.0, 300.0);
        let dest = p.view_to_dir(200.0, 150.0);
        let src = crate::equirect::equirect_to_dir(360.0, 180.0, 720.0, 360.0);
        let q = rotation_between(dest, src);
        clone(&mut s, 180.0, [200.0, 150.0], &q);
        for x in [1, 718] {
            let c = rgba(&s, x, 180);
            assert!(c[0] > 0.9 && c[3] > 0.9, "x {x}: {c:?}");
        }
        assert_eq!(rgba(&s, 200, 180)[3], 0.0, "nothing else changes");
        // One History step.
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(rgba(&s, 1, 180)[3], 0.0);
    }

    #[test]
    fn aligned_rotation_survives_a_yaw_change() {
        let mut s = doc();
        // Green at longitude +90° (u = 540); the source rotation is "90° to the right of here".
        s.execute("select.rect", json!({"x": 520, "y": 160, "width": 40, "height": 40})).unwrap();
        s.execute("edit.fill", json!({"color": "#00ff00"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        let a = ViewProjection::new(Camera { yaw: 0.0, pitch: 0.0, fov: 90.0 }, 400.0, 300.0);
        let q = rotation_between(a.view_to_dir(200.0, 150.0), crate::equirect::equirect_to_dir(540.0, 180.0, 720.0, 360.0));
        // Stroke 1 at yaw 0, centre (u = 360) → green.
        clone(&mut s, 0.0, [200.0, 150.0], &q);
        assert!(rgba(&s, 360, 180)[1] > 0.9);
        // Look 20° to the left and paint at another sphere point (right of centre): the
        // same rotation still samples the green patch.
        let b = ViewProjection::new(Camera { yaw: -20.0, pitch: 0.0, fov: 90.0 }, 400.0, 300.0);
        let d = crate::equirect::equirect_to_dir(370.0, 180.0, 720.0, 360.0);
        let at = b.dir_to_view(d).unwrap();
        clone(&mut s, -20.0, at, &q);
        let c = rgba(&s, 370, 180);
        assert!(c[1] > 0.9 && c[3] > 0.9, "{c:?}");
        let m = rot(&q, d);
        let [u, _] = crate::equirect::dir_to_equirect(m, 720.0, 360.0);
        assert!((u - 550.0).abs() < 1.0, "the source moved with the stroke: {u}");
    }
}
