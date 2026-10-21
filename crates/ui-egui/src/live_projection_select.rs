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
    matches!(
        t,
        Tool::RectMarquee
            | Tool::EllipseMarquee
            | Tool::SingleRowMarquee
            | Tool::SingleColumnMarquee
            | Tool::Lasso
            | Tool::PolygonLasso
            | Tool::MagneticLasso
            | Tool::MagicWand
            | Tool::QuickSelection
            | Tool::ObjectSelection
    )
}

/// A selection being drawn in a projection view (view device pixels).
pub struct SelDrag {
    doc: usize,
    tool: Tool,
    mode: &'static str,
    points: Vec<[f64; 2]>,
    /// Where the press was (marquees: ⌥ draws from it as the centre).
    anchor: [f64; 2],
    /// The camera the shape is drawn in: looking around (the wheel) drops it.
    cam: [u32; 5],
    /// The image resampled into the view: live Quick Selection and Magnetic Lasso tracing.
    pixels: Option<photocraft_engine::projected_select::ViewPixels>,
    /// Magnetic Lasso: the live segment to the pointer, the pointer's trail since the last
    /// fastening point, and the fastening points (indices into `points`).
    live: Vec<[f64; 2]>,
    trail: Vec<[f64; 2]>,
    anchors: Vec<usize>,
    /// Live Quick Selection overlay: texture, its view rect (device px), points it shows, when.
    quick: Option<(egui::TextureHandle, [f32; 4])>,
    quick_n: usize,
    quick_t: f64,
}

impl SelDrag {
    #[cfg(test)]
    pub(crate) fn points_for_test(&self) -> Vec<[f64; 2]> {
        self.points.clone()
    }
    #[cfg(test)]
    pub(crate) fn has_quick_preview(&self) -> bool {
        self.quick.is_some()
    }
    fn new(doc: usize, tool: Tool, mode: &'static str, q: [f64; 2], cam: [u32; 5]) -> Self {
        SelDrag { doc, tool, mode, points: vec![q], anchor: q, cam, pixels: None, live: Vec::new(), trail: vec![q], anchors: vec![0], quick: None, quick_n: 0, quick_t: 0.0 }
    }
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
    let mut p = json!({"mode": mode, "antiAlias": o.anti_alias, "sampleAllLayers": o.sample_all.get(tool), "projection": proj_json(cam, w, h)});
    let (a, b) = (points.first().copied()?, points.last().copied()?);
    let rect = json!([a[0], a[1], b[0], b[1]]);
    match tool {
        Tool::RectMarquee | Tool::EllipseMarquee => {
            p["kind"] = json!(if tool == Tool::RectMarquee { "rect" } else { "ellipse" });
            p["rect"] = rect;
            p["feather"] = json!(o.feather);
        }
        Tool::SingleRowMarquee | Tool::SingleColumnMarquee => {
            p["kind"] = json!(if tool == Tool::SingleRowMarquee { "row" } else { "column" });
            p["points"] = json!([a]);
        }
        Tool::Lasso | Tool::PolygonLasso | Tool::MagneticLasso => {
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
        Tool::RectMarquee | Tool::EllipseMarquee | Tool::Lasso | Tool::PolygonLasso | Tool::MagneticLasso
            if (matches!(d.tool, Tool::RectMarquee | Tool::EllipseMarquee) && small(&d.points))
                || (matches!(d.tool, Tool::Lasso | Tool::PolygonLasso | Tool::MagneticLasso) && d.points.len() < 3) =>
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

fn magnetic_settings(app: &PhotocraftApp) -> photocraft_algo::magnetic::Settings {
    let o = &app.ui.tool_options;
    photocraft_algo::magnetic::Settings::new(f64::from(o.magnetic_width), o.magnetic_contrast / 100.0)
}

/// Handle a selection tool in the projection canvas.
#[allow(clippy::too_many_arguments)]
pub fn tool(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, rect: Rect, ppp: f32, cam: Camera, idx: usize, tool: Tool) {
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    let to_screen = |q: [f64; 2]| -> Pos2 { rect.min + vec2(q[0] as f32, q[1] as f32) / ppp };
    let (w, h) = (f64::from(rect.width() * ppp), f64::from(rect.height() * ppp));
    let ckey = [cam.yaw as f32, cam.pitch as f32, cam.fov as f32, w as f32, h as f32].map(f32::to_bits);
    let mods = ctx.input(|i| i.modifiers);
    // Another tool or document, or the view turned: drop a half-drawn shape.
    if app.projection_select.as_ref().is_some_and(|d| d.doc != idx || d.tool != tool || d.cam != ckey) {
        app.projection_select = None;
    }
    let pointer = response.interact_pointer_pos().or(response.hover_pos());
    let press = (response.clicked() || response.drag_started_by(egui::PointerButton::Primary)).then(|| response.interact_pointer_pos()).flatten();
    let projection_params = |app: &PhotocraftApp| json!({"projection": proj_json(cam, w, h), "sampleAllLayers": app.ui.tool_options.sample_all.get(tool)});
    match tool {
        Tool::SingleRowMarquee | Tool::SingleColumnMarquee => {
            if let Some(p) = press {
                let q = to_view(p);
                let d = SelDrag::new(idx, tool, mode_for(app, tool, mods), q, ckey);
                commit(app, d, cam, w, h);
            }
        }
        Tool::PolygonLasso => {
            // Clicks add corners; a click on the first corner, a double-click or ↩ closes; Esc drops.
            if let Some(p) = press {
                let q = to_view(p);
                match app.projection_select.as_mut() {
                    None => app.projection_select = Some(SelDrag::new(idx, tool, mode_for(app, tool, mods), q, ckey)),
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
        }
        Tool::MagneticLasso => {
            let st = magnetic_settings(app);
            if let Some(p) = press {
                let q = to_view(p);
                if app.projection_select.is_none() {
                    let mut d = SelDrag::new(idx, tool, mode_for(app, tool, mods), q, ckey);
                    match photocraft_engine::projected_select::ViewPixels::new(&app.session, &projection_params(app)) {
                        Ok(mut px) => {
                            let s0 = px.snap(q, st).unwrap_or(q);
                            d.points = vec![s0];
                            d.pixels = Some(px);
                            app.projection_select = Some(d);
                        }
                        Err(e) => {
                            app.ui.status = e.to_string();
                            app.ui.status_error = true;
                        }
                    }
                } else if let Some(d) = app.projection_select.as_mut() {
                    if d.points.len() >= 3 && to_screen(d.points[0]).distance(p) < 8.0 {
                        close_magnetic(app, mods.alt, st, cam, w, h);
                    } else {
                        // A click fastens a point (⌥-click: a straight segment to it).
                        let seg = if mods.alt { vec![q] } else { std::mem::take(&mut d.live) };
                        d.points.extend(seg);
                        d.anchors.push(d.points.len() - 1);
                        d.live.clear();
                        d.trail = vec![q];
                    }
                }
            } else if let (Some(h), Some(d)) = (response.hover_pos().or(response.interact_pointer_pos()), app.projection_select.as_mut()) {
                let q = to_view(h);
                if d.trail.last().is_none_or(|l| (l[0] - q[0]).abs() + (l[1] - q[1]).abs() > 1.0)
                    && let (Some(&from), Some(px)) = (d.points.last(), d.pixels.as_mut())
                {
                    d.trail.push(q);
                    d.live = px.trace(from, q, &d.trail, st);
                    // Fasten by itself every so often (Frequency).
                    let f = f64::from(app.ui.tool_options.magnetic_frequency.clamp(0.0, 100.0));
                    let spacing = (8.0 + (100.0 - f) * 0.9) * f64::from(ppp);
                    let len: f64 = d.live.windows(2).map(|s| (s[1][0] - s[0][0]).hypot(s[1][1] - s[0][1])).sum();
                    if len > spacing {
                        let live = std::mem::take(&mut d.live);
                        d.points.extend(live);
                        d.anchors.push(d.points.len() - 1);
                        d.trail = vec![q];
                    }
                }
            }
            if response.double_clicked() || ctx.input(|i| i.key_pressed(Key::Enter)) {
                close_magnetic(app, mods.alt, st, cam, w, h);
            }
            if ctx.input(|i| i.key_pressed(Key::Backspace) || i.key_pressed(Key::Delete))
                && let Some(d) = app.projection_select.as_mut()
                && d.anchors.len() > 1
            {
                d.anchors.pop();
                let keep = d.anchors.last().copied().unwrap_or(0) + 1;
                d.points.truncate(keep);
                d.live.clear();
            }
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                app.projection_select = None;
            }
            ctx.request_repaint();
        }
        Tool::MagicWand => {
            if response.clicked()
                && let Some(p) = response.interact_pointer_pos()
            {
                let d = SelDrag::new(idx, tool, mode_for(app, tool, mods), to_view(p), ckey);
                commit(app, d, cam, w, h);
            }
        }
        _ => {
            if response.drag_started_by(egui::PointerButton::Primary)
                && let Some(p) = ctx.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
            {
                let mut d = SelDrag::new(idx, tool, mode_for(app, tool, mods), to_view(p), ckey);
                if tool == Tool::QuickSelection {
                    d.pixels = photocraft_engine::projected_select::ViewPixels::new(&app.session, &projection_params(app)).ok();
                }
                app.projection_select = Some(d);
            }
            if response.dragged_by(egui::PointerButton::Primary)
                && let Some(p) = response.interact_pointer_pos()
                && let Some(d) = app.projection_select.as_mut()
            {
                let mut q = to_view(p);
                if matches!(tool, Tool::RectMarquee | Tool::EllipseMarquee) {
                    let a = d.anchor;
                    // ⇧ after the press makes a square / circle; ⌥ draws from the centre.
                    if mods.shift && d.mode != "add" {
                        let s = (q[0] - a[0]).abs().max((q[1] - a[1]).abs());
                        q = [a[0] + s * (q[0] - a[0]).signum(), a[1] + s * (q[1] - a[1]).signum()];
                    }
                    let from = if mods.alt && d.mode != "subtract" { [2.0 * a[0] - q[0], 2.0 * a[1] - q[1]] } else { a };
                    d.points = vec![from, q];
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
                let mut d = SelDrag::new(idx, tool, mode_for(app, tool, mods), q, ckey);
                d.points = vec![q, q];
                commit(app, d, cam, w, h);
            }
            if tool == Tool::QuickSelection {
                live_quick(app, ctx);
            }
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
            Tool::MagneticLasso => {
                let pts: Vec<Pos2> = d.points.iter().chain(&d.live).map(|q| to_screen(*q)).collect();
                dash(pts, false);
                for &i in &d.anchors {
                    if let Some(q) = d.points.get(i) {
                        painter.rect_stroke(Rect::from_center_size(to_screen(*q), vec2(5.0, 5.0)), 0.0, Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Middle);
                    }
                }
            }
            Tool::QuickSelection => {
                if let Some((t, r)) = &d.quick {
                    let at = Rect::from_min_max(to_screen([f64::from(r[0]), f64::from(r[1])]), to_screen([f64::from(r[2]), f64::from(r[3])]));
                    painter.image(t.id(), at, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
            }
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

/// Close the Magnetic Lasso border along the edges (⌥: straight) and commit it.
fn close_magnetic(app: &mut PhotocraftApp, straight: bool, st: photocraft_algo::magnetic::Settings, cam: Camera, w: f64, h: f64) {
    let Some(mut d) = app.projection_select.take() else { return };
    let live = std::mem::take(&mut d.live);
    d.points.extend(live);
    if let (false, Some(&first), Some(&last), Some(px)) = (straight, d.points.first(), d.points.last(), d.pixels.as_mut()) {
        let back = px.trace(last, first, &[], st);
        d.points.extend(back);
    }
    commit(app, d, cam, w, h);
}

/// Live Quick Selection: the region the stroke so far selects, tinted over the view (at most
/// every 80 ms, on the view-sized pixels only). The commit on release reprojects it.
fn live_quick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let size = app.session.tools.brush.size;
    let now = ctx.input(|i| i.time);
    let Some(d) = app.projection_select.as_mut() else { return };
    if d.points.len() == d.quick_n || now - d.quick_t < 0.08 {
        return;
    }
    let Some(px) = d.pixels.as_ref() else { return };
    let pts: Vec<(f32, f32)> = d.points.iter().map(|q| (q[0] as f32, q[1] as f32)).collect();
    d.quick_n = d.points.len();
    d.quick_t = now;
    let Some(r) = px.quick(&pts, size) else { return };
    let (bw, bh) = (r.bbox.width() as usize, r.bbox.height() as usize);
    if bw == 0 || bh == 0 {
        return;
    }
    let tint = |c: u8| -> Color32 {
        let a = (u16::from(c) * 110 / 255) as u8;
        Color32::from_rgba_unmultiplied(70, 140, 255, a)
    };
    let img = egui::ColorImage { size: [bw, bh], pixels: r.mask.iter().map(|c| tint(*c)).collect(), source_size: vec2(bw as f32, bh as f32) };
    let rr = [r.bbox.x0 as f32, r.bbox.y0 as f32, r.bbox.x1 as f32, r.bbox.y1 as f32];
    match &mut d.quick {
        Some((t, at)) => {
            t.set(img, egui::TextureOptions::LINEAR);
            *at = rr;
        }
        None => d.quick = Some((ctx.load_texture("pc-live-projection-quick", img, egui::TextureOptions::LINEAR), rr)),
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
        // A Patch / Content-Aware Move drag carries the outline with the pointer.
        let shift = ctx.data(|d| d.get_temp::<egui::Vec2>(crate::live_projection::ants_shift_id(idx))).unwrap_or_default();
        let at = Rect::from_min_max(rect.min + vec2(x0 as f32, y0 as f32) * k, rect.min + vec2(x1 as f32, y1 as f32) * k).translate(shift);
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
