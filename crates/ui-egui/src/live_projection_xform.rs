//! Move, Free Transform (Ctrl+T) and Paste in View › Equirectangular Live Projection.
//!
//! - Move: a drag turns the layer (or the selection's contents) across the sphere, from where
//!   the press points to where the pointer points; a live preview follows and the release
//!   commits `layer.projectedRotate`.
//! - Free Transform: handles in view space around the selection (or the layer's content) in
//!   the view: corners scale (proportionally; Shift frees), edges stretch, Ctrl/⌘-corner
//!   distorts (perspective), outside rotates, inside moves. Enter commits `layer.projectedPlace`
//!   (reprojected onto the equirect: seams and poles are just directions), Esc cancels. The
//!   view is held while transforming.
//! - Paste: the clipboard image as a new layer, upright and undistorted in the middle of the
//!   view, one image pixel per equirect pixel at the view centre (fitted to the view if larger).

use egui::{Color32, Pos2, Rect, Stroke, vec2};
use photocraft_engine::equirect::{Rot, ViewProjection, rotation_between};
use photocraft_engine::projected_transform::{XformPreview, apply_h, homography};
use serde_json::json;

use crate::PhotocraftApp;
use crate::live_projection::ProjectionView;

/// A Move drag or a Free Transform in progress.
pub struct Xform {
    pub(crate) doc: usize,
    pub(crate) kind: Kind,
    preview: XformPreview,
    /// The view it was set up in (yaw, pitch, fov, w, h).
    view: serde_json::Value,
    pub(crate) texture: Option<egui::TextureHandle>,
    shown: Option<photocraft_geom::Rect>,
    dirty: bool,
    last: f64,
}

pub(crate) enum Kind {
    Move { start: [f64; 3], q: Rot },
    Transform { rect: [f64; 4], quad: [[f64; 2]; 4], drag: Option<Drag> },
}

#[derive(Clone, Copy)]
pub(crate) struct Drag {
    handle: Handle,
    from: [f64; 2],
    quad: [[f64; 2]; 4],
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Handle {
    Corner(usize),
    Edge(usize),
    Inside,
    Rotate,
}

fn view_json(cam: &ProjectionView, vp: &ViewProjection) -> serde_json::Value {
    json!({"yaw": cam.yaw, "pitch": cam.pitch, "fov": cam.fov, "width": vp.w, "height": vp.h})
}

/// The projection and size of view `idx` as last drawn.
fn current(app: &PhotocraftApp, ctx: &egui::Context, idx: usize) -> Option<(ProjectionView, ViewProjection)> {
    let cam = app.ui.views.get(idx)?.projection?;
    let ppp = ctx.pixels_per_point();
    let r = app.last_canvas_rect;
    Some((cam, ViewProjection::new(cam.camera(), f64::from(r.width() * ppp), f64::from(r.height() * ppp))))
}

/// Edit › Paste and Edit › Free Transform while the active view is in projection.
pub fn intercept(app: &mut PhotocraftApp, ctx: &egui::Context, id: &str, params: &serde_json::Value) -> Option<Result<serde_json::Value, String>> {
    let idx = app.session.active_index()?;
    if !crate::live_projection::active(app, idx) || app.ui.text_edit.is_some() {
        return None;
    }
    match id {
        "edit.paste" if params.as_object().is_none_or(|o| o.is_empty()) => Some(paste(app, ctx, idx)),
        "edit.freeTransform" => Some(begin_transform(app, ctx, idx).map(|_| serde_json::Value::Null)),
        _ => None,
    }
}

fn paste(app: &mut PhotocraftApp, ctx: &egui::Context, idx: usize) -> Result<serde_json::Value, String> {
    app.import_os_clipboard();
    app.clip_read_for_paste = true;
    let (cam, vp) = current(app, ctx, idx).ok_or("no projection view")?;
    let clip = app.session.clipboard.as_ref().ok_or("the clipboard is empty")?;
    let b = clip.surface.content_bounds();
    let b = if b.is_empty() { clip.bounds } else { b };
    let iw = f64::from(app.session.documents()[idx].doc.size.width);
    // View px per equirect px at the view centre.
    let k = (vp.h / 2.0) / (f64::from(cam.fov).to_radians() / 2.0).tan() * std::f64::consts::TAU / iw;
    let (mut w, mut h) = (f64::from(b.width()) * k, f64::from(b.height()) * k);
    let fit = (vp.w * 0.9 / w).min(vp.h * 0.9 / h).min(1.0);
    (w, h) = (w * fit, h * fit);
    let (cx, cy) = (vp.w / 2.0, vp.h / 2.0);
    let quad = [[cx - w / 2.0, cy - h / 2.0], [cx + w / 2.0, cy - h / 2.0], [cx + w / 2.0, cy + h / 2.0], [cx - w / 2.0, cy + h / 2.0]];
    app.run("layer.projectedPlace", json!({"source": "clipboard", "quad": quad, "projection": view_json(&cam, &vp)}))
}

fn begin_transform(app: &mut PhotocraftApp, ctx: &egui::Context, idx: usize) -> Result<(), String> {
    let (cam, vp) = current(app, ctx, idx).ok_or("no projection view")?;
    let view = view_json(&cam, &vp);
    let preview = XformPreview::new(&app.session, &json!({"projection": view})).map_err(|e| e.to_string())?;
    let rect = preview.content_rect().ok_or("Nothing to transform in the view")?;
    let quad = corners(rect);
    app.projection_xform = Some(Xform { doc: idx, kind: Kind::Transform { rect, quad, drag: None }, preview, view, texture: None, shown: None, dirty: false, last: 0.0 });
    app.ui.status = "Free Transform: drag the handles (Shift: free scale, Ctrl: distort), outside to rotate; Enter applies, Esc cancels.".into();
    app.ui.status_error = false;
    Ok(())
}

fn corners(r: [f64; 4]) -> [[f64; 2]; 4] {
    [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]]
}

/// Whether a Free Transform holds the view (no looking around while it is up).
pub fn holds_view(app: &PhotocraftApp, idx: usize) -> bool {
    app.projection_xform.as_ref().is_some_and(|x| x.doc == idx && matches!(x.kind, Kind::Transform { .. }))
}

impl Xform {
    fn redraw(&mut self, ctx: &egui::Context, painter: &egui::Painter, rect: Rect, ppp: f32) {
        let now = ctx.input(|i| i.time);
        if self.dirty && now - self.last >= 0.05 {
            self.dirty = false;
            self.last = now;
            let o = match &self.kind {
                Kind::Move { q, .. } => self.preview.rotate(q),
                Kind::Transform { rect, quad, .. } => self.preview.place(*rect, *quad),
            };
            match o {
                Some(o) => {
                    let size = [o.rect.width() as usize, o.rect.height() as usize];
                    let img = egui::ColorImage::from_rgba_unmultiplied(size, o.px.as_flattened());
                    match &mut self.texture {
                        Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                        None => self.texture = Some(ctx.load_texture("pc-live-projection-xform", img, egui::TextureOptions::LINEAR)),
                    }
                    self.shown = Some(o.rect);
                }
                None => self.shown = None,
            }
        }
        if self.dirty {
            ctx.request_repaint();
        }
        if let (Some(t), Some(r)) = (&self.texture, self.shown) {
            let min = rect.min + vec2(r.x0 as f32, r.y0 as f32) / ppp;
            let at = Rect::from_min_size(min, vec2(r.width() as f32, r.height() as f32) / ppp);
            painter.image(t.id(), at, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
}

/// The Move tool in the projection canvas.
#[allow(clippy::too_many_arguments)]
pub fn move_tool(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, rect: Rect, ppp: f32, cam: &ProjectionView, vp: &ViewProjection, idx: usize) {
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Move);
    }
    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(p) = ctx.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
    {
        let [x, y] = to_view(p);
        let view = view_json(cam, vp);
        match XformPreview::new(&app.session, &json!({"projection": view})) {
            Ok(preview) => {
                app.projection_xform = Some(Xform { doc: idx, kind: Kind::Move { start: vp.view_to_dir(x, y), q: photocraft_engine::equirect::IDENTITY }, preview, view, texture: None, shown: None, dirty: false, last: 0.0 });
            }
            Err(e) => {
                app.ui.status = e.to_string();
                app.ui.status_error = true;
            }
        }
    }
    let Some(x) = app.projection_xform.as_mut().filter(|x| x.doc == idx && matches!(x.kind, Kind::Move { .. })) else { return };
    if let (Some(p), Kind::Move { start, q }) = (response.interact_pointer_pos(), &mut x.kind)
        && response.dragged()
    {
        let [vx, vy] = to_view(p);
        let nq = rotation_between(*start, vp.view_to_dir(vx, vy));
        if nq != *q {
            *q = nq;
            x.dirty = true;
        }
    }
    x.redraw(ctx, painter, rect, ppp);
    if response.drag_stopped() || !ctx.input(|i| i.pointer.any_down()) {
        let Some(x) = app.projection_xform.take() else { return };
        if let Kind::Move { q, .. } = x.kind
            && q != photocraft_engine::equirect::IDENTITY
            && let Err(e) = app.run("layer.projectedRotate", json!({"rotation": q.concat()}))
        {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
}

const HANDLE: f32 = 5.0;

fn hit(quad: &[[f64; 2]; 4], p: [f64; 2], tol: f64) -> Handle {
    let d = |a: [f64; 2]| ((a[0] - p[0]).powi(2) + (a[1] - p[1]).powi(2)).sqrt();
    for (i, c) in quad.iter().enumerate() {
        if d(*c) <= tol {
            return Handle::Corner(i);
        }
    }
    for i in 0..4 {
        let (a, b) = (quad[i], quad[(i + 1) % 4]);
        if d([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]) <= tol {
            return Handle::Edge(i);
        }
    }
    if inside_quad(quad, p) { Handle::Inside } else { Handle::Rotate }
}

fn inside_quad(q: &[[f64; 2]; 4], p: [f64; 2]) -> bool {
    let mut sign = 0.0;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let c = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        if c != 0.0 {
            if sign != 0.0 && c.signum() != sign {
                return false;
            }
            sign = c.signum();
        }
    }
    true
}

/// The new quad for dragging `d.handle` from `d.from` to `p`.
fn dragged(rect: [f64; 4], d: &Drag, p: [f64; 2], shift: bool, ctrl: bool) -> [[f64; 2]; 4] {
    let q = d.quad;
    let centre = [q.iter().map(|c| c[0]).sum::<f64>() / 4.0, q.iter().map(|c| c[1]).sum::<f64>() / 4.0];
    match d.handle {
        Handle::Inside => q.map(|c| [c[0] + p[0] - d.from[0], c[1] + p[1] - d.from[1]]),
        Handle::Rotate => {
            let a = (p[1] - centre[1]).atan2(p[0] - centre[0]) - (d.from[1] - centre[1]).atan2(d.from[0] - centre[0]);
            let (s, c) = a.sin_cos();
            q.map(|v| {
                let (x, y) = (v[0] - centre[0], v[1] - centre[1]);
                [centre[0] + x * c - y * s, centre[1] + x * s + y * c]
            })
        }
        Handle::Corner(i) if ctrl => {
            let mut n = q;
            n[i] = [q[i][0] + p[0] - d.from[0], q[i][1] + p[1] - d.from[1]];
            n
        }
        Handle::Corner(_) | Handle::Edge(_) => {
            // In the source rect's own space, so rotated or perspective quads scale along their sides.
            let src = corners(rect);
            let Some(h) = homography(src, q) else { return q };
            let Some(hi) = homography(q, src) else { return q };
            let Some(s) = apply_h(&hi, p[0], p[1]) else { return q };
            let mut r = rect;
            match d.handle {
                Handle::Corner(i) => {
                    let (ax, ay) = (if i == 0 || i == 3 { rect[2] } else { rect[0] }, if i < 2 { rect[3] } else { rect[1] });
                    let (mut sx, mut sy) = ((s[0] - ax) / (src[i][0] - ax), (s[1] - ay) / (src[i][1] - ay));
                    if !shift {
                        let k = if sx.abs() > sy.abs() { sx } else { sy };
                        (sx, sy) = (k, k);
                    }
                    let (nx, ny) = (ax + (src[i][0] - ax) * sx, ay + (src[i][1] - ay) * sy);
                    if i == 0 || i == 3 { r[0] = nx } else { r[2] = nx }
                    if i < 2 { r[1] = ny } else { r[3] = ny }
                }
                Handle::Edge(0) => r[1] = s[1],
                Handle::Edge(1) => r[2] = s[0],
                Handle::Edge(2) => r[3] = s[1],
                _ => r[0] = s[0],
            }
            corners(r).map(|c| apply_h(&h, c[0], c[1]).unwrap_or(c))
        }
    }
}

/// Free Transform in the projection canvas: handles, drags, Enter / Esc. Returns whether it
/// took the pointer this frame.
#[allow(clippy::too_many_arguments)]
pub fn transform(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, rect: Rect, ppp: f32, idx: usize) -> bool {
    if !holds_view(app, idx) {
        return false;
    }
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    let to_screen = |q: [f64; 2]| -> Pos2 { rect.min + vec2(q[0] as f32, q[1] as f32) / ppp };
    let (enter, esc, m) = ctx.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape), i.modifiers));
    if esc {
        app.projection_xform = None;
        app.ui.status = "Free Transform cancelled".into();
        return true;
    }
    if enter {
        let Some(x) = app.projection_xform.take() else { return true };
        if let Kind::Transform { rect: r, quad, .. } = x.kind
            && quad != corners(r)
            && let Err(e) = app.run("layer.projectedPlace", json!({"source": "layer", "rect": r, "quad": quad, "projection": x.view}))
        {
            app.ui.status = e;
            app.ui.status_error = true;
        }
        return true;
    }
    let Some(x) = app.projection_xform.as_mut() else { return true };
    let Kind::Transform { rect: r, quad, drag } = &mut x.kind else { return true };
    let tol = f64::from(HANDLE * 2.0 * ppp);
    if let Some(h) = response.hover_pos() {
        let icon = match drag.map(|d| d.handle).unwrap_or_else(|| hit(quad, to_view(h), tol)) {
            Handle::Inside => egui::CursorIcon::Move,
            Handle::Rotate => egui::CursorIcon::Alias,
            _ => egui::CursorIcon::Crosshair,
        };
        ctx.set_cursor_icon(icon);
    }
    if response.drag_started_by(egui::PointerButton::Primary)
        && let Some(p) = ctx.input(|i| i.pointer.press_origin())
    {
        let v = to_view(p);
        *drag = Some(Drag { handle: hit(quad, v, tol), from: v, quad: *quad });
    }
    if let (Some(d), Some(p)) = (*drag, response.interact_pointer_pos())
        && response.dragged()
    {
        let n = dragged(*r, &d, to_view(p), m.shift, m.command || m.ctrl);
        if n != *quad && n.iter().all(|c| c[0].is_finite() && c[1].is_finite()) {
            *quad = n;
            x.dirty = true;
        }
    }
    if response.drag_stopped() {
        *drag = None;
    }
    let q = *quad;
    x.redraw(ctx, painter, rect, ppp);
    // The box and its handles.
    let pts: Vec<Pos2> = q.iter().map(|c| to_screen(*c)).collect();
    for (w, c) in [(3.0, Color32::from_black_alpha(160)), (1.0, Color32::WHITE)] {
        painter.add(egui::Shape::closed_line(pts.clone(), Stroke::new(w, c)));
    }
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        for c in [a, [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]] {
            let r = Rect::from_center_size(to_screen(c), vec2(HANDLE * 2.0, HANDLE * 2.0));
            painter.rect_filled(r, 0.0, Color32::WHITE);
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Inside);
        }
    }
    true
}

#[cfg(test)]
pub(crate) fn quad_for_test(app: &PhotocraftApp) -> Option<[[f64; 2]; 4]> {
    match &app.projection_xform.as_ref()?.kind {
        Kind::Transform { quad, .. } => Some(*quad),
        _ => None,
    }
}
