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
        let resample = |surf: &Surface, src: Option<&Surface>| -> Surface {
            let mut out = to_view(surf, &proj, view, iw, ih);
            if let Some(sp) = &src_proj {
                let r = to_view(src.unwrap_or(surf), sp, view, iw, ih);
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
        if self.cmd != "paint.patch" {
            p["points"] = json!(points);
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
    if points.is_empty() && t.cmd != "paint.patch" {
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
}
