//! The Type and shape tools in View › Equirectangular Live Projection: text is typed at the
//! click and shapes are dragged out in the view, then rendered upright there and placed on the
//! equirect as a new pixel layer (`layer.projectedRender`).

use egui::{Color32, PointerButton, Pos2, Rect, Stroke, vec2};
use photocraft_engine::equirect::ViewProjection;
use serde_json::json;

use crate::live_projection::ProjectionView;
use crate::{PhotocraftApp, Tool};

pub fn supported(t: Tool) -> bool {
    matches!(t, Tool::Type | Tool::VerticalType) || (crate::vector_ui::is_shape_tool(t) && t != Tool::CustomShape)
}

#[derive(Clone, Debug)]
pub(crate) struct Typing {
    pub(crate) at: [f64; 2],
    screen: Pos2,
    pub(crate) text: String,
}

pub(crate) fn typing_id(idx: usize) -> egui::Id {
    egui::Id::new(("pc-live-projection-typing", idx))
}

fn view_json(cam: &ProjectionView, vp: &ViewProjection) -> serde_json::Value {
    json!({"yaw": cam.yaw, "pitch": cam.pitch, "fov": cam.fov, "width": vp.w, "height": vp.h})
}

fn run(app: &mut PhotocraftApp, p: serde_json::Value) {
    if let Err(e) = app.run("layer.projectedRender", p) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Commit the text being typed, if any.
pub(crate) fn commit_text(app: &mut PhotocraftApp, ctx: &egui::Context, cam: &ProjectionView, vp: &ViewProjection, idx: usize) {
    let Some(t) = ctx.data(|d| d.get_temp::<Typing>(typing_id(idx))) else { return };
    ctx.data_mut(|d| d.remove::<Typing>(typing_id(idx)));
    if t.text.trim().is_empty() {
        return;
    }
    let params = crate::type_tool::point_text_params(app, t.at[0], t.at[1], &t.text);
    run(app, json!({"command": "type.create", "params": params, "projection": view_json(cam, vp)}));
}

#[allow(clippy::too_many_arguments)]
pub fn tool(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, painter: &egui::Painter, rect: Rect, ppp: f32, cam: &ProjectionView, vp: &ViewProjection, idx: usize, tool: Tool) {
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    if matches!(tool, Tool::Type | Tool::VerticalType) {
        if response.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Text);
        }
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            // A click elsewhere commits the text being typed and starts new text there.
            commit_text(app, ctx, cam, vp, idx);
            ctx.data_mut(|d| d.insert_temp(typing_id(idx), Typing { at: to_view(p), screen: p, text: String::new() }));
        }
        let Some(mut t) = ctx.data(|d| d.get_temp::<Typing>(typing_id(idx))) else { return };
        let (esc, done) = ctx.input(|i| (i.key_pressed(egui::Key::Escape), i.key_pressed(egui::Key::Enter) && i.modifiers.command));
        if esc {
            ctx.data_mut(|d| d.remove::<Typing>(typing_id(idx)));
            return;
        }
        if done {
            commit_text(app, ctx, cam, vp, idx);
            return;
        }
        let size = (app.ui.tool_options.type_size as f32 / ppp).max(8.0);
        egui::Area::new(egui::Id::new(("pc-live-projection-type-area", idx))).fixed_pos(t.screen - vec2(0.0, size)).order(egui::Order::Foreground).show(ctx, |ui| {
            let fg = app.session.tools.foreground;
            let col = Color32::from_rgb((fg[0].clamp(0.0, 1.0) * 255.0) as u8, (fg[1].clamp(0.0, 1.0) * 255.0) as u8, (fg[2].clamp(0.0, 1.0) * 255.0) as u8);
            let r = ui.add(egui::TextEdit::multiline(&mut t.text).font(egui::FontId::proportional(size)).text_color(col).desired_rows(1).desired_width(400.0).hint_text("Type, then Ctrl+Enter"));
            r.request_focus();
        });
        ctx.data_mut(|d| d.insert_temp(typing_id(idx), t));
        return;
    }
    // Shapes: drag out in the view.
    if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let id = egui::Id::new(("pc-live-projection-shape", idx));
    if response.drag_started_by(PointerButton::Primary)
        && let Some(p) = ctx.input(|i| i.pointer.press_origin())
    {
        ctx.data_mut(|d| d.insert_temp(id, to_view(p)));
    }
    let Some(start) = ctx.data(|d| d.get_temp::<[f64; 2]>(id)) else { return };
    let Some(now) = response.interact_pointer_pos() else { return };
    let end = to_view(now);
    let m = ctx.input(|i| i.modifiers);
    if response.drag_stopped() {
        ctx.data_mut(|d| d.remove::<[f64; 2]>(id));
        if let Some(params) = crate::vector_ui::shape_params(app, tool, start, end, m) {
            run(app, json!({"command": "shape.create", "params": params, "projection": view_json(cam, vp)}));
        }
        return;
    }
    let a = rect.min + vec2(start[0] as f32, start[1] as f32) / ppp;
    let r = Rect::from_two_pos(a, now);
    let st = Stroke::new(1.0, Color32::from_rgb(60, 160, 255));
    match tool {
        Tool::EllipseShape => {
            painter.add(egui::Shape::ellipse_stroke(r.center(), r.size() / 2.0, st));
        }
        Tool::Line => {
            painter.line_segment([a, now], st);
        }
        _ => {
            painter.rect_stroke(r, 0.0, st, egui::StrokeKind::Inside);
        }
    }
    ctx.request_repaint();
}
