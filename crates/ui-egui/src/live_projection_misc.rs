//! The remaining tools in View › Equirectangular Live Projection.
//!
//! - Point tools (Eyedropper, Color Sampler, Count, Note, Magic Eraser): a click in the view is
//!   the flat tool's click at the equirect pixel under it. The Magic Eraser floods the whole
//!   latlong, so the fill isn't cut off at the view's edge.
//! - Zoom: a click narrows the field of view about the click; Alt-click widens it.
//! - Ruler: a drag measures the great-circle angle between its ends.
//! - Crop, Perspective Crop, Slice, Artboard and Frame have no meaning in a spherical view.
//!   They show a clear message instead.

use egui::{Color32, Pos2, Rect, Stroke, vec2};
use photocraft_engine::equirect::ViewProjection;

use crate::canvas::ToolEvent;
use crate::live_projection::ProjectionView;
use crate::{PhotocraftApp, Tool};

pub fn supported(t: Tool) -> bool {
    matches!(t, Tool::Eyedropper | Tool::ColorSampler | Tool::Count | Tool::Note | Tool::MagicEraser | Tool::Zoom | Tool::Ruler)
}

/// What the status bar says when a tool can't work in the projection.
pub fn unavailable_message(tool: Tool) -> String {
    let name = crate::live_projection::tool_name(tool);
    match tool {
        Tool::Crop | Tool::PerspectiveCrop => format!("{name}: cropping a latlong breaks the 360° panorama. Turn off Equirectangular Live Projection (View menu) to crop the flat image."),
        Tool::Slice | Tool::SliceSelect => format!("{name}: slices are web-export rectangles on the flat image. Turn off Equirectangular Live Projection (View menu) to use them."),
        Tool::Artboard | Tool::Frame => format!("{name}: artboards and frames are flat layout tools and aren't available in Equirectangular Live Projection."),
        _ => format!("{name} is not available in Equirectangular Live Projection: turn it off in the View menu to use it."),
    }
}

fn doc_size(app: &PhotocraftApp, idx: usize) -> (f64, f64) {
    let d = &app.session.documents()[idx].doc;
    (f64::from(d.size.width), f64::from(d.size.height))
}

/// The equirect pixel under screen point `p`.
pub(crate) fn equirect_at(app: &PhotocraftApp, rect: Rect, ppp: f32, vp: &ViewProjection, idx: usize, p: Pos2) -> [f64; 2] {
    let (w, h) = doc_size(app, idx);
    vp.view_to_equirect(f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp), w, h)
}

/// Great-circle angle in degrees between two view points.
pub(crate) fn angle_deg(vp: &ViewProjection, a: [f64; 2], b: [f64; 2]) -> f64 {
    let (u, v) = (vp.view_to_dir(a[0], a[1]), vp.view_to_dir(b[0], b[1]));
    let d = (u[0] * v[0] + u[1] * v[1] + u[2] * v[2]).clamp(-1.0, 1.0);
    d.acos().to_degrees()
}

#[allow(clippy::too_many_arguments)]
pub fn tool(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, rect: Rect, ppp: f32, cam: &mut ProjectionView, vp: &ViewProjection, idx: usize, tool: Tool) {
    let mods = ctx.input(|i| i.modifiers);
    if response.hovered() {
        ctx.set_cursor_icon(match tool {
            Tool::Zoom if mods.alt => egui::CursorIcon::ZoomOut,
            Tool::Zoom => egui::CursorIcon::ZoomIn,
            _ => egui::CursorIcon::Crosshair,
        });
    }
    match tool {
        Tool::Zoom => {
            if response.clicked()
                && let Some(p) = response.interact_pointer_pos()
            {
                // Turn the clicked point to the centre, then narrow (or widen) the field of view.
                let d = vp.view_to_dir(f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp));
                let yaw = d[0].atan2(-d[2]).to_degrees();
                let pitch = d[1].clamp(-1.0, 1.0).asin().to_degrees();
                if yaw.is_finite() && pitch.is_finite() {
                    let c = vp.view_to_dir(vp.w / 2.0, vp.h / 2.0);
                    let (cy, cp) = (c[0].atan2(-c[2]).to_degrees(), c[1].clamp(-1.0, 1.0).asin().to_degrees());
                    cam.yaw += (yaw - cy) as f32;
                    cam.pitch += (pitch - cp) as f32;
                }
                cam.scale_fov(if mods.alt { 1.25 } else { 0.8 });
                cam.clamp();
                app.ui.status = format!("Field of view {:.0}°", cam.fov);
                app.ui.status_error = false;
            }
        }
        Tool::Ruler => {
            let id = egui::Id::new(("pc-live-projection-ruler", idx));
            if response.drag_started()
                && let Some(p) = ctx.input(|i| i.pointer.press_origin())
            {
                ctx.data_mut(|d| d.insert_temp(id, p));
            }
            let Some(a) = ctx.data(|d| d.get_temp::<Pos2>(id)) else { return };
            let Some(b) = response.interact_pointer_pos().or(response.hover_pos()) else { return };
            let to_view = |p: Pos2| [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)];
            let ang = angle_deg(vp, to_view(a), to_view(b));
            let (w, _) = doc_size(app, idx);
            app.ui.status = format!("Ruler: {ang:.2}° on the sphere ≈ {:.0} equirect px at the equator", ang / 360.0 * w);
            app.ui.status_error = false;
            painter.line_segment([a, b], Stroke::new(1.0, Color32::from_rgb(60, 160, 255)));
            if response.drag_stopped() {
                ctx.data_mut(|d| d.remove::<Pos2>(id));
            }
        }
        _ => {
            // Point tools: the flat tool's click at the equirect pixel.
            if response.clicked()
                && let Some(p) = response.interact_pointer_pos()
            {
                let [x, y] = equirect_at(app, rect, ppp, vp, idx, p);
                crate::canvas::tool_event(app, ToolEvent::Down { x, y, pressure: 1.0 }, mods);
                crate::canvas::tool_event(app, ToolEvent::Up { x, y }, mods);
            }
            if let Some(h) = response.hover_pos().filter(|_| response.hovered() && matches!(tool, Tool::Eyedropper | Tool::ColorSampler)) {
                painter.circle_stroke(h, 4.0, Stroke::new(1.0, Color32::WHITE));
                painter.circle_stroke(h + vec2(0.0, 0.0), 5.0, Stroke::new(1.0, Color32::BLACK));
            }
        }
    }
}
