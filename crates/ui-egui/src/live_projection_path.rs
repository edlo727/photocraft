//! The Pen tool family in View › Equirectangular Live Projection: Pen, Freeform Pen and
//! Curvature Pen draw in view space and store the path in equirect pixels (`path.projectedSet`);
//! the work path is drawn back through the view; Path Selection turns it across the sphere,
//! Direct Selection drags anchors and handles, and the anchor tools add, delete and convert
//! points. Make Selection and Fill Path count the path across the seam (`wrap`), Stroke Path
//! strokes it with the brush in the view (`paint.projectedStroke`).

use egui::{Color32, PointerButton, Pos2, Rect, Stroke, vec2};
use photocraft_engine::equirect::{ViewProjection, equirect_to_dir, rotation_between};
use photocraft_engine::projected_path::{rotate_path, view_polylines, view_to_equirect_near};
use serde_json::{Value, json};

use crate::live_projection::ProjectionView;
use crate::{PhotocraftApp, Tool};

pub fn supported(t: Tool) -> bool {
    matches!(t, Tool::Pen | Tool::FreeformPen | Tool::CurvaturePen | Tool::AddAnchorPoint | Tool::DeleteAnchorPoint | Tool::ConvertPoint | Tool::PathSelection | Tool::DirectSelection)
}

/// A knot being drawn, in view px.
#[derive(Clone, Copy, Debug, PartialEq)]
struct VKnot {
    a: [f64; 2],
    i: [f64; 2],
    o: [f64; 2],
    smooth: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Draft {
    knots: Vec<VKnot>,
    /// Pen: the knot whose handle a drag pulls.
    pulling: bool,
    /// Freeform: the stroke so far.
    free: Vec<[f64; 2]>,
}

#[derive(Clone, Copy, Debug)]
enum Grab {
    /// Path Selection: the whole path, from this direction.
    Path([f64; 3]),
    Anchor(usize, usize),
    Handle(usize, usize, bool),
}

fn draft_id(idx: usize) -> egui::Id {
    egui::Id::new(("pc-live-projection-pen", idx))
}
fn grab_id(idx: usize) -> egui::Id {
    egui::Id::new(("pc-live-projection-path-grab", idx))
}

fn view_json(cam: &ProjectionView, vp: &ViewProjection) -> Value {
    json!({"yaw": cam.yaw, "pitch": cam.pitch, "fov": cam.fov, "width": vp.w, "height": vp.h})
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Catmull-Rom handles so the Curvature Pen's curve flows through its points.
fn curvature(points: &[[f64; 2]]) -> Vec<VKnot> {
    let n = points.len();
    (0..n)
        .map(|i| {
            let a = points[i];
            if n < 3 || i == 0 || i == n - 1 {
                return VKnot { a, i: a, o: a, smooth: n >= 3 };
            }
            let (p, q) = (points[i - 1], points[i + 1]);
            let t = [(q[0] - p[0]) / 6.0, (q[1] - p[1]) / 6.0];
            VKnot { a, i: [a[0] - t[0], a[1] - t[1]], o: [a[0] + t[0], a[1] + t[1]], smooth: true }
        })
        .collect()
}

fn commit(app: &mut PhotocraftApp, cam: &ProjectionView, vp: &ViewProjection, knots: &[VKnot], closed: bool) {
    if knots.len() < 2 {
        return;
    }
    let k: Vec<Value> = knots.iter().map(|k| json!({"anchor": k.a, "in": k.i, "out": k.o, "smooth": k.smooth})).collect();
    let mut p = json!({"path": {"subpaths": [{"closed": closed, "knots": k}]}, "projection": view_json(cam, vp)});
    if app.session.active().is_some_and(|d| d.doc.work_path.is_some()) {
        p["op"] = json!("combine");
    }
    if let Err(e) = app.run("path.projectedSet", p) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// The work path's anchors and handles in the view: `(subpath, knot, anchor, in, out)`.
fn view_knots(app: &PhotocraftApp, vp: &ViewProjection) -> Vec<(usize, usize, Option<[f64; 2]>, Option<[f64; 2]>, Option<[f64; 2]>)> {
    let Some(st) = app.session.active() else { return Vec::new() };
    let Some(path) = st.doc.work_path.as_ref() else { return Vec::new() };
    let (iw, ih) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let v = |p: photocraft_geom::Point| vp.dir_to_view(equirect_to_dir(p.x, p.y, iw, ih));
    let mut out = Vec::new();
    for (si, sp) in path.subpaths.iter().enumerate() {
        for (ki, k) in sp.knots.iter().enumerate() {
            out.push((si, ki, v(k.anchor), v(k.in_ctrl), v(k.out_ctrl)));
        }
    }
    out
}

/// Draw the work path (and the draft) over the view.
pub fn draw(app: &PhotocraftApp, ctx: &egui::Context, painter: &egui::Painter, rect: Rect, ppp: f32, vp: &ViewProjection, idx: usize, tool: Tool) {
    let to_screen = |q: [f64; 2]| -> Pos2 { rect.min + vec2(q[0] as f32, q[1] as f32) / ppp };
    let line = |pts: Vec<Pos2>, closed: bool| {
        for (w, c) in [(3.0, Color32::from_black_alpha(120)), (1.0, Color32::from_rgb(60, 160, 255))] {
            painter.add(if closed { egui::Shape::closed_line(pts.clone(), Stroke::new(w, c)) } else { egui::Shape::line(pts.clone(), Stroke::new(w, c)) });
        }
    };
    let live = live_path(ctx, idx);
    // As on the flat canvas (vector_ui): the stored path shows only while a path/shape tool is
    // active (or Direct Selection / the Pen with Ctrl), and View › Show › Target Path is on.
    let path_tool = matches!(tool, Tool::Pen | Tool::FreeformPen | Tool::CurvaturePen | Tool::PathSelection)
        || crate::vector_ui::is_shape_tool(tool)
        || crate::direct_select::shows(app, ctx.input(|i| i.modifiers));
    let show = live.is_some() || (path_tool && app.ui.view.shows(app.ui.view.show.target_path));
    if show
        && let Some(st) = app.session.documents().get(idx)
        && let Some(path) = live.as_ref().or(st.doc.work_path.as_ref())
    {
        let (iw, ih) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
        for (pts, closed) in view_polylines(path, vp, iw, ih) {
            line(pts.into_iter().map(to_screen).collect(), closed);
        }
        if supported(tool) {
            for (_, _, a, i, o) in view_knots(app, vp) {
                let Some(a) = a else { continue };
                if tool == Tool::DirectSelection || tool == Tool::ConvertPoint {
                    for h in [i, o].into_iter().flatten().filter(|h| dist(*h, a) > 0.5) {
                        painter.line_segment([to_screen(a), to_screen(h)], Stroke::new(1.0, Color32::from_rgb(60, 160, 255)));
                        painter.circle_filled(to_screen(h), 2.5, Color32::from_rgb(60, 160, 255));
                    }
                }
                let r = Rect::from_center_size(to_screen(a), vec2(5.0, 5.0));
                painter.rect_filled(r, 0.0, Color32::WHITE);
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_rgb(60, 160, 255)), egui::StrokeKind::Inside);
            }
        }
    }
    let Some(d) = ctx.data(|x| x.get_temp::<Draft>(draft_id(idx))) else { return };
    let ks = &d.knots;
    for w in ks.windows(2) {
        let pts: Vec<Pos2> = (0..=16)
            .map(|i| {
                let t = i as f64 / 16.0;
                let mt = 1.0 - t;
                let (a, b, c, e) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
                to_screen([a * w[0].a[0] + b * w[0].o[0] + c * w[1].i[0] + e * w[1].a[0], a * w[0].a[1] + b * w[0].o[1] + c * w[1].i[1] + e * w[1].a[1]])
            })
            .collect();
        line(pts, false);
    }
    for k in ks {
        painter.rect_filled(Rect::from_center_size(to_screen(k.a), vec2(5.0, 5.0)), 0.0, Color32::from_rgb(60, 160, 255));
    }
    if d.free.len() >= 2 {
        line(d.free.iter().map(|q| to_screen(*q)).collect(), false);
    }
}

/// Handle a Pen-family tool on the projection canvas.
#[allow(clippy::too_many_arguments)]
pub fn tool(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, rect: Rect, ppp: f32, cam: &ProjectionView, vp: &ViewProjection, idx: usize, tool: Tool) {
    let to_view = |p: Pos2| -> [f64; 2] { [f64::from((p.x - rect.min.x) * ppp), f64::from((p.y - rect.min.y) * ppp)] };
    let tol = f64::from(6.0 * ppp);
    if response.hovered() {
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let (enter, esc) = ctx.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
    let mut d: Draft = ctx.data(|x| x.get_temp(draft_id(idx))).unwrap_or_default();
    match tool {
        Tool::Pen | Tool::CurvaturePen => {
            if (enter || esc) && !d.knots.is_empty() {
                commit(app, cam, vp, &d.knots, false);
                ctx.data_mut(|x| x.remove::<Draft>(draft_id(idx)));
                return;
            }
            if (response.drag_started_by(PointerButton::Primary) || (response.clicked() && !d.pulling))
                && let Some(p) = ctx.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
            {
                let q = to_view(p);
                if d.knots.len() >= 2 && dist(q, d.knots[0].a) <= tol {
                    // Back on the first anchor: close and store.
                    let knots = if tool == Tool::CurvaturePen { curvature(&d.knots.iter().map(|k| k.a).collect::<Vec<_>>()) } else { d.knots.clone() };
                    commit(app, cam, vp, &knots, true);
                    ctx.data_mut(|x| x.remove::<Draft>(draft_id(idx)));
                    return;
                }
                d.knots.push(VKnot { a: q, i: q, o: q, smooth: false });
                if tool == Tool::CurvaturePen {
                    let pts: Vec<[f64; 2]> = d.knots.iter().map(|k| k.a).collect();
                    d.knots = curvature(&pts);
                }
                d.pulling = tool == Tool::Pen && response.drag_started_by(PointerButton::Primary);
            }
            if d.pulling
                && response.dragged()
                && let (Some(p), Some(k)) = (response.interact_pointer_pos(), d.knots.last_mut())
            {
                // Dragging out of a new anchor pulls symmetric handles: a smooth point.
                let q = to_view(p);
                *k = VKnot { a: k.a, o: q, i: [2.0 * k.a[0] - q[0], 2.0 * k.a[1] - q[1]], smooth: true };
            }
            if response.drag_stopped() {
                d.pulling = false;
            }
        }
        Tool::FreeformPen => {
            if let Some(p) = response.interact_pointer_pos()
                && (response.drag_started_by(PointerButton::Primary) || response.dragged())
            {
                let q = to_view(p);
                if d.free.last().is_none_or(|l| dist(*l, q) >= 6.0) {
                    d.free.push(q);
                }
            }
            if response.drag_stopped() {
                let knots: Vec<VKnot> = d.free.iter().map(|&a| VKnot { a, i: a, o: a, smooth: false }).collect();
                let closed = d.free.len() > 3 && dist(d.free[0], *d.free.last().unwrap()) <= tol;
                commit(app, cam, vp, &knots, closed);
                ctx.data_mut(|x| x.remove::<Draft>(draft_id(idx)));
                return;
            }
        }
        _ => {
            edit(app, ctx, response, cam, vp, idx, tool, to_view, tol);
            return;
        }
    }
    // Only repaint when the draft changed: egui already repaints on pointer movement, and a
    // repaint every frame re-renders the 20k projection continuously while a Pen is selected.
    let changed = ctx.data(|x| x.get_temp::<Draft>(draft_id(idx))) != Some(d.clone());
    ctx.data_mut(|x| x.insert_temp(draft_id(idx), d));
    if changed {
        ctx.request_repaint();
    }
}

/// Path Selection, Direct Selection and the anchor tools on the stored work path.
#[allow(clippy::too_many_arguments)]
fn edit(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, _cam: &ProjectionView, vp: &ViewProjection, idx: usize, tool: Tool, to_view: impl Fn(Pos2) -> [f64; 2], tol: f64) {
    let Some(st) = app.session.documents().get(idx) else { return };
    let (iw, ih) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let Some(path) = st.doc.work_path.clone() else { return };
    let knots = view_knots(app, vp);
    let near_anchor = |q: [f64; 2]| knots.iter().filter_map(|(s, k, a, ..)| a.map(|a| (dist(a, q), *s, *k))).filter(|(d, ..)| *d <= tol).min_by(|a, b| a.0.total_cmp(&b.0));
    let near_handle = |q: [f64; 2]| {
        knots
            .iter()
            .flat_map(|(s, k, a, i, o)| [(i, false), (o, true)].into_iter().filter_map(move |(h, out)| h.filter(|h| a.is_none_or(|a| dist(a, *h) > 0.5)).map(|h| (dist(h, q), *s, *k, out))))
            .filter(|(d, ..)| *d <= tol)
            .min_by(|a, b| a.0.total_cmp(&b.0))
    };
    let report = |app: &mut PhotocraftApp, r: Result<Value, String>| {
        if let Err(e) = r {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    };
    if response.clicked()
        && let Some(p) = response.interact_pointer_pos()
    {
        let q = to_view(p);
        match tool {
            Tool::DeleteAnchorPoint => {
                if let Some((_, s, k)) = near_anchor(q) {
                    let r = app.run("path.deleteAnchor", json!({"subpath": s, "knot": k}));
                    report(app, r);
                }
            }
            Tool::ConvertPoint => {
                if let Some((_, s, k)) = near_anchor(q) {
                    let r = app.run("path.convertPoint", json!({"subpath": s, "knot": k}));
                    report(app, r);
                }
            }
            Tool::AddAnchorPoint => {
                // The nearest point on a segment, by sampling it through the view.
                let mut best: Option<(f64, usize, usize, f64)> = None;
                for (si, sp) in path.subpaths.iter().enumerate() {
                    for (ki, s) in sp.segments().iter().enumerate() {
                        for i in 1..32 {
                            let t = i as f64 / 32.0;
                            let mt = 1.0 - t;
                            let (a, b, c, e) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
                            let (x, y) = (a * s[0].x + b * s[1].x + c * s[2].x + e * s[3].x, a * s[0].y + b * s[1].y + c * s[2].y + e * s[3].y);
                            if let Some(v) = vp.dir_to_view(equirect_to_dir(x, y, iw, ih)) {
                                let dd = dist(v, q);
                                if dd <= tol && best.is_none_or(|b| dd < b.0) {
                                    best = Some((dd, si, ki, t));
                                }
                            }
                        }
                    }
                }
                if let Some((_, s, k, t)) = best {
                    let r = app.run("path.addAnchor", json!({"subpath": s, "knot": k, "t": t}));
                    report(app, r);
                }
            }
            _ => {}
        }
        return;
    }
    if response.drag_started_by(PointerButton::Primary)
        && let Some(p) = ctx.input(|i| i.pointer.press_origin())
    {
        let q = to_view(p);
        let g = match tool {
            Tool::PathSelection => Some(Grab::Path(vp.view_to_dir(q[0], q[1]))),
            Tool::DirectSelection | Tool::ConvertPoint => near_handle(q).map(|(_, s, k, o)| Grab::Handle(s, k, o)).or_else(|| near_anchor(q).map(|(_, s, k)| Grab::Anchor(s, k))),
            _ => None,
        };
        if let Some(g) = g {
            ctx.data_mut(|x| x.insert_temp(grab_id(idx), (g, path.clone())));
        }
    }
    let Some((g, orig)) = ctx.data(|x| x.get_temp::<(Grab, photocraft_doc::Path)>(grab_id(idx))) else { return };
    let Some(p) = response.interact_pointer_pos() else { return };
    let q = to_view(p);
    // Each frame restates the drag from where it began, as one step at the release.
    let moved = match g {
        Grab::Path(from) => rotate_path(&orig, &rotation_between(from, vp.view_to_dir(q[0], q[1])), iw, ih),
        Grab::Anchor(s, k) => {
            let mut np = orig.clone();
            if let Some(kn) = np.subpaths.get_mut(s).and_then(|sp| sp.knots.get_mut(k)) {
                let to = view_to_equirect_near(vp, q, iw, ih, kn.anchor.x);
                let (dx, dy) = (to[0] - kn.anchor.x, to[1] - kn.anchor.y);
                for c in [&mut kn.anchor, &mut kn.in_ctrl, &mut kn.out_ctrl] {
                    c.x += dx;
                    c.y += dy;
                }
            }
            np
        }
        Grab::Handle(s, k, out) => {
            let mut np = orig.clone();
            if let Some(kn) = np.subpaths.get_mut(s).and_then(|sp| sp.knots.get_mut(k)) {
                let to = view_to_equirect_near(vp, q, iw, ih, kn.anchor.x);
                let to = photocraft_geom::Point::new(to[0], to[1]);
                // A smooth point keeps its other handle opposite (Convert Point breaks it).
                let other = if out { kn.in_ctrl } else { kn.out_ctrl };
                let len = (other.x - kn.anchor.x).hypot(other.y - kn.anchor.y);
                let dl = (to.x - kn.anchor.x).hypot(to.y - kn.anchor.y).max(1e-9);
                let mirrored = photocraft_geom::Point::new(kn.anchor.x - (to.x - kn.anchor.x) / dl * len, kn.anchor.y - (to.y - kn.anchor.y) / dl * len);
                let keep = kn.smooth && tool == Tool::DirectSelection;
                if out {
                    kn.out_ctrl = to;
                    if keep {
                        kn.in_ctrl = mirrored;
                    }
                } else {
                    kn.in_ctrl = to;
                    if keep {
                        kn.out_ctrl = mirrored;
                    }
                }
                if tool == Tool::ConvertPoint {
                    kn.smooth = false;
                }
            }
            np
        }
    };
    if response.drag_stopped() {
        ctx.data_mut(|x| x.remove::<(Grab, photocraft_doc::Path)>(grab_id(idx)));
        let label_cmd = json!({"path": photocraft_engine::vector_cmds::path_json(&moved)});
        let r = app.run("path.set", label_cmd);
        report(app, r);
    } else if response.dragged() {
        // Live: shown by replacing the work path for drawing only.
        ctx.data_mut(|x| x.insert_temp(egui::Id::new(("pc-live-projection-path-live", idx)), moved));
        ctx.request_repaint();
    }
}

/// The path being dragged (drawn instead of the stored one), if any.
pub fn live_path(ctx: &egui::Context, idx: usize) -> Option<photocraft_doc::Path> {
    let grabbing = ctx.data(|x| x.get_temp::<(Grab, photocraft_doc::Path)>(grab_id(idx))).is_some();
    if !grabbing {
        ctx.data_mut(|x| x.remove::<photocraft_doc::Path>(egui::Id::new(("pc-live-projection-path-live", idx))));
        return None;
    }
    ctx.data(|x| x.get_temp(egui::Id::new(("pc-live-projection-path-live", idx))))
}

/// Make Selection, Fill Path and Stroke Path while the active view is in projection.
pub fn intercept_run(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !matches!(id, "path.toSelection" | "path.fill" | "path.stroke") || params.get("wrap").is_some() {
        return None;
    }
    let idx = app.session.active_index()?;
    let cam = app.ui.views.get(idx)?.projection?;
    let (w, h) = app.projection_view_px.filter(|v| v.0 == idx).map(|v| (v.1, v.2))?;
    let mut p = params.clone();
    if id != "path.stroke" {
        p["wrap"] = json!(true);
        return Some(app.run(id, p));
    }
    // Stroke Path: the brush along the path as seen in the view.
    let vp = ViewProjection::new(cam.camera(), w, h);
    let st = app.session.active()?;
    let (iw, ih) = (f64::from(st.doc.size.width), f64::from(st.doc.size.height));
    let path = match params.get("name").and_then(Value::as_str) {
        Some(n) if n != "work" => st.doc.paths.iter().find(|q| q.name == n).map(|q| q.path.clone()),
        _ => st.doc.work_path.clone(),
    }?;
    let mut last = Ok(Value::Null);
    for (pts, closed) in view_polylines(&path, &vp, iw, ih) {
        let mut pts: Vec<[f64; 3]> = pts.iter().map(|q| [q[0], q[1], 1.0]).collect();
        if closed && let Some(f) = pts.first().copied() {
            pts.push(f);
        }
        let q = json!({"erase": false, "zoom": 1.0, "target": crate::canvas::paint_target(app), "points": pts, "projection": {"yaw": cam.yaw, "pitch": cam.pitch, "fov": cam.fov, "width": w, "height": h}});
        last = app.run("paint.projectedStroke", q);
        if last.is_err() {
            break;
        }
    }
    Some(last)
}
