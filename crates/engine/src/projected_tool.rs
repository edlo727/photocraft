//! `paint.projectedTool`: any flat-canvas retouch or paint tool (Healing Brush, Spot Healing,
//! Patch, Smudge, Blur, Sharpen, Dodge, Burn, Sponge, Pencil, Mixer Brush, Color Replacement,
//! History Brush) in View › Equirectangular Live Projection.
//!
//! The document is resampled into the view once — the target layer (and its mask), every pixel
//! layer when the tool samples all layers, and the selection — as a small temporary document. The
//! ordinary command runs there on view-space points, untouched; what it changed (`post − pre`)
//! is written back onto the equirect pixels like a projected Brush stroke, clipped by the
//! selection, as one History step. Tools that read a source elsewhere on the sphere (Healing,
//! Patch, History Brush) get a second view-sized half holding the source — the view rotated by
//! the source rotation, or the history state — and an offset of one view width pointing at it.

use std::sync::Arc;

use photocraft_doc::{Document, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::brush_cmds::projected::{PreviewOverlay, overlay, to_view, write_back};
use crate::equirect::{Camera, ViewProjection, rot_from_slice};
use crate::{EngineError, Result, Session};

const CMD: &str = "paint.projectedTool";

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

/// The tools this runs, and their History labels.
fn label_of(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "paint.healingBrush" => "Healing Brush",
        "paint.spotHealing" => "Spot Healing Brush",
        "paint.patch" => "Patch",
        "paint.smudge" => "Smudge",
        "paint.blur" => "Blur",
        "paint.sharpen" => "Sharpen",
        "paint.dodge" => "Dodge",
        "paint.burn" => "Burn",
        "paint.sponge" => "Sponge",
        "paint.pencil" => "Pencil",
        "paint.mixerBrush" => "Mixer Brush",
        "paint.colorReplacement" => "Color Replacement",
        "paint.historyBrush" => "History Brush",
        "paint.remove" => "Remove",
        "paint.contentAwareMove" => "Content-Aware Move",
        "paint.redEye" => "Red Eye",
        "paint.gradient" => "Gradient",
        "paint.bucket" => "Paint Bucket",
        _ => return None,
    })
}

/// A flat tool ready to run on the view: see the module docs.
pub struct ProjectedTool {
    temp: Session,
    base: Arc<Document>,
    pre: Surface,
    proj: ViewProjection,
    view: Rect,
    layer: Option<LayerId>,
    cmd: String,
    params: Value,
    label: &'static str,
    /// The source half: the offset the command reads it through.
    offset: Option<[i32; 2]>,
}

impl ProjectedTool {
    /// From `paint.projectedTool` params: `command`, its `params` (without `points`), the
    /// view's `projection`, and for Healing / Patch the sphere `rotation` from destination to
    /// source (row-major 3×3).
    pub fn new(s: &Session, p: &Value) -> Result<Self> {
        let cmd = p.get("command").and_then(Value::as_str).ok_or_else(|| bad("missing `command`"))?.to_owned();
        let label = label_of(&cmd).ok_or_else(|| bad(format!("{cmd} does not run in the projection view")))?;
        let mut params = p.get("params").cloned().unwrap_or_else(|| json!({}));
        let q = p.get("projection").ok_or_else(|| bad("missing `projection`"))?;
        let g = |k: &str, d: f64| q.get(k).and_then(Value::as_f64).unwrap_or(d);
        let (w, h) = (g("width", 0.0), g("height", 0.0));
        if !(w >= 1.0 && h >= 1.0 && w <= 16384.0 && h <= 16384.0) {
            return Err(bad("`projection.width` and `height` must be 1..=16384"));
        }
        let proj = ViewProjection::new(Camera { yaw: g("yaw", 0.0), pitch: g("pitch", 0.0), fov: g("fov", 90.0) }, w, h);
        let view = Rect::new(0, 0, w as i32, h as i32);
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let layer = st.active_layer;
        let (iw, ih) = (st.doc.size.width as i32, st.doc.size.height as i32);
        if iw < 2 || ih < 1 {
            return Err(EngineError::Other("document too small for a projected stroke".into()));
        }
        // The source half: the view rotated onto the source, or (History Brush) the state.
        let rotation = match p.get("rotation").and_then(Value::as_array) {
            Some(a) => Some(rot_from_slice(&a.iter().filter_map(Value::as_f64).collect::<Vec<_>>()).ok_or_else(|| bad("`rotation` must be 9 numbers"))?),
            None => None,
        };
        let history = cmd == "paint.historyBrush";
        let src_proj = if history { Some(proj.clone()) } else { rotation.map(|r| proj.rotated(&r)) };
        let history_src = if history { Some(crate::retouch_cmds::history_source(s, layer, &params, &cmd)?) } else { None };
        let source_half = Rect::new(view.x1, 0, 2 * view.x1, view.y1);
        // Real HDR latlongs carry `inf` (the sun) and `NaN` pixels: the healing solves and patch
        // searches spread one of them over the whole fill (written back as black). The view copy
        // holds finite values; write-back only adds finite deltas, so those pixels stay as they were.
        let finite = |mut s: Surface| -> Surface {
            let mut b = s.read_region(view);
            if b.iter().all(|v| v.is_finite()) {
                return s;
            }
            for v in &mut b {
                if !v.is_finite() {
                    *v = if v.is_nan() { 0.0 } else { v.signum() * 65504.0 };
                }
            }
            s.write_region(view, &b);
            s
        };
        let resample = |surf: &Surface, src: Option<&Surface>| -> Surface {
            let mut out = finite(to_view(surf, &proj, view, iw, ih));
            if let Some(sp) = &src_proj {
                let r = finite(to_view(src.unwrap_or(surf), sp, view, iw, ih));
                out.write_region(source_half, &r.read_region(view));
            }
            out
        };
        let all = params.get("sampleAllLayers").and_then(Value::as_bool).unwrap_or(false)
            || params.get("sampleLayer").and_then(Value::as_str).is_some_and(|v| v != "current");
        let mut doc = (*st.doc).clone();
        let ids: Vec<LayerId> = doc.walk().iter().map(|(_, _, l)| l.id).collect();
        for id in ids {
            let keep = Some(id) == layer || all;
            let Some(l) = doc.layer_mut(id) else { continue };
            let target = Some(id) == layer;
            if let Some(sf) = l.surface_mut() {
                *sf = if keep { resample(sf, history_src.as_ref().filter(|_| target)) } else { Surface::new(sf.format()) };
            }
            if let Some(m) = l.mask.as_mut() {
                m.surface = if keep { resample(&m.surface, None) } else { Surface::new(m.surface.format()) };
            }
        }
        doc.selection = doc.selection.take().map(|m| to_view(&m, &proj, view, iw, ih));
        for c in &mut doc.channels {
            c.surface = Surface::new(c.surface.format());
        }
        let width = if src_proj.is_some() { 2 * view.x1 } else { view.x1 };
        doc.size.width = width as u32;
        doc.size.height = view.y1 as u32;
        let mut temp = Session::new();
        temp.tools = s.tools.clone();
        // The current gradient, patterns and other presets the tools read.
        temp.presets = s.presets.clone();
        temp.patterns = s.patterns.clone();
        let i = temp.add_document(doc, None);
        temp.set_active(i);
        if let Some(d) = temp.active_mut() {
            d.active_layer = layer;
        }
        let offset = src_proj.as_ref().map(|_| [view.x1, 0]);
        if let Some(o) = offset {
            if let Some(m) = params.as_object_mut() {
                m.remove("source");
            }
            params["offset"] = json!(o);
            if history {
                params["aligned"] = json!(true);
            }
        }
        let base = temp.active().ok_or(EngineError::NoDocument)?.doc.clone();
        let pre = crate::channel_cmds::read_target_surface(&base, layer, &params)?.clone();
        Ok(Self { temp, base, pre, proj, view, layer, cmd, params, label, offset })
    }

    /// Run the tool on `points` (view px, `[x, y, pressure]`) from the resampled state: the
    /// target as it then is, and the view rect it changed.
    fn run(&mut self, points: &[[f64; 3]]) -> Result<(Surface, Rect)> {
        if let Some(d) = self.temp.active_mut() {
            d.doc = self.base.clone();
        }
        let mut p = self.params.clone();
        match self.cmd.as_str() {
            // Selection-driven: the selection (and the offset in the params) say where.
            "paint.patch" | "paint.contentAwareMove" => {}
            // Drawn in the view: from the first point to the last.
            "paint.gradient" => {
                let (a, b) = (points.first().copied().unwrap_or_default(), points.last().copied().unwrap_or_default());
                p["from"] = json!([a[0], a[1]]);
                p["to"] = json!([b[0], b[1]]);
            }
            "paint.redEye" | "paint.bucket" => {
                let [x, y, _] = points.first().copied().unwrap_or_default();
                p["x"] = json!(x.floor());
                p["y"] = json!(y.floor());
            }
            _ => p["points"] = json!(points),
        }
        // The History Brush paints the state from the source half through the Clone Stamp.
        let cmd = if self.cmd == "paint.historyBrush" { "paint.cloneStamp" } else { self.cmd.as_str() };
        if cmd == "paint.cloneStamp" {
            p["opacity"] = json!(self.temp.tools.brush.opacity * 100.0);
            p["flow"] = json!(self.temp.tools.brush.flow * 100.0);
            p["sampleLayer"] = json!("current");
        }
        self.temp.execute(cmd, p.clone())?;
        let doc = self.temp.active().ok_or(EngineError::NoDocument)?.doc.clone();
        let post = crate::channel_cmds::read_target_surface(&doc, self.layer, &p)?.clone();
        let damage = diff_bounds(&self.pre, &post, self.view);
        Ok((post, damage))
    }

    /// One live preview frame: what the stroke so far changes, as an overlay.
    pub fn preview(&mut self, points: &[[f64; 3]]) -> Option<PreviewOverlay> {
        let (post, damage) = self.run(points).ok()?;
        if damage.is_empty() {
            return None;
        }
        overlay(&self.pre, &post, damage)
    }

    /// Whether the tool reads a source half (needs a source rotation).
    pub fn has_source(&self) -> bool {
        self.offset.is_some()
    }
}

/// Bounding box (inside `view`) of the pixels that differ between `a` and `b`.
fn diff_bounds(a: &Surface, b: &Surface, view: Rect) -> Rect {
    let (x, y) = (a.read_region(view), b.read_region(view));
    let n = a.channels();
    let w = view.width() as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (i, (p, q)) in x.chunks_exact(n).zip(y.chunks_exact(n)).enumerate() {
        if p != q {
            let (px, py) = ((i % w) as i32, (i / w) as i32);
            (x0, y0, x1, y1) = (x0.min(px), y0.min(py), x1.max(px + 1), y1.max(py + 1));
        }
    }
    if x0 > x1 { Rect::EMPTY } else { Rect::new(view.x0 + x0, view.y0 + y0, view.x0 + x1, view.y0 + y1) }
}

/// `paint.projectedTool` — see the module docs; `points` in view pixels.
pub fn projected_tool(s: &mut Session, p: &Value) -> Result<Value> {
    let points: Vec<[f64; 3]> = p
        .get("points")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|q| {
                    let q = q.as_array()?;
                    Some([q.first()?.as_f64()?, q.get(1)?.as_f64()?, q.get(2).and_then(Value::as_f64).unwrap_or(1.0)])
                })
                .collect()
        })
        .unwrap_or_default();
    let mut t = ProjectedTool::new(s, p)?;
    if points.is_empty() && !matches!(t.cmd.as_str(), "paint.patch" | "paint.contentAwareMove") {
        return Err(bad("no points"));
    }
    let (post, damage) = t.run(&points)?;
    let (layer, params, proj, pre, label) = (t.layer, t.params.clone(), t.proj.clone(), t.pre, t.label);
    let dmg = s.edit(label, |doc, _| {
        let (iw, ih) = (doc.size.width as i32, doc.size.height as i32);
        if damage.is_empty() {
            return Ok(Rect::EMPTY);
        }
        let selection = doc.selection.clone();
        let (surf, _) = crate::channel_cmds::target_surface(doc, layer, &params)?;
        Ok(write_back(surf, &proj, &pre, &post, damage, iw, ih, selection.as_ref()))
    })?;
    // The canvas uploads only the last damage to the GPU: without it the view keeps showing the
    // old pixels (or a previous stroke's rect) and the tool looks like it did nothing.
    if let Some(st) = s.active_mut() {
        st.last_damage = Some(dmg);
    }
    Ok(json!({"damage": [dmg.x0, dmg.y0, dmg.width(), dmg.height()]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equirect::{IDENTITY, rotation_between};

    fn doc() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 720, "height": 360, "background": "white"})).unwrap();
        // A black band left of the centre, and a patch of grey dots to heal.
        s.execute("select.rect", json!({"x": 300, "y": 150, "width": 30, "height": 60})).unwrap();
        s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        s
    }
    fn view() -> Value {
        json!({"yaw": 0.0, "pitch": 0.0, "fov": 60.0, "width": 200, "height": 200})
    }
    fn rgba(s: &Session, x: i32, y: i32) -> [f32; 4] {
        let d = s.active().unwrap();
        d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
    }

    #[test]
    fn blur_softens_the_band_edge_and_undoes() {
        let mut s = doc();
        let before = rgba(&s, 330, 180);
        // The band's right edge (equirect x 330) is left of the view centre (x 360).
        let edge = 100.0 - (30f64.to_radians().tan() / 30f64.to_radians().tan()) * 100.0 * (30.0 / 30.0) * 0.0;
        let _ = edge;
        let r = s.execute(CMD, json!({"command": "paint.blur", "params": {"strength": 100, "size": 40}, "projection": view(), "points": [[60, 100], [80, 100], [60, 100], [80, 100]]}));
        assert!(r.is_ok(), "{r:?}");
        let changed = (320..340).any(|x| rgba(&s, x, 180) != if x < 330 { [0.0, 0.0, 0.0, 1.0] } else { [1.0; 4] });
        assert!(changed, "the blur reached the equirect pixels");
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(rgba(&s, 330, 180), before);
    }

    #[test]
    fn dodge_is_clipped_by_the_selection() {
        let mut s = doc();
        // Select the left half of the band only (equirect x < 315).
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 315, "height": 360})).unwrap();
        let p = json!({"command": "paint.dodge", "params": {"exposure": 100, "range": "shadows", "size": 120, "hardness": 100}, "projection": view(), "points": [[70, 100], [72, 100]]});
        let mut t = ProjectedTool::new(&s, &p).unwrap();
        assert!(t.preview(&[[70.0, 100.0, 1.0], [72.0, 100.0, 1.0]]).is_some(), "a live preview");
        s.execute(CMD, p).unwrap();
        let (l, r) = (rgba(&s, 310, 180), rgba(&s, 322, 180));
        assert!(l[0] > 0.01, "dodged inside the selection: {l:?}");
        assert_eq!(r, [0.0, 0.0, 0.0, 1.0], "untouched outside");
    }

    #[test]
    fn healing_reads_its_source_through_the_rotation() {
        let mut s = doc();
        let vp = ViewProjection::new(Camera { yaw: 0.0, pitch: 0.0, fov: 60.0 }, 200.0, 200.0);
        // Heal the band (view ~x 60) from plain white 40 view px to the right.
        let q = rotation_between(vp.view_to_dir(45.0, 100.0), vp.view_to_dir(150.0, 100.0));
        let p = json!({"command": "paint.healingBrush", "params": {"size": 30, "hardness": 100}, "rotation": q.concat(), "projection": view(), "points": [[45, 100], [46, 100]]});
        let t = ProjectedTool::new(&s, &p).unwrap();
        assert!(t.has_source());
        let _ = IDENTITY;
        s.execute(CMD, p).unwrap();
        let best = (322..330).map(|x| rgba(&s, x, 180)[0]).fold(0.0f32, f32::max);
        assert!(best > 0.3, "the band healed toward white: {best}");
    }

    /// A 32-bit float (EXR-like) latlong: smooth HDR gradient with texture, and a black dot.
    fn float_doc() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 720, "height": 360, "depth": 32, "background": "white"})).unwrap();
        let id = s.active().unwrap().doc.walk().iter().find(|(_, _, l)| l.surface().is_some()).map(|(_, _, l)| l.id).unwrap();
        s.select_layer(id).unwrap();
        s.edit("fill", |doc, _| {
            let surf = doc.layer_mut(id).unwrap().surface_mut().unwrap();
            let r = Rect::new(0, 0, 720, 360);
            let n = surf.channels();
            let mut d = vec![0f32; 720 * 360 * n];
            for y in 0..360 {
                for x in 0..720 {
                    let i = (y * 720 + x) * n;
                    let t = ((x * 7 + y * 13) % 17) as f32 / 17.0;
                    d[i] = 0.2 + 1.5 * x as f32 / 720.0 + 0.1 * t;
                    d[i + 1] = 0.3 + 0.1 * t;
                    d[i + 2] = 0.6 * y as f32 / 360.0 + 0.05;
                    if n == 4 {
                        d[i + 3] = 1.0;
                    }
                    if (x as i32 - 330).pow(2) + (y as i32 - 180).pow(2) < 36 {
                        d[i] = 0.0;
                        d[i + 1] = 0.0;
                        d[i + 2] = 0.0;
                    }
                }
            }
            surf.write_region(r, &d);
            Ok(())
        })
        .unwrap();
        s
    }

    #[test]
    fn float_doc_healing_group_runs_and_reports_damage() {
        let vp = ViewProjection::new(Camera { yaw: 0.0, pitch: 0.0, fov: 60.0 }, 200.0, 200.0);
        let dot = vp.equirect_to_view(330.5, 180.5, 720.0, 360.0).unwrap();
        for ty in ["contentAware", "proximityMatch", "createTexture"] {
            let mut s = float_doc();
            let r = s.execute(CMD, json!({"command": "paint.spotHealing", "params": {"type": ty, "sampleAllLayers": false, "target": "pixels"}, "projection": view(), "points": [[dot[0], dot[1], 1.0], [dot[0] + 0.5, dot[1], 1.0]]}));
            assert!(r.is_ok() && rgba(&s, 330, 180)[0] > 0.5, "F32 spot {ty}: {r:?} -> {:?}", rgba(&s, 330, 180));
            assert!(s.active().unwrap().last_damage.is_some_and(|d| d.contains(330, 180)), "the canvas is told what changed");
        }
        let mut s = float_doc();
        let q = rotation_between(vp.view_to_dir(dot[0], dot[1]), vp.view_to_dir(dot[0] + 30.0, dot[1]));
        let r = s.execute(CMD, json!({"command": "paint.healingBrush", "params": {"aligned": true, "sampleLayer": "current", "target": "pixels"}, "rotation": q.concat(), "projection": view(), "points": [[dot[0], dot[1], 1.0], [dot[0] + 0.5, dot[1], 1.0]]}));
        assert!(r.is_ok() && rgba(&s, 330, 180)[0] > 0.5, "F32 healing: {r:?} -> {:?}", rgba(&s, 330, 180));
        let mut s = float_doc();
        s.execute("select.rect", json!({"x": 322, "y": 172, "width": 16, "height": 16})).unwrap();
        let r = s.execute(CMD, json!({"command": "paint.patch", "params": {"mode": "source", "target": "pixels"}, "rotation": q.concat(), "projection": view()}));
        assert!(r.is_ok() && rgba(&s, 330, 180)[0] > 0.5, "F32 patch: {r:?} -> {:?}", rgba(&s, 330, 180));
        // Remove, Red Eye and Content-Aware Move run too.
        for (cmd, extra) in [("paint.remove", json!({"points": [[dot[0], dot[1], 1.0]]})), ("paint.redEye", json!({"points": [[dot[0], dot[1], 1.0]]}))] {
            let mut s = float_doc();
            let mut p = json!({"command": cmd, "params": {"target": "pixels"}, "projection": view()});
            p["points"] = extra["points"].clone();
            let r = s.execute(CMD, p);
            assert!(r.is_ok(), "F32 {cmd}: {r:?}");
        }
        let r = s.execute(CMD, json!({"command": "paint.contentAwareMove", "params": {"offset": [20, 0], "target": "pixels"}, "projection": view()}));
        assert!(r.is_ok(), "F32 content-aware move: {r:?}");
    }


}

#[cfg(test)]
mod fills {
    use super::*;

    #[test]
    fn gradient_and_bucket_run_in_the_view_on_f32() {
        let mut s = crate::projected_tool::realistic::hdr_session();
        let view = json!({"yaw": 0.0, "pitch": 0.0, "fov": 70.0, "width": 800, "height": 600});
        let vp = ViewProjection::new(Camera { yaw: 0.0, pitch: 0.0, fov: 70.0 }, 800.0, 600.0);
        s.execute("tools.setColors", json!({"foreground": "#ff0000", "background": "#0000ff"})).unwrap();
        let p = json!({"command": "paint.gradient", "params": {"target": "pixels", "colors": ["#ff0000", "#0000ff"]}, "projection": view, "points": [[100, 300, 1.0], [700, 300, 1.0]]});
        let mut t = ProjectedTool::new(&s, &p).unwrap();
        assert!(t.preview(&[[100.0, 300.0, 1.0], [700.0, 300.0, 1.0]]).is_some(), "a live preview");
        s.execute(CMD, p).unwrap();
        let px = |s: &Session, vx: f64| {
            let [u, v] = vp.view_to_equirect(vx, 300.0, 4096.0, 2048.0);
            let d = s.active().unwrap();
            d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(u as i32, v as i32)
        };
        let (l, r) = (px(&s, 120.0), px(&s, 680.0));
        assert!(l[0] > 0.8 && l[2] < 0.1 && r[2] > 0.8 && r[0] < 0.1, "red to blue across the view: {l:?} {r:?}");
        // The bucket fills in the view.
        let p = json!({"command": "paint.bucket", "params": {"target": "pixels", "tolerance": 255, "contiguous": true, "contents": "foreground"}, "projection": view, "points": [[400, 300, 1.0]]});
        s.execute(CMD, p).unwrap();
        let c = px(&s, 400.0);
        assert!(c[0] > 0.8 && c[2] < 0.1 && c.iter().all(|v| v.is_finite()), "filled: {c:?}");
    }
}

#[cfg(test)]
mod realistic {
    use super::*;
    use crate::equirect::rotation_between;

    /// Ed's material, scaled: a 4096×2048 F32 latlong (multi-tile) with a gradient, per-pixel
    /// noise, HDR sky up to ~30 and a dark blemish.
    pub(crate) fn hdr_session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 4096, "height": 2048, "depth": 32})).unwrap();
        let id = s.active().unwrap().doc.walk().iter().find(|(_, _, l)| l.surface().is_some()).map(|(_, _, l)| l.id).unwrap();
        s.select_layer(id).unwrap();
        s.edit("fill", |doc, _| {
            let surf = doc.layer_mut(id).unwrap().surface_mut().unwrap();
            let n = surf.channels();
            let (w, h) = (4096usize, 2048usize);
            let mut d = vec![0f32; w * h * n];
            let mut seed = 0x1234_5678u32;
            for y in 0..h {
                for x in 0..w {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    let t = (seed % 1000) as f32 / 1000.0;
                    let i = (y * w + x) * n;
                    let sky = if y < h / 2 { 30.0 * (1.0 - y as f32 / (h / 2) as f32).powi(3) } else { 0.0 };
                    d[i] = 0.2 + 0.8 * x as f32 / w as f32 + 0.2 * t + sky;
                    d[i + 1] = 0.3 + 0.2 * t + sky * 0.9;
                    d[i + 2] = 0.05 + 0.6 * y as f32 / h as f32 + sky * 0.8;
                    if n == 4 {
                        d[i + 3] = 1.0;
                    }
                    if (x as i32 - 2048).pow(2) + (y as i32 - 1000).pow(2) < 25 {
                        d[i..i + 3].fill(0.0);
                    }
                    // As in real HDR files: an `inf` sun pixel and a `NaN` near the blemish.
                    if (x, y) == (2070, 990) {
                        d[i..i + 3].fill(f32::INFINITY);
                    }
                    if (x, y) == (2030, 1012) {
                        d[i] = f32::NAN;
                    }
                }
            }
            surf.write_region(Rect::new(0, 0, w as i32, h as i32), &d);
            Ok(())
        })
        .unwrap();
        s
    }

    fn bad_pixels(s: &Session, r: Rect) -> (usize, usize) {
        let d = s.active().unwrap();
        let surf = d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap();
        let b = surf.read_region(r);
        let n = surf.channels();
        let nan = b.iter().filter(|v| !v.is_finite()).count();
        let nan = nan.saturating_sub(if r.contains(2030, 1012) { 1 } else { 0 });
        let black = b.chunks_exact(n).filter(|p| p[0] <= 1e-6 && p[1] <= 1e-6 && p[2] <= 1e-6).count();
        (nan, black)
    }

    #[test]
    fn spot_healing_and_healing_on_a_big_hdr_f32_leave_no_black_or_nan() {
        let view = json!({"yaw": 0.0, "pitch": 5.0, "fov": 70.0, "width": 1920, "height": 1080});
        let vp = ViewProjection::new(Camera { yaw: 0.0, pitch: 5.0, fov: 70.0 }, 1920.0, 1080.0);
        let dot = vp.equirect_to_view(2048.5, 1000.5, 4096.0, 2048.0).unwrap();
        let blem = Rect::new(2040, 992, 2057, 1009);
        for size in [30.0, 80.0, 200.0] {
            for ty in ["contentAware", "proximityMatch", "createTexture"] {
                let mut s = hdr_session();
                s.tools.brush.size = size;
                let pts: Vec<[f64; 3]> = (0..12).map(|i| [dot[0] - 20.0 + i as f64 * 4.0, dot[1], 1.0]).collect();
                let p = json!({"command": "paint.spotHealing", "params": {"type": ty, "sampleAllLayers": false, "target": "pixels"}, "projection": view, "points": pts});
                let mut t = ProjectedTool::new(&s, &p).unwrap();
                let o = t.preview(&pts).expect("a preview");
                let dark = o.px.iter().filter(|q| q[3] > 0 && q[0] < 4 && q[1] < 4 && q[2] < 4).count();
                s.execute(CMD, p).unwrap();
                let (nan, black) = bad_pixels(&s, blem);
                eprintln!("spot {ty} size {size}: nan {nan} black {black} preview-dark {dark}/{}", o.px.len());
                assert!(nan == 0 && black == 0 && dark == 0, "spot {ty} size {size}: nan {nan} black {black} preview-dark {dark}");
            }
        }
        let mut s = hdr_session();
        s.tools.brush.size = 80.0;
        let q = rotation_between(vp.view_to_dir(dot[0], dot[1]), vp.view_to_dir(dot[0] + 150.0, dot[1]));
        let pts: Vec<[f64; 3]> = (0..12).map(|i| [dot[0] - 20.0 + i as f64 * 4.0, dot[1], 1.0]).collect();
        let r = s.execute(CMD, json!({"command": "paint.healingBrush", "params": {"aligned": true, "sampleLayer": "current", "target": "pixels"}, "rotation": q.concat(), "projection": view, "points": pts}));
        let (nan, black) = bad_pixels(&s, blem);
        assert!(r.is_ok() && nan == 0 && black == 0, "healing: {r:?} nan {nan} black {black}");
    }
}
