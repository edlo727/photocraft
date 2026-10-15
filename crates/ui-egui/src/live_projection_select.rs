//! The selection tools in View › Equirectangular Live Projection: Rectangular and Elliptical
//! Marquee, Lasso, Polygonal Lasso, Magic Wand, Quick Selection and Object Selection draw (or
//! click) in the perspective view and commit one `select.projected`, which reprojects the shape
//! onto the equirect selection. The marching ants are the selection resampled into the view.

use egui::{Color32, Key, Pos2, Rect, Stroke, vec2};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::state::Tool;
use photocraft_engine::equirect::Camera;

/// The selection tools that work in the projection view.
pub fn supported(t: Tool) -> bool {
    matches!(t, Tool::RectMarquee | Tool::EllipseMarquee | Tool::Lasso | Tool::PolygonLasso | Tool::MagicWand | Tool::QuickSelection | Tool::ObjectSelection)
}

/// A selection being drawn in a projection view (view device pixels).
pub struct SelDrag {
    doc: usize,
    tool: Tool,
    mode: &'static str,
    points: Vec<[f64; 2]>,
}

/// Cached marching ants for one view.
pub struct Ants {
    key: (usize, [u32; 3], [u32; 2]),
    /// Edge pixels (half-resolution view), and the box they span.
    edges: Vec<(u32, u32)>,
    bbox: [u32; 4],
    texture: Option<egui::TextureHandle>,
    phase: u32,
}

/// Half-resolution ants: cheap to rebuild every frame while looking around.
const ANTS_SCALE: f32 = 2.0;

fn mode_for(app: &PhotocraftApp, tool: Tool, m: egui::Modifiers) -> &'static str {
    match tool {
        // Quick Selection adds unless ⌥ subtracts (Photoshop).
        Tool::QuickSelection => {
            if m.alt {
                "subtract"
            } else {
                "add"
            }
        }
        _ => crate::tool_feedback::document_selection_mode(app, tool, m),
    }
}

fn proj_json(cam: Camera, w: f64, h: f64) -> Value {
    json!({"yaw": cam.yaw, "pitch": cam.pitch, "fov": cam.fov, "width": w, "height": h})
}

/// `select.projected` params for `tool` with `points` (view device px).
pub(crate) fn params(app: &PhotocraftApp, tool: Tool, mode: &str, points: &[[f64; 2]], cam: Camera, w: f64, h: f64) -> Option<Value> {
    let o = &app.ui.tool_options;
    let mut p = json!({"mode": mode, "antiAlias": o.anti_alias, "sampleAllLayers": o.sample_all_layers, "projection": proj_json(cam, w, h)});
    let (a, b) = (points.first().copied()?, points.last().copied()?);
    let rect = json!([a[0], a[1], b[0], b[1]]);
    match tool {
        Tool::RectMarquee | Tool::EllipseMarquee => {
            p["kind"] = json!(if tool == Tool::RectMarquee { "rect" } else { "ellipse" });
            p["rect"] = rect;
            p["feather"] = json!(o.feather);
        }
        Tool::Lasso | Tool::PolygonLasso => {
            p["kind"] = json!("lasso");
            p["points"] = json!(points);
            p["feather"] = json!(o.feather);
        }
        Tool::MagicWand => {
            p["kind"] = json!("wand");
            p["points"] = json!([a]);
            p["tolerance"] = json!(o.tolerance);
            p["contiguous"] = json!(o.contiguous);
        }
        Tool::QuickSelection => {
            p["kind"] = json!("quick");
            p["points"] = json!(points);
            p["size"] = json!(app.session.tools.brush.size);
        }
        Tool::ObjectSelection => {
            p["kind"] = json!("object");
            p["rect"] = rect;
        }
        _ => return None,
    }
    Some(p)
}

fn commit(app: &mut PhotocraftApp, d: SelDrag, cam: Camera, w: f64, h: f64) {
    let small = |pts: &[[f64; 2]]| {
        let (a, b) = (pts[0], pts[pts.len() - 1]);
        (a[0] - b[0]).abs() < 2.0 || (a[1] - b[1]).abs() < 2.0
    };
    let r = match d.tool {
        // A click with a marquee or the lasso (no shape) deselects, as on the flat canvas.
        Tool::RectMarquee | Tool::EllipseMarquee | Tool::Lasso | Tool::PolygonLasso
            if (matches!(d.tool, Tool::RectMarquee | Tool::EllipseMarquee) && small(&d.points)) || (matches!(d.tool, Tool::Lasso | Tool::PolygonLasso) && d.points.len() < 3) =>
        {
            if d.mode == "replace" { app.run("select.deselect", json!({})).map(|_| ()) } else { Ok(()) }
        }
        Tool::ObjectSelection if small(&d.points) => {
            app.ui.status = "Drag a rectangle around the object to select it".into();
            app.ui.status_error = false;
            return;
        }
        t => match params(app, t, d.mode, &d.points, cam, w, h) {
            Some(p) => app.run("select.projected", p).map(|_| ()),
            None => Ok(()),
        },
    };
    if let Err(e) = r {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Handle a selection tool in the projection canvas.
#[allow(clippy::too_many_arguments)]
pub fn tool(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, rect: Rect, ppp: f32, cam: Camera, idx: usize, tool: Tool) {
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    let to_screen = |q: [f64; 2]| -> Pos2 { rect.min + vec2(q[0] as f32, q[1] as f32) / ppp };
    let (w, h) = (f64::from(rect.width() * ppp), f64::from(rect.height() * ppp));
    let mods = ctx.input(|i| i.modifiers);
    // Another tool or document: drop a half-drawn shape.
    if app.projection_select.as_ref().is_some_and(|d| d.doc != idx || d.tool != tool) {
        app.projection_select = None;
    }
    let pointer = response.interact_pointer_pos().or(response.hover_pos());
    if tool == Tool::PolygonLasso {
        // Clicks add corners; a click on the first corner, a double-click or ↩ closes; Esc drops.
        if (response.clicked() || response.drag_started())
            && let Some(p) = response.interact_pointer_pos()
        {
            let q = to_view(p);
            match app.projection_select.as_mut() {
                None => app.projection_select = Some(SelDrag { doc: idx, tool, mode: mode_for(app, tool, mods), points: vec![q] }),
                Some(d) => {
                    let first = to_screen(d.points[0]);
                    if d.points.len() >= 3 && first.distance(p) < 6.0 {
                        let d = app.projection_select.take().unwrap();
                        commit(app, d, cam, w, h);
                    } else {
                        d.points.push(q);
                    }
                }
            }
        }
        let close = response.double_clicked() || ctx.input(|i| i.key_pressed(Key::Enter));
        if close && let Some(d) = app.projection_select.take() {
            commit(app, d, cam, w, h);
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            app.projection_select = None;
        }
    } else if matches!(tool, Tool::MagicWand) {
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            let d = SelDrag { doc: idx, tool, mode: mode_for(app, tool, mods), points: vec![to_view(p)] };
            commit(app, d, cam, w, h);
        }
    } else {
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(p) = response.interact_pointer_pos()
        {
            app.projection_select = Some(SelDrag { doc: idx, tool, mode: mode_for(app, tool, mods), points: vec![to_view(p)] });
        }
        if response.dragged_by(egui::PointerButton::Primary)
            && let Some(p) = response.interact_pointer_pos()
            && let Some(d) = app.projection_select.as_mut()
        {
            let mut q = to_view(p);
            if matches!(tool, Tool::RectMarquee | Tool::EllipseMarquee) {
                // ⇧ after the press makes a square / circle.
                if mods.shift && d.mode != "add" {
                    let a = d.points[0];
                    let s = (q[0] - a[0]).abs().max((q[1] - a[1]).abs());
                    q = [a[0] + s * (q[0] - a[0]).signum(), a[1] + s * (q[1] - a[1]).signum()];
                }
                d.points.truncate(1);
                d.points.push(q);
            } else if d.points.last().is_none_or(|l| (l[0] - q[0]).abs() + (l[1] - q[1]).abs() > 1.0) {
                d.points.push(q);
            }
        }
        if response.drag_stopped_by(egui::PointerButton::Primary)
            && let Some(d) = app.projection_select.take()
        {
            commit(app, d, cam, w, h);
        } else if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            let q = to_view(p);
            let d = SelDrag { doc: idx, tool, mode: mode_for(app, tool, mods), points: vec![q, q] };
            commit(app, d, cam, w, h);
        }
    }
    // Feedback: the shape so far.
    if let Some(d) = app.projection_select.as_ref() {
        let dash = |pts: Vec<Pos2>, closed: bool| {
            let mut pts = pts;
            if closed && let Some(f) = pts.first().copied() {
                pts.push(f);
            }
            painter.add(egui::Shape::line(pts.clone(), Stroke::new(1.0, Color32::BLACK)));
            painter.extend(egui::Shape::dashed_line(&pts, Stroke::new(1.0, Color32::WHITE), 4.0, 4.0));
        };
        let (a, b) = (to_screen(d.points[0]), to_screen(*d.points.last().unwrap()));
        match tool {
            Tool::RectMarquee | Tool::ObjectSelection => {
                let r = Rect::from_two_pos(a, b);
                dash(vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()], true);
            }
            Tool::EllipseMarquee => {
                let (c, rr) = (Rect::from_two_pos(a, b).center(), (b - a).abs() / 2.0);
                dash((0..=96).map(|i| std::f32::consts::TAU * i as f32 / 96.0).map(|t| c + vec2(rr.x * t.cos(), rr.y * t.sin())).collect(), false);
            }
            Tool::Lasso => dash(d.points.iter().map(|q| to_screen(*q)).collect(), false),
            Tool::PolygonLasso => {
                let mut pts: Vec<Pos2> = d.points.iter().map(|q| to_screen(*q)).collect();
                if let Some(h) = response.hover_pos() {
                    pts.push(h);
                }
                dash(pts, false);
            }
            Tool::QuickSelection => dash(d.points.iter().map(|q| to_screen(*q)).collect(), false),
            _ => {}
        }
        ctx.request_repaint();
    }
    // Cursor.
    if let Some(h) = pointer.filter(|_| response.hovered()) {
        if tool == Tool::QuickSelection {
            let r = (app.session.tools.brush.size / ppp / 2.0).max(1.0);
            painter.circle_stroke(h, r, Stroke::new(1.0, Color32::from_white_alpha(200)));
            painter.circle_stroke(h, r + 1.0, Stroke::new(1.0, Color32::from_black_alpha(160)));
            ctx.set_cursor_icon(egui::CursorIcon::None);
        } else {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
    }
}

/// Draw the marching ants of document `idx`'s selection over the projection view.
pub fn ants(app: &mut PhotocraftApp, ctx: &egui::Context, painter: &egui::Painter, rect: Rect, ppp: f32, cam: Camera, idx: usize) {
    let Some(st) = app.session.documents().get(idx) else { return };
    if st.doc.selection.is_none() {
        app.projection_ants = None;
        return;
    }
    let (w, h) = (((rect.width() * ppp) / ANTS_SCALE).ceil().max(1.0) as u32, ((rect.height() * ppp) / ANTS_SCALE).ceil().max(1.0) as u32);
    let key = (std::sync::Arc::as_ptr(&st.doc) as usize, [cam.yaw as f32, cam.pitch as f32, cam.fov as f32].map(f32::to_bits), [w, h]);
    if app.projection_ants.as_ref().is_none_or(|a| a.key != key) {
        let Some(m) = photocraft_engine::projected_select::projected_selection_mask(&st.doc, cam, w, h) else {
            app.projection_ants = None;
            return;
        };
        let (edges, bbox) = edges(&m, w, h);
        let texture = app.projection_ants.take().and_then(|a| a.texture);
        app.projection_ants = Some(Ants { key, edges, bbox, texture, phase: u32::MAX });
    }
    let Some(a) = app.projection_ants.as_mut() else { return };
    if a.edges.is_empty() {
        return;
    }
    let phase = ((ctx.input(|i| i.time) * 8.0) as u32) % 8;
    if phase != a.phase || a.texture.is_none() {
        a.phase = phase;
        let [x0, y0, x1, y1] = a.bbox;
        let (bw, bh) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut px = vec![Color32::TRANSPARENT; bw * bh];
        for &(x, y) in &a.edges {
            let on = ((x + y + phase) / 4) % 2 == 0;
            px[(y - y0) as usize * bw + (x - x0) as usize] = if on { Color32::WHITE } else { Color32::BLACK };
        }
        let img = egui::ColorImage { size: [bw, bh], pixels: px, source_size: vec2(bw as f32, bh as f32) };
        match &mut a.texture {
            Some(t) => t.set(img, egui::TextureOptions::NEAREST),
            None => a.texture = Some(ctx.load_texture("pc-live-projection-ants", img, egui::TextureOptions::NEAREST)),
        }
    }
    if let Some(t) = &a.texture {
        let [x0, y0, x1, y1] = a.bbox;
        let k = ANTS_SCALE / ppp;
        let at = Rect::from_min_max(rect.min + vec2(x0 as f32, y0 as f32) * k, rect.min + vec2(x1 as f32, y1 as f32) * k);
        painter.image(t.id(), at, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(125));
}

/// Pixels on the 50% boundary of `m` (`w` × `h`), and their bounding box [x0, y0, x1, y1).
fn edges(m: &[u8], w: u32, h: u32) -> (Vec<(u32, u32)>, [u32; 4]) {
    let at = |x: i64, y: i64| -> bool { x >= 0 && y >= 0 && x < i64::from(w) && y < i64::from(h) && m[(y as usize) * w as usize + x as usize] >= 128 };
    let mut out = Vec::new();
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for y in 0..h {
        for x in 0..w {
            let (xi, yi) = (i64::from(x), i64::from(y));
            if at(xi, yi) && !(at(xi - 1, yi) && at(xi + 1, yi) && at(xi, yi - 1) && at(xi, yi + 1)) {
                out.push((x, y));
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    (out, [x0, y0, x1, y1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_ring_a_square() {
        let (w, h) = (10u32, 10u32);
        let mut m = vec![0u8; 100];
        for y in 2..6 {
            for x in 3..7 {
                m[y * 10 + x] = 255;
            }
        }
        let (e, b) = edges(&m, w, h);
        assert_eq!(b, [3, 2, 7, 6]);
        assert_eq!(e.len(), 12, "the 4×4 square's border");
    }
}
