//! View › Equirectangular Live Projection (Affinity Photo's Live Projection): a latlong (2:1)
//! document shown through a perspective camera you look around in, per document view.
//!
//! - Drag with the Hand tool, Space or the middle button to look around; the wheel and the
//!   zoom commands change the field of view.
//! - Brush and Eraser strokes are drawn in the view and committed as one
//!   `paint.projectedStroke`, which reprojects them onto the equirect pixels (one History step).
//! - Other tools are off in this view; the canvas says so.
//!
//! Drawing is the GPU canvas's `fs_proj` pass over the document's existing tiles
//! (gpu_canvas.rs); the maths is `photocraft_engine::equirect`, shared with the engine.

use egui::{Color32, PointerButton, Pos2, Rect, Sense, Stroke, vec2};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::{Tool, View};
use photocraft_engine::equirect::{Camera, MAX_FOV, MAX_PITCH, MIN_FOV, wrap_deg};

/// Command id of the View menu toggle.
pub const COMMAND: &str = "view.equirectLiveProjection";

/// A view's live projection camera (degrees), with the view zoom it last saw so the ordinary
/// zoom commands (⌘+, ⌘−, the Zoom tool) can narrow and widen the field of view.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectionView {
    pub yaw: f32,
    pub pitch: f32,
    pub fov: f32,
    #[serde(default)]
    pub zoom: f32,
}

impl Default for ProjectionView {
    fn default() -> Self {
        ProjectionView { yaw: 0.0, pitch: 0.0, fov: 90.0, zoom: 0.0 }
    }
}

impl ProjectionView {
    pub fn camera(&self) -> Camera {
        Camera { yaw: f64::from(self.yaw), pitch: f64::from(self.pitch), fov: f64::from(self.fov) }.normalized()
    }

    fn clamp(&mut self) {
        let c = self.camera();
        (self.yaw, self.pitch, self.fov) = (c.yaw as f32, c.pitch as f32, c.fov as f32);
    }

    /// Look by a pointer drag of `d` points in a view `h` points tall: the image follows the
    /// pointer, one view height turning by the field of view.
    pub fn look(&mut self, d: egui::Vec2, h: f32) {
        let k = self.fov / h.max(1.0);
        self.yaw = wrap_deg(f64::from(self.yaw - d.x * k)) as f32;
        self.pitch = (self.pitch + d.y * k).clamp(-MAX_PITCH as f32, MAX_PITCH as f32);
    }

    /// Scale the field of view by `f` (< 1 zooms in).
    pub fn scale_fov(&mut self, f: f32) {
        if f.is_finite() && f > 0.0 {
            self.fov = (self.fov * f).clamp(MIN_FOV as f32, MAX_FOV as f32);
        }
    }
}

/// Toggle the projection of document `idx`'s view; returns whether it is now on.
pub fn toggle(app: &mut PhotocraftApp, idx: usize, on: Option<bool>) -> Result<bool, String> {
    let v = app.ui.views.get_mut(idx).ok_or("no document")?;
    let on = on.unwrap_or(v.projection.is_none());
    v.projection = if on { Some(ProjectionView { zoom: v.zoom, ..v.projection.unwrap_or_default() }) } else { None };
    if on && !supported(app.ui.tool) {
        // Move, marquees, type…: start in the Hand tool so the view responds at once. The
        // toolbox and tool keys switch tools as usual.
        crate::tool_memory::select(app, Tool::Hand);
    }
    if on {
        let size = app.session.documents().get(idx).map(|st| (st.doc.size.width, st.doc.size.height));
        if let Some((w, h)) = size
            && w != 2 * h
        {
            app.ui.status = format!("Live Projection expects a 2:1 equirectangular image; this one is {w} × {h}.");
        }
    }
    Ok(on)
}

/// Tools that work in the projection view.
pub fn supported(t: Tool) -> bool {
    matches!(t, Tool::Brush | Tool::Eraser | Tool::Hand | Tool::Zoom)
}

/// Whether document `idx`'s view shows the projection.
pub fn active(app: &PhotocraftApp, idx: usize) -> bool {
    app.ui.views.get(idx).is_some_and(|v| v.projection.is_some())
}

#[derive(Clone, Default)]
struct Stroke2 {
    /// View device pixels and pen pressure.
    points: Vec<[f64; 3]>,
    erase: bool,
}

/// The canvas of a view in live projection: draws, looks around and paints. Returns the view.
pub fn canvas(app: &mut PhotocraftApp, ui: &mut egui::Ui, idx: usize, rect: Rect, mut view: View, ppp: f32, output: Option<u32>) -> View {
    let ctx = ui.ctx().clone();
    let response = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_gray(28));
    let mut cam = view.projection.unwrap_or_default();
    // The zoom commands act on `view.zoom`: turn their change into a field of view change.
    if cam.zoom > 0.0 && view.zoom > 0.0 && (view.zoom - cam.zoom).abs() > 1e-6 {
        cam.scale_fov(cam.zoom / view.zoom);
    }
    cam.zoom = view.zoom;
    view.fit_pending = false;
    view.fill_pending = false;

    let tool = app.ui.tool;
    // The app's own temporary-Hand key (Space unless rebound), not a raw key read that can stick
    // when the window loses focus mid-press.
    let space = matches!(crate::hold_keys::held_tool(app, &ctx), Some(crate::hold_keys::Temporary::Hand));
    let middle = ctx.input(|i| i.pointer.button_down(PointerButton::Middle));
    let looking = tool == Tool::Hand || space || middle;
    // A stroke whose release this canvas never saw (released over a panel, focus lost) ends.
    if !ctx.input(|i| i.pointer.any_down()) && !response.drag_stopped() {
        ctx.data_mut(|d| d.remove_temp::<Stroke2>(egui::Id::new(("pc-live-projection-stroke", idx))));
    }
    let paints = matches!(tool, Tool::Brush | Tool::Eraser) && !looking;

    // Wheel: field of view about the view centre.
    if response.hovered() {
        let dy = ctx.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            cam.scale_fov((-dy * 0.002).exp());
        }
    }
    let stroke_id = egui::Id::new(("pc-live-projection-stroke", idx));
    if response.dragged() && (looking || !paints) {
        if looking {
            cam.look(response.drag_delta(), rect.height());
        }
    }
    cam.clamp();

    // GPU drawing.
    let doc = app.session.documents()[idx].doc.clone();
    let full = photocraft_geom::Rect::new(0, 0, doc.size.width as i32, doc.size.height as i32);
    let drawn = app.gpu.is_some() && crate::canvas::ensure_gpu(app, &ctx, idx, full);
    if drawn {
        let params = crate::gpu_canvas::ViewParams {
            doc: doc.id.0,
            doc_size: [doc.size.width, doc.size.height],
            zoom: 1.0,
            center: [0.0, 0.0],
            rotation: 0.0,
            shadow: false,
            pixel_grid: false,
            view_key: egui::Id::new(("pc-canvas-projection", ctx.viewport_id(), idx)).value(),
            display: crate::canvas::sync_display_lut(app, &doc, doc.id.0, output),
            output: output.unwrap_or(0),
            hdr: crate::canvas::hdr_preview(app, &doc),
            offset: [0.0, 0.0],
            projection: Some([cam.yaw, cam.pitch, cam.fov]),
        };
        crate::gpu_canvas::GpuCanvas::paint(&painter, rect, params);
    } else {
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Equirectangular Live Projection needs the GPU canvas", egui::FontId::proportional(14.0), Color32::LIGHT_GRAY);
    }

    // Painting: collect the stroke in view device pixels, commit it on release.
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    let size_px = app.session.tools.brush.size;
    if paints && drawn {
        let pressure = f64::from(app.stylus.pressure());
        if response.drag_started_by(PointerButton::Primary) || response.drag_started_by(PointerButton::Secondary) {
            let erase = tool == Tool::Eraser;
            ctx.data_mut(|d| d.insert_temp(stroke_id, Stroke2 { points: Vec::new(), erase }));
        }
        if (response.dragged() || response.drag_started())
            && let Some(p) = response.interact_pointer_pos()
        {
            let [x, y] = to_view(p);
            if let Some(mut s) = ctx.data(|d| d.get_temp::<Stroke2>(stroke_id)) {
                s.points.push([x, y, pressure]);
                ctx.data_mut(|d| d.insert_temp(stroke_id, s));
            }
        }
        let click = response.clicked().then(|| response.interact_pointer_pos()).flatten();
        let finished: Option<Stroke2> = if response.drag_stopped() {
            ctx.data_mut(|d| d.remove_temp::<Stroke2>(stroke_id))
        } else if let Some(p) = click {
            let [x, y] = to_view(p);
            Some(Stroke2 { points: vec![[x, y, pressure]], erase: tool == Tool::Eraser })
        } else {
            None
        };
        // Preview: the path so far at the brush width.
        if let Some(s) = ctx.data(|d| d.get_temp::<Stroke2>(stroke_id)) {
            let pts: Vec<Pos2> = s.points.iter().map(|q| rect.min + vec2(q[0] as f32, q[1] as f32) / ppp).collect();
            let fg = app.session.tools.foreground;
            let c = if s.erase { Color32::from_white_alpha(90) } else { Color32::from_rgba_unmultiplied((fg[0] * 255.0) as u8, (fg[1] * 255.0) as u8, (fg[2] * 255.0) as u8, 140) };
            painter.add(egui::Shape::line(pts, Stroke::new((size_px / ppp).max(1.0), c)));
            ctx.request_repaint();
        }
        if let Some(s) = finished.filter(|s| !s.points.is_empty()) {
            let p = json!({
                "points": s.points,
                "erase": s.erase,
                "zoom": 1.0,
                "target": crate::canvas::paint_target(app),
                "projection": {"yaw": cam.yaw, "pitch": cam.pitch, "fov": cam.fov, "width": rect.width() * ppp, "height": rect.height() * ppp},
            });
            if let Err(e) = app.run("paint.projectedStroke", p) {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
        // Brush outline at the pointer.
        if let Some(h) = response.hover_pos().filter(|_| response.hovered()) {
            painter.circle_stroke(h, (size_px / ppp / 2.0).max(1.0), Stroke::new(1.0, Color32::from_white_alpha(200)));
            painter.circle_stroke(h, (size_px / ppp / 2.0).max(1.0) + 1.0, Stroke::new(1.0, Color32::from_black_alpha(160)));
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
    } else if looking {
        // Only over the canvas (or while dragging from it): the cursor is per frame and global.
        if response.dragged() {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if response.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
    } else if !matches!(tool, Tool::Zoom) && response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::NotAllowed);
        if response.clicked() || response.drag_started() {
            app.ui.status = format!("{} is not available in Equirectangular Live Projection: use the Brush, Eraser or Hand tool, or turn it off in the View menu.", tool_name(tool));
            app.ui.status_error = false;
        }
    }
    // Heads-up: camera readout.
    let label = format!("Live Projection   yaw {:.1}°   pitch {:.1}°   FOV {:.1}°", cam.yaw, cam.pitch, cam.fov);
    let at = rect.left_bottom() + vec2(8.0, -8.0);
    painter.text(at + vec2(1.0, 1.0), egui::Align2::LEFT_BOTTOM, &label, egui::FontId::proportional(12.0), Color32::from_black_alpha(200));
    painter.text(at, egui::Align2::LEFT_BOTTOM, &label, egui::FontId::proportional(12.0), Color32::from_white_alpha(230));
    view.projection = Some(cam);
    view
}

fn tool_name(t: Tool) -> String {
    format!("{t:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Pos2;

    #[test]
    fn look_follows_the_pointer_and_wraps() {
        let mut v = ProjectionView { yaw: 170.0, ..Default::default() };
        // Dragging left by a whole view height turns right by the field of view.
        v.look(vec2(-100.0, 0.0), 100.0);
        assert!((v.yaw - (-100.0)).abs() < 1e-3, "{}", v.yaw);
        v.look(vec2(0.0, 1000.0), 100.0);
        assert_eq!(v.pitch, MAX_PITCH as f32);
    }

    /// A Hand-tool drag through the primary canvas accumulates frame by frame and stays put
    /// after release (Ed's report: the view moved ~50 px and snapped back, because the camera
    /// was never stored in `app.ui.views`).
    #[test]
    fn hand_drag_accumulates_and_persists_across_frames() {
        let ctx = egui::Context::default();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": 400, "height": 200})).unwrap();
        app.sync_views();
        toggle(&mut app, 0, Some(true)).unwrap();
        app.ui.tool = Tool::Hand;
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
        let frame = |app: &mut PhotocraftApp, events: Vec<egui::Event>| {
            let input = egui::RawInput { events, screen_rect: Some(rect), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                let view = app.ui.views[0].clone();
                crate::canvas::canvas_view(app, ui, 0, rect, view, true);
            });
            out.textures_delta.clear();
        };
        let at = |x: f32| egui::pos2(x, 150.0);
        frame(&mut app, vec![egui::Event::PointerMoved(at(300.0))]);
        frame(&mut app, vec![egui::Event::PointerButton { pos: at(300.0), button: PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
        let mut x = 300.0;
        for _ in 0..15 {
            x -= 10.0;
            frame(&mut app, vec![egui::Event::PointerMoved(at(x))]);
        }
        frame(&mut app, vec![egui::Event::PointerButton { pos: at(x), button: PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
        frame(&mut app, vec![]);
        let cam = app.ui.views[0].projection.expect("still on");
        // 150 points left in a 300-point-tall 90° view: about 45° to the right (egui may hold
        // back the first few points until the drag threshold).
        assert!(cam.yaw > 35.0 && cam.yaw <= 45.5, "yaw {}", cam.yaw);
    }

    #[test]
    fn fov_scales_within_limits() {
        let mut v = ProjectionView::default();
        v.scale_fov(0.5);
        assert_eq!(v.fov, 45.0);
        v.scale_fov(0.0001);
        assert_eq!(v.fov, MIN_FOV as f32);
        v.scale_fov(1e6);
        assert_eq!(v.fov, MAX_FOV as f32);
    }
}
