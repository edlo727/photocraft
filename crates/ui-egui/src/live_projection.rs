//! View › Equirectangular Live Projection (Affinity Photo's Live Projection): a latlong (2:1)
//! document shown through a perspective camera you look around in, per document view.
//!
//! - Drag with the Hand tool, Space or the middle button to look around; the wheel and the
//!   zoom commands change the field of view.
//! - Brush and Eraser strokes are drawn in the view and committed as one
//!   `paint.projectedStroke`, which reprojects them onto the equirect pixels (one History step).
//! - The selection tools draw in the view and commit `select.projected` (live_projection_select.rs);
//!   the marching ants follow the camera, and strokes are clipped by the selection.
//! - Other tools are off in this view; the canvas says so.
//!
//! Drawing is the GPU canvas's `fs_proj` pass over the document's existing tiles
//! (gpu_canvas.rs); the maths is `photocraft_engine::equirect`, shared with the engine.

use egui::{Color32, PointerButton, Pos2, Rect, Sense, Stroke, vec2};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::PhotocraftApp;
use crate::state::{Tool, View};
use photocraft_engine::equirect::{Camera, MAX_FOV, MAX_PITCH, MIN_FOV, Rot, ViewProjection, rot, rotation_between, wrap_deg};

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
    /// Clone Stamp source as a direction on the sphere (⌥/Alt-click), so it survives looking
    /// around.
    #[serde(default)]
    pub clone_source: Option<[f64; 3]>,
    /// Aligned Clone Stamp: the sphere rotation from destination to source fixed by the first
    /// stroke after the source was set (row-major 3×3).
    #[serde(default)]
    pub clone_rotation: Option<[f64; 9]>,
}

impl Default for ProjectionView {
    fn default() -> Self {
        ProjectionView { yaw: 0.0, pitch: 0.0, fov: 90.0, zoom: 0.0, clone_source: None, clone_rotation: None }
    }
}

impl ProjectionView {
    pub fn camera(&self) -> Camera {
        Camera { yaw: f64::from(self.yaw), pitch: f64::from(self.pitch), fov: f64::from(self.fov) }.normalized()
    }

    pub(crate) fn clamp(&mut self) {
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

/// Arrow keys look around by 2° (Shift 10°, Alt 0.5°); Home resets yaw, pitch and the field
/// of view. The keys are consumed so they don't also nudge the layer.
pub(crate) fn look_keys(ctx: &egui::Context, cam: &mut ProjectionView) {
    use egui::Key;
    ctx.input_mut(|i| {
        let m = i.modifiers;
        let step = if m.shift { 10.0 } else if m.alt { 0.5 } else { 2.0 };
        for (k, dy, dp) in [(Key::ArrowLeft, -1.0, 0.0), (Key::ArrowRight, 1.0, 0.0), (Key::ArrowUp, 0.0, 1.0), (Key::ArrowDown, 0.0, -1.0)] {
            if i.consume_key(m, k) {
                cam.yaw = wrap_deg(f64::from(cam.yaw + dy * step)) as f32;
                cam.pitch = (cam.pitch + dp * step).clamp(-MAX_PITCH as f32, MAX_PITCH as f32);
            }
        }
        if i.consume_key(egui::Modifiers::NONE, Key::Home) {
            let d = ProjectionView::default();
            (cam.yaw, cam.pitch, cam.fov) = (d.yaw, d.pitch, d.fov);
        }
    });
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
    matches!(
        t,
        Tool::Brush
            | Tool::Eraser
            | Tool::CloneStamp
            | Tool::Healing
            | Tool::SpotHealing
            | Tool::Blur
            | Tool::Sharpen
            | Tool::Smudge
            | Tool::Dodge
            | Tool::Burn
            | Tool::Sponge
            | Tool::Pencil
            | Tool::MixerBrush
            | Tool::ColorReplacement
            | Tool::HistoryBrush
            | Tool::Patch
            | Tool::Remove
            | Tool::ContentAwareMove
            | Tool::RedEye
            | Tool::Hand
            | Tool::Zoom
            | Tool::Move
            | Tool::Gradient
            | Tool::PaintBucket
            | Tool::BackgroundEraser
            | Tool::PatternStamp
    ) || crate::live_projection_select::supported(t)
        || crate::live_projection_misc::supported(t)
        || crate::live_projection_path::supported(t)
        || crate::live_projection_text::supported(t)
}

/// Whether document `idx`'s view shows the projection.
pub fn active(app: &PhotocraftApp, idx: usize) -> bool {
    app.ui.views.get(idx).is_some_and(|v| v.projection.is_some())
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
    if !ctx.input(|i| i.pointer.any_down()) && !response.drag_stopped() && app.projection_live.as_ref().is_some_and(|l| l.doc == idx) {
        app.projection_live = None;
    }
    let paints = (matches!(tool, Tool::Brush | Tool::Eraser | Tool::CloneStamp) || retouch_cmd(app, tool, false).is_some()) && !looking;

    // Wheel: field of view about the view centre.
    if response.hovered() {
        let dy = ctx.input(|i| i.smooth_scroll_delta.y);
        if dy != 0.0 {
            cam.scale_fov((-dy * 0.002).exp());
        }
    }
    // Keyboard look: arrows turn the view (Shift ×5, Alt ×¼), Home resets it.
    if response.hovered() && !ctx.wants_keyboard_input() {
        look_keys(&ctx, &mut cam);
    }
    if response.dragged() && (looking || !paints) {
        if looking {
            cam.look(response.drag_delta(), rect.height());
        }
    }
    cam.clamp();
    // A Free Transform holds the view: its handles live in it.
    if crate::live_projection_xform::holds_view(app, idx)
        && let Some(held) = view.projection
    {
        cam = ProjectionView { zoom: cam.zoom, ..held };
    }

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
    let vp = ViewProjection::new(cam.camera(), f64::from(rect.width() * ppp), f64::from(rect.height() * ppp));
    let cloning = matches!(tool, Tool::CloneStamp | Tool::Healing);
    let alt = ctx.input(|i| i.modifiers.alt);
    // Free Transform's handles take the pointer while it is up, whatever the tool.
    let transforming = crate::live_projection_xform::transform(app, &ctx, &response, &painter, rect, ppp, idx);
    if transforming {
    } else if paints && cloning && alt {
        // ⌥/Alt-click (or press) sets the source: a direction, so looking around keeps it.
        if (response.clicked() || response.drag_started())
            && let Some(p) = response.interact_pointer_pos()
        {
            let [x, y] = to_view(p);
            cam.clone_source = Some(vp.view_to_dir(x, y));
            cam.clone_rotation = None;
            app.ui.status = "Clone source set".into();
            app.ui.status_error = false;
        }
        if let Some(h) = response.hover_pos().filter(|_| response.hovered()) {
            crosshair(&painter, h, Color32::WHITE);
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
    } else if paints {
        let pressure = f64::from(app.stylus.pressure());
        // Right-click: the Brush Preset picker, as on the flat canvas.
        if response.secondary_clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            crate::brush_picker::open(&mut app.ui, &ctx, [p.x, p.y]);
        }
        let start = |app: &mut PhotocraftApp, cam: &ProjectionView, first: [f64; 3]| -> Option<Live> {
            let alt = ctx.input(|i| i.modifiers.alt);
            if let Some((flat_cmd, mut fp)) = retouch_cmd(app, tool, alt) {
                // Retouch and paint tools: the flat tool on the view (`paint.projectedTool`).
                fp["target"] = crate::canvas::paint_target(app);
                let mut p = json!({"command": flat_cmd, "params": fp, "projection": view_json(cam, &vp)});
                let q = if tool == Tool::Healing {
                    let q = clone_rotation_for(app, cam, &vp, first)?;
                    p["rotation"] = json!(q.concat());
                    Some(q)
                } else {
                    None
                };
                return match photocraft_engine::projected_tool::ProjectedTool::new(&app.session, &p) {
                    Ok(t) => Some(Live { doc: idx, cmd: "paint.projectedTool", params: p, rotation: q, points: vec![first], preview: LivePreview::Tool(Box::new(t)), texture: None, shown: None, rendered: 0, last: 0.0 }),
                    Err(e) => {
                        app.ui.status = e.to_string();
                        app.ui.status_error = true;
                        None
                    }
                };
            }
            let (cmd, p, q) = if cloning {
                let q = clone_rotation_for(app, cam, &vp, first)?;
                ("paint.projectedClone", clone_params(app, cam, &vp, &q), Some(q))
            } else {
                ("paint.projectedStroke", brush_params(app, cam, &vp, tool == Tool::Eraser), None)
            };
            let preview = if cloning {
                photocraft_engine::retouch_cmds::projected_clone_preview(&app.session, &p)
            } else {
                photocraft_engine::brush_cmds::ProjectedPreview::brush(&app.session, &p)
            }
            .map(LivePreview::Stroke);
            match preview {
                Ok(preview) => Some(Live { doc: idx, cmd, params: p, rotation: q, points: vec![first], preview, texture: None, shown: None, rendered: 0, last: 0.0 }),
                Err(e) => {
                    app.ui.status = e.to_string();
                    app.ui.status_error = true;
                    None
                }
            }
        };
        if response.drag_started_by(PointerButton::Primary)
            && let Some(p) = response.interact_pointer_pos()
        {
            let [x, y] = to_view(p);
            app.projection_live = start(app, &cam, [x, y, pressure]);
        } else if response.dragged_by(PointerButton::Primary)
            && let Some(p) = response.interact_pointer_pos()
            && let Some(l) = app.projection_live.as_mut().filter(|l| l.doc == idx)
        {
            let [x, y] = to_view(p);
            if l.points.last().is_none_or(|q| (q[0] - x).abs() + (q[1] - y).abs() > 0.25) {
                l.points.push([x, y, pressure]);
            }
        }
        let finished: Option<Live> = if response.drag_stopped_by(PointerButton::Primary) {
            app.projection_live.take().filter(|l| l.doc == idx)
        } else if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            let [x, y] = to_view(p);
            start(app, &cam, [x, y, pressure])
        } else {
            None
        };
        // The live preview: the real dabs (or cloned pixels), redrawn as points arrive.
        if let Some(l) = app.projection_live.as_mut().filter(|l| l.doc == idx) {
            l.draw(&ctx, &painter, rect, ppp);
            ctx.request_repaint();
        }
        if let Some(mut l) = finished {
            // Keep showing the stroke on the frame it is committed.
            l.draw(&ctx, &painter, rect, ppp);
            l.params["points"] = json!(l.points);
            let cmd = l.cmd;
            match app.run(l.cmd, l.params) {
                Ok(v) => {
                    if cloning {
                        cam.clone_rotation = l.rotation.filter(|_| app.ui.tool_options.clone_aligned).map(|q| flat(&q));
                    }
                    // Say so when a stroke changed nothing, rather than leaving it a mystery.
                    if cmd == "paint.projectedTool" && v["damage"].get(2).and_then(serde_json::Value::as_i64).is_some_and(|w| w <= 0) {
                        app.ui.status = format!("{}: the stroke changed no pixels", tool_name(tool));
                        app.ui.status_error = false;
                    }
                }
                Err(e) => {
                    app.ui.status = e;
                    app.ui.status_error = true;
                }
            }
        }
        // Clone source crosshair: where the pointer (or the stroke) samples from.
        if cloning && let Some(src) = cam.clone_source {
            let live = app.projection_live.as_ref().filter(|l| l.doc == idx);
            let at = live.and_then(|l| l.points.last().copied()).or_else(|| response.hover_pos().map(|h| {
                let [x, y] = to_view(h);
                [x, y, 1.0]
            }));
            let first = live.and_then(|l| l.points.first().copied());
            let shown = match (at, cam.clone_rotation.filter(|_| app.ui.tool_options.clone_aligned)) {
                (Some(a), Some(q)) => vp.dir_to_view(rot(&rot_of(&q), vp.view_to_dir(a[0], a[1]))),
                (Some(a), None) => {
                    let start = first.unwrap_or(a);
                    let q = rotation_between(vp.view_to_dir(start[0], start[1]), src);
                    vp.dir_to_view(rot(&q, vp.view_to_dir(a[0], a[1])))
                }
                _ => vp.dir_to_view(src),
            };
            if let Some([x, y]) = shown {
                let p = rect.min + vec2(x as f32, y as f32) / ppp;
                if rect.contains(p) {
                    crosshair(&painter, p, Color32::WHITE);
                }
            }
        }
        // Brush outline at the pointer.
        if let Some(h) = response.hover_pos().filter(|_| response.hovered()) {
            painter.circle_stroke(h, (size_px / ppp / 2.0).max(1.0), Stroke::new(1.0, Color32::from_white_alpha(200)));
            painter.circle_stroke(h, (size_px / ppp / 2.0).max(1.0) + 1.0, Stroke::new(1.0, Color32::from_black_alpha(160)));
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
    } else if crate::live_projection_misc::supported(tool) && !looking {
        crate::live_projection_misc::tool(app, &ctx, &response, &painter, rect, ppp, &mut cam, &vp, idx, tool);
    } else if crate::live_projection_text::supported(tool) && !looking {
        crate::live_projection_text::tool(app, &ctx, &response, &painter, rect, ppp, &cam, &vp, idx, tool);
    } else if crate::live_projection_path::supported(tool) && !looking {
        crate::live_projection_path::tool(app, &ctx, &response, rect, ppp, &cam, &vp, idx, tool);
    } else if tool == Tool::Move && !looking {
        crate::live_projection_xform::move_tool(app, &ctx, &response, &painter, rect, ppp, &cam, &vp, idx);
    } else if matches!(tool, Tool::Patch | Tool::ContentAwareMove) && !looking {
        patch(app, &ctx, &response, &painter, rect, ppp, &cam, &vp, idx, tool);
    } else if matches!(tool, Tool::Gradient | Tool::PaintBucket) && !looking {
        fill_tool(app, &ctx, &response, &painter, rect, ppp, &cam, &vp, idx, tool);
    } else if tool == Tool::RedEye && !looking {
        if response.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            let [x, y] = to_view(p);
            let o = &app.ui.tool_options;
            let fp = json!({"pupilSize": o.red_eye_pupil_size.clamp(1.0, 100.0), "darken": o.red_eye_darken.clamp(0.0, 100.0), "target": crate::canvas::paint_target(app)});
            let p = json!({"command": "paint.redEye", "params": fp, "points": [[x, y, 1.0]], "projection": view_json(&cam, &vp)});
            if let Err(e) = app.run("paint.projectedTool", p) {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    } else if crate::live_projection_select::supported(tool) && !looking {
        crate::live_projection_select::tool(app, &ctx, &response, &painter, rect, ppp, cam.camera(), idx, tool);
    } else if looking {
        // Only over the canvas (or while dragging from it): the cursor is per frame and global.
        if response.dragged() {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if response.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
    } else if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::NotAllowed);
        if response.clicked() || response.drag_started() {
            app.ui.status = crate::live_projection_misc::unavailable_message(tool);
            app.ui.status_error = false;
        }
    }
    // Right-click: the tool's canvas context menu (selection tools: Deselect, Select Inverse,
    // Feather, Layer via Copy…; Hand and Zoom: the view menu), as on the flat canvas. The
    // painting tools open the brush picker above.
    if response.secondary_clicked()
        && !paints
        && let Some(p) = response.interact_pointer_pos()
    {
        let [x, y] = to_view(p);
        let d = vp.view_to_equirect(x, y, f64::from(doc.size.width), f64::from(doc.size.height));
        app.ui.canvas_tool_menu = None;
        if crate::paint_mouse::has_brush_picker(tool) {
            // Quick Selection (and later brush-like tools): the brush picker, as on the canvas.
            crate::brush_picker::open(&mut app.ui, &ctx, [p.x, p.y]);
        } else if crate::canvas_tool_menu::is_view_tool(tool) {
            crate::canvas_tool_menu::open_view(app, &ctx, tool, [p.x, p.y], d);
        } else {
            crate::canvas_tool_menu::open_at(app, &ctx, tool, [p.x, p.y], Some(d));
        }
    }
    crate::live_projection_select::ants(app, &ctx, &painter, rect, ppp, cam.camera(), idx);
    app.projection_view_px = Some((idx, vp.w, vp.h));
    crate::live_projection_path::draw(app, &ctx, &painter, rect, ppp, &vp, idx, tool);
    // Heads-up: camera readout.
    let label = format!("Live Projection   yaw {:.1}°   pitch {:.1}°   FOV {:.1}°", cam.yaw, cam.pitch, cam.fov);
    let at = rect.left_bottom() + vec2(8.0, -8.0);
    painter.text(at + vec2(1.0, 1.0), egui::Align2::LEFT_BOTTOM, &label, egui::FontId::proportional(12.0), Color32::from_black_alpha(200));
    painter.text(at, egui::Align2::LEFT_BOTTOM, &label, egui::FontId::proportional(12.0), Color32::from_white_alpha(230));
    view.projection = Some(cam);
    view
}

/// A projected stroke being drawn: its command, params (without points), the points so far
/// (view device pixels, pressure) and the engine's live preview.
pub struct Live {
    doc: usize,
    cmd: &'static str,
    params: serde_json::Value,
    rotation: Option<Rot>,
    points: Vec<[f64; 3]>,
    preview: LivePreview,
    /// When the preview last rendered (the flat tools re-run at most every 50 ms).
    last: f64,
    texture: Option<egui::TextureHandle>,
    /// The overlay's view rect (device px) when it was uploaded.
    shown: Option<photocraft_geom::Rect>,
    /// Points rendered into the texture.
    rendered: usize,
}

impl Live {
    #[cfg(test)]
    pub(crate) fn has_preview(&self) -> bool {
        self.texture.is_some()
    }

    /// Render new points with the real brush engine and draw the overlay over the view.
    fn draw(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: Rect, ppp: f32) {
        let now = ctx.input(|i| i.time);
        let throttled = matches!(self.preview, LivePreview::Tool(_)) && now - self.last < 0.05;
        if self.rendered != self.points.len() && !throttled {
            self.rendered = self.points.len();
            self.last = now;
            let o = match &mut self.preview {
                LivePreview::Stroke(p) => p.render(&self.points),
                LivePreview::Tool(t) => t.preview(&self.points),
            };
            if let Some(o) = o {
                let size = [o.rect.width() as usize, o.rect.height() as usize];
                let img = egui::ColorImage::from_rgba_unmultiplied(size, bytemuck_flat(&o.px));
                match &mut self.texture {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("pc-live-projection-stroke", img, egui::TextureOptions::LINEAR)),
                }
                self.shown = Some(o.rect);
            }
        }
        if let (Some(t), Some(r)) = (&self.texture, self.shown) {
            let min = rect.min + vec2(r.x0 as f32, r.y0 as f32) / ppp;
            let at = Rect::from_min_size(min, vec2(r.width() as f32, r.height() as f32) / ppp);
            painter.image(t.id(), at, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
}

/// A Patch / Content-Aware Move drag: drawing its own freehand selection, or moving it.
#[derive(Clone)]
enum PatchDrag {
    Lasso { points: Vec<[f64; 2]>, mode: &'static str },
    Move { start: [f64; 2] },
}

/// Where the marching ants of document `idx` are drawn shifted to (view points), while a Patch
/// or Content-Aware Move drag moves the selection.
pub(crate) fn ants_shift_id(idx: usize) -> egui::Id {
    egui::Id::new(("pc-live-projection-ants-shift", idx))
}

/// Whether view device pixel `v` lies inside the selection of the active document.
fn in_selection(app: &PhotocraftApp, vp: &ViewProjection, v: [f64; 2]) -> bool {
    let Some(st) = app.session.active() else { return false };
    let Some(sel) = st.doc.selection.as_ref() else { return false };
    let (w, h) = (st.doc.size.width, st.doc.size.height);
    let [x, y] = vp.view_to_equirect(v[0], v[1], f64::from(w), f64::from(h));
    let (x, y) = ((x.floor() as i64).rem_euclid(i64::from(w)) as i32, (y.floor() as i32).clamp(0, h as i32 - 1));
    let mut m = vec![0f32; sel.channels()];
    sel.read_pixel(x, y, &mut m);
    m[0] >= 0.5
}

/// The Patch and Content-Aware Move tools, as in Photoshop: drag outside the selection (or with
/// none, or with Shift/Alt) to draw a freehand selection; drag inside it to move it, with the
/// marching ants and the patch preview following the pointer; release applies (Patch: Normal,
/// Source; the preview runs the flat Patch on the view, rotated onto the pointer, throttled).
#[allow(clippy::too_many_arguments)]
fn patch(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, rect: Rect, ppp: f32, cam: &ProjectionView, vp: &ViewProjection, idx: usize, tool: Tool) {
    let id = egui::Id::new(("pc-live-projection-patch", idx));
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    let to_screen = |q: [f64; 2]| -> Pos2 { rect.min + vec2(q[0] as f32, q[1] as f32) / ppp };
    let params = |app: &PhotocraftApp, start: [f64; 2], end: [f64; 2]| -> serde_json::Value {
        if tool == Tool::ContentAwareMove {
            // The content moves within the view: a view-space offset.
            let mut fp = crate::retouch_ui::content_aware_move_params(app, [(end[0] - start[0]).round() as i64, (end[1] - start[1]).round() as i64]);
            fp["target"] = crate::canvas::paint_target(app);
            return json!({"command": "paint.contentAwareMove", "params": fp, "projection": view_json(cam, vp)});
        }
        let q = rotation_between(vp.view_to_dir(start[0], start[1]), vp.view_to_dir(end[0], end[1]));
        json!({"command": "paint.patch", "params": {"mode": "source", "target": crate::canvas::paint_target(app)}, "rotation": q.concat(), "projection": view_json(cam, vp)})
    };
    // A drag whose release this canvas never saw ends (and the ants stop following it).
    if !ctx.input(|i| i.pointer.any_down()) && !response.drag_stopped() {
        ctx.data_mut(|d| {
            d.remove::<PatchDrag>(id);
            d.remove_temp::<egui::Vec2>(ants_shift_id(idx));
        });
    }
    let drag: Option<PatchDrag> = ctx.data(|d| d.get_temp(id));
    if response.hovered() {
        let inside = response.hover_pos().is_some_and(|h| in_selection(app, vp, to_view(h)));
        let moving = matches!(drag, Some(PatchDrag::Move { .. }));
        ctx.set_cursor_icon(if moving || (inside && drag.is_none()) { egui::CursorIcon::Move } else { egui::CursorIcon::Crosshair });
    }
    if response.drag_started_by(PointerButton::Primary)
        && let Some(p) = ctx.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
    {
        let q = to_view(p);
        let m = ctx.input(|i| i.modifiers);
        let d = if !m.shift && !m.alt && in_selection(app, vp, q) {
            if tool == Tool::Patch && (app.ui.tool_options.patch_mode != "source" || app.ui.tool_options.patch_content_aware) {
                app.ui.status = "In Live Projection the Patch tool patches from where you drag (Normal, Source).".into();
                app.ui.status_error = false;
            }
            PatchDrag::Move { start: q }
        } else {
            let selected = app.session.active().is_some_and(|d| d.doc.selection.is_some());
            let mode = if selected { crate::tool_feedback::document_selection_mode(app, Tool::Lasso, m) } else { "replace" };
            PatchDrag::Lasso { points: vec![q], mode }
        };
        ctx.data_mut(|x| x.insert_temp(id, d));
    }
    let Some(drag) = ctx.data(|d| d.get_temp::<PatchDrag>(id)) else { return };
    let now = response.interact_pointer_pos().map(to_view);
    match drag {
        PatchDrag::Lasso { mut points, mode } => {
            if let Some(q) = now
                && points.last().is_none_or(|l| (l[0] - q[0]).abs() + (l[1] - q[1]).abs() > 1.0)
            {
                points.push(q);
            }
            if response.drag_stopped() {
                ctx.data_mut(|d| d.remove::<PatchDrag>(id));
                if points.len() < 3 {
                    if mode == "replace" {
                        let _ = app.run("select.deselect", json!({}));
                    }
                    return;
                }
                if let Some(p) = crate::live_projection_select::params(app, Tool::Lasso, mode, &points, cam.camera(), vp.w, vp.h)
                    && let Err(e) = app.run("select.projected", p)
                {
                    app.ui.status = e;
                    app.ui.status_error = true;
                }
                return;
            }
            let line: Vec<Pos2> = points.iter().map(|q| to_screen(*q)).collect();
            painter.add(egui::Shape::line(line.clone(), Stroke::new(3.0, Color32::from_black_alpha(160))));
            painter.add(egui::Shape::line(line, Stroke::new(1.0, Color32::WHITE)));
            ctx.data_mut(|d| d.insert_temp(id, PatchDrag::Lasso { points, mode }));
            ctx.request_repaint();
        }
        PatchDrag::Move { start } => {
            let Some(now) = now else { return };
            if response.drag_stopped() {
                ctx.data_mut(|d| {
                    d.remove::<PatchDrag>(id);
                    d.remove_temp::<egui::Vec2>(ants_shift_id(idx));
                });
                app.projection_live = None;
                if (now[0] - start[0]).abs() + (now[1] - start[1]).abs() < 1.0 {
                    return;
                }
                let p = params(app, start, now);
                if let Err(e) = app.run("paint.projectedTool", p) {
                    app.ui.status = e;
                    app.ui.status_error = true;
                }
                return;
            }
            if !response.dragged() {
                return;
            }
            // The marching ants follow the pointer (drawn after this, shifted).
            let shift = vec2((now[0] - start[0]) as f32, (now[1] - start[1]) as f32) / ppp;
            ctx.data_mut(|d| d.insert_temp(ants_shift_id(idx), shift));
            painter.line_segment([to_screen(start), to_screen(now)], Stroke::new(1.0, Color32::from_white_alpha(160)));
            // The preview: rebuilt for each new drag position (a new source rotation), throttled.
            let t = ctx.input(|i| i.time);
            let stale = app.projection_live.as_ref().is_none_or(|l| l.doc != idx || (t - l.last > 0.15 && l.points.first().is_none_or(|q| q[0] != now[0] || q[1] != now[1])));
            // Content-Aware Move is a heavy content-aware fill: no live re-run, just the ants.
            if stale && tool == Tool::Patch {
                let p = params(app, start, now);
                match photocraft_engine::projected_tool::ProjectedTool::new(&app.session, &p) {
                    Ok(tool) => {
                        let texture = app.projection_live.take().and_then(|l| l.texture);
                        let mut l = Live { doc: idx, cmd: "paint.projectedTool", params: p, rotation: None, points: vec![[now[0], now[1], 1.0]], preview: LivePreview::Tool(Box::new(tool)), texture, shown: None, rendered: 0, last: 0.0 };
                        // Patch takes no points: render once now.
                        l.draw(ctx, painter, rect, ppp);
                        l.last = t;
                        app.projection_live = Some(l);
                    }
                    Err(e) => {
                        app.ui.status = e.to_string();
                        app.ui.status_error = true;
                    }
                }
            } else if let Some(l) = app.projection_live.as_mut() {
                l.draw(ctx, painter, rect, ppp);
            }
            ctx.request_repaint();
        }
    }
}

/// Gradient (drag, with a live preview) and Paint Bucket (click) in the view: the flat tools
/// on the resampled view through `paint.projectedTool`, with the options bar's settings.
#[allow(clippy::too_many_arguments)]
fn fill_tool(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, rect: Rect, ppp: f32, cam: &ProjectionView, vp: &ViewProjection, idx: usize, tool: Tool) {
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    if response.hovered() {
        ctx.set_cursor_icon(if tool == Tool::PaintBucket { crate::canvas::paint_bucket_cursor(ctx, response.hover_pos().unwrap_or_default()) } else { egui::CursorIcon::Crosshair });
    }
    let o = app.ui.tool_options.clone();
    let target = crate::canvas::paint_target(app);
    if tool == Tool::PaintBucket {
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            let [x, y] = to_view(p);
            let contents = if o.bucket_fill_pattern { "pattern" } else { "foreground" };
            let fp = json!({"tolerance": o.tolerance, "contiguous": o.contiguous, "antiAlias": o.anti_alias, "sampleAllLayers": o.sample_all_layers, "opacity": o.fill_opacity, "contents": contents, "target": target});
            if let Err(e) = app.run("paint.projectedTool", json!({"command": "paint.bucket", "params": fp, "points": [[x, y, 1.0]], "projection": view_json(cam, vp)})) {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
        return;
    }
    let fp = json!({"style": o.gradient_style, "reverse": o.gradient_reverse, "dither": o.gradient_dither, "opacity": o.fill_opacity, "mode": o.gradient_blend_mode.label(), "target": target});
    let p = json!({"command": "paint.gradient", "params": fp, "projection": view_json(cam, vp)});
    if response.drag_started_by(PointerButton::Primary)
        && let Some(at) = ctx.input(|i| i.pointer.press_origin())
    {
        let [x, y] = to_view(at);
        app.projection_live = match photocraft_engine::projected_tool::ProjectedTool::new(&app.session, &p) {
            Ok(t) => Some(Live { doc: idx, cmd: "paint.projectedTool", params: p.clone(), rotation: None, points: vec![[x, y, 1.0]], preview: LivePreview::Tool(Box::new(t)), texture: None, shown: None, rendered: 0, last: 0.0 }),
            Err(e) => {
                app.ui.status = e.to_string();
                app.ui.status_error = true;
                None
            }
        };
    }
    let Some(l) = app.projection_live.as_mut().filter(|l| l.doc == idx && l.params["command"] == "paint.gradient") else { return };
    if let Some(at) = response.interact_pointer_pos() {
        let [x, y] = to_view(at);
        if l.points.len() < 2 {
            l.points.push([x, y, 1.0]);
        } else if l.points[1][0] != x || l.points[1][1] != y {
            l.points[1] = [x, y, 1.0];
            // A new end point: render again.
            l.rendered = 0;
        }
    }
    if l.points.len() == 2 {
        l.draw(ctx, painter, rect, ppp);
        let s = |q: [f64; 3]| rect.min + vec2(q[0] as f32, q[1] as f32) / ppp;
        painter.line_segment([s(l.points[0]), s(l.points[1])], Stroke::new(3.0, Color32::from_black_alpha(140)));
        painter.line_segment([s(l.points[0]), s(l.points[1])], Stroke::new(1.0, Color32::WHITE));
    }
    ctx.request_repaint();
    if response.drag_stopped() {
        let Some(mut l) = app.projection_live.take() else { return };
        if l.points.len() == 2 && (l.points[0][0] - l.points[1][0]).abs() + (l.points[0][1] - l.points[1][1]).abs() >= 2.0 {
            l.params["points"] = json!(l.points);
            if let Err(e) = app.run(l.cmd, l.params) {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
    }
}

/// What draws a live projected stroke.
enum LivePreview {
    /// Brush, Eraser, Clone Stamp: the brush engine on the view.
    Stroke(photocraft_engine::brush_cmds::ProjectedPreview),
    /// The retouch and paint tools: the flat tool re-run on the resampled view.
    Tool(Box<photocraft_engine::projected_tool::ProjectedTool>),
}

/// The flat command and options-bar params (without points and target) of a retouch or paint
/// tool that runs in projection through `paint.projectedTool`; ⌥ flips Dodge/Burn and
/// Blur/Sharpen as on the canvas.
pub(crate) fn retouch_cmd(app: &PhotocraftApp, tool: Tool, alt: bool) -> Option<(&'static str, serde_json::Value)> {
    let o = &app.ui.tool_options;
    Some(match tool {
        Tool::Healing => ("paint.healingBrush", json!({"aligned": o.clone_aligned, "sampleLayer": o.clone_sample})),
        Tool::SpotHealing => ("paint.spotHealing", json!({"type": o.spot_type, "sampleAllLayers": o.sample_all_layers})),
        Tool::Blur | Tool::Sharpen | Tool::Smudge | Tool::Dodge | Tool::Burn | Tool::Sponge => crate::retouch_ui::dab_params(app, crate::retouch_ui::alt_flipped(tool, alt))?,
        Tool::Pencil => ("paint.pencil", json!({})),
        Tool::MixerBrush => ("paint.mixerBrush", json!({})),
        Tool::ColorReplacement => ("paint.colorReplacement", crate::retouch_ui::color_replacement_params(app)),
        Tool::HistoryBrush => ("paint.historyBrush", json!({})),
        // Each stroke is one removal here (the pending-strokes mode and New Layer stay on the
        // flat canvas: the projection writes back into the target layer).
        Tool::Remove => ("paint.remove", json!({"sampleAllLayers": o.sample_all_layers, "newLayer": false})),
        Tool::BackgroundEraser => ("paint.backgroundEraser", json!({"sampling": o.bg_sampling, "limits": o.bg_limits, "tolerance": o.bg_tolerance, "protectForegroundColor": o.bg_protect_fg})),
        Tool::PatternStamp => {
            // Aligned phase is per view here: the pattern is laid out in the view.
            let mut p = json!({"aligned": o.pattern_stamp_aligned, "impressionist": o.pattern_stamp_impressionist, "scale": o.pattern_stamp_scale, "angle": o.pattern_stamp_angle});
            if let Some(id) = photocraft_engine::presets::patterns::current(&app.session).or(app.session.patterns.items.first()).map(|x| x.id.clone()) {
                p["pattern"] = json!(id);
            }
            ("paint.patternStamp", p)
        }
        _ => return None,
    })
}

fn bytemuck_flat(px: &[[u8; 4]]) -> &[u8] {
    // SAFETY-free: [[u8; 4]] is laid out as consecutive bytes.
    px.as_flattened()
}

fn view_json(cam: &ProjectionView, vp: &ViewProjection) -> serde_json::Value {
    json!({"yaw": cam.yaw, "pitch": cam.pitch, "fov": cam.fov, "width": vp.w, "height": vp.h})
}

/// `paint.projectedStroke` params (without points) for the Brush or Eraser.
fn brush_params(app: &PhotocraftApp, cam: &ProjectionView, vp: &ViewProjection, erase: bool) -> serde_json::Value {
    json!({"erase": erase, "zoom": 1.0, "target": crate::canvas::paint_target(app), "projection": view_json(cam, vp)})
}

/// `paint.projectedClone` params (without points) through sphere rotation `q`.
fn clone_params(app: &PhotocraftApp, cam: &ProjectionView, vp: &ViewProjection, q: &Rot) -> serde_json::Value {
    json!({
        "rotation": q.concat(),
        "sampleLayer": app.ui.tool_options.clone_sample,
        "opacity": app.session.tools.brush.opacity * 100.0,
        "flow": app.session.tools.brush.flow * 100.0,
        "target": crate::canvas::paint_target(app),
        "projection": view_json(cam, vp),
    })
}

/// The source rotation of a Clone Stamp stroke starting at `first`: the aligned one kept from an
/// earlier stroke, else from here to the source. `None` (with the status hint) without a source.
fn clone_rotation_for(app: &mut PhotocraftApp, cam: &ProjectionView, vp: &ViewProjection, first: [f64; 3]) -> Option<Rot> {
    let Some(src) = cam.clone_source else {
        app.ui.status = if cfg!(target_os = "macos") { "Option-click to define a source point to clone from" } else { "Alt-click to define a source point to clone from" }.into();
        app.ui.status_error = true;
        return None;
    };
    Some(match cam.clone_rotation.filter(|_| app.ui.tool_options.clone_aligned) {
        Some(q) => rot_of(&q),
        None => rotation_between(vp.view_to_dir(first[0], first[1]), src),
    })
}

fn flat(q: &Rot) -> [f64; 9] {
    [q[0][0], q[0][1], q[0][2], q[1][0], q[1][1], q[1][2], q[2][0], q[2][1], q[2][2]]
}

fn rot_of(q: &[f64; 9]) -> Rot {
    [[q[0], q[1], q[2]], [q[3], q[4], q[5]], [q[6], q[7], q[8]]]
}

fn crosshair(painter: &egui::Painter, p: Pos2, c: Color32) {
    for (w, col) in [(3.0, Color32::from_black_alpha(160)), (1.0, c)] {
        let s = Stroke::new(w, col);
        painter.line_segment([p - vec2(8.0, 0.0), p + vec2(8.0, 0.0)], s);
        painter.line_segment([p - vec2(0.0, 8.0), p + vec2(0.0, 8.0)], s);
    }
}

pub(crate) fn tool_name(t: Tool) -> String {
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
    fn clone_source_and_aligned_rotation_survive_looking_around() {
        let mut v = ProjectionView { clone_source: Some([0.0, 0.0, 1.0]), clone_rotation: Some([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]), ..Default::default() };
        let before = (v.clone_source, v.clone_rotation);
        v.look(vec2(-250.0, 40.0), 300.0);
        v.scale_fov(0.5);
        assert_eq!((v.clone_source, v.clone_rotation), before);
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
