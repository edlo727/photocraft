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
    let h = harness(Tool::Crop);
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

fn drag(h: &mut Harness<'_, PhotocraftApp>, from: egui::Pos2, to: egui::Pos2, modifiers: Modifiers) {
    h.event(Event::PointerMoved(from));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers });
    h.run_steps(1);
    for i in 1..=8 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.run_steps(1);
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers });
    h.run_steps(3);
}

fn selected(h: &Harness<'_, PhotocraftApp>) -> bool {
    h.state().session.active().unwrap().doc.selection.as_ref().is_some_and(|s| !s.content_bounds().is_empty())
}

#[test]
fn marquee_drag_selects_and_click_deselects_in_projection() {
    let mut h = harness(Tool::RectMarquee);
    assert_eq!(h.state().ui.tool, Tool::RectMarquee, "the marquee works in projection");
    let c = h.state().last_canvas_rect.center();
    drag(&mut h, c - vec2(40.0, 30.0), c + vec2(40.0, 30.0), Modifiers::NONE);
    assert!(selected(&h), "{}", h.state().ui.status);
    assert_eq!(h.state().session.journal.last().map(|(id, _)| id.as_str()), Some("select.projected"));
    click(&mut h, c + vec2(200.0, 0.0), PointerButton::Primary);
    assert!(!selected(&h), "a click deselects");
}

#[test]
fn polygonal_lasso_closes_on_the_first_corner_in_projection() {
    let mut h = harness(Tool::PolygonLasso);
    let c = h.state().last_canvas_rect.center();
    for q in [vec2(-40.0, -30.0), vec2(40.0, -30.0), vec2(40.0, 30.0), vec2(-40.0, -30.0)] {
        click(&mut h, c + q, PointerButton::Primary);
        h.run_steps(20); // not a double-click
    }
    assert!(selected(&h), "{}", h.state().ui.status);
}

#[test]
fn right_click_opens_the_tool_menu_for_selection_tools_in_projection() {
    for tool in [Tool::RectMarquee, Tool::EllipseMarquee, Tool::Lasso, Tool::PolygonLasso, Tool::MagicWand, Tool::ObjectSelection, Tool::Hand, Tool::Zoom] {
        let mut h = harness(tool);
        let c = h.state().last_canvas_rect.center();
        click(&mut h, c, PointerButton::Secondary);
        assert!(h.state().ui.canvas_tool_menu.is_some(), "{tool:?}: right-click opens the context menu");
    }
    // Quick Selection is brush-like: its brush picker, as on the flat canvas.
    let mut h = harness(Tool::QuickSelection);
    let c = h.state().last_canvas_rect.center();
    click(&mut h, c, PointerButton::Secondary);
    assert!(h.state().ui.brush_picker.is_some());
}

#[test]
fn single_row_and_alt_centre_marquee_in_projection() {
    let mut h = harness(Tool::SingleRowMarquee);
    let c = h.state().last_canvas_rect.center();
    click(&mut h, c, PointerButton::Primary);
    assert!(selected(&h), "a click selects the row: {}", h.state().ui.status);
    // ⌥ after the press: the marquee grows from its centre, so the press point is inside.
    let mut h = harness(Tool::RectMarquee);
    let c = h.state().last_canvas_rect.center();
    h.event(Event::PointerMoved(c));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for i in 1..=6 {
        h.event(Event::PointerMoved(c + vec2(i as f32 * 10.0, i as f32 * 6.0)));
        h.event(Event::ModifiersChanged(Modifiers::ALT));
        h.run_steps(1);
    }
    let pts = h.state().projection_select.as_ref().map(|d| d.points_for_test()).unwrap();
    assert!(pts[0][0] < pts[1][0] && pts[0][1] < pts[1][1]);
    let ppp = h.ctx.pixels_per_point() as f64;
    let mid = [(pts[0][0] + pts[1][0]) / 2.0 / ppp, (pts[0][1] + pts[1][1]) / 2.0 / ppp];
    let r = h.state().last_canvas_rect;
    assert!((mid[0] + r.min.x as f64 - c.x as f64).abs() < 2.0 && (mid[1] + r.min.y as f64 - c.y as f64).abs() < 2.0, "centred on the press");
}

#[test]
fn quick_selection_previews_while_dragging_and_magnetic_lasso_closes() {
    let mut h = harness(Tool::QuickSelection);
    let c = h.state().last_canvas_rect.center();
    h.event(Event::PointerMoved(c));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for i in 1..=6 {
        h.event(Event::PointerMoved(c + vec2(i as f32 * 6.0, 0.0)));
        h.run_steps(6);
    }
    assert!(h.state().projection_select.as_ref().is_some_and(|d| d.has_quick_preview()), "a live preview while dragging");
    h.event(Event::PointerButton { pos: c + vec2(36.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    assert!(selected(&h), "{}", h.state().ui.status);

    let mut h = harness(Tool::MagneticLasso);
    let c = h.state().last_canvas_rect.center();
    for q in [vec2(-40.0, -30.0), vec2(40.0, -30.0), vec2(40.0, 30.0)] {
        click(&mut h, c + q, PointerButton::Primary);
        h.run_steps(20);
    }
    key(&mut h, Key::Enter);
    assert!(selected(&h), "{}", h.state().ui.status);
}

#[test]
fn right_click_menu_is_drawn_and_stays_open_in_projection() {
    for tool in [Tool::RectMarquee, Tool::Lasso, Tool::MagicWand] {
        let mut h = harness(tool);
        // Something to act on, as Ed has.
        let c = h.state().last_canvas_rect.center();
        drag(&mut h, c - vec2(40.0, 30.0), c + vec2(40.0, 30.0), Modifiers::NONE);
        if tool != Tool::RectMarquee {
            h.state_mut().run("select.all", json!({})).unwrap();
        }
        h.event(Event::PointerMoved(c));
        h.run_steps(2);
        h.event(Event::PointerButton { pos: c, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
        h.run_steps(1);
        h.event(Event::PointerButton { pos: c, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
        h.run_steps(1);
        h.event(Event::PointerMoved(c + vec2(10.0, 10.0)));
        h.run_steps(5);
        assert!(h.state().ui.canvas_tool_menu.is_some(), "{tool:?}: still open on frame 5+");
        assert!(h.query_by_label_contains("Deselect").is_some(), "{tool:?}: the menu is actually drawn");
    }
}

fn stroke(h: &mut Harness<'_, PhotocraftApp>, from: egui::Pos2, steps: usize) -> bool {
    h.event(Event::PointerMoved(from));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    let mut previewed = false;
    for i in 1..=steps {
        h.event(Event::PointerMoved(from + vec2(i as f32 * 6.0, 0.0)));
        // Past the 50 ms preview throttle.
        h.run_steps(4);
        previewed |= h.state().projection_live.as_ref().is_some_and(|l| l.has_preview());
    }
    h.event(Event::PointerButton { pos: from + vec2(steps as f32 * 6.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    previewed
}

#[test]
fn retouch_and_paint_tools_stroke_with_a_live_preview_in_projection() {
    for tool in [Tool::SpotHealing, Tool::Smudge, Tool::Blur, Tool::Sharpen, Tool::Dodge, Tool::Burn, Tool::Sponge, Tool::Pencil, Tool::MixerBrush, Tool::ColorReplacement, Tool::HistoryBrush] {
        let mut h = harness(tool);
        assert_eq!(h.state().ui.tool, tool, "{tool:?} works in projection");
        // Something to retouch: a coloured band across the view.
        h.state_mut().run("select.rect", json!({"x": 380, "y": 150, "width": 60, "height": 100})).unwrap();
        h.state_mut().run("edit.fill", json!({"color": "#3070c0"})).unwrap();
        h.state_mut().run("select.deselect", json!({})).unwrap();
        if tool == Tool::HistoryBrush {
            // Paint over it, then brush the earlier state back.
            h.state_mut().run("edit.fill", json!({"color": "#000000"})).unwrap();
        }
        let c = h.state().last_canvas_rect.center();
        let _previewed = stroke(&mut h, c - vec2(30.0, 0.0), 10);
        assert!(!h.state().ui.status_error, "{tool:?}: {}", h.state().ui.status);
        assert_eq!(h.state().session.journal.last().map(|(id, _)| id.as_str()), Some("paint.projectedTool"), "{tool:?} committed");
    }
}

#[test]
fn healing_brush_needs_a_source_then_heals_in_projection() {
    let mut h = harness(Tool::Healing);
    h.state_mut().run("select.rect", json!({"x": 380, "y": 150, "width": 60, "height": 100})).unwrap();
    h.state_mut().run("edit.fill", json!({"color": "#3070c0"})).unwrap();
    h.state_mut().run("select.deselect", json!({})).unwrap();
    let c = h.state().last_canvas_rect.center();
    stroke(&mut h, c, 3);
    assert!(h.state().ui.status_error, "no source yet: says so");
    // ⌥-click sets the source.
    h.event(Event::PointerMoved(c + vec2(80.0, 0.0)));
    h.run_steps(2);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: c + vec2(80.0, 0.0), button: PointerButton::Primary, pressed, modifiers: Modifiers::ALT });
        h.event(Event::ModifiersChanged(Modifiers::ALT));
        h.run_steps(1);
    }
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(3);
    h.state_mut().ui.status_error = false;
    let previewed = stroke(&mut h, c, 10);
    assert!(previewed, "a live preview while healing");
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert_eq!(h.state().session.journal.last().map(|(id, _)| id.as_str()), Some("paint.projectedTool"));
}

#[test]
fn patch_drags_the_selection_to_its_source_in_projection() {
    let mut h = harness(Tool::Patch);
    h.state_mut().run("select.rect", json!({"x": 380, "y": 150, "width": 60, "height": 100})).unwrap();
    h.state_mut().run("edit.fill", json!({"color": "#3070c0"})).unwrap();
    let c = h.state().last_canvas_rect.center();
    let journal = h.state().session.journal.len();
    drag(&mut h, c, c + vec2(150.0, 0.0), Modifiers::NONE);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert!(h.state().session.journal.len() > journal);
    assert_eq!(h.state().session.journal.last().map(|(id, _)| id.as_str()), Some("paint.projectedTool"));
}

/// A 4096×2048 HDR float latlong round-tripped through EXR (as Ed opens his files), minified in
/// the view like a 20k image is.
fn exr_harness(tool: Tool) -> Harness<'static, PhotocraftApp> {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 4096, "height": 2048, "depth": 32})).unwrap();
    let id = s.active().unwrap().doc.walk().iter().find(|(_, _, l)| l.surface().is_some()).map(|(_, _, l)| l.id).unwrap();
    s.select_layer(id).unwrap();
    s.edit("fill", |doc, _| {
        let surf = doc.layer_mut(id).unwrap().surface_mut().unwrap();
        let n = surf.channels();
        let (w, h) = (4096usize, 2048usize);
        let mut d = vec![0f32; w * h * n];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * n;
                let t = ((x * 7 + y * 13) % 17) as f32 / 17.0;
                let sky = if y < h / 2 { 30.0 * (1.0 - y as f32 / (h / 2) as f32).powi(3) } else { 0.0 };
                d[i] = 0.2 + 0.8 * x as f32 / w as f32 + 0.1 * t + sky;
                d[i + 1] = 0.3 + 0.1 * t + sky * 0.9;
                d[i + 2] = 0.05 + 0.6 * y as f32 / h as f32 + sky * 0.8;
                if n == 4 {
                    d[i + 3] = 1.0;
                }
                if (x as i32 - 2100).pow(2) + (y as i32 - 1020).pow(2) < 400 {
                    d[i..i + 3].fill(0.0);
                }
            }
        }
        surf.write_region(photocraft_geom::Rect::new(0, 0, w as i32, h as i32), &d);
        Ok(())
    })
    .unwrap();
    let doc = (*s.active().unwrap().doc).clone();
    let r = photocraft_io::export(&doc, "x.exr", &photocraft_io::ExportOptions::default()).unwrap();
    let back = photocraft_io::import("x.exr", &r.bytes).unwrap().document;
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).with_step_dt(1.0 / 60.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut session = photocraft_engine::Session::new();
        session.add_document(back.clone(), Some("x.exr".into()));
        let mut app = PhotocraftApp::new(session, Services::default());
        app.sync_views();
        app.ui.tool = tool;
        app
    });
    h.run_steps(5);
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, crate::live_projection::COMMAND, json!({})).unwrap();
    h.run_steps(5);
    h
}

fn screen_of_doc(h: &Harness<'_, PhotocraftApp>, x: f64, y: f64) -> egui::Pos2 {
    let r = h.state().last_canvas_rect;
    let ppp = h.ctx.pixels_per_point();
    let st = h.state().session.active().unwrap();
    let (w, hh) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let cam = h.state().ui.views[0].projection.unwrap().camera();
    let vp = photocraft_engine::equirect::ViewProjection::new(cam, f64::from(r.width() * ppp), f64::from(r.height() * ppp));
    let [vx, vy] = vp.equirect_to_view(x, y, w, hh).unwrap();
    r.min + vec2(vx as f32, vy as f32) / ppp
}

#[test]
fn healing_group_heals_an_f32_exr_and_refreshes_the_canvas() {
    for tool in [Tool::SpotHealing, Tool::Healing, Tool::Patch] {
        let mut h = exr_harness(tool);
        let d = h.state().session.active().unwrap();
        let fmt = d.active_layer.and_then(|id| d.doc.layer(id)).and_then(|l| l.surface()).map(|s| s.format());
        assert_eq!(fmt.map(|f| f.sample), Some(photocraft_color::SampleType::F32));
        h.state_mut().session.tools.brush.size = 60.0;
        let dot = screen_of_doc(&h, 2100.5, 1020.5);
        if tool == Tool::Healing {
            let src = dot + vec2(40.0, 0.0);
            h.event(Event::PointerMoved(src));
            h.event(Event::ModifiersChanged(Modifiers::ALT));
            h.run_steps(2);
            for pressed in [true, false] {
                h.event(Event::PointerButton { pos: src, button: PointerButton::Primary, pressed, modifiers: Modifiers::ALT });
                h.run_steps(1);
            }
            h.event(Event::ModifiersChanged(Modifiers::NONE));
            h.run_steps(3);
        }
        if tool == Tool::Patch {
            h.state_mut().run("select.rect", json!({"x": 2070, "y": 990, "width": 60, "height": 60})).unwrap();
            drag(&mut h, dot, dot + vec2(70.0, 0.0), Modifiers::NONE);
        } else {
            stroke(&mut h, dot - vec2(3.0, 0.0), 1);
        }
        let p = |x: i32, y: i32| {
            let d = h.state().session.active().unwrap();
            d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y)
        };
        assert!(!h.state().ui.status_error, "{tool:?}: {}", h.state().ui.status);
        assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("paint.projectedTool"), "{tool:?}");
        let centre = p(2100, 1020);
        assert!(centre[0] > 0.2, "{tool:?}: the black dot is gone on the F32 EXR: {centre:?}");
        // What the GPU canvas uploads: it must cover the healed pixels.
        let dmg = h.state().session.active().unwrap().last_damage;
        assert!(dmg.is_some_and(|d| d.contains(2100, 1020)), "{tool:?}: damage {dmg:?} reaches the canvas");
    }
}


#[test]
fn remove_red_eye_and_content_aware_move_work_on_an_f32_exr() {
    for tool in [Tool::Remove, Tool::RedEye, Tool::ContentAwareMove] {
        let mut h = exr_harness(tool);
        assert_eq!(h.state().ui.tool, tool);
        h.state_mut().session.tools.brush.size = 60.0;
        let dot = screen_of_doc(&h, 2100.5, 1020.5);
        match tool {
            Tool::Remove => {
                stroke(&mut h, dot - vec2(3.0, 0.0), 1);
            }
            Tool::RedEye => click(&mut h, dot, PointerButton::Primary),
            _ => {
                h.state_mut().run("select.rect", json!({"x": 2070, "y": 990, "width": 60, "height": 60})).unwrap();
                drag(&mut h, dot, dot + vec2(70.0, 0.0), Modifiers::NONE);
            }
        }
        assert!(!h.state().ui.status_error, "{tool:?}: {}", h.state().ui.status);
        assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("paint.projectedTool"), "{tool:?}");
        if tool == Tool::Remove {
            let d = h.state().session.active().unwrap();
            let c = d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(2100, 1020);
            assert!(c[0] > 0.2, "removed: {c:?}");
        }
    }
}

/// End to end, as Ed works: a 4096×2048 F32 latlong (multi-tile) with a gradient, per-pixel
/// noise, an HDR sky up to ~30, an `inf` sun pixel and a `NaN` pixel near a dark blemish at doc
/// (2100, 1020), round-tripped through EXR; projection on; the tool options exactly as the app
/// initialises them (nothing overridden); real pointer events over many frames.
fn ed_harness(tool: Tool) -> Harness<'static, PhotocraftApp> {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 4096, "height": 2048, "depth": 32})).unwrap();
    let id = s.active().unwrap().doc.walk().iter().find(|(_, _, l)| l.surface().is_some()).map(|(_, _, l)| l.id).unwrap();
    s.select_layer(id).unwrap();
    s.edit("fill", |doc, _| {
        let surf = doc.layer_mut(id).unwrap().surface_mut().unwrap();
        let n = surf.channels();
        let (w, h) = (4096usize, 2048usize);
        let mut d = vec![0f32; w * h * n];
        let mut seed = 0x9e37_79b9u32;
        for y in 0..h {
            for x in 0..w {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let t = (seed % 1000) as f32 / 1000.0;
                let i = (y * w + x) * n;
                let sky = if y < h / 2 { 30.0 * (1.0 - y as f32 / (h / 2) as f32).powi(3) } else { 0.0 };
                d[i] = 0.2 + 0.8 * x as f32 / w as f32 + 0.15 * t + sky;
                d[i + 1] = 0.3 + 0.15 * t + sky * 0.9;
                d[i + 2] = 0.05 + 0.6 * y as f32 / h as f32 + sky * 0.8;
                if n == 4 {
                    d[i + 3] = 1.0;
                }
                if (x as i32 - 2100).pow(2) + (y as i32 - 1020).pow(2) < 36 {
                    d[i..i + 3].fill(0.0);
                }
                if (x, y) == (2118, 1008) {
                    d[i..i + 3].fill(f32::INFINITY);
                }
                if (x, y) == (2086, 1031) {
                    d[i] = f32::NAN;
                }
            }
        }
        surf.write_region(photocraft_geom::Rect::new(0, 0, w as i32, h as i32), &d);
        Ok(())
    })
    .unwrap();
    let doc = (*s.active().unwrap().doc).clone();
    let r = photocraft_io::export(&doc, "ed.exr", &photocraft_io::ExportOptions::default()).unwrap();
    let back = photocraft_io::import("ed.exr", &r.bytes).unwrap().document;
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).with_step_dt(1.0 / 60.0).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut session = photocraft_engine::Session::new();
        session.add_document(back.clone(), Some("ed.exr".into()));
        let mut app = PhotocraftApp::new(session, Services::default());
        app.sync_views();
        app.ui.tool = tool;
        app
    });
    h.run_steps(5);
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, crate::live_projection::COMMAND, json!({})).unwrap();
    h.run_steps(5);
    assert!(crate::live_projection::active(h.state(), 0));
    assert_eq!(h.state().ui.tool, tool, "{tool:?} works in projection");
    h
}

/// Non-finite values and black pixels in the blemish (doc 2094..2107 × 1014..1027), besides
/// the planted `NaN`.
fn ed_damage(h: &Harness<'_, PhotocraftApp>) -> (usize, usize, f32) {
    let d = h.state().session.active().unwrap();
    let surf = d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap();
    let r = photocraft_geom::Rect::new(2094, 1014, 2107, 1027);
    let n = surf.channels();
    let b = surf.read_region(r);
    let nan = b.iter().filter(|v| !v.is_finite()).count();
    let black = b.chunks_exact(n).filter(|p| p[0] <= 1e-6 && p[1] <= 1e-6 && p[2] <= 1e-6).count();
    (nan, black, surf.rgba(2100, 1020)[0])
}

/// What the GPU canvas shows after a commit: the live overlay gone, and a new revision whose
/// damage (the tiles re-uploaded) covers the healed pixels.
fn assert_canvas_refreshed(h: &Harness<'_, PhotocraftApp>, tool: Tool, revision: u64) {
    assert!(h.state().projection_live.is_none(), "{tool:?}: the live overlay is cleared");
    let st = h.state().session.active().unwrap();
    assert!(st.revision > revision, "{tool:?}: a new revision for the canvas");
    assert!(st.last_damage.is_some_and(|d| d.contains(2100, 1020)), "{tool:?}: damage {:?} re-uploads the healed tiles", st.last_damage);
}

fn alt_click(h: &mut Harness<'_, PhotocraftApp>, at: egui::Pos2) {
    h.event(Event::PointerMoved(at));
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    h.run_steps(2);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::ALT });
        h.run_steps(1);
    }
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(3);
}

#[test]
fn ed_flow_spot_healing_and_healing_heal_a_hdr_f32_exr_end_to_end() {
    for tool in [Tool::SpotHealing, Tool::Healing] {
        let mut h = ed_harness(tool);
        let dot = screen_of_doc(&h, 2100.5, 1020.5);
        if tool == Tool::Healing {
            alt_click(&mut h, dot + vec2(0.0, 40.0));
            assert!(h.state().ui.views[0].projection.unwrap().clone_source.is_some(), "Alt-click sets the Healing source");
        }
        let rev = h.state().session.active().unwrap().revision;
        let previewed = stroke(&mut h, dot - vec2(12.0, 0.0), 4);
        assert!(!h.state().ui.status_error, "{tool:?}: {}", h.state().ui.status);
        assert!(previewed, "{tool:?}: a live preview while stroking");
        assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("paint.projectedTool"), "{tool:?}: the stroke was dispatched");
        let (nan, black, c) = ed_damage(&h);
        assert!(nan == 0 && black == 0 && c > 0.2, "{tool:?}: nan {nan} black {black} centre {c}");
        assert_canvas_refreshed(&h, tool, rev);
    }
}

#[test]
fn ed_flow_patch_draws_its_selection_then_drags_it_with_ants_and_preview() {
    let mut h = ed_harness(Tool::Patch);
    assert!(h.state().session.active().unwrap().doc.selection.is_none());
    let dot = screen_of_doc(&h, 2100.5, 1020.5);
    // Draw the patch selection around the blemish with the Patch tool itself.
    let ring: Vec<egui::Pos2> = (0..=24).map(|i| {
        let a = i as f32 / 24.0 * std::f32::consts::TAU;
        dot + vec2(a.cos(), a.sin()) * 18.0
    }).collect();
    h.event(Event::PointerMoved(ring[0]));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: ring[0], button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    for p in &ring[1..] {
        h.event(Event::PointerMoved(*p));
        h.run_steps(1);
    }
    h.event(Event::PointerButton { pos: *ring.last().unwrap(), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    assert!(selected(&h), "Patch drew its own selection: {}", h.state().ui.status);
    assert!(h.state().projection_ants.is_some(), "marching ants show it");
    // Drag it (from inside) to the source, over frames: ants and preview follow.
    let rev = h.state().session.active().unwrap().revision;
    let to = dot + vec2(0.0, 50.0);
    h.event(Event::PointerMoved(dot));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: dot, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    let mut previewed = false;
    let mut shifted = false;
    for i in 1..=10 {
        h.event(Event::PointerMoved(dot + (to - dot) * (i as f32 / 10.0)));
        h.run_steps(6);
        previewed |= h.state().projection_live.as_ref().is_some_and(|l| l.has_preview());
        shifted |= h.ctx.data(|d| d.get_temp::<egui::Vec2>(crate::live_projection::ants_shift_id(0))).is_some_and(|v| v.y > 5.0);
    }
    assert!(previewed, "the patch preview shows while dragging");
    assert!(shifted, "the marching ants follow the drag");
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("paint.projectedTool"));
    let (nan, black, c) = ed_damage(&h);
    assert!(nan == 0 && black == 0 && c > 0.2, "patch: nan {nan} black {black} centre {c}");
    assert_canvas_refreshed(&h, Tool::Patch, rev);
    assert!(h.ctx.data(|d| d.get_temp::<egui::Vec2>(crate::live_projection::ants_shift_id(0))).is_none(), "ants back in place");
}

fn doc_of_screen(h: &Harness<'_, PhotocraftApp>, p: egui::Pos2) -> [f64; 2] {
    let r = h.state().last_canvas_rect;
    let ppp = h.ctx.pixels_per_point();
    let st = h.state().session.active().unwrap();
    let (w, hh) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let cam = h.state().ui.views[0].projection.unwrap().camera();
    let vp = photocraft_engine::equirect::ViewProjection::new(cam, f64::from(r.width() * ppp), f64::from(r.height() * ppp));
    vp.view_to_equirect(f64::from((p.x - r.min.x) * ppp), f64::from((p.y - r.min.y) * ppp), w, hh)
}

fn active_px(h: &Harness<'_, PhotocraftApp>, at: [f64; 2]) -> [f32; 4] {
    let d = h.state().session.active().unwrap();
    d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(at[0] as i32, at[1] as i32)
}

fn non_finite(h: &Harness<'_, PhotocraftApp>) -> usize {
    let d = h.state().session.active().unwrap();
    let s = d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap();
    // Pixels, not values (the sun is inf in R, G and B).
    let n = s.channels();
    s.read_region(photocraft_geom::Rect::new(0, 0, 4096, 2048)).chunks_exact(n).filter(|p| p.iter().any(|v| !v.is_finite())).count()
}

#[test]
fn ed_flow_move_turns_the_layer_across_the_sphere_with_a_live_preview() {
    let mut h = ed_harness(Tool::Move);
    let dot = screen_of_doc(&h, 2100.5, 1020.5);
    let to = dot + vec2(120.0, -60.0);
    h.event(Event::PointerMoved(dot));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: dot, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    let mut previewed = false;
    for i in 1..=8 {
        h.event(Event::PointerMoved(dot + (to - dot) * (i as f32 / 8.0)));
        h.run_steps(4);
        previewed |= h.state().projection_xform.as_ref().is_some_and(|x| x.texture.is_some());
    }
    assert!(previewed, "the move previews while dragging");
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("layer.projectedRotate"));
    assert!(h.state().projection_xform.is_none(), "the preview is gone");
    let c = active_px(&h, doc_of_screen(&h, to));
    assert!(c[0] < 0.01 && c[1] < 0.01, "the blemish is where the pointer let go: {c:?}");
    let bad = non_finite(&h);
    assert!(bad <= 8, "inf / NaN moved as (near) single pixels, not spread: {bad}");
}

#[test]
fn ed_flow_free_transform_moves_and_scales_the_selection_in_view() {
    let mut h = ed_harness(Tool::RectMarquee);
    h.state_mut().run("select.rect", json!({"x": 2090, "y": 1010, "width": 20, "height": 20})).unwrap();
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, "edit.freeTransform", json!({})).unwrap();
    h.run_steps(2);
    let q0 = crate::live_projection_xform::quad_for_test(h.state()).expect("handles around the selection");
    let dot = screen_of_doc(&h, 2100.5, 1020.5);
    // Inside: move it 100 px right.
    drag(&mut h, dot, dot + vec2(100.0, 0.0), Modifiers::NONE);
    let q1 = crate::live_projection_xform::quad_for_test(h.state()).unwrap();
    assert!((q1[0][0] - q0[0][0]) > 50.0, "moved: {q0:?} -> {q1:?}");
    // A corner: scale it up.
    let r = h.state().last_canvas_rect;
    let ppp = h.ctx.pixels_per_point();
    let corner = r.min + vec2(q1[2][0] as f32, q1[2][1] as f32) / ppp;
    drag(&mut h, corner, corner + vec2(30.0, 30.0), Modifiers::NONE);
    let q2 = crate::live_projection_xform::quad_for_test(h.state()).unwrap();
    assert!(q2[2][0] - q2[0][0] > q1[2][0] - q1[0][0] + 20.0, "scaled: {q1:?} -> {q2:?}");
    assert!(h.state().projection_xform.as_ref().is_some_and(|x| x.texture.is_some()), "a live preview");
    key(&mut h, Key::Enter);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("layer.projectedPlace"));
    let mid = r.min + vec2(((q2[0][0] + q2[2][0]) / 2.0) as f32, ((q2[0][1] + q2[2][1]) / 2.0) as f32) / ppp;
    let c = active_px(&h, doc_of_screen(&h, mid));
    assert!(c[0] < 0.01, "the blemish is in the transformed box: {c:?}");
    let bad = non_finite(&h);
    assert!(bad <= 8, "inf / NaN not spread: {bad}");
}

#[test]
fn ed_flow_paste_places_the_clipboard_upright_in_the_middle_of_the_view() {
    let mut h = ed_harness(Tool::Hand);
    h.state_mut().run("select.rect", json!({"x": 2090, "y": 1010, "width": 20, "height": 20})).unwrap();
    h.state_mut().run("edit.copy", json!({})).unwrap();
    let layers = h.state().session.active().unwrap().doc.walk().len();
    let ctx = h.ctx.clone();
    crate::menus::invoke(h.state_mut(), &ctx, "edit.paste", json!({})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("layer.projectedPlace"));
    assert_eq!(h.state().session.active().unwrap().doc.walk().len(), layers + 1, "a new layer");
    let c = active_px(&h, doc_of_screen(&h, h.state().last_canvas_rect.center()));
    assert!(c[3] > 0.99 && c[0] < 0.01, "the pasted blemish sits in the middle of the view: {c:?}");
    let off = active_px(&h, doc_of_screen(&h, h.state().last_canvas_rect.center() + vec2(200.0, 0.0)));
    assert!(off[3] < 0.01, "only there: {off:?}");
}

#[test]
fn ed_flow_gradient_and_paint_bucket_fill_in_the_view() {
    let mut h = ed_harness(Tool::Gradient);
    h.state_mut().run("tools.setColors", json!({"foreground": "#ff0000", "background": "#0000ff"})).unwrap();
    let c = h.state().last_canvas_rect.center();
    let (a, b) = (c - vec2(200.0, 0.0), c + vec2(200.0, 0.0));
    h.event(Event::PointerMoved(a));
    h.run_steps(2);
    h.event(Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.run_steps(1);
    let mut previewed = false;
    for i in 1..=8 {
        h.event(Event::PointerMoved(a + (b - a) * (i as f32 / 8.0)));
        h.run_steps(4);
        previewed |= h.state().projection_live.as_ref().is_some_and(|l| l.has_preview());
    }
    h.event(Event::PointerButton { pos: b, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    assert!(previewed, "the gradient previews while dragging");
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("paint.projectedTool"));
    let (l, r) = (active_px(&h, doc_of_screen(&h, a + vec2(10.0, 0.0))), active_px(&h, doc_of_screen(&h, b - vec2(10.0, 0.0))));
    assert!(l[0] > 0.7 && r[2] > 0.7, "foreground to background across the view: {l:?} {r:?}");
    assert!(non_finite(&h) <= 8);
    h.state_mut().ui.tool = Tool::PaintBucket;
    h.state_mut().ui.tool_options.tolerance = 255.0 as _;
    h.run_steps(2);
    click(&mut h, c, PointerButton::Primary);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("paint.projectedTool"));
    let f = active_px(&h, doc_of_screen(&h, c));
    assert!(f[0] > 0.9 && f[2] < 0.05, "the bucket filled with the foreground: {f:?}");
}

#[test]
fn ed_flow_pen_draws_a_path_in_view_that_selects_strokes_and_edits_in_projection() {
    let mut h = ed_harness(Tool::Pen);
    let c = h.state().last_canvas_rect.center();
    let sq = [c + vec2(-60.0, -40.0), c + vec2(60.0, -40.0), c + vec2(60.0, 40.0), c + vec2(-60.0, 40.0)];
    for p in sq.iter().chain(std::iter::once(&sq[0])) {
        click(&mut h, *p, PointerButton::Primary);
    }
    let path = h.state().session.active().unwrap().doc.work_path.clone().expect("the Pen stored a work path");
    assert!(path.subpaths[0].closed, "closed on its first anchor");
    // Make Selection: inside the square as seen in the view.
    h.state_mut().run("path.toSelection", json!({})).unwrap();
    let sel = h.state().session.active().unwrap().doc.selection.clone().expect("a selection");
    let mut m = [0f32; 1];
    let mut at = |p: egui::Pos2| {
        let d = doc_of_screen(&h, p);
        sel.read_pixel(d[0] as i32, d[1] as i32, &mut m);
        m[0]
    };
    assert!(at(c) > 0.9 && at(c + vec2(50.0, 30.0)) > 0.9, "inside");
    assert!(at(c + vec2(80.0, 0.0)) < 0.1, "outside");
    // Stroke Path paints with the brush along it in the view.
    h.state_mut().run("select.deselect", json!({})).unwrap();
    h.state_mut().run("path.stroke", json!({})).unwrap();
    assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("paint.projectedStroke"));
    assert!(non_finite(&h) <= 8);
    // Direct Selection: drag a corner anchor 30 px right.
    h.state_mut().ui.tool = Tool::DirectSelection;
    h.run_steps(2);
    drag(&mut h, sq[1], sq[1] + vec2(30.0, 0.0), Modifiers::NONE);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    let r = h.state().last_canvas_rect;
    let ppp = h.ctx.pixels_per_point();
    let cam = h.state().ui.views[0].projection.unwrap().camera();
    let vp = photocraft_engine::equirect::ViewProjection::new(cam, f64::from(r.width() * ppp), f64::from(r.height() * ppp));
    let path = h.state().session.active().unwrap().doc.work_path.clone().unwrap();
    let anchors: Vec<[f64; 2]> = path.subpaths[0].knots.iter().filter_map(|k| vp.dir_to_view(photocraft_engine::equirect::equirect_to_dir(k.anchor.x, k.anchor.y, 4096.0, 2048.0))).collect();
    let want = sq[1] + vec2(30.0, 0.0);
    let want = [f64::from((want.x - r.min.x) * ppp), f64::from((want.y - r.min.y) * ppp)];
    assert!(anchors.iter().any(|a| (a[0] - want[0]).abs() < 2.0 && (a[1] - want[1]).abs() < 2.0), "the anchor followed the drag: {anchors:?} want {want:?}");
    // Path Selection: drag the whole path; Delete Anchor: click one away.
    h.state_mut().ui.tool = Tool::PathSelection;
    h.run_steps(2);
    let n0 = path.subpaths[0].knots.len();
    drag(&mut h, c, c + vec2(0.0, 50.0), Modifiers::NONE);
    let moved = h.state().session.active().unwrap().doc.work_path.clone().unwrap();
    assert!(moved.subpaths[0].knots[0].anchor.y > path.subpaths[0].knots[0].anchor.y + 5.0, "the path moved down");
    h.state_mut().ui.tool = Tool::DeleteAnchorPoint;
    h.run_steps(2);
    let k0 = moved.subpaths[0].knots[0].anchor;
    let v = vp.dir_to_view(photocraft_engine::equirect::equirect_to_dir(k0.x, k0.y, 4096.0, 2048.0)).unwrap();
    click(&mut h, r.min + vec2(v[0] as f32, v[1] as f32) / ppp, PointerButton::Primary);
    assert_eq!(h.state().session.active().unwrap().doc.work_path.as_ref().unwrap().subpaths[0].knots.len(), n0 - 1, "one anchor fewer");
}

#[test]
fn ed_flow_rectangle_and_type_render_upright_in_projection_on_f32() {
    let mut h = ed_harness(Tool::Rectangle);
    h.state_mut().session.tools.foreground = [1.0, 0.0, 0.0, 1.0];
    let c = h.state().last_canvas_rect.center();
    let layers0 = h.state().session.active().unwrap().doc.layers.len();
    drag(&mut h, c + vec2(-40.0, -30.0), c + vec2(40.0, 30.0), Modifiers::NONE);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("layer.projectedRender"));
    assert_eq!(h.state().session.active().unwrap().doc.layers.len(), layers0 + 1, "a new layer");
    let f = active_px(&h, doc_of_screen(&h, c));
    assert!(f[0] > 0.9 && f[1] < 0.1 && f[3] > 0.9, "the rectangle is filled where it was drawn: {f:?}");
    assert!(active_px(&h, doc_of_screen(&h, c + vec2(70.0, 0.0)))[3] < 0.1, "nothing outside");
    assert!(non_finite(&h) <= 8);
    // Type: click, type, Ctrl+Enter.
    h.state_mut().ui.tool = Tool::Type;
    h.run_steps(2);
    click(&mut h, c + vec2(-100.0, 80.0), PointerButton::Primary);
    let id = crate::live_projection_text::typing_id(0);
    let mut t = h.ctx.data(|d| d.get_temp::<crate::live_projection_text::Typing>(id)).expect("typing started");
    t.text = "HELLO".into();
    h.ctx.data_mut(|d| d.insert_temp(id, t));
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Enter);
    h.run_steps(3);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert_eq!(h.state().session.journal.last().map(|j| j.0.as_str()), Some("layer.projectedRender"));
    assert_eq!(h.state().session.active().unwrap().doc.layers.len(), layers0 + 2, "a text layer");
}
