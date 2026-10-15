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
