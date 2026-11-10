use super::{
    Color32, CurveUi, Edit, EditorCx, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Tokens, ToneSpace, Value, adjust_params, curve_edit, json, label, pos2,
    space_of, tone, vec2, widgets,
};

// ---------------------------------------------------------------------------------------------
// Channels of Levels / Curves

/// A channel of a tone editor: the params key (None = the top-level / composite keys for Levels;
/// Curves uses "points"), its label, and its colour name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneChannel {
    pub key: &'static str,
    pub label: &'static str,
}

/// The channels a tone editor lists for `space` (histogram index = position).
pub fn tone_channels(space: ToneSpace, gray: bool) -> &'static [ToneChannel] {
    const RGB: [ToneChannel; 4] = [
        ToneChannel { key: "", label: "RGB" },
        ToneChannel { key: "red", label: "Red" },
        ToneChannel { key: "green", label: "Green" },
        ToneChannel { key: "blue", label: "Blue" },
    ];
    const GRAY: [ToneChannel; 1] = [ToneChannel { key: "", label: "Gray" }];
    const CMYK: [ToneChannel; 5] = [
        ToneChannel { key: "", label: "CMYK" },
        ToneChannel { key: "cyan", label: "Cyan" },
        ToneChannel { key: "magenta", label: "Magenta" },
        ToneChannel { key: "yellow", label: "Yellow" },
        ToneChannel { key: "black", label: "Black" },
    ];
    const LAB: [ToneChannel; 3] =
        [ToneChannel { key: "lightness", label: "Lightness" }, ToneChannel { key: "a", label: "a" }, ToneChannel { key: "b", label: "b" }];
    match space {
        ToneSpace::Rgb if gray => &GRAY,
        ToneSpace::Rgb => &RGB,
        ToneSpace::Cmyk => &CMYK,
        ToneSpace::Lab => &LAB,
    }
}

/// Visible tone records and their histogram indices. A partial selection has a neutral combined
/// entry followed by the selected native components; Properties keeps the complete editor.
pub(super) fn editor_channels(cx: &EditorCx, space: ToneSpace) -> Vec<(ToneChannel, String, usize)> {
    let channels = tone_channels(space, cx.gray);
    if let Some(iso) = &cx.isolated_curve {
        if let Some(colors) = iso.components {
            let keys = adjust_params::channel_keys(space);
            let mut result = vec![(ToneChannel { key: "", label: "" }, iso.name.clone(), 0)];
            for k in colors.indices() {
                if let Some(key) = keys.get(k) {
                    // HistSource::Colors reserves slot 0 for the combined set, including Lab.
                    let histogram = k + 1;
                    let channel = channels.iter().find(|c| c.key == *key).copied().unwrap_or(ToneChannel { key, label: key });
                    result.push((channel, tl!(channel.label).to_owned(), histogram));
                }
            }
            return result;
        }
        return vec![(ToneChannel { key: iso.key, label: "" }, if iso.translate { tl!(&iso.name).to_owned() } else { iso.name.clone() }, iso.hist)];
    }
    channels.iter().enumerate().map(|(i, c)| (*c, tl!(c.label).to_owned(), i)).collect()
}

pub(super) fn editor_channel_picker(ui: &mut egui::Ui, id: egui::Id, channels: &[(ToneChannel, String, usize)]) -> usize {
    let options: Vec<_> = channels.iter().enumerate().map(|(i, (_, name, _))| (i, name.as_str())).collect();
    channel_picker_opts(ui, id, &options, tl!("Channel:"), false)
}

/// The popup, with `translate` for whether the built-in labels follow the interface language;
/// a user-typed channel name is shown exactly as entered (#5335).
fn channel_picker_opts(ui: &mut egui::Ui, id: egui::Id, opts: &[(usize, &str)], text: &str, translate: bool) -> usize {
    let mut ch: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0);
    if ch >= opts.len() {
        ch = 0;
    }
    ui.horizontal(|ui| {
        label(ui, text);
        let width = if opts.len() == 1 { 160.0 } else { 110.0 };
        let key = format!("{id:?}-ch");
        if translate {
            widgets::dropdown(ui, &key, text, &mut ch, opts, width);
        } else {
            widgets::dropdown_literal(ui, &key, text, &mut ch, opts, width);
        }
    });
    ui.data_mut(|d| d.insert_temp(id, ch));
    ch
}

pub(super) fn hist_color(ch: &ToneChannel, t: &Tokens) -> Color32 {
    match ch.key {
        "" | "lightness" | "black" => Color32::from_gray(if t.pro { 150 } else { 110 }),
        k => tone::channel_color(k, t),
    }
}

pub(super) fn bar_color(ch: &ToneChannel, t: &Tokens) -> Color32 {
    match ch.key {
        "" | "lightness" | "black" => Color32::WHITE,
        k => tone::channel_color(k, t),
    }
}

/// Horizontal (or vertical) black → `to` gradient bar.
pub(super) fn gradient_bar(p: &egui::Painter, r: Rect, from: Color32, to: Color32, vertical: bool) {
    let mut mesh = egui::Mesh::default();
    let (a, b, c, d) = if vertical {
        (r.left_bottom(), r.right_bottom(), r.right_top(), r.left_top())
    } else {
        (r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom())
    };
    let (ca, cb) = if vertical { (from, from) } else { (from, to) };
    let (cc, cd) = if vertical { (to, to) } else { (to, from) };
    mesh.colored_vertex(a, ca);
    mesh.colored_vertex(b, cb);
    mesh.colored_vertex(c, cc);
    mesh.colored_vertex(d, cd);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(mesh);
}

// ---------------------------------------------------------------------------------------------
// Curves

pub(super) const CURVE_HANDLE_SIZE: f32 = 7.0;

const CURVE_GRAPH_PADDING: f32 = 5.0;

const CURVE_SAMPLES: usize = 256;

/// One entry per editor/channel, independent of selection, tint and stroke width.
#[derive(Clone, Default)]
struct CurvePolyline {
    points: Vec<[f32; 2]>,
    graph: Option<Rect>,
    /// Drawn as a Nuke ColorLookup curve (unclamped): Some(hold ends), None for Photoshop's.
    lookup: Option<bool>,
    line: Vec<Pos2>,
}

impl CurvePolyline {
    /// Returns whether the screen geometry changed. Moving the graph also invalidates screen
    /// coordinates (for example when scrolling the Properties panel or moving the dialog).
    fn update(&mut self, points: &[[f32; 2]], graph: Rect, lookup: Option<bool>) -> bool {
        if self.points == points && self.graph == Some(graph) && self.lookup == lookup {
            return false;
        }
        self.lookup = lookup;
        let cp: Vec<photocraft_doc::adjust::CurvePoint> =
            points.iter().map(|q| photocraft_doc::adjust::CurvePoint { input: q[0] / 255.0, output: q[1] / 255.0 }).collect();
        // Graph units (0..1 across the graph): the spline is the same at any uniform scale.
        self.line = photocraft_compose::adjust::spline_samples(&cp, CURVE_SAMPLES, 1.0, lookup.is_none(), lookup == Some(false))
            .into_iter()
            .enumerate()
            .map(|(i, y)| pos2(graph.left() + i as f32 / (CURVE_SAMPLES - 1) as f32 * graph.width(), graph.bottom() - y * graph.height()))
            .collect();
        self.points.clear();
        self.points.extend_from_slice(points);
        self.graph = Some(graph);
        true
    }
}

fn cached_curve_line(ctx: &egui::Context, id: egui::Id, points: &[[f32; 2]], graph: Rect, lookup: Option<bool>) -> Vec<Pos2> {
    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<CurvePolyline>(id);
        cache.update(points, graph, lookup);
        // egui owns the submitted path. Only this small copy is needed on a cache hit;
        // normalization, spline solving, sampling and screen mapping are all skipped.
        cache.line.clone()
    })
}

/// The room left on screen for the Curves dialog's other rows (title, channel and mode, Input and
/// Output, eyedroppers, buttons) when its graph is as tall as it can be.
const CURVES_CHROME: f32 = 360.0;

/// The smallest and the default largest side of the Curves graph: 300 points was the fixed
/// size until #8984.
const CURVE_SIDE: (f32, f32) = (120.0, 300.0);

/// The Curves graph's side (#8984): the width it is given, in the Properties panel and in the
/// resizable dialog, and no taller than the window leaves room for beside the other rows.
fn curve_side(ui: &egui::Ui) -> f32 {
    let tallest = curve_side_max(ui.ctx());
    let width = ui.available_width();
    if width.is_finite() { width.clamp(CURVE_SIDE.0, tallest) } else { CURVE_SIDE.1 }
}

/// The largest the Curves graph gets in this window: as tall as the window leaves room for beside
/// the dialog's other rows, and never under the old 300 points. The Curves dialog is no wider.
pub(crate) fn curve_side_max(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() - CURVES_CHROME).max(CURVE_SIDE.1)
}

pub(super) fn curve_graph(full: Rect, side: f32) -> Rect {
    let base = Rect::from_min_size(full.min + vec2(14.0, 0.0), vec2(side - 14.0, side - 14.0));
    Rect::from_min_max(base.min + vec2(CURVE_GRAPH_PADDING, CURVE_GRAPH_PADDING), base.max - vec2(CURVE_GRAPH_PADDING, CURVE_GRAPH_PADDING))
}

pub(crate) fn read_curve(v: &Value, key: &str) -> Vec<[f32; 2]> {
    let pts: Vec<[f32; 2]> = v
        .get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|q| {
                    let q = q.as_array()?;
                    Some([q.first()?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])
                })
                .collect()
        })
        .unwrap_or_default();
    if pts.len() < 2 { vec![[0.0, 0.0], [255.0, 255.0]] } else { pts }
}

fn curve_key(ch: &ToneChannel) -> &'static str {
    if ch.key.is_empty() { "points" } else { ch.key }
}

/// A pencil-drawn channel: its 256 output levels (identity when absent or not a map).
fn read_map(v: &Value, key: &str) -> Vec<f32> {
    let map: Option<Vec<f32>> = v
        .get(key)
        .and_then(Value::as_array)
        .filter(|a| a.len() == photocraft_doc::adjust::CURVE_MAP_LEN)
        .and_then(|a| a.iter().map(|o| o.as_f64().map(|o| o as f32)).collect());
    map.unwrap_or_else(|| (0..photocraft_doc::adjust::CURVE_MAP_LEN).map(|k| k as f32).collect())
}

fn write_map(v: &mut Value, key: &str, map: &[f32]) {
    v[key] = json!(map.iter().map(|o| (o * 100.0).round() / 100.0).collect::<Vec<_>>());
}

/// Switches the Curves values between point editing and the pencil: the engine draws the point
/// curves into maps, or fits points to the drawn maps (Photoshop's two curve buttons).
fn set_pencil(v: &mut Value, pencil: bool, gray: bool) -> bool {
    let mut p = v.clone();
    p["pencil"] = json!(pencil);
    let mode = if gray { photocraft_doc::ColorMode::Grayscale } else { photocraft_doc::ColorMode::Rgb };
    let Ok(a) = adjust_params::from_params("curves", &p, None, mode) else { return false };
    *v = adjust_params::to_params(&a);
    // Explicit, so an update over a pencil-drawn layer leaves the pencil.
    v["pencil"] = json!(pencil);
    true
}

/// Partial sets have a combined record even in Lab. Convert each displayed curve separately,
/// so the normal Lab serializer (which aliases the composite to Lightness) cannot discard it.
fn set_editor_pencil(v: &mut Value, pencil: bool, cx: &EditorCx) -> bool {
    if cx.isolated_curve.as_ref().is_none_or(|c| c.components.is_none()) {
        return set_pencil(v, pencil, cx.gray);
    }
    let mut next = v.clone();
    for (channel, _, _) in editor_channels(cx, space_of(v)) {
        let key = curve_key(&channel);
        let mut scalar = json!({"pencil":v.get("pencil").and_then(Value::as_bool).unwrap_or(false)});
        if let Some(curve) = v.get(key) {
            scalar["points"] = curve.clone();
        }
        if !set_pencil(&mut scalar, pencil, true) {
            return false;
        }
        if let Some(curve) = scalar.get("points") {
            next[key] = curve.clone();
        }
    }
    next["pencil"] = json!(pencil);
    *v = next;
    true
}

/// Mode buttons above the graph: points, pencil, and Smooth for the shown channel's drawing.
fn curves_mode_row(ui: &mut egui::Ui, v: &mut Value, key: &str, cx: &EditorCx) -> Edit {
    let pencil = v.get("pencil").and_then(Value::as_bool).unwrap_or(false);
    let mut e = Edit::default();
    ui.horizontal(|ui| {
        if crate::icons::button(ui, "spline", 24.0, !pencil, tl!("Edit points to modify the curve")).clicked() && pencil {
            e.add(Edit::discrete(set_editor_pencil(v, false, cx)));
        }
        if crate::icons::button(ui, "pencil", 24.0, pencil, tl!("Draw to modify the curve")).clicked() && !pencil {
            e.add(Edit::discrete(set_editor_pencil(v, true, cx)));
        }
        let label = crate::i18n::tr_ctx(crate::i18n::current(), "curves", "Smooth");
        let smooth = ui
            .add_enabled_ui(pencil, |ui| widgets::secondary_button(ui, label, 0.0))
            .inner
            .on_hover_text(tl!("Smooth the curve values"))
            .on_disabled_hover_text(tl!("Smooth the curve values"));
        if smooth.clicked() {
            let mut map = read_map(v, key);
            adjust_params::smooth_curve_map(&mut map);
            write_map(v, key, &map);
            e.add(Edit::discrete(true));
        }
        // Nuke ColorLookup settings (range, ends, order) open on demand, so Photoshop's Curves
        // keep their layout; a curve already using them shows them anyway.
        if !pencil && space_of(v) == ToneSpace::Rgb && cx.isolated_curve.is_none() {
            let open_id = cx.mem.with("curves-lookup-open");
            let mut open = ui.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
            if ui.selectable_label(open, "Range…").on_hover_text("Nuke ColorLookup: range, held ends and channel order").clicked() {
                open = !open;
                ui.data_mut(|d| d.insert_temp(open_id, open));
            }
        }
    });
    e
}

/// The Nuke ColorLookup settings of a Curves adjustment as its params hold them: (range, hold
/// ends, master first), Photoshop's (1, false, false) when absent.
fn lookup_of(v: &Value) -> (f32, bool, bool) {
    let range = v.get("range").and_then(Value::as_f64).map_or(1.0, |r| r as f32);
    let range = if range.is_finite() { range.clamp(1.0, photocraft_doc::adjust::CurveLookup::MAX_RANGE) } else { 1.0 };
    let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    (range, flag("holdEnds"), flag("masterFirst"))
}

/// Range, ends and channel order (Nuke's ColorLookup) for point curves in RGB.
fn lookup_row(ui: &mut egui::Ui, v: &mut Value) -> Edit {
    let (mut range, mut hold, mut master_first) = lookup_of(v);
    let mut e = Edit::default();
    ui.horizontal(|ui| {
        ui.label("Range:");
        let opts: Vec<(f32, &str)> =
            vec![(1.0, "0–1"), (2.0, "0–2"), (4.0, "0–4"), (8.0, "0–8"), (16.0, "0–16"), (32.0, "0–32"), (64.0, "0–64")];
        let old = range;
        if widgets::dropdown_literal(ui, "curves-lookup-range", "Range", &mut range, &opts, 70.0) {
            // Straight curves follow the range (the engine does the same); edited curves keep
            // their values, and the graph frames them (see `CurveView`).
            for key in ["points", "red", "green", "blue"] {
                let pts = read_curve(v, key);
                let top = 255.0 * old;
                if pts.len() == 2 && pts[0] == [0.0, 0.0] && (pts[1][0] - top).abs() < 0.01 && (pts[1][1] - top).abs() < 0.01 {
                    v[key] = json!([[0.0, 0.0], [255.0 * range, 255.0 * range]]);
                }
            }
            v["range"] = json!(range);
            e.add(Edit::discrete(true));
        }
        if ui.checkbox(&mut hold, "Hold ends").on_hover_text("Past the first and last points, hold their values (off: continue along the end slopes)").changed() {
            v["holdEnds"] = json!(hold);
            e.add(Edit::discrete(true));
        }
        if ui.checkbox(&mut master_first, "Master first").on_hover_text("Apply the RGB curve before the channel curves, as Nuke's ColorLookup (off: after, as Photoshop)").changed() {
            v["masterFirst"] = json!(master_first);
            e.add(Edit::discrete(true));
        }
    });
    e
}

/// The part of a lookup range the Curves graph shows, in linear values (Nuke's curve editor
/// zoom): input `x0..x0 + sx`, output `y0..y0 + sy`. Display only; the curve is unchanged.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CurveView {
    range: f32,
    x0: f32,
    y0: f32,
    sx: f32,
    sy: f32,
}

impl CurveView {
    fn full(range: f32) -> Self {
        CurveView { range, x0: 0.0, y0: 0.0, sx: range, sy: range }
    }

    /// Frames the points (levels: 255 = 1.0) and the origin, with a margin.
    fn fit(range: f32, pts: &[[f32; 2]]) -> Self {
        let mut lo = [0.0f32; 2];
        let mut hi = [f32::MIN; 2];
        for q in pts.iter().filter(|q| q.iter().all(|x| x.is_finite())) {
            for a in 0..2 {
                lo[a] = lo[a].min(q[a] / 255.0);
                hi[a] = hi[a].max(q[a] / 255.0);
            }
        }
        if hi[0] <= lo[0] || hi[1] <= lo[1] {
            return Self::full(range);
        }
        let frame = |a: usize| {
            let span = (hi[a] - lo[a]).max(0.05);
            let m = span * 0.05;
            (lo[a] - m, span + 2.0 * m)
        };
        let ((x0, sx), (y0, sy)) = (frame(0), frame(1));
        CurveView { range, x0, y0, sx, sy }
    }

    fn load(a: [f32; 5]) -> Self {
        CurveView { range: a[0], x0: a[1], y0: a[2], sx: a[3], sy: a[4] }
    }

    fn store(&self) -> [f32; 5] {
        [self.range, self.x0, self.y0, self.sx, self.sy]
    }

    /// Levels (255 = 1.0) → graph units (0..255 across the shown window).
    fn to_g(self, q: [f32; 2]) -> [f32; 2] {
        [(q[0] / 255.0 - self.x0) / self.sx * 255.0, (q[1] / 255.0 - self.y0) / self.sy * 255.0]
    }

    /// Graph units → levels.
    fn levels_from_g(self, g: [f32; 2]) -> [f32; 2] {
        [(self.x0 + g[0] / 255.0 * self.sx) * 255.0, (self.y0 + g[1] / 255.0 * self.sy) * 255.0]
    }

    /// Zooms by `factor` (< 1: in) about `at`, a point of the graph as fractions (0..1, y up).
    fn zoom(&mut self, factor: f32, at: [f32; 2]) {
        let (min, max) = ((self.range / 4000.0).max(0.001), self.range * 4.0);
        let (ax, ay) = (self.x0 + at[0] * self.sx, self.y0 + at[1] * self.sy);
        self.sx = (self.sx * factor).clamp(min, max);
        self.sy = (self.sy * factor).clamp(min, max);
        self.x0 = ax - at[0] * self.sx;
        self.y0 = ay - at[1] * self.sy;
    }

    /// Pans by a screen drag of `d` graph fractions (y down).
    fn pan(&mut self, d: [f32; 2]) {
        self.x0 -= d[0] * self.sx;
        self.y0 += d[1] * self.sy;
    }
}

/// A round grid step showing about four lines over `span` (1, 2 or 5 × a power of ten).
fn grid_step(span: f32) -> f32 {
    let raw = (span / 4.0).max(1e-6);
    let p = 10f32.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * p).find(|s| *s >= raw).unwrap_or(10.0 * p)
}

/// Nuke-style navigation for a lookup curve: the wheel zooms about the pointer and Alt- or
/// middle-drag pans. Returns whether this drag is a pan, which must not move any point.
fn zoom_and_pan(ui: &mut egui::Ui, resp: &egui::Response, graph: Rect, view: &mut CurveView, view_id: egui::Id) -> bool {
    if resp.hovered()
        && let Some(pos) = resp.hover_pos()
    {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let at = [(pos.x - graph.left()) / graph.width().max(1.0), (graph.bottom() - pos.y) / graph.height().max(1.0)];
            view.zoom((-scroll / 300.0).exp(), at);
            // The wheel zooms the graph, not the panel around it.
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
    let alt = ui.input(|i| i.modifiers.alt);
    if resp.dragged_by(egui::PointerButton::Middle) || (alt && resp.dragged_by(egui::PointerButton::Primary)) {
        let d = resp.drag_delta();
        view.pan([d.x / graph.width().max(1.0), d.y / graph.height().max(1.0)]);
    }
    ui.data_mut(|d| d.insert_temp(view_id, view.store()));
    alt || resp.dragged_by(egui::PointerButton::Middle)
}

/// The Pencil's freehand stroke: it draws while the button is down and commits on release, and
/// Shift carries on from the last stroke's end in a straight line. Returns whether the map
/// changed, and whether the stroke has just ended (which commits the edit).
fn pencil_stroke(ui: &mut egui::Ui, resp: &egui::Response, graph: Rect, mem: egui::Id, map: &mut [f32]) -> (bool, bool) {
    let stroke_id = mem.with("curves-pencil");
    let (mut active, mut last): (bool, Option<[f32; 2]>) = ui.data(|d| d.get_temp(stroke_id)).unwrap_or_default();
    let (mut drawn, mut commit) = (false, false);
    if resp.is_pointer_button_down_on()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let q = [
            ((pos.x - graph.left()) / graph.width().max(1.0) * 255.0).clamp(0.0, 255.0),
            ((graph.bottom() - pos.y) / graph.height().max(1.0) * 255.0).clamp(0.0, 255.0),
        ];
        let from = if active || ui.input(|i| i.modifiers.shift) { last.unwrap_or(q) } else { q };
        drawn = curve_edit::pencil_line(map, from, q);
        active = true;
        last = Some(q);
    } else if active {
        active = false;
        commit = true;
    }
    ui.data_mut(|d| d.insert_temp(stroke_id, (active, last)));
    (drawn, commit)
}

pub(super) fn curves(ui: &mut egui::Ui, v: &mut Value, cx: &EditorCx) -> Edit {
    let t = Tokens::get(ui.ctx());
    let space = space_of(v);
    let channels = editor_channels(cx, space);
    let state_id = cx.mem.with("curves");
    let mut st: CurveUi = ui.data(|d| d.get_temp(state_id)).unwrap_or_default();
    let isolated = cx.isolated_curve.as_ref();
    let ch = editor_channel_picker(ui, cx.mem.with("curves-ch"), &channels);
    if ch != st.channel {
        st = CurveUi { channel: ch, ..Default::default() };
    }
    let (chan, _, hist_i) = channels.get(ch).cloned().unwrap_or((ToneChannel { key: "", label: "" }, String::new(), 0));
    let overlay_others = isolated.is_none();
    let tinted = isolated.is_some_and(|c| c.tint) || (isolated.is_none_or(|c| c.components.is_some()) && ch != 0);
    let key = curve_key(&chan);
    let mut e = curves_mode_row(ui, v, key, cx);
    let pencil = v.get("pencil").and_then(Value::as_bool).unwrap_or(false);
    // Nuke ColorLookup settings: point curves in RGB only (pencil maps stay 0..1).
    let lookup_ok = !pencil && space == ToneSpace::Rgb && isolated.is_none();
    let lookup_open = ui.data(|d| d.get_temp::<bool>(cx.mem.with("curves-lookup-open"))).unwrap_or(false);
    if lookup_ok && (lookup_open || lookup_of(v) != (1.0, false, false)) {
        e.add(lookup_row(ui, v));
    }
    let settings = if lookup_ok { lookup_of(v) } else { (1.0, false, false) };
    let (range, lookup) = (settings.0, settings != (1.0, false, false));
    // The shown window (lookup curves zoom and pan like Nuke's curve editor; Photoshop's
    // always show 0..1). A new range frames the curve's points.
    let levels = read_curve(v, key);
    let view_id = cx.mem.with("curves-view");
    let mut view = if lookup {
        match ui.data(|d| d.get_temp::<[f32; 5]>(view_id)).map(CurveView::load) {
            Some(view) if view.range == range => view,
            _ => CurveView::fit(range, &levels),
        }
    } else {
        CurveView::full(1.0)
    };
    if lookup {
        ui.horizontal(|ui| {
            ui.label("View:");
            if ui.small_button("Fit").on_hover_text("Frame the curve's points").clicked() {
                view = CurveView::fit(range, &levels);
            }
            if ui.small_button("Full").on_hover_text("Show the whole range").clicked() {
                view = CurveView::full(range);
            }
            let t = Tokens::get(ui.ctx());
            ui.label(RichText::new("wheel: zoom · Alt- or middle-drag: pan").color(t.text_faint).size(11.0));
        });
    }
    let mut map = read_map(v, key);
    let side = curve_side(ui);
    ui.add_space(4.0);
    let (full, resp) = ui.allocate_exact_size(vec2(side, side + 14.0), Sense::click_and_drag());
    let graph = curve_graph(full, side);
    // Zoom (wheel, about the pointer) and pan (Alt or middle drag) before the points are placed.
    let panning = lookup && zoom_and_pan(ui, &resp, graph, &mut view, view_id);
    // Points in graph units (0..255 across the shown window); the params hold levels (255 = 1.0).
    let mut pts: Vec<[f32; 2]> = levels.iter().map(|q| view.to_g(*q)).collect();
    ui.data_mut(|d| d.insert_temp(cx.mem.with("curves-graph"), graph));
    let to_scr = |q: [f32; 2]| pos2(graph.left() + q[0] / 255.0 * graph.width(), graph.bottom() - q[1] / 255.0 * graph.height());
    let mut changed = false;
    let mut drawn = false;
    if pencil {
        let (stroked, commit) = pencil_stroke(ui, &resp, graph, cx.mem, &mut map);
        drawn = stroked;
        e.commit |= commit;
    } else if !panning {
        let interaction = crate::point_curve::interact(ui, &resp, graph, &mut pts, &mut st.gesture);
        changed = interaction.changed;
        e.commit |= interaction.commit;
    }

    // Draw: histogram, quarter grid, baseline, gradient bars, other channels, the curve, points.
    let p = ui.painter_at(full.expand(2.0));
    p.rect_filled(graph, 0.0, t.field);
    // Linear values → screen.
    let sx = |x: f32| graph.left() + (x - view.x0) / view.sx * graph.width();
    let sy = |y: f32| graph.bottom() - (y - view.y0) / view.sy * graph.height();
    // Lookup curves are drawn clipped to the graph (with room for handles on its edges).
    let pg = if lookup { ui.painter_at(graph.expand(CURVE_HANDLE_SIZE)) } else { p.clone() };
    if let Some(h) = &cx.hist {
        // The histogram covers 0..1.
        let hist_rect = Rect::from_min_max(pos2(sx(0.0), graph.top()), pos2(sx(1.0), graph.bottom())).intersect(graph);
        if hist_rect.width() > 0.0 {
            tone::draw_histogram(&p, hist_rect, &h[hist_i], Color32::from_gray(if t.pro { 88 } else { 190 }).gamma_multiply(0.75));
        }
    }
    if lookup {
        // A grid at round values for the zoom, labelled along the bottom and left.
        let g = Stroke::new(1.0, t.separator);
        let label = |v: f32, step: f32| if step >= 1.0 { format!("{v:.0}") } else { format!("{:.*}", (-step.log10()).ceil() as usize, v) };
        let font = egui::FontId::proportional(9.0);
        let step = grid_step(view.sx);
        let mut k = (view.x0 / step).ceil();
        while k * step <= view.x0 + view.sx && k.is_finite() {
            let x = sx(k * step);
            pg.line_segment([pos2(x, graph.top()), pos2(x, graph.bottom())], g);
            pg.text(pos2(x + 2.0, graph.bottom() - 1.0), egui::Align2::LEFT_BOTTOM, label(k * step, step), font.clone(), t.text_faint);
            k += 1.0;
        }
        let step = grid_step(view.sy);
        let mut k = (view.y0 / step).ceil();
        while k * step <= view.y0 + view.sy && k.is_finite() {
            let y = sy(k * step);
            pg.line_segment([pos2(graph.left(), y), pos2(graph.right(), y)], g);
            pg.text(pos2(graph.left() + 2.0, y - 1.0), egui::Align2::LEFT_BOTTOM, label(k * step, step), font.clone(), t.text_faint);
            k += 1.0;
        }
        // Display white (1.0).
        let w = Stroke::new(1.0, t.accent.gamma_multiply(0.6));
        pg.line_segment([pos2(sx(1.0), graph.top()), pos2(sx(1.0), graph.bottom())], w);
        pg.line_segment([pos2(graph.left(), sy(1.0)), pos2(graph.right(), sy(1.0))], w);
        // The unchanged line y = x across the window.
        let (a, b) = (view.x0.max(view.y0), (view.x0 + view.sx).min(view.y0 + view.sy));
        if b > a {
            pg.line_segment([pos2(sx(a), sy(a)), pos2(sx(b), sy(b))], Stroke::new(1.0, t.separator.gamma_multiply(1.6)));
        }
    } else {
        for i in 1..4 {
            let f = i as f32 / 4.0;
            let g = Stroke::new(1.0, t.separator);
            p.line_segment([pos2(graph.left() + f * graph.width(), graph.top()), pos2(graph.left() + f * graph.width(), graph.bottom())], g);
            p.line_segment([pos2(graph.left(), graph.top() + f * graph.height()), pos2(graph.right(), graph.top() + f * graph.height())], g);
        }
        p.line_segment([graph.left_bottom(), graph.right_top()], Stroke::new(1.0, t.separator.gamma_multiply(1.6)));
    }
    p.rect_stroke(
        graph,
        0.0,
        Stroke::new(if resp.has_focus() { 1.5 } else { 1.0 }, if resp.has_focus() { t.accent } else { t.field_border }),
        StrokeKind::Outside,
    );
    let hbar = Rect::from_min_max(pos2(graph.left(), graph.bottom() + 4.0), pos2(graph.right(), graph.bottom() + 12.0));
    let vbar = Rect::from_min_max(pos2(full.left(), graph.top()), pos2(full.left() + 8.0, graph.bottom()));
    gradient_bar(&p, hbar, Color32::BLACK, bar_color(&chan, &t), false);
    gradient_bar(&p, vbar, Color32::BLACK, bar_color(&chan, &t), true);
    let draw_curve = |key: &str, pts: &[[f32; 2]], color: Color32, width: f32| {
        let line = cached_curve_line(ui.ctx(), cx.mem.with(("curves-polyline", key)), pts, graph, lookup.then_some(settings.1));
        pg.add(egui::Shape::line(line, Stroke::new(width, color)));
    };
    let draw_map = |map: &[f32], color: Color32, width: f32| {
        let last = map.len().saturating_sub(1).max(1) as f32;
        let line: Vec<Pos2> = map.iter().enumerate().map(|(k, o)| to_scr([k as f32 / last * 255.0, *o])).collect();
        p.add(egui::Shape::line(line, Stroke::new(width, color)));
    };
    if overlay_others && ch == 0 {
        // Photoshop overlays the edited channel curves on the composite.
        for c in tone_channels(space, cx.gray).iter().skip(1) {
            if pencil {
                let m = read_map(v, c.key);
                if m.iter().enumerate().any(|(k, o)| (o - k as f32).abs() > 0.5) {
                    draw_map(&m, bar_color(c, &t).gamma_multiply(0.8), 1.0);
                }
                continue;
            }
            let lv = read_curve(v, c.key);
            let cp: Vec<[f32; 2]> = lv.iter().map(|q| view.to_g(*q)).collect();
            if lv.len() > 2 || lv.first().is_some_and(|q| q[1] != 0.0) || lv.last().is_some_and(|q| q[1] != 255.0 * range) {
                draw_curve(c.key, &cp, bar_color(c, &t).gamma_multiply(0.8), 1.0);
            }
        }
    }
    let color = if tinted { bar_color(&chan, &t) } else { t.text };
    if pencil {
        draw_map(&map, color, 1.5);
    } else {
        draw_curve(key, &pts, color, 1.5);
    }
    for (i, q) in pts.iter().enumerate().filter(|_| !pencil) {
        let r = Rect::from_center_size(to_scr(*q), vec2(CURVE_HANDLE_SIZE, CURVE_HANDLE_SIZE));
        if Some(i) == st.gesture.selected {
            pg.rect_filled(r, 0.0, t.text);
            pg.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::BLACK), StrokeKind::Outside);
        } else {
            pg.rect_filled(r, 0.0, t.field);
            pg.rect_stroke(r, 0.0, Stroke::new(1.0, t.text), StrokeKind::Inside);
        }
    }

    // Input / Output of the selected point.
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let i = st.gesture.selected.filter(|i| *i < pts.len() && !pencil);
        // Lookup curves show linear values (0..range); Photoshop's show levels (0..255).
        let shown = |g: [f32; 2]| if lookup { view.levels_from_g(g).map(|l| l / 255.0) } else { g };
        let (mut vi, mut vo) = i.map_or((0.0, 0.0), |i| {
            let s = shown(pts[i]);
            (s[0], s[1])
        });
        let field = |ui: &mut egui::Ui, x: &mut f32, name: &str| {
            ui.add_enabled_ui(i.is_some(), |ui| {
                let r = if lookup { widgets::value_field_prec(ui, x, 0.0..=range, "", 52.0, 3) } else { widgets::value_field(ui, x, 0.0..=255.0, "", 52.0) };
                r.accessible_name(name)
            })
            .inner
        };
        label(ui, tl!("Input:"));
        let ri = field(ui, &mut vi, tl!("Input:"));
        label(ui, tl!("Output:"));
        let ro = field(ui, &mut vo, tl!("Output:"));
        if let Some(i) = i
            && (ri.changed() || ro.changed())
        {
            let moved = if lookup {
                // Typed values are exact, inside the view or not; the input stays between the
                // neighbouring points'.
                let g = view.to_g([vi * 255.0, vo * 255.0]);
                let gap = 0.01;
                let lo = i.checked_sub(1).and_then(|j| pts.get(j)).map_or(f32::MIN, |q| q[0] + gap);
                let hi = i.checked_add(1).and_then(|j| pts.get(j)).map_or(f32::MAX, |q| q[0] - gap);
                let new = [g[0].clamp(lo, hi.max(lo)), g[1]];
                let moved = pts[i] != new;
                pts[i] = new;
                moved
            } else {
                curve_edit::move_to(&mut pts, i, [vi, vo])
            };
            if moved {
                changed = true;
                e.commit |= !(ri.dragged() || ro.dragged());
            }
        }
        if ri.drag_stopped() || ro.drag_stopped() {
            e.commit = true;
        }
    });
    let t2 = Tokens::get(ui.ctx());
    let hint = if pencil {
        crate::i18n::fmt(tl!("Drag to draw the curve · {key}-click draws a straight line"), &[("key", &crate::shortcuts::pretty("Shift"))])
    } else {
        crate::i18n::fmt(tl!("Click to add a point · drag off or {key}-click or Delete to remove"), &[("key", &crate::shortcuts::pretty("Cmd"))])
    };
    ui.label(RichText::new(hint).color(t2.text_faint).size(11.0));
    ui.data_mut(|d| d.insert_temp(state_id, st));
    if drawn {
        write_map(v, key, &map);
        if space == ToneSpace::Lab
            && isolated.is_none_or(|c| c.components.is_none())
            && let Some(o) = v.as_object_mut()
        {
            o.remove("points");
        }
        e.changed = true;
    }
    if changed && !pencil {
        // Back to levels (hundredths of a level, the params' precision; whole levels for
        // Photoshop's curves).
        let out: Vec<[f32; 2]> = if lookup {
            pts.iter().map(|q| view.levels_from_g(*q).map(|l| (l.clamp(0.0, 255.0 * range) * 100.0).round() / 100.0)).collect()
        } else {
            pts.iter().map(|q| [q[0].round(), q[1].round()]).collect()
        };
        v[key] = json!(out);
        if space == ToneSpace::Lab && isolated.is_none_or(|c| c.components.is_none()) {
            // Lab has no composite: "points" would alias lightness.
            if let Some(o) = v.as_object_mut() {
                o.remove("points");
            }
        }
        e.changed = true;
    }
    e
}

#[cfg(test)]
mod multi_channel_tests {
    use super::super::levels::read_levels;
    use super::super::{auto_levels, editor};
    use super::*;
    use crate::{PhotocraftApp, tone::HistSource};
    use photocraft_engine::{Session, channel_cmds};
    use std::sync::Arc;

    fn context(mode: &str) -> EditorCx {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":32,"height":24,"mode":mode})).unwrap();
        s.execute("channel.target", json!({"colors":[0,2]})).unwrap();
        EditorCx {
            mem: egui::Id::unique("subset-tone-test"),
            hist: None,
            gray: false,
            swatches: [[0.0; 3], [1.0; 3]],
            gradients: Vec::new(),
            dialog: true,
            dynamic_color_sliders: false,
            isolated_curve: channel_cmds::isolated_curve_channel(s.active().unwrap()),
        }
    }
    #[test]
    fn combined_tone_entry_is_neutral_and_only_selected_components_are_listed() {
        for (mode, space, keys, label) in [
            ("rgb", ToneSpace::Rgb, vec!["", "red", "blue"], "RB"),
            ("cmyk", ToneSpace::Cmyk, vec!["", "cyan", "yellow"], "CY"),
            ("lab", ToneSpace::Lab, vec!["", "lightness", "b"], "Lb"),
        ] {
            let cx = context(mode);
            let options = editor_channels(&cx, space);
            assert_eq!(options.iter().map(|(c, _, _)| c.key).collect::<Vec<_>>(), keys);
            assert_eq!(options.first().unwrap().1, label);
            assert!(!cx.isolated_curve.as_ref().unwrap().tint);
        }
    }
    #[test]
    fn both_tone_editors_render_subset_choices_without_mutating_values() {
        for kind in ["levels", "curves"] {
            let cx = context("rgb");
            let base =
                photocraft_engine::adjust_params::to_params(&photocraft_engine::adjust_params::default_for(kind, photocraft_doc::ColorMode::Rgb).unwrap());
            let mut values = base.clone();
            let mut harness = egui_kittest::Harness::new_ui(|ui| {
                if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    editor(ui, kind, &mut values, &cx);
                }
            });
            PhotocraftApp::setup_context(&harness.ctx, Default::default());
            harness.run();
            drop(harness);
            assert_eq!(values, base);
        }
    }
    #[test]
    fn subset_histograms_match_both_editors_in_all_native_spaces_and_depths() {
        use egui_kittest::kittest::Queryable;
        use photocraft_geom::Rect as PixelRect;

        for mode in ["rgb", "cmyk", "lab"] {
            for depth in [8, 16, 32] {
                let mut s = Session::new();
                s.execute("file.new", json!({"width":32,"height":24,"mode":mode,"depth":depth})).unwrap();
                s.edit("Distinct native histogram bins", |doc, id| {
                    let surface = doc.layer_mut(id.unwrap()).unwrap().surface_mut().unwrap();
                    let mut pixel = vec![0.25, 0.5, 0.75];
                    pixel.resize(surface.format().mode.color_channels(), 0.0);
                    pixel.push(1.0);
                    surface.fill_rect(PixelRect::new(0, 0, 32, 24), &pixel);
                    pixel[0] = 0.375;
                    pixel[2] = 0.875;
                    surface.fill_rect(PixelRect::new(16, 0, 32, 24), &pixel);
                    Ok(())
                })
                .unwrap();
                s.execute("channel.target", json!({"colors":[0,2]})).unwrap();
                let st = s.active().unwrap();
                let isolated = channel_cmds::isolated_curve_channel(st).unwrap();
                let colors = isolated.components.unwrap();
                let mut cx = context(mode);
                cx.isolated_curve = Some(isolated);
                for kind in ["levels", "curves"] {
                    let base = adjust_params::to_params(&adjust_params::default_for(kind, st.doc.mode).unwrap());
                    let space = space_of(&base);
                    let hist = Arc::new(tone::compute_histograms(&st.doc, HistSource::Colors { layer: st.active_layer.unwrap(), colors }, space));
                    // Native subset histograms have their own combined slot even in Lab, whose
                    // ordinary composite histograms instead start with Lightness at slot 0.
                    assert_eq!(hist.ch.len(), 1 + st.doc.pixel_format().mode.color_channels());
                    assert_eq!(hist[0].iter().sum::<u32>(), 2 * 32 * 24);
                    assert_eq!(hist[1].iter().sum::<u32>(), 32 * 24);
                    assert_eq!(hist[2], [0; 256]);
                    assert_eq!(hist[3].iter().sum::<u32>(), 32 * 24);
                    assert_ne!(hist[1], hist[3]);
                    cx.hist = Some(hist.clone());
                    for (choice, (channel, _, histogram)) in editor_channels(&cx, space).into_iter().enumerate() {
                        assert!(hist[histogram].iter().any(|n| *n > 0));
                        let mut values = base.clone();
                        let mut harness = egui_kittest::Harness::new_ui(|ui| {
                            if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                                editor(ui, kind, &mut values, &cx);
                            }
                        });
                        PhotocraftApp::setup_context(&harness.ctx, Default::default());
                        harness.ctx.data_mut(|d| d.insert_temp(cx.mem.with(format!("{kind}-ch")), choice));
                        harness.run_steps(4);
                        if kind == "levels" {
                            harness.get_by_label("Auto").click();
                            harness.run_steps(2);
                        }
                        drop(harness);
                        if kind == "levels" {
                            let (lo, hi) = auto_levels(&hist[histogram]);
                            let levels = read_levels(&values, channel.key);
                            assert_eq!((levels[0], levels[2]), (lo, hi), "{mode}@{depth} {}", channel.key);
                        } else {
                            assert_eq!(values, base);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn lab_subset_keeps_combined_curve_in_point_pencil_and_component_edits() {
        for pencil in [false, true] {
            for channel in [0usize, 2] {
                let cx = context("lab");
                let mut values = adjust_params::to_params(&adjust_params::default_for("curves", photocraft_doc::ColorMode::Lab).unwrap());
                values["points"] = json!([[0, 0], [100, 150], [255, 255]]);
                if pencil {
                    assert!(set_editor_pencil(&mut values, true, &cx));
                }
                let combined = values["points"].clone();
                let component = values["b"].clone();
                let mut harness = egui_kittest::Harness::new_ui(|ui| {
                    if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                        editor(ui, "curves", &mut values, &cx);
                    }
                });
                PhotocraftApp::setup_context(&harness.ctx, Default::default());
                harness.ctx.data_mut(|d| d.insert_temp(cx.mem.with("curves-ch"), channel));
                harness.run_steps(4);
                let graph = harness.ctx.data(|d| d.get_temp::<Rect>(cx.mem.with("curves-graph"))).unwrap();
                let pos = pos2(graph.left() + graph.width() * 0.45, graph.top() + graph.height() * 0.25);
                harness.event(egui::Event::PointerMoved(pos));
                for pressed in [true, false] {
                    harness.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
                    harness.step();
                }
                harness.run_steps(2);
                drop(harness);
                assert!(values.get("points").is_some(), "combined record lost");
                if channel == 0 {
                    assert_ne!(values["points"], combined);
                } else {
                    assert_eq!(values["points"], combined);
                    assert_ne!(values["b"], component);
                }
                assert!(set_editor_pencil(&mut values, !pencil, &cx));
                assert!(values.get("points").is_some());
                assert!(values.get("b").is_some());
            }
        }
    }
}

#[cfg(test)]
mod polyline_tests {
    use super::*;

    #[test]
    fn curve_polyline_reuses_static_geometry_and_refreshes_edits_and_rect() {
        let mut cache = CurvePolyline::default();
        let mut points = [[0.0, 0.0], [128.0, 180.0], [255.0, 255.0]];
        let graph = Rect::from_min_size(pos2(20.0, 30.0), vec2(256.0, 256.0));
        assert!(cache.update(&points, graph, None));
        assert_eq!(cache.line.len(), CURVE_SAMPLES);
        assert_eq!(cache.line.first(), Some(&graph.left_bottom()));
        assert_eq!(cache.line.last(), Some(&graph.right_top()));
        let line = cache.line.clone();
        assert!(!cache.update(&points, graph, None));
        assert_eq!(cache.line, line);

        points[1][1] = 80.0;
        assert!(cache.update(&points, graph, None));
        assert_ne!(cache.line, line);
        points[1][1] = 180.0; // Undo/preset restore must also invalidate.
        assert!(cache.update(&points, graph, None));
        assert_eq!(cache.line, line);

        let moved = graph.translate(vec2(13.0, 25.0));
        assert!(cache.update(&points, moved, None));
        for (before, after) in line.iter().zip(&cache.line) {
            assert!((*after - (*before + vec2(13.0, 25.0))).length() < 1e-4);
        }
        let resized = Rect::from_min_size(moved.min, vec2(128.0, 180.0));
        assert!(cache.update(&points, resized, None));
        assert_eq!(cache.line.last(), Some(&resized.right_top()));
        assert!(!cache.update(&points, resized, None));
    }

    #[test]
    fn curve_polyline_memory_is_separate_per_editor_and_channel() {
        let ctx = egui::Context::default();
        let graph = Rect::from_min_size(Pos2::ZERO, vec2(256.0, 256.0));
        let identity = [[0.0, 0.0], [255.0, 255.0]];
        let inverted = [[0.0, 255.0], [255.0, 0.0]];
        let editor = egui::Id::unique("dialog");
        let master = editor.with(("curves-polyline", "points"));
        let red = editor.with(("curves-polyline", "red"));
        let other_editor = egui::Id::unique("properties").with(("curves-polyline", "points"));
        let line = cached_curve_line(&ctx, master, &identity, graph, None);
        assert_ne!(cached_curve_line(&ctx, red, &inverted, graph, None), line);
        assert_ne!(cached_curve_line(&ctx, other_editor, &inverted, graph, None), line);
        assert_eq!(cached_curve_line(&ctx, master, &identity, graph, None), line);
        ctx.data_mut(|d| {
            assert!(!d.get_temp_mut_or_default::<CurvePolyline>(master).update(&identity, graph, None));
            assert!(!d.get_temp_mut_or_default::<CurvePolyline>(red).update(&inverted, graph, None));
            assert!(!d.get_temp_mut_or_default::<CurvePolyline>(other_editor).update(&inverted, graph, None));
        });
        assert_ne!(cached_curve_line(&ctx, master, &inverted, graph, None), line);
    }

    #[test]
    #[ignore = "release timing probe; run on GitHub Actions with --release --ignored --nocapture"]
    fn curve_polyline_release_comparison() {
        use std::{hint::black_box, time::Instant};
        let graph = Rect::from_min_size(pos2(20.0, 30.0), vec2(300.0, 300.0));
        let points = [[0.0, 0.0], [64.0, 48.0], [192.0, 208.0], [255.0, 255.0]];
        let iterations = 10_000;
        let start = Instant::now();
        for _ in 0..iterations {
            let cp: Vec<_> = black_box(&points).iter().map(|q| photocraft_doc::adjust::CurvePoint { input: q[0] / 255.0, output: q[1] / 255.0 }).collect();
            let lut = photocraft_compose::adjust::curve_lut(&cp);
            let line: Vec<_> = lut
                .iter()
                .enumerate()
                .step_by(16)
                .map(|(i, y)| pos2(graph.left() + i as f32 / 4095.0 * graph.width(), graph.bottom() - y * graph.height()))
                .collect();
            black_box(line);
        }
        let old = start.elapsed();
        let start = Instant::now();
        for _ in 0..iterations {
            let mut cache = CurvePolyline::default();
            cache.update(black_box(&points), graph, None);
            black_box(cache.line.clone());
        }
        let miss = start.elapsed();
        let ctx = egui::Context::default();
        let id = egui::Id::unique("curve-perf");
        cached_curve_line(&ctx, id, &points, graph, None);
        let start = Instant::now();
        for _ in 0..iterations {
            black_box(cached_curve_line(&ctx, id, black_box(&points), graph, None));
        }
        let hit = start.elapsed();
        println!("{iterations} polylines: old 4096 samples {old:?}; direct 256 samples {miss:?}; cached screen line {hit:?}");
    }
}

#[cfg(test)]
mod curve_view_tests {
    use super::*;

    #[test]
    fn view_maps_levels_both_ways_and_full_is_the_old_scale() {
        let full = CurveView::full(4.0);
        // Full range: graph units are levels / range, as before zooming existed.
        assert_eq!(full.to_g([510.0, 255.0]), [127.5, 63.75]);
        let v = CurveView { range: 4.0, x0: 0.5, y0: -0.25, sx: 2.0, sy: 1.5 };
        for q in [[0.0, 0.0], [255.0, 127.5], [1020.0, 900.0]] {
            let back = v.levels_from_g(v.to_g(q));
            assert!((back[0] - q[0]).abs() < 1e-3 && (back[1] - q[1]).abs() < 1e-3, "{q:?} → {back:?}");
        }
    }

    #[test]
    fn fit_frames_the_points_and_zoom_keeps_the_pointer_still() {
        // A curve living in 0..1 of a 0..64 range is framed around 0..1, not 0..64.
        let v = CurveView::fit(64.0, &[[0.0, 0.0], [128.0, 200.0], [255.0, 255.0]]);
        assert!(v.sx < 1.2 && v.sy < 1.2 && v.x0 <= 0.0 && v.y0 <= 0.0, "{v:?}");
        let mut z = CurveView::full(8.0);
        let at = [0.25, 0.5];
        let before = [z.x0 + at[0] * z.sx, z.y0 + at[1] * z.sy];
        z.zoom(0.5, at);
        let after = [z.x0 + at[0] * z.sx, z.y0 + at[1] * z.sy];
        assert!((before[0] - after[0]).abs() < 1e-5 && (before[1] - after[1]).abs() < 1e-5);
        assert_eq!((z.sx, z.sy), (4.0, 4.0));
        assert!((grid_step(1.1) - 0.5).abs() < 1e-6);
        assert!((grid_step(64.0) - 20.0).abs() < 1e-4);
    }
}
