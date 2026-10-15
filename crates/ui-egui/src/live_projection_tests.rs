//! Live projection inside the whole app: the toolbox, keys and cursor keep working.

use egui::{Event, Key, Modifiers, PointerButton, pos2, vec2};
use egui_kittest::{Harness, kittest::Queryable};
use serde_json::json;

use crate::{PhotocraftApp, Services, Tool};

fn harness(tool: Tool) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).with_step_dt(1.0 / 60.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        app.run("file.new", json!({"width": 800, "height": 400})).unwrap();
        app.ui.tool = tool;
        app
    });
    h.run_steps(5);
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, crate::live_projection::COMMAND, json!({})).unwrap();
    h.run_steps(5);
    assert!(crate::live_projection::active(h.state(), 0));
    h
}

#[test]
fn enabling_with_an_unsupported_tool_picks_the_hand() {
    let h = harness(Tool::Move);
    assert_eq!(h.state().ui.tool, Tool::Hand);
    let h = harness(Tool::Brush);
    assert_eq!(h.state().ui.tool, Tool::Brush, "a supported tool stays");
}

fn key(h: &mut Harness<'_, PhotocraftApp>, k: Key) {
    h.event(Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

#[test]
fn tool_keys_switch_tools_in_projection() {
    let mut h = harness(Tool::Move);
    // Hover the canvas first, as Ed would.
    let c = h.state().last_canvas_rect.center();
    h.event(Event::PointerMoved(c));
    h.run_steps(2);
    key(&mut h, Key::B);
    assert_eq!(h.state().ui.tool, Tool::Brush, "B picks the Brush");
    key(&mut h, Key::E);
    assert_eq!(h.state().ui.tool, Tool::Eraser, "E picks the Eraser");
    key(&mut h, Key::H);
    assert_eq!(h.state().ui.tool, Tool::Hand);
}

#[test]
fn toolbox_click_switches_tools_in_projection() {
    let mut h = harness(Tool::Move);
    let c = h.state().last_canvas_rect.center();
    h.event(Event::PointerMoved(c));
    h.run_steps(2);
    let nodes: Vec<_> = h.query_all_by_label_contains("Brush Tool").map(|n| (n.rect(), format!("{n:?}").chars().take(80).collect::<String>())).collect();
    let at = nodes.iter().min_by(|a, b| a.0.min.x.total_cmp(&b.0.min.x)).map(|n| n.0.center()).expect("Brush button in the toolbox");
    h.event(Event::PointerMoved(at));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    assert_eq!(h.state().ui.tool, Tool::Brush, "a toolbox click picks the Brush");
    let _ = pos2(0.0, 0.0);
}

fn click(h: &mut Harness<'_, PhotocraftApp>, at: egui::Pos2, button: PointerButton) {
    h.event(Event::PointerMoved(at));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: at, button, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    h.event(Event::PointerButton { pos: at, button, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

#[test]
fn right_click_opens_the_brush_picker_in_projection() {
    for tool in [Tool::Brush, Tool::Eraser, Tool::CloneStamp] {
        let mut h = harness(tool);
        let c = h.state().last_canvas_rect.center();
        click(&mut h, c, PointerButton::Secondary);
        assert!(h.state().ui.brush_picker.is_some(), "{tool:?}: right-click opens the preset picker");
    }
}

#[test]
fn bracket_keys_resize_the_brush_in_projection() {
    let mut h = harness(Tool::Brush);
    let c = h.state().last_canvas_rect.center();
    h.event(Event::PointerMoved(c));
    h.run_steps(2);
    let before = h.state().session.tools.brush.size;
    h.event(Event::Text("]".into()));
    key(&mut h, Key::CloseBracket);
    assert!(h.state().session.tools.brush.size > before, "] grows the brush");
    let grown = h.state().session.tools.brush.size;
    h.event(Event::Text("[".into()));
    key(&mut h, Key::OpenBracket);
    assert!(h.state().session.tools.brush.size < grown, "[ shrinks the brush");
}

#[test]
fn a_drag_shows_a_live_preview_and_commits_on_release() {
    let mut h = harness(Tool::Brush);
    let c = h.state().last_canvas_rect.center();
    h.event(Event::PointerMoved(c));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for i in 1..=10 {
        h.event(Event::PointerMoved(c + vec2(i as f32 * 8.0, 0.0)));
        h.run_steps(1);
    }
    let live = h.state().projection_live.as_ref().expect("a stroke in progress");
    assert!(live.has_preview(), "the stroke is rendered while dragging");
    h.event(Event::PointerButton { pos: c + vec2(80.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    assert!(h.state().projection_live.is_none());
    assert!(h.state().ui.status_error == false, "{}", h.state().ui.status);
}
