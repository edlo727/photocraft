use super::display::{canvas_display, display_image};
use super::drag::Drag;
use super::paint_thread::{PaintThread, Step};
use super::textures::{canvas_texture_options, shown_as_document};
use super::view_xform::ViewXform;
use super::{Color32, DRect, PhotocraftApp, Pos2, Stroke, Tool, json, pos2};

pub(crate) struct ClonePreviewCache {
    pub(super) key: String,
    // Reopening a file can retain its document id and revision; distinguish its new snapshot.
    // A weak reference detects that replacement without keeping a closed image's pixels alive.
    source_document: Option<std::sync::Weak<photocraft_doc::Document>>,
    /// The brush a clipped overlay took its coverage from: a hardness or tip change redraws it.
    pub(super) brush: Option<photocraft_paint::BrushSettings>,
    /// The document area the texture covers.
    pub(super) rect: DRect,
    pub(super) texture: egui::TextureHandle,
}

/// The most document pixels one dab's coverage is rendered over for the overlay: a 1000 px brush,
/// the largest the overlay draws, at hardness 0 reaches about 1.73 × its radius (`tip_reach`).
const CLONE_PREVIEW_MAX_PIXELS: u64 = 1800 * 1800;

/// The largest side `retouch_cmds::clone_preview_periodic` samples, for the overlay's area and for
/// the source area it reads (`Mapping::source_rect`: the overlay's, mapped, with a 2 px apron).
const CLONE_PREVIEW_MAX_SIDE: u32 = 1024;

/// The coverage of one dab of `brush` at `at`, including flow, tip, angle, roundness and
/// hardness (a soft tip reaches past its radius). Stroke opacity is applied when drawing. The area
/// it covers and its `width × height` coverage values in 0..1. A very large soft tip, or a source
/// the Clone Source panel scales down (which reads a larger area than it covers), is cropped
/// around `at` to the sampling budget, keeping the middle of the dab.
fn clone_dab_coverage(
    brush: &photocraft_paint::BrushSettings,
    at: [f64; 2],
    map: &photocraft_engine::presets::clone_source::Mapping,
    flow: f32,
) -> Option<(DRect, Vec<f32>)> {
    let brush = photocraft_paint::BrushSettings { opacity: 1.0, flow, erase: false, ..brush.clone() };
    let stroke = photocraft_paint::Stroke { brush, points: vec![photocraft_paint::StrokePoint::new(at[0], at[1], 1.0)] };
    let (rect, cov) = photocraft_paint::retouch::stroke_coverage_within(&stroke, CLONE_PREVIEW_MAX_PIXELS)?;
    let fits = |r: DRect| r.width() <= CLONE_PREVIEW_MAX_SIDE && r.height() <= CLONE_PREVIEW_MAX_SIDE;
    let (cx, cy) = (at[0].floor() as i32, at[1].floor() as i32);
    let around = |half: i32| rect.intersect(&DRect::new(cx.saturating_sub(half), cy.saturating_sub(half), cx.saturating_add(half), cy.saturating_add(half)));
    // The source area scales with the overlay's: shrink by what overflows, a few times for the
    // apron's rounding.
    let mut half = i32::try_from((CLONE_PREVIEW_MAX_SIDE - 4) / 2).unwrap_or(510);
    let mut window = around(half);
    for _ in 0..8 {
        let src = map.source_rect(window);
        if fits(window) && fits(src) {
            break;
        }
        let over = f64::from(src.width().max(src.height())) / f64::from(CLONE_PREVIEW_MAX_SIDE - 4);
        half = ((f64::from(half) / over.max(1.0)).floor() as i32).saturating_sub(2).max(1);
        window = around(half);
    }
    if window.is_empty() || !fits(window) || !fits(map.source_rect(window)) {
        return None;
    }
    if window == rect {
        return Some((rect, cov));
    }
    let stride = rect.width() as usize;
    let mut cropped = Vec::with_capacity(window.width() as usize * window.height() as usize);
    for y in window.y0..window.y1 {
        let row = (y - rect.y0) as usize * stride;
        let from = row + (window.x0 - rect.x0) as usize;
        cropped.extend_from_slice(cov.get(from..from + window.width() as usize)?);
    }
    Some((window, cropped))
}

/// A transient source image beneath the brush outline; never part of the document render. The
/// Clone Source panel drives the Clone Stamp and the Healing Brush alike, so both show it.
pub(super) fn draw_clone_preview(app: &mut PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, at: [f64; 2], output: Option<u32>) {
    use photocraft_engine::presets::clone_source::overlay_blend;
    let overlay = app.session.presets.clone.overlay.clone();
    let tool = app.ui.tool;
    if !matches!(tool, Tool::CloneStamp | Tool::Healing) || !overlay.show || (overlay.auto_hide && app.drag.is_some()) {
        return;
    }
    let slot = app.session.presets.clone.active().clone();
    let Some(map) = crate::preset_panels::clone_mapping_at(app, Some(at)) else { return };
    let radius = f64::from(app.session.tools.brush.size) / 2.0;
    if !radius.is_finite() || radius <= 0.0 || radius > 500.0 || !at.iter().all(|v| v.is_finite() && v.abs() < 1_000_000.0) {
        return;
    }
    // Both tools repeat source reads in Pattern Preview; this also refreshes a stationary hover.
    let wrap = app.ui.view.pattern_preview;
    let Some(st) = app.session.active() else { return };
    let target = paint_target(app);
    if slot.document.is_some_and(|id| id != st.doc.id) && photocraft_engine::channel_cmds::gray_target(&json!({"target": target})).is_some() {
        // Strokes reject these cross-document targets, including a mask selected only in the UI.
        return;
    }
    let source_revision =
        slot.document.and_then(|id| app.session.documents().iter().find(|st| st.doc.id == id)).map(|st| (st.doc.id, st.revision, st.active_layer));
    let source_document = slot.document.and_then(|id| app.session.documents().iter().find(|st| st.doc.id == id)).map(|st| std::sync::Arc::downgrade(&st.doc));
    let (display, display_key) = canvas_display(app, &st.doc, output);
    let key = format!(
        "{tool:?}:{source_revision:?}:{:?}:{}:{:?}:{:?}:{map:?}:{:?}:{}:{}:{}:{}:{wrap}:{}:{}",
        st.doc.id,
        st.revision,
        st.active_layer,
        at.map(f64::to_bits),
        slot.layer,
        app.ui.tool_options.clone_sample,
        display_key,
        overlay.invert,
        overlay.blend,
        overlay.clipped,
        radius.to_bits()
    );
    let brush = overlay.clipped.then_some(&app.session.tools.brush);
    if app.clone_preview.as_ref().is_none_or(|c| {
        let same_source = match (&c.source_document, &source_document) {
            (Some(a), Some(b)) => std::sync::Weak::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        c.key != key || c.brush.as_ref() != brush || !same_source
    }) {
        // Clipped (Photoshop's default): the dab the tool paints here, so a soft brush previews
        // soft and a shaped tip keeps its outline. Unclipped: the brush's whole square.
        let (rect, coverage) = match brush {
            Some(b) => {
                let flow = if tool == Tool::CloneStamp { b.flow } else { 1.0 };
                let Some((rect, coverage)) = clone_dab_coverage(b, at, &map, flow) else { return };
                (rect, Some(coverage))
            }
            None => (
                DRect::new((at[0] - radius).floor() as i32, (at[1] - radius).floor() as i32, (at[0] + radius).ceil() as i32, (at[1] + radius).ceil() as i32),
                None,
            ),
        };
        let preview = if tool == Tool::Healing {
            photocraft_engine::retouch_cmds::healing_preview_periodic(&app.session, rect, &map, &app.ui.tool_options.clone_sample, wrap)
        } else {
            photocraft_engine::retouch_cmds::clone_preview_periodic(&app.session, rect, &map, &app.ui.tool_options.clone_sample, wrap)
        };
        let Ok(mut buf) = preview else {
            return;
        };
        if overlay.invert {
            for px in &mut buf.px {
                for c in px.iter_mut().take(3) {
                    *c = 1.0 - *c;
                }
            }
        }
        // Darken, Lighten and Difference mix the source with the image under the brush (the
        // composite, whatever Sample reads), so Difference turns black where they line up.
        if overlay.blend != "normal" {
            let Ok(under) = photocraft_engine::retouch_cmds::clone_destination_preview(&app.session, rect, wrap) else {
                return;
            };
            for (px, d) in buf.px.iter_mut().zip(&under.px) {
                *px = overlay_blend(&overlay.blend, *px, *d);
            }
        }
        if let Some(coverage) = &coverage {
            for (px, c) in buf.px.iter_mut().zip(coverage) {
                px[3] *= c.clamp(0.0, 1.0);
            }
        }
        let image = display_image(display.as_deref(), &buf);
        let brush = brush.cloned();
        match app.clone_preview.as_mut() {
            Some(cache) => {
                cache.texture.set(image, canvas_texture_options(1));
                cache.key = key;
                cache.source_document = source_document;
                cache.brush = brush;
                cache.rect = rect;
            }
            None => {
                let texture = painter.ctx().load_texture("clone-preview", image, canvas_texture_options(1));
                app.clone_preview = Some(ClonePreviewCache { key, source_document, brush, rect, texture });
            }
        }
    }
    let Some(cache) = &app.clone_preview else { return };
    let rect = cache.rect;
    // Clone Stamp sends its opacity/flow with every stroke; Healing Brush uses full strength.
    let brush_alpha = if tool == Tool::CloneStamp {
        app.session.tools.brush.opacity.clamp(0.0, 1.0) * if overlay.clipped { 1.0 } else { app.session.tools.brush.flow.clamp(0.0, 1.0) }
    } else {
        1.0
    };
    let tint = Color32::from_white_alpha((overlay.opacity.clamp(0.0, 100.0) * brush_alpha * 2.55).round() as u8);
    // A textured quad over the covered area: its texels carry the clip, in mirrored and rotated
    // canvas views too.
    let mut mesh = egui::Mesh::with_texture(cache.texture.id());
    for (x, y, u, v) in [(rect.x0, rect.y0, 0.0, 0.0), (rect.x1, rect.y0, 1.0, 0.0), (rect.x1, rect.y1, 1.0, 1.0), (rect.x0, rect.y1, 0.0, 1.0)] {
        mesh.vertices.push(egui::epaint::Vertex { pos: xf.to_screen(x as f32, y as f32), uv: pos2(u, v), color: tint });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

/// The engine's live stroke of a tool: the Brush, Pencil and Eraser paint dabs; the Clone Stamp,
/// Healing Brush and History Brush composite their source.
pub(super) enum EngineStroke {
    Brush(Box<photocraft_engine::brush_cmds::LiveStroke>),
    Retouch(Box<photocraft_engine::retouch_cmds::LiveRetouch>),
    Dab(Box<photocraft_engine::retouch_cmds::LiveDab>),
    Mixer(Box<photocraft_engine::brush_cmds::mixer_live::LiveMixer>),
}

impl EngineStroke {
    /// The brush engine's strokes can change while the pointer is held still (see
    /// `LiveStroke::wants_time`); the retouch strokes don't.
    pub(super) fn wants_time(&self) -> bool {
        match self {
            EngineStroke::Brush(l) => l.wants_time(),
            EngineStroke::Mixer(l) => l.wants_time(),
            EngineStroke::Retouch(_) | EngineStroke::Dab(_) => false,
        }
    }

    pub(super) fn doc(&self) -> &std::sync::Arc<photocraft_doc::Document> {
        match self {
            Self::Brush(s) => &s.doc,
            Self::Retouch(s) => &s.doc,
            Self::Dab(s) => &s.doc,
            Self::Mixer(s) => &s.doc,
        }
    }
    fn bounds(&self) -> DRect {
        match self {
            Self::Brush(s) => s.bounds(),
            Self::Retouch(s) => s.bounds(),
            Self::Dab(s) => s.bounds(),
            Self::Mixer(s) => s.bounds(),
        }
    }
    pub(super) fn push(&mut self, pts: &[photocraft_engine::paint::StrokePoint]) -> photocraft_engine::Result<DRect> {
        match self {
            Self::Brush(s) => s.push(pts),
            Self::Retouch(s) => s.push(pts),
            Self::Dab(s) => s.push(pts),
            Self::Mixer(s) => s.push(pts),
        }
    }
    /// The jitter seed the commit must reuse (the Clone Stamp's comes from the session brush).
    fn seed(&self) -> Option<u64> {
        match self {
            Self::Brush(s) => Some(s.seed),
            Self::Mixer(s) => Some(s.seed),
            Self::Retouch(_) => None,
            Self::Dab(_) => None,
        }
    }
}

/// A stroke shown while it is drawn: the engine renders the real result onto a copy of the
/// document, on a thread of its own (`PaintThread`), and the canvas redraws only what each step
/// changed.
pub(crate) struct LiveStroke {
    pub(super) preview_failed: bool,
    pub(super) paint: PaintThread,
    /// The jitter seed the commit must reuse (`EngineStroke::seed`).
    pub(super) seed: Option<u64>,
    pub(super) doc: photocraft_doc::DocId,
    revision: u64,
    /// Preview key of the stroke; step `n` displays as `key + n`.
    pub(super) key: u64,
    /// Damage of each step (step 0 = the document before the stroke).
    pub(super) damage: Vec<DRect>,
    /// Drag points rendered so far.
    fed: usize,
}

impl LiveStroke {
    /// The document as the stroke last painted it.
    pub(super) fn doc(&self) -> &std::sync::Arc<photocraft_doc::Document> {
        &self.paint.shown().doc
    }

    /// See `EngineStroke::wants_time`.
    pub(super) fn wants_time(&self) -> bool {
        self.paint.shown().wants_time
    }

    /// See `photocraft_engine::brush_cmds::LiveStroke::smoothing_string`.
    pub(super) fn smoothing_string(&self) -> Option<([f64; 2], [f64; 2])> {
        self.paint.shown().smoothing_string
    }

    pub(super) fn display_key(&self) -> u64 {
        self.key + self.damage.len() as u64
    }

    /// What changed since the canvas showed preview `key` (0 = the document before the stroke).
    pub(super) fn since(&self, key: u64) -> Option<DRect> {
        let seen = if key == 0 { 0 } else { usize::try_from(key.checked_sub(self.key)?).ok()? };
        Some(self.damage.get(seen..)?.iter().fold(DRect::EMPTY, |a, r| a.union(r)))
    }
}

/// A live stroke whose pixels its commit took over (`"fromLive"`): document `doc` at `revision`
/// holds exactly the live stroke's last step, so a cache that showed step `n` of it only misses
/// the steps after `n` (step 0 = the document before the stroke: everything).
pub(crate) struct CommittedStroke {
    pub(crate) doc: photocraft_doc::DocId,
    pub(crate) revision: u64,
    key: u64,
    damage: Vec<DRect>,
    /// What the commit repaints beyond the stroke whatever a cache showed: the layer's whole
    /// composite when effects laid out over its frame moved with it (a Gradient Overlay spans the
    /// grown layer, #4053; `photocraft_compose::relayout_damage`).
    pub(super) relayout: DRect,
}

impl CommittedStroke {
    #[cfg(test)]
    pub(crate) fn for_test(key: u64, damage: Vec<DRect>) -> Self {
        Self { doc: photocraft_doc::DocId(0), revision: 0, key, damage, relayout: DRect::EMPTY }
    }

    /// The raw preview key of the live stroke's last step: what a cache showed at release.
    #[cfg(test)]
    pub(crate) fn last_key(&self) -> u64 {
        self.key + self.damage.len() as u64
    }

    /// What a cache that showed raw preview `key` of this stroke at the revision before misses.
    pub(crate) fn since(&self, key: u64) -> Option<DRect> {
        let seen = if key == 0 { 0 } else { usize::try_from(key.checked_sub(self.key)?).ok()? };
        Some(self.damage.get(seen..)?.iter().fold(DRect::EMPTY, |a, r| a.union(r)))
    }
}

/// The live stroke on document `idx`, while it is current.
pub(super) fn live_stroke(app: &PhotocraftApp, idx: usize) -> Option<&LiveStroke> {
    let st = app.session.documents().get(idx)?;
    app.live_stroke.as_ref().filter(|l| app.drag.is_some() && l.doc == st.doc.id && l.revision == st.revision)
}

/// `paint.stroke` params for a Brush/Eraser drag (shared by the live preview and the commit). The
/// stroke smoothing is the session brush's (the options bar's Smoothing %), except on a ⇧-click
/// line, which isn't smoothed (#11887).
pub(super) fn stroke_params(app: &PhotocraftApp, d: &Drag, points: &[Vec<f64>]) -> serde_json::Value {
    let tool = d.tool;
    let mut p = json!({ "points": points, "erase": d.erase, "zoom": app.point_zoom(), "target": paint_target(app) });
    unsmoothed_line(&mut p, d.line);
    if d.clear {
        p["mode"] = json!("clear");
    }
    if tool == Tool::Pencil {
        p["autoErase"] = json!(app.ui.tool_options.pencil_auto_erase);
    }
    // HDR paint (32-bit documents only; the engine ignores it on others).
    let ev = app.ui.tool_options.hdr_intensity;
    if matches!(tool, Tool::Brush | Tool::Pencil) && ev.is_finite() && ev != 0.0 {
        p["intensity"] = json!(ev);
    }
    // Block mode: the engine sizes the square from the stroke's zoom (16 / zoom px, #2770).
    if crate::eraser_ui::block_mode(app, tool) {
        p["block"] = json!(true);
    }
    p
}

/// A ⇧-click line isn't smoothed, so it ends at the click whatever Catch-up on Stroke End is set
/// to (#11887). Applied to the params of every painting tool's live preview and commit alike, so
/// a retouching stroke's commit still takes the pixels its preview drew.
pub(crate) fn unsmoothed_line(p: &mut serde_json::Value, line: bool) {
    if line && let Some(o) = p.as_object_mut() {
        o.insert("smoothing".into(), json!(0.0));
    }
}

/// Tools whose strokes the engine renders while they are drawn (`LiveStroke`).
pub(crate) fn strokes_live(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::Brush
            | Tool::Pencil
            | Tool::MixerBrush
            | Tool::Eraser
            | Tool::ColorReplacement
            | Tool::Blur
            | Tool::Sharpen
            | Tool::Smudge
            | Tool::Dodge
            | Tool::Burn
            | Tool::Sponge
    ) || live_retouch_command(tool).is_some()
}

/// The Spot Healing Brush's stroke while dragging: Photoshop shows the area it will heal as a
/// semi-opaque dark overlay (an image overlay, the same in every UI theme) and heals on release.
pub(crate) const SPOT_HEALING_TRAIL: Color32 = Color32::from_black_alpha(128);

/// The retouching tools drawn live (`LiveRetouch`; the Background Eraser's and an Impressionist
/// Pattern Stamp's dabs are laid one by one, `LiveDab`), by the command their live stroke
/// previews. The Healing Brush shows its texture while drawing; the blend comes on release.
pub(super) fn live_retouch_command(tool: Tool) -> Option<&'static str> {
    match tool {
        Tool::CloneStamp => Some("paint.cloneStamp"),
        Tool::Healing => Some("paint.healingBrush"),
        Tool::HistoryBrush => Some("paint.historyBrush"),
        Tool::PatternStamp => Some("paint.patternStamp"),
        Tool::BackgroundEraser => Some("paint.backgroundEraser"),
        _ => None,
    }
}

/// The command a live-stroking tool commits: the Pencil's (and the Pencil- or Block-mode
/// Eraser's) `paint.pencil`, else `paint.stroke`.
pub(crate) fn stroke_command(app: &PhotocraftApp, tool: Tool) -> &'static str {
    if tool == Tool::Pencil || crate::eraser_ui::pencil_mode(app, tool) || crate::eraser_ui::block_mode(app, tool) { "paint.pencil" } else { "paint.stroke" }
}

/// Starts the live preview of the current drag, from the params its tool commits. `None` without
/// an active document or drag, or when the tool's preview can't begin.
pub(super) fn begin_live_stroke(app: &PhotocraftApp) -> Option<LiveStroke> {
    static STROKES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let st = app.session.active()?;
    let d = app.drag.as_ref()?;
    let stroke = if d.tool == Tool::MixerBrush {
        let points = app.stylus.stroke_points(&d.points);
        let p = stroke_params(app, d, points.get(..1)?);
        EngineStroke::Mixer(Box::new(photocraft_engine::brush_cmds::mixer_live::LiveMixer::begin(&app.session, &p).ok()?))
    } else if d.tool == Tool::ColorReplacement {
        // The params `retouch_ui::finish_stroke` commits.
        let mut p = crate::retouch_ui::color_replacement_params(app);
        p["points"] = json!(d.points);
        unsmoothed_line(&mut p, d.line);
        EngineStroke::Dab(Box::new(photocraft_engine::brush_cmds::live_color_replacement(&app.session, &p).ok()?))
    } else if let Some((cmd, mut p)) = crate::retouch_ui::dab_params(app, d.tool) {
        // The params `retouch_ui::finish_stroke` commits; Sample All Layers isn't previewed.
        p["points"] = json!(d.points);
        p["target"] = paint_target(app);
        unsmoothed_line(&mut p, d.line);
        EngineStroke::Dab(Box::new(photocraft_engine::retouch_cmds::LiveDab::begin(&app.session, cmd, &p).ok()?))
    } else if d.tool == Tool::PatternStamp {
        // The params `retouch_ui::finish_stroke` commits.
        let mut p = crate::retouch_ui::pattern_stamp_params(app, &d.points);
        p["points"] = json!(d.points);
        p["target"] = paint_target(app);
        unsmoothed_line(&mut p, d.line);
        if app.ui.tool_options.pattern_stamp_impressionist {
            EngineStroke::Dab(Box::new(photocraft_engine::retouch_cmds::live_pattern_impressionist(&app.session, &p).ok()?))
        } else {
            EngineStroke::Retouch(Box::new(photocraft_engine::retouch_cmds::LiveRetouch::begin(&app.session, "paint.patternStamp", &p).ok()?))
        }
    } else if d.tool == Tool::BackgroundEraser {
        // The params `eraser_ui::finish_stroke` commits (layer pixels only).
        let mut p = crate::eraser_ui::background_eraser_params(app, &d.points);
        unsmoothed_line(&mut p, d.line);
        EngineStroke::Dab(Box::new(photocraft_engine::eraser_cmds::live_background_eraser(&app.session, &p).ok()?))
    } else if let Some(cmd) = live_retouch_command(d.tool) {
        // The params `retouch_ui::finish_stroke` commits.
        let mut p = if d.tool == Tool::HistoryBrush {
            d.history_params.clone().unwrap_or_else(|| crate::retouch_ui::history_brush_params(app))
        } else {
            crate::retouch_ui::clone_params(app)?
        };
        p["points"] = json!(d.points);
        p["target"] = paint_target(app);
        unsmoothed_line(&mut p, d.line);
        EngineStroke::Retouch(Box::new(photocraft_engine::retouch_cmds::LiveRetouch::begin(&app.session, cmd, &p).ok()?))
    } else {
        let p = stroke_params(app, d, &app.stylus.stroke_points(&d.points));
        EngineStroke::Brush(Box::new(photocraft_engine::brush_cmds::LiveStroke::begin_with(&app.session, stroke_command(app, d.tool), &p).ok()?))
    };
    let n = STROKES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0xff_ffff;
    let damage = vec![stroke.bounds()];
    Some(LiveStroke {
        seed: stroke.seed(),
        paint: PaintThread::start(stroke),
        doc: st.doc.id,
        revision: st.revision,
        key: (1 << 44) | (n << 20),
        damage,
        fed: if d.tool == Tool::MixerBrush { 1 } else { d.points.len() },
        preview_failed: false,
    })
}

/// A Large Brush Mode stroke (options bar): its dabs are generated as the commit will generate
/// them and drawn on the GPU as an overlay (`gpu_canvas::GpuCanvas::stroke_dabs`); the document
/// is untouched until release, when `paint.stroke` renders the stroke once at full precision
/// with the same points and seed, off the UI thread ([`QueuedCommit`]).
pub(crate) struct GpuStroke {
    preview: photocraft_engine::brush_cmds::gpu_preview::DabPreview,
    pub(super) doc: photocraft_doc::DocId,
    /// The overlay slot showing it.
    slot: usize,
    /// Drag points drawn so far.
    fed: usize,
    /// Every dab drawn so far: what the overlay shows, for an ⌥-click colour pick before the
    /// stroke's write lands (`pending_paint`).
    dabs: Vec<photocraft_engine::brush_cmds::gpu_preview::GpuDab>,
    /// Painted into its layer by the compositor (each frame recomposites where new dabs land,
    /// `live_damage`), not laid over the canvas.
    in_layer: bool,
}

/// Document rect `dabs` reach, with a pixel to spare.
fn dab_rect(dabs: &[photocraft_engine::brush_cmds::gpu_preview::GpuDab]) -> DRect {
    dabs.iter().filter(|d| d.reach.is_finite() && d.center.iter().all(|v| v.is_finite())).fold(DRect::EMPTY, |b, d| {
        let r = d.reach + 2.0;
        let x0 = (d.center[0] - r).floor().clamp(-1e9, 1e9) as i32;
        let y0 = (d.center[1] - r).floor().clamp(-1e9, 1e9) as i32;
        let x1 = (d.center[0] + r).ceil().clamp(-1e9, 1e9) as i32;
        let y1 = (d.center[1] + r).ceil().clamp(-1e9, 1e9) as i32;
        b.union(&DRect::new(x0, y0, x1.max(x0 + 1), y1.max(y0 + 1)))
    })
}

/// New dabs of a stroke painted into its layer: the canvas recomposites where they land.
fn live_damage(app: &mut PhotocraftApp, doc: photocraft_doc::DocId, dabs: &[photocraft_engine::brush_cmds::gpu_preview::GpuDab]) {
    let r = dab_rect(dabs);
    if r.is_empty() {
        return;
    }
    app.gpu_live_damage = Some(match app.gpu_live_damage {
        Some((d, old)) if d == doc => (doc, old.union(&r)),
        _ => (doc, r),
    });
}

/// Where `doc`'s canvas must recomposite for strokes painted into layers since it last did.
pub(crate) fn take_live_damage(app: &mut PhotocraftApp, doc: photocraft_doc::DocId) -> Option<DRect> {
    match app.gpu_live_damage {
        Some((d, r)) if d == doc => {
            app.gpu_live_damage = None;
            Some(r)
        }
        _ => None,
    }
}

/// Landed strokes of `doc` painted into their layers stop now, before the refresh that draws
/// their written pixels: the compositor would otherwise lay them in once more over those.
pub(crate) fn end_landed_in_layer(app: &mut PhotocraftApp, gpu: &crate::gpu_canvas::GpuCanvas, doc: photocraft_doc::DocId) {
    app.gpu_landed.retain(|&(slot, d, _)| {
        if d == doc && gpu.stroke_in_layer(slot) {
            gpu.stroke_end(slot);
            return false;
        }
        true
    });
}

/// The pen's points `from..` of drag `d`, with the pressure, tilt, rotation and times the
/// commit's `paint.stroke` gets for them.
fn drag_points(app: &PhotocraftApp, d: &Drag, from: usize) -> Vec<photocraft_engine::paint::StrokePoint> {
    let pose = &app.stylus.stroke;
    let t0 = app.stylus.point_ms(0);
    (from..d.points.len())
        .filter_map(|i| {
            let p = d.points.get(i)?;
            let t = pose.get(i).or(pose.last()).copied().unwrap_or_default();
            let mut sp = photocraft_engine::paint::StrokePoint::new(p[0], p[1], p[2] as f32);
            (sp.tilt_x, sp.tilt_y, sp.rotation) = (t[0], t[1], t[2]);
            sp.time = app.stylus.point_ms(i) - t0;
            Some(sp)
        })
        .collect()
}

/// Start a Large Brush Mode stroke for the drag just begun, when the option is on and the
/// stroke qualifies: the Brush (not erasing) on a document the GPU canvas draws, with a brush and
/// target the overlay shows faithfully (`DabPreview::begin_or_why`). `None` keeps the normal live
/// stroke, and the status bar says why the option didn't apply. Released strokes still writing
/// keep showing in their own slots; only when all are taken does the oldest land first.
pub(super) fn begin_gpu_stroke(app: &mut PhotocraftApp) -> Option<GpuStroke> {
    if !app.ui.tool_options.large_brush_mode || app.drag.as_ref().is_none_or(|d| d.tool != Tool::Brush) {
        return None;
    }
    // Timed into the stroke trace's stroke column, with landings (`land`).
    let started = crate::gpu_canvas::now_ms();
    if app.gpu.as_ref().is_some_and(|gpu| !gpu.stroke_slot_free()) {
        let waited = app.perf.stage.wait_ms;
        land_oldest_commit(app);
        note_gpu_wait(app, waited, "a new stroke: every overlay slot was in use");
    }
    let tried = try_gpu_stroke(app);
    app.perf.stage.stroke_ms += crate::gpu_canvas::now_ms() - started;
    match tried {
        Ok(g) => {
            if g.in_layer {
                live_damage(app, g.doc, &g.dabs);
            }
            Some(g)
        }
        Err(why) => {
            app.ui.status = format!("{} {why}", tl!("Large Brush Mode isn't used for this stroke:"));
            app.ui.status_error = false;
            None
        }
    }
}

fn try_gpu_stroke(app: &PhotocraftApp) -> Result<GpuStroke, String> {
    static STROKES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let d = app.drag.as_ref().ok_or("no stroke")?;
    if d.erase || d.clear {
        return Err("erasing".into());
    }
    if stroke_command(app, d.tool) != "paint.stroke" {
        return Err("the brush is in Pencil mode".into());
    }
    let gpu = app.gpu.clone().ok_or("no GPU canvas (CPU rendering)")?;
    if gpu.fault().is_some() {
        return Err("the GPU canvas failed (CPU rendering)".into());
    }
    let st = app.session.active().ok_or("no document")?;
    let (doc, size) = (st.doc.id, [st.doc.size.width, st.doc.size.height]);
    // The overlay draws in the GPU canvas: the document's display textures must exist (layers
    // over the GPU budget still show there; a canvas too large for the GPU doesn't).
    if !gpu.has(doc.0, size) {
        let why = app.render_path.current(doc.0).map_or("not on the GPU canvas", |r| r.kind());
        return Err(format!("the canvas is drawn on the CPU ({why})"));
    }
    // The commit must reuse the jitter seed: by default it comes from the whole point list.
    let n = STROKES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let seed = (crate::gpu_canvas::now_ms().to_bits()).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ n;
    let mut p = stroke_params(app, d, &app.stylus.stroke_points(&d.points));
    p["seed"] = json!(seed);
    let mut preview = match photocraft_engine::brush_cmds::gpu_preview::DabPreview::begin_or_why(&app.session, &p) {
        Ok(Ok(preview)) => preview,
        Ok(Err(why)) => return Err(why.into()),
        Err(e) => return Err(e.to_string()),
    };
    // Painted into its layer by the GPU compositor, in its place in the stack; laid over the
    // canvas where the compositor doesn't draw the document or the layer has layer effects.
    // The overlay is the cheaper of the two (it draws at screen resolution, nothing
    // recomposites), and exact for a Normal layer with nothing over it and no mask: there it
    // stays. Anything over the layer, a mask or another blend mode (the layer's or the brush's)
    // needs the stroke in place.
    let needs_place = !preview.on_top
        || st.doc.layer(preview.layer).is_some_and(|l| l.mask.as_ref().is_some_and(|m| m.enabled) || l.clipped)
        || covered_above(&st.doc, preview.layer, st.doc.bounds());
    let in_layer = needs_place && preview.in_layer && app.render_path.current(doc.0).is_none();
    if !in_layer && !preview.on_top {
        let what = if preview.mode == photocraft_color::BlendMode::Normal { "the layer's blend mode" } else { "the brush mode" };
        return Err(format!("{what} isn't Normal and the stroke can't be painted into its layer on the GPU"));
    }
    let slot = if in_layer {
        let blend = (preview.mode, preview.float);
        gpu.stroke_begin(doc.0, size, app.current_zoom(), preview.color, preview.brush_opacity, preview.linear, Some((preview.layer, preview.lock)), blend)
    } else {
        gpu.stroke_begin(doc.0, size, app.current_zoom(), preview.color, preview.opacity, preview.linear, None, (photocraft_color::BlendMode::Normal, false))
    }
    .ok_or("the GPU couldn't start the overlay")?;
    let pts = drag_points(app, d, 0);
    let fed = d.points.len();
    let dabs = preview.push(&pts);
    gpu.stroke_dabs(slot, &dabs);
    Ok(GpuStroke { preview, doc, slot, fed, dabs, in_layer })
}

/// Draw the drag points the Large Brush Mode stroke hasn't seen yet. True when there is one.
fn feed_gpu_stroke(app: &mut PhotocraftApp) -> bool {
    if app.gpu_stroke.is_none() {
        return false;
    }
    if app.drag.is_none() || app.gpu.is_none() {
        end_gpu_stroke(app);
        return true;
    }
    let (Some(d), Some(gpu)) = (app.drag.as_ref(), app.gpu.clone()) else { return true };
    let from = app.gpu_stroke.as_ref().map_or(0, |g| g.fed);
    let pts = drag_points(app, d, from);
    let len = d.points.len();
    if pts.is_empty() {
        return true;
    }
    let started = crate::gpu_canvas::now_ms();
    let mut fresh = None;
    if let Some(g) = app.gpu_stroke.as_mut() {
        g.fed = len;
        let dabs = g.preview.push(&pts);
        gpu.stroke_dabs(g.slot, &dabs);
        if g.in_layer {
            fresh = Some((g.doc, dabs.clone()));
        }
        g.dabs.extend(dabs);
    }
    if let Some((doc, dabs)) = fresh {
        live_damage(app, doc, &dabs);
        super::textures::refresh_live_now(app);
    }
    app.perf.stage.stroke_ms += crate::gpu_canvas::now_ms() - started;
    true
}

/// A released Large Brush Mode stroke waiting to land in the document: its overlay slot keeps
/// showing it meanwhile. Strokes land in release order; each is set up on the session (and its
/// full-precision render started on a worker thread) once the one before it has landed, so it
/// paints over that one's pixels.
pub(crate) struct QueuedCommit {
    slot: usize,
    cmd: &'static str,
    p: serde_json::Value,
    doc: photocraft_doc::DocId,
    /// The worker rendering it, once started.
    rx: Option<std::sync::mpsc::Receiver<Result<photocraft_engine::brush_cmds::LiveStroke, String>>>,
    queued: f64,
    /// The stroke's dabs, paint colour (document encoding) and opacity as its overlay shows
    /// them: what an ⌥-click picks before the write lands (`pending_paint`).
    dabs: Vec<photocraft_engine::brush_cmds::gpu_preview::GpuDab>,
    paint: [f32; 3],
    opacity: f32,
    /// Painted into its layer (the layer, its transparency lock, the brush's own opacity), not
    /// laid over the canvas: an ⌥-click pick paints it into a copy of the picked pixels
    /// (`pending_in_layer`) instead of laying its colour over the sample.
    in_layer: Option<(photocraft_doc::LayerId, bool, f32)>,
    /// The brush and colours as they were at release: the stroke renders and lands with them,
    /// however they change before it does (`with_tools`), so brush and colour commands needn't
    /// wait for it.
    tools: photocraft_engine::ToolState,
}

/// Run `f` with the session's tool state (brush, colours) set to `tools`, then put the current
/// one back.
fn with_tools<R>(app: &mut PhotocraftApp, tools: &photocraft_engine::ToolState, f: impl FnOnce(&mut PhotocraftApp) -> R) -> R {
    let current = std::mem::replace(&mut app.session.tools, tools.clone());
    let r = f(app);
    app.session.tools = current;
    r
}

/// Commands that change only the brush, the colours, the tool or which layers are selected: they
/// don't read or change the document's pixels, so they run at once, over Large Brush Mode
/// strokes still being written (which keep the brush, colours and layer they were painted with,
/// and still match the document: `LiveCommit::base_doc`). Waiting for those writes paused a
/// brush resize, colour change or layer click after a big stroke for up to 3 s.
pub(crate) fn tool_state_only(id: &str) -> bool {
    matches!(
        id,
        "layer.select"
            | "layer.selectTop"
            | "layer.selectBottom"
            | "layer.selectAbove"
            | "layer.selectBelow"
            | "layer.selectInRect"
            | "layer.selectLinkedLayers"
            | "layer.selectUnderCursor"
            | "tools.setBrush"
            | "tools.setColors"
            | "tools.swapColors"
            | "tools.defaultColors"
            | "tools.increaseBrushSize"
            | "tools.decreaseBrushSize"
            | "tools.increaseBrushHardness"
            | "tools.decreaseBrushHardness"
            | "tools.rotateBrush"
            | "tools.nextBrush"
            | "tools.previousBrush"
            | "tools.firstBrush"
            | "tools.lastBrush"
    )
}

/// The threads Large Brush Mode strokes are written on: every core but two (one at least),
/// apart from the shared pool the UI's parallel work runs on. `None` if it can't be made.
#[cfg(not(target_arch = "wasm32"))]
fn commit_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
        rayon::ThreadPoolBuilder::new().num_threads(cores.saturating_sub(2).max(1)).thread_name(|i| format!("pc-stroke-commit-{i}")).build().ok()
    })
    .as_ref()
}

/// Start rendering `c` on a worker thread (the engine's live stroke over its points and seed).
/// False when it can't start that way (the land renders it then).
fn start_commit(app: &mut PhotocraftApp, c: &mut QueuedCommit) -> bool {
    let prepared = with_tools(app, &c.tools, |app| photocraft_engine::brush_cmds::LiveStroke::prepare_with(&app.session, c.cmd, &c.p));
    let Ok((mut live, pts)) = prepared else { return false };
    let (tx, rx) = std::sync::mpsc::channel();
    // The canvas polls it each frame (`sweep_gpu_stroke` keeps frames coming meanwhile).
    let spawned = std::thread::Builder::new()
        .name("pc-stroke-commit".into())
        .spawn(move || {
            // On its own threads, two cores short of all: the UI's parallel work (the canvas
            // refresh, the next stroke's set-up) never queues behind a big write.
            // The changed tiles are converted to the canvas's texels here too, so the frame that
            // lands the stroke only uploads them (converting a big stroke on a 32-bit layer
            // took most of that frame's 85–185 ms).
            let write = |live: &mut photocraft_engine::brush_cmds::LiveStroke| {
                let r = live.push(&pts);
                if r.is_ok()
                    && let Some((surf, pre, bounds)) = live.painted()
                {
                    photocraft_gpu::preconvert(surf, bounds, Some(pre));
                }
                r
            };
            #[cfg(not(target_arch = "wasm32"))]
            let r = match commit_pool() {
                Some(pool) => pool.install(|| write(&mut live)),
                None => write(&mut live),
            };
            // rayon is a desktop-only dependency: on the web the write runs on this thread.
            #[cfg(target_arch = "wasm32")]
            let r = write(&mut live);
            let _ = tx.send(r.map(|_| live).map_err(|e| e.to_string()));
        })
        .is_ok();
    if spawned {
        c.rx = Some(rx);
    }
    spawned
}

/// Land queued Large Brush Mode strokes, oldest first: those whose writes are done, or every
/// one when `wait` (before any command, and before a stroke that isn't Large Brush Mode, so
/// the document they see has them). Each landed stroke's overlay goes with the canvas refresh
/// that shows it (`committed_shown`).
pub(crate) fn finish_gpu_commit(app: &mut PhotocraftApp, wait: bool) {
    if app.gpu_commit_landing {
        return;
    }
    app.gpu_commit_landing = true;
    while let Some(mut c) = app.gpu_commits.pop_front() {
        if c.rx.is_none() && !start_commit(app, &mut c) {
            // Rendered on this thread instead.
            land(app, c, None);
            continue;
        }
        // Some(done) when its write finished (or failed: `done` is then `None`).
        let got: Option<Option<photocraft_engine::brush_cmds::LiveStroke>> = match c.rx.as_ref() {
            Some(rx) if wait => {
                let t0 = crate::gpu_canvas::now_ms();
                let got = rx.recv().ok().and_then(Result::ok);
                app.perf.stage.wait_ms += crate::gpu_canvas::now_ms() - t0;
                Some(got)
            }
            Some(rx) => match rx.try_recv() {
                Ok(r) => Some(r.ok()),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(None),
            },
            None => Some(None),
        };
        let Some(live) = got else {
            app.gpu_commits.push_front(c);
            break;
        };
        land(app, c, live);
    }
    // The next one renders while the user carries on.
    if let Some(mut c) = app.gpu_commits.pop_front() {
        if c.rx.is_some() || start_commit(app, &mut c) {
            app.gpu_commits.push_front(c);
        } else {
            land(app, c, None);
        }
    }
    app.gpu_commit_landing = false;
}

/// exp(-u) as (1 - u/64)^64: the overlay's dab falloff (`stroke_overlay::DAB_WGSL`).
fn soft_exp(u: f32) -> f32 {
    let mut g = (1.0 - u / 64.0).clamp(0.0, 1.0);
    for _ in 0..6 {
        g *= g;
    }
    g
}

/// Dab `d`'s coverage at document point (px, py), as the overlay's dab shader draws it.
fn dab_value(d: &photocraft_engine::brush_cmds::gpu_preview::GpuDab, px: f32, py: f32) -> f32 {
    let ux = px - d.center[0];
    let uy = -(py - d.center[1]);
    if ux.abs() > d.reach + 1.0 || uy.abs() > d.reach + 1.0 {
        return 0.0;
    }
    let (sn, cs) = d.angle.sin_cos();
    let u = ux * cs + uy * sn;
    let v = -ux * sn + uy * cs;
    let dd = ((u * d.roundness) * (u * d.roundness) + v * v).sqrt();
    if dd.is_nan() || dd >= d.reach + 0.5 {
        return 0.0;
    }
    let val = if dd > d.core {
        let x = (dd - d.core) / d.shoulder.max(1e-3);
        soft_exp(x * x)
    } else {
        1.0
    };
    (val * d.alpha).clamp(0.0, 1.0)
}

/// Large Brush Mode strokes of `doc` still being written that paint at document point (x, y),
/// oldest first: each one's paint colour (document encoding) and how much of it the overlay
/// shows there, averaged over a `size`×`size` square (sampled on at most a 5×5 grid). An
/// ⌥-click colour pick lays these over the document's own sample instead of waiting for the
/// writes, so it picks what the canvas shows (waiting took up to 1.7 s after a big stroke).
pub(crate) fn pending_paint(app: &PhotocraftApp, doc: photocraft_doc::DocId, x: f64, y: f64, size: u32) -> Vec<([f32; 3], f32)> {
    let cx = x.floor() as f32 + 0.5;
    let cy = y.floor() as f32 + 0.5;
    let side = size.clamp(1, 101) as f32;
    let n = size.clamp(1, 5) as usize;
    let at = |i: usize| if n == 1 { 0.0 } else { (i as f32 / (n - 1) as f32 - 0.5) * (side - 1.0) };
    app.gpu_commits
        .iter()
        .filter(|c| c.doc == doc && c.in_layer.is_none())
        .filter_map(|c| {
            let mut sum = 0.0f32;
            for j in 0..n {
                for i in 0..n {
                    sum += dabs_coverage(&c.dabs, cx + at(i), cy + at(j));
                }
            }
            let a = (sum / (n * n) as f32 * c.opacity).clamp(0.0, 1.0);
            (a > 0.0).then_some((c.paint, a))
        })
        .collect()
}

/// The coverage `dabs` build at document point (px, py), as the stroke buffer builds it: each
/// dab towards its own ceiling.
pub(crate) fn dabs_coverage(dabs: &[photocraft_engine::brush_cmds::gpu_preview::GpuDab], px: f32, py: f32) -> f32 {
    dabs.iter().fold(0.0f32, |acc, d| {
        let o = dab_value(d, px, py);
        if o > 0.0 && d.ceil > acc { acc + o * (d.ceil - acc) } else { acc }
    })
}

/// A Large Brush Mode stroke painted into its layer and still being written: its layer, whether
/// that layer's transparency is locked, its paint colour (document values) and brush opacity,
/// and its dabs.
pub(crate) struct PendingInLayer {
    pub(crate) layer: photocraft_doc::LayerId,
    pub(crate) lock: bool,
    pub(crate) paint: [f32; 3],
    pub(crate) opacity: f32,
    pub(crate) dabs: Vec<photocraft_engine::brush_cmds::gpu_preview::GpuDab>,
}

/// The strokes of `doc` painted into layers and still being written, oldest first: an ⌥-click
/// pick paints them into a copy of the picked pixels and samples the layer stack as usual, so
/// it picks the colour the canvas shows (under the layers above, through the layer's mask and
/// blend mode) without waiting for the writes.
pub(crate) fn pending_in_layer(app: &PhotocraftApp, doc: photocraft_doc::DocId) -> Vec<PendingInLayer> {
    app.gpu_commits
        .iter()
        .filter(|c| c.doc == doc)
        .filter_map(|c| {
            let (layer, lock, opacity) = c.in_layer?;
            Some(PendingInLayer { layer, lock, paint: c.paint, opacity, dabs: c.dabs.clone() })
        })
        .collect()
}

/// Name what made the UI wait for Large Brush Mode writes (`wait_ms` grew from `before`): the
/// stroke trace's `waited_for` column and a line on stderr, so a pause can be traced to its cause.
pub(crate) fn note_gpu_wait(app: &mut PhotocraftApp, before: f64, why: &str) {
    let ms = app.perf.stage.wait_ms - before;
    if ms < 1.0 {
        return;
    }
    if app.stroke_trace.enabled() {
        eprintln!("Large Brush Mode: waited {ms:.0} ms for a stroke's write before {why}");
    }
    if !app.perf.stage.wait_for.is_empty() {
        app.perf.stage.wait_for.push_str("; ");
    }
    app.perf.stage.wait_for.push_str(why);
}

/// Land the oldest queued stroke now (all overlay slots are taken), and stop showing strokes
/// that already landed: a new stroke needs a slot.
fn land_oldest_commit(app: &mut PhotocraftApp) {
    finish_one(app);
    let mut landed: Vec<usize> = app.gpu_landed.drain(..).map(|(slot, _, _)| slot).collect();
    // A stroke landing in bands gives its slot up too; the rows it hadn't refreshed go with the
    // edit just landed (`flush_band`).
    if let Some(b) = &mut app.gpu_band {
        landed.append(&mut b.slots);
    }
    if let Some(gpu) = &app.gpu {
        for slot in landed {
            gpu.stroke_end(slot);
        }
    }
}

/// Land the oldest queued stroke, waiting for its write.
fn finish_one(app: &mut PhotocraftApp) {
    if app.gpu_commit_landing {
        return;
    }
    app.gpu_commit_landing = true;
    if let Some(mut c) = app.gpu_commits.pop_front() {
        let t0 = crate::gpu_canvas::now_ms();
        let live = if c.rx.is_some() || start_commit(app, &mut c) { c.rx.as_ref().and_then(|rx| rx.recv().ok()).and_then(Result::ok) } else { None };
        app.perf.stage.wait_ms += crate::gpu_canvas::now_ms() - t0;
        land(app, c, live);
    }
    app.gpu_commit_landing = false;
}

/// Put released stroke `c` into the document: from its finished live stroke when there is one
/// (the commit takes those pixels), rendered here otherwise.
fn land(app: &mut PhotocraftApp, c: QueuedCommit, live: Option<photocraft_engine::brush_cmds::LiveStroke>) {
    let offered = live.is_some_and(|live| app.session.offer_live_stroke(live).is_ok());
    if !offered {
        app.session.clear_live_stroke();
    }
    let started = crate::gpu_canvas::now_ms();
    let tools = c.tools.clone();
    let result = with_tools(app, &tools, |app| app.run(c.cmd, c.p));
    let took = crate::gpu_canvas::now_ms() - started;
    app.perf.stage.stroke_ms += took;
    // With the stroke trace on: where the landing's time went (applying includes rendering the
    // stroke again when its background write no longer matched the document).
    if app.stroke_trace.enabled() {
        let [apply, compare, history, edit] = app.session.edit_timing;
        eprintln!(
            "Large Brush Mode landing: {took:.1} ms (background write {}; edit {edit:.1}: apply {apply:.1}, compare {compare:.1}, history {history:.1}; rest {:.1})",
            if offered { "offered" } else { "unusable" },
            took - edit
        );
    }
    app.session.clear_live_stroke();
    log::debug!("large brush commit landed after {:.0} ms", crate::gpu_canvas::now_ms() - c.queued);
    match result {
        // The overlay stays until the canvas refresh shows the stroke (`committed_shown`).
        Ok(_) => app.gpu_landed.push((c.slot, c.doc, 0)),
        Err(e) => {
            if let Some(gpu) = &app.gpu {
                gpu.stroke_end(c.slot);
            }
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
}

/// The canvas of `doc` has just drawn the pixels landed Large Brush Mode strokes wrote: their
/// overlays go now, in the same frame, so a stroke neither blinks out nor shows twice.
pub(crate) fn committed_shown(app: &mut PhotocraftApp, doc: photocraft_doc::DocId) {
    if app.gpu_landed.iter().all(|(_, d, _)| *d != doc) {
        return;
    }
    let (done, keep): (Vec<_>, Vec<_>) = app.gpu_landed.drain(..).partition(|(_, d, _)| *d == doc);
    app.gpu_landed = keep;
    if let Some(gpu) = &app.gpu {
        for (slot, _, _) in done {
            gpu.stroke_end(slot);
        }
    }
}

/// A big landed Large Brush Mode stroke refreshing a band of rows per frame: uploading every
/// tile of it at once took 100–230 ms on an RTX 4090 (about 0.17 ms per 256 px tile of a
/// 16/32-bit layer). Its overlay keeps covering the rows not refreshed yet and gives way band by
/// band, so nothing blinks or shows twice.
pub(crate) struct LandingBand {
    doc: photocraft_doc::DocId,
    /// The document revision it lands, and the document: any other edit refreshes the rest at
    /// once (`flush_band`); a revision that didn't change the document (a layer selected) doesn't.
    revision: u64,
    base: std::sync::Weak<photocraft_doc::Document>,
    region: DRect,
    /// Where the next band starts.
    next: i32,
    /// The overlay slots of the strokes it lands.
    slots: Vec<usize>,
}

/// Changed tiles a band uploads at most: 64 tiles of RGBA16F are 32 MiB, about 11 ms.
const BAND_TILES: i64 = 64;

/// Tile columns and rows `r` spans.
fn tile_span(r: DRect) -> (i64, i64) {
    let t = photocraft_geom::TILE_SIZE;
    let cols = i64::from((r.x1 - 1).div_euclid(t) - r.x0.div_euclid(t) + 1);
    let rows = i64::from((r.y1 - 1).div_euclid(t) - r.y0.div_euclid(t) + 1);
    (cols.max(1), rows.max(1))
}

/// The band of `region` from row `next`: whole tile rows, about [`BAND_TILES`] tiles.
pub(crate) fn next_band(region: DRect, next: i32) -> DRect {
    let t = photocraft_geom::TILE_SIZE;
    let (cols, _) = tile_span(region);
    let rows = (BAND_TILES / cols).clamp(1, 64) as i32;
    let y1 = next.div_euclid(t).saturating_add(rows).saturating_mul(t).min(region.y1);
    DRect::new(region.x0, next, region.x1, y1)
}

/// Land `region` of `doc` (revision `revision`) in bands when it is big enough to be worth it,
/// taking over the overlays of its landed strokes; then refresh the first band. `None` when
/// it isn't banded: a small stroke, a document with layer effects (they reach past a band,
/// into rows not refreshed yet), Pattern Preview, or a preview shown in place of the document.
#[allow(clippy::too_many_arguments)]
pub(crate) fn start_bands(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    gpu: &crate::gpu_canvas::GpuCanvas,
    doc: &photocraft_doc::Document,
    revision: u64,
    region: DRect,
    raw_key: u64,
    display: Option<&photocraft_engine::display_color::CanvasDisplay>,
) -> Option<crate::gpu_canvas::Refresh> {
    let (cols, rows) = tile_span(region);
    if region.is_empty() || cols * rows <= BAND_TILES * 2 || raw_key != 0 || app.ui.view.pattern_preview || super::textures::effect_reach(&doc.layers) > 0 {
        return None;
    }
    let slots: Vec<usize> = app.gpu_landed.iter().filter(|(_, d, _)| *d == doc.id).map(|(s, _, _)| *s).collect();
    if slots.is_empty() || app.gpu_band.is_some() {
        return None;
    }
    // The overlay lays a stroke over everything; layers above the painted one put the landed
    // pixels under theirs. Landing that band by band swept those layers over the stroke like a
    // scanline: such a stroke lands in one refresh instead.
    // Strokes painted into their layers land in place, band by band, with no such sweep.
    let target = app.session.active().filter(|st| st.doc.id == doc.id).and_then(|st| st.active_layer);
    let over_canvas = slots.iter().any(|&s| !gpu.stroke_in_layer(s));
    if over_canvas && target.is_none_or(|id| covered_above(doc, id, region)) {
        return None;
    }
    app.gpu_landed.retain(|(_, d, _)| *d != doc.id);
    let base = app.session.documents().iter().find(|st| st.doc.id == doc.id).map_or_else(std::sync::Weak::new, |st| std::sync::Arc::downgrade(&st.doc));
    app.gpu_band = Some(LandingBand { doc: doc.id, revision, base, region, next: region.y0, slots });
    step_band(app, ctx, gpu, doc, display)
}

/// Whether a visible layer above `layer` in `doc` can show over `region`: pixel layers with
/// content there, and every other kind (adjustments, fills, type, groups' own effects).
fn covered_above(doc: &photocraft_doc::Document, layer: photocraft_doc::LayerId, region: DRect) -> bool {
    let walk = doc.walk();
    let Some(at) = walk.iter().position(|(_, _, l)| l.id == layer) else { return true };
    walk.iter().skip(at + 1).any(|(_, _, l)| {
        l.visible
            && match l.surface() {
                // Tiles it stores there (or a default pixel that isn't clear): cheap and on the safe side.
                Some(s) if l.children().is_none() => s.default_pixel().iter().any(|v| *v != 0.0) || region.tiles().any(|c| s.tile(c).is_some()),
                _ => l.children().is_none(),
            }
    })
}

/// Refresh the next band of the stroke landing in `doc`, if there is one. The last band ends
/// its overlays; a refresh the GPU couldn't do ends the banding (the canvas redraws anyway).
pub(crate) fn step_band(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    gpu: &crate::gpu_canvas::GpuCanvas,
    doc: &photocraft_doc::Document,
    display: Option<&photocraft_engine::display_color::CanvasDisplay>,
) -> Option<crate::gpu_canvas::Refresh> {
    let b = app.gpu_band.as_mut().filter(|b| b.doc == doc.id)?;
    let band = next_band(b.region, b.next);
    // Rows the band refreshes show the written pixels: the strokes give way there first (a
    // stroke painted into its layer would otherwise be laid in once more over its own pixels).
    let shown = DRect::new(i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, band.y1);
    for &slot in &b.slots {
        gpu.stroke_hide(slot, shown);
    }
    let r = gpu.refresh_band(doc.id.0, doc, band, display);
    b.next = band.y1;
    let failed = !r.kind.starts_with("gpu");
    if failed || band.y1 >= b.region.y1 || band.is_empty() {
        if let Some(b) = app.gpu_band.take() {
            if failed {
                // Shown until the canvas redraws, as any landed stroke.
                app.gpu_landed.extend(b.slots.into_iter().map(|s| (s, b.doc, 0)));
            } else {
                for slot in b.slots {
                    gpu.stroke_end(slot);
                }
            }
        }
    } else {
        ctx.request_repaint();
    }
    Some(r)
}

/// End the banding of `doc` when the document has moved on from the revision it lands (another
/// edit): returns the rows it hadn't refreshed, for that edit's refresh to include; its overlays
/// go with that refresh (`committed_shown`).
pub(crate) fn flush_band(app: &mut PhotocraftApp, doc: photocraft_doc::DocId, revision: u64) -> Option<DRect> {
    let current = app.session.documents().iter().find(|st| st.doc.id == doc).map(|st| st.doc.clone());
    let b = app.gpu_band.as_mut().filter(|b| b.doc == doc && b.revision != revision)?;
    // Only the revision moved (a layer selected): the band carries on under the new one.
    if b.base.upgrade().zip(current.as_ref()).is_some_and(|(a, c)| std::sync::Arc::ptr_eq(&a, c)) {
        b.revision = revision;
        return None;
    }
    let b = app.gpu_band.take()?;
    app.gpu_landed.extend(b.slots.iter().map(|&s| (s, b.doc, 0)));
    let rest = DRect::new(b.region.x0, b.next.max(b.region.y0), b.region.x1, b.region.y1);
    (!rest.is_empty()).then_some(rest)
}

/// Whether landed strokes of `doc` wait for the canvas refresh that shows them (it draws them
/// at once rather than in background blocks, so the overlay can go in the same frame).
pub(crate) fn committing(app: &PhotocraftApp, doc: photocraft_doc::DocId) -> bool {
    app.gpu_landed.iter().any(|(_, d, _)| *d == doc)
}

/// Drop the Large Brush Mode stroke being drawn and its overlay (cancelled or abandoned).
pub(crate) fn end_gpu_stroke(app: &mut PhotocraftApp) {
    if let (Some(g), Some(gpu)) = (app.gpu_stroke.take(), &app.gpu) {
        gpu.stroke_end(g.slot);
    }
}

/// Once a frame: queued Large Brush Mode strokes land as their writes finish; a stroke's overlay
/// never outlives its drag or document (an Escape, a lost pointer or a document switch ends the
/// drag without a release); a landed stroke's overlay waits for the canvas refresh that shows it
/// (`committed_shown`), a few frames at most.
pub(crate) fn sweep_gpu_stroke(app: &mut PhotocraftApp, ctx: &egui::Context) {
    // A stroke landing in bands finishes before the next lands (that would refresh the rest at
    // once); the next one's write carries on meanwhile.
    if app.gpu_band.as_ref().is_some_and(|b| app.session.documents().iter().all(|st| st.doc.id != b.doc))
        && let (Some(b), Some(gpu)) = (app.gpu_band.take(), &app.gpu)
    {
        for slot in b.slots {
            gpu.stroke_end(slot);
        }
    }
    if !app.gpu_commits.is_empty() && app.gpu_band.is_none() {
        finish_gpu_commit(app, false);
        if !app.gpu_commits.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }
    let active = app.session.active().map(|st| st.doc.id);
    let mut expired = Vec::new();
    for (slot, doc, frames) in app.gpu_landed.iter_mut() {
        *frames += 1;
        if *frames > 3 || Some(*doc) != active {
            expired.push(*slot);
        }
    }
    if !expired.is_empty() {
        app.gpu_landed.retain(|(slot, _, _)| !expired.contains(slot));
        if let Some(gpu) = &app.gpu {
            for slot in expired {
                gpu.stroke_end(slot);
            }
        }
    }
    if app.gpu_stroke.as_ref().is_some_and(|g| app.drag.is_none() || active != Some(g.doc)) {
        end_gpu_stroke(app);
    }
    // Nothing drawn, queued or landing, yet something shows (a lost device's leftovers).
    if app.gpu_stroke.is_none()
        && app.gpu_commits.is_empty()
        && app.gpu_landed.is_empty()
        && app.gpu_band.is_none()
        && let Some(gpu) = &app.gpu
        && gpu.stroke_showing()
    {
        gpu.stroke_end_all();
    }
}

/// Cancel uncommitted mixer paint when its document, tool or input context goes away.
pub(crate) fn cancel_mixer_on_context_change(app: &mut PhotocraftApp, ctx: &egui::Context) -> bool {
    if !app.drag.as_ref().is_some_and(|drag| drag.tool == Tool::MixerBrush) {
        return false;
    }
    let changed = app.ui.tool != Tool::MixerBrush
        || app.live_stroke.as_ref().is_some_and(|live| !app.session.active().is_some_and(|st| st.doc.id == live.doc && st.revision == live.revision));
    if changed || ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        app.drag = None;
        app.live_stroke = None;
        return true;
    }
    false
}

/// Render the drag points the live stroke hasn't seen yet, with the pen pressure, tilt and
/// rotation the commit's `paint.stroke` gets for them.
pub(super) fn feed_live_stroke(app: &mut PhotocraftApp) {
    feed_live_stroke_waiting(app, LIVE_STROKE_WAIT);
}

/// [`feed_live_stroke`], waiting at most `wait` for the stroke to paint (tests: not at all, to
/// hand points to a stroke that is busy).
pub(super) fn feed_live_stroke_waiting(app: &mut PhotocraftApp, wait: std::time::Duration) {
    // A batched move replays several samples in one frame and calls this once at the end, so the
    // per-sample calls from `tool_event` are skipped (see `canvas_view`).
    if app.defer_live_stroke {
        return;
    }
    if feed_gpu_stroke(app) {
        return;
    }
    let (Some(l), Some(d)) = (app.live_stroke.as_mut(), app.drag.as_ref()) else { return };
    if l.preview_failed {
        return;
    }
    let pose = &app.stylus.stroke;
    // The same times `stroke_points` gives the commit (from the drag's first point).
    let t0 = app.stylus.point_ms(0);
    let pts: Vec<_> = (l.fed..d.points.len())
        .filter_map(|i| {
            let p = d.points.get(i)?;
            let t = pose.get(i).or(pose.last()).copied().unwrap_or_default();
            let mut sp = photocraft_engine::paint::StrokePoint::new(p[0], p[1], p[2] as f32);
            (sp.tilt_x, sp.tilt_y, sp.rotation) = (t[0], t[1], t[2]);
            sp.time = app.stylus.point_ms(i) - t0;
            Some(sp)
        })
        .collect();
    l.fed = d.points.len();
    let started = crate::gpu_canvas::now_ms();
    l.paint.push(pts);
    let steps = l.paint.catch_up(wait);
    let tool = d.tool;
    take_steps(app, tool, steps);
    app.perf.stage.stroke_ms += crate::gpu_canvas::now_ms() - started;
}

/// How long a frame waits for the live stroke to paint the points it was just given. A light
/// brush is done well within it and shows them in the same frame, as when it painted on the UI
/// thread; a heavy one shows them in a later frame instead of holding this one up. Tests wait for
/// every step, so they see each one.
const LIVE_STROKE_WAIT: std::time::Duration = if cfg!(test) { std::time::Duration::MAX } else { std::time::Duration::from_millis(LIVE_STROKE_WAIT_MS) };
const LIVE_STROKE_WAIT_MS: u64 = 6;

/// Record painted steps on the live stroke. A failed one drops the live stroke (the commit then
/// renders the stroke itself), except the Mixer Brush's, which keeps its last valid preview.
fn take_steps(app: &mut PhotocraftApp, tool: Tool, steps: Vec<Step>) {
    let Some(l) = app.live_stroke.as_mut() else { return };
    for step in steps {
        match step {
            Ok(r) => l.damage.push(r),
            Err(e) => {
                if tool == Tool::MixerBrush {
                    // Keep the last valid preview and its document/revision guard. All input
                    // stays in the drag; release replays it without the preview tile budget.
                    l.preview_failed = true;
                    app.ui.status = e.to_string();
                    app.ui.status_error = true;
                } else {
                    app.live_stroke = None;
                }
                return;
            }
        }
    }
}

/// Is the live stroke still painting points it was given?
pub(super) fn live_stroke_busy(app: &PhotocraftApp) -> bool {
    app.live_stroke.as_ref().is_some_and(|l| l.paint.busy())
}

impl PhotocraftApp {
    /// Is the live stroke still painting, or holding steps the canvas hasn't shown yet? Lets a
    /// test read the canvas only once what it shows is the whole stroke so far (#13477).
    pub fn live_stroke_busy(&self) -> bool {
        live_stroke_busy(self)
    }
}

/// The points the live retouching stroke has rendered.
#[cfg(test)]
pub(crate) fn live_retouch_points(app: &PhotocraftApp) -> Option<Vec<photocraft_engine::paint::StrokePoint>> {
    app.live_stroke.as_ref()?.paint.peek(|stroke| match stroke {
        EngineStroke::Retouch(l) => Some(l.points().to_vec()),
        _ => None,
    })?
}

/// Offer the Clone Stamp's or History Brush's live stroke, or a Dodge, Burn, Sponge, Blur, Sharpen
/// or Smudge one, with the drag points it hasn't rendered yet, to the command
/// `retouch_ui::finish_stroke` commits, which then takes its pixels instead of rendering the
/// stroke again.
pub(super) fn offer_live_retouch(app: &mut PhotocraftApp, d: &Drag) {
    let dab = crate::retouch_ui::dab_params(app, d.tool).is_some();
    if !dab && !matches!(d.tool, Tool::CloneStamp | Tool::HistoryBrush) {
        return;
    }
    let Some(LiveStroke { paint, fed, .. }) = app.live_stroke.take() else { return };
    let (steps, Some(stroke)) = paint.finish() else { return };
    if steps.iter().any(Result::is_err) {
        return;
    }
    let rest: Vec<_> = d.points.get(fed..).unwrap_or_default().iter().map(|q| photocraft_engine::paint::StrokePoint::new(q[0], q[1], q[2] as f32)).collect();
    // A refused offer (the brush changed mid-stroke, say) leaves the commit to render the stroke.
    match stroke {
        EngineStroke::Retouch(live) if !dab => offer_retouch(app, live, &rest),
        EngineStroke::Dab(live) if dab => offer_dab(app, live, &rest),
        _ => {}
    }
}

fn offer_retouch(app: &mut PhotocraftApp, mut live: Box<photocraft_engine::retouch_cmds::LiveRetouch>, rest: &[photocraft_engine::paint::StrokePoint]) {
    if rest.is_empty() || live.push(rest).is_ok() {
        let _ = app.session.offer_live_retouch(*live);
    }
}

fn offer_dab(app: &mut PhotocraftApp, mut live: Box<photocraft_engine::retouch_cmds::LiveDab>, rest: &[photocraft_engine::paint::StrokePoint]) {
    if rest.is_empty() || live.push(rest).is_ok() {
        let _ = app.session.offer_live_dab(*live);
    }
}

/// A pen tap (a drag still at its one press point when released) paints its dab at the highest
/// pressure the pen reached during the contact, not just the first sample's (#1798). A mouse is
/// unchanged at full pressure.
pub(super) fn tap_at_peak_pressure(app: &mut PhotocraftApp) {
    if app.stylus.sample().is_none() {
        return;
    }
    let peak = f64::from(app.stylus.tap_pressure());
    let mut refresh_mixer = false;
    if let Some(d) = app.drag.as_mut().filter(|d| d.points.len() == 1)
        && let Some(p) = d.points.first_mut()
    {
        refresh_mixer = d.tool == Tool::MixerBrush && peak > p[2];
        p[2] = p[2].max(peak);
    }
    if refresh_mixer {
        app.live_stroke = begin_live_stroke(app);
    }
}

/// While a stroke is held still, its point repeats this often (in ms) so time-based brush
/// features (airbrush build-up, smoothing catch-up) keep going, as when the pen moves.
pub(super) const HOLD_REPEAT_MS: f64 = 16.0;

/// Is the live stroke one that changes while the pointer is held still (airbrush Build-up, or
/// smoothing catching up)? Only then does a held pointer repeat its point and keep repainting.
pub(super) fn stroke_wants_time(app: &PhotocraftApp) -> bool {
    app.live_stroke.as_ref().is_some_and(LiveStroke::wants_time)
}

/// Time of sample `k` of the `n` samples a frame delivered, spread evenly after `base` (the
/// previous point) up to `now` (this frame): egui's pointer events carry no timestamps.
pub(crate) fn spread_ms(base: f64, now: f64, k: usize, n: usize) -> f64 {
    // Not after `base` (or not comparable, NaN): every sample takes the frame time.
    if n == 0 || now.partial_cmp(&base) != Some(std::cmp::Ordering::Greater) {
        return now;
    }
    base + (now - base) * (k + 1) as f64 / n as f64
}

/// Tools whose gesture follows a freehand path (a polyline of the input points), so every pointer
/// sample the OS delivered improves the result. Other tools are driven by the pointer's latest
/// position (endpoints, anchors, guides), so feeding them a whole frame's moves only repeats work.
pub(crate) fn freehand_tool(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::Brush
            | Tool::Pencil
            | Tool::MixerBrush
            | Tool::ColorReplacement
            | Tool::Eraser
            | Tool::BackgroundEraser
            | Tool::HistoryBrush
            | Tool::Remove
            | Tool::ArtHistoryBrush
            | Tool::SpotHealing
            | Tool::Healing
            | Tool::CloneStamp
            | Tool::PatternStamp
            | Tool::Blur
            | Tool::Sharpen
            | Tool::Smudge
            | Tool::Dodge
            | Tool::Burn
            | Tool::Sponge
            | Tool::Lasso
            | Tool::MagneticLasso
            | Tool::Patch
            | Tool::ContentAwareMove
            | Tool::QuickSelection
            | Tool::FreeformPen
    )
}

/// The pointer moves this frame delivered while `button` was held, in order. `down_at_start` is
/// whether the button was already down when the frame began. A move before a press or after a
/// release in the same frame is dropped, so a gesture never picks up input from outside its own
/// press..release interval.
pub(super) fn pointer_moves(events: &[egui::Event], button: egui::PointerButton, down_at_start: bool) -> Vec<Pos2> {
    let mut down = down_at_start;
    let mut out = Vec::new();
    for e in events {
        match e {
            egui::Event::PointerButton { button: b, pressed, .. } if *b == button => down = *pressed,
            egui::Event::PointerMoved(p) if down => out.push(*p),
            _ => {}
        }
    }
    out
}

/// Modifiers attached to the pointer event itself. Some input sources provide these without a
/// separate `ModifiersChanged` event, so use them for click gestures before falling back to the
/// frame-wide modifier state.
pub(super) fn pointer_button_modifiers(events: &[egui::Event], button: egui::PointerButton) -> Option<egui::Modifiers> {
    events.iter().rev().find_map(|e| match e {
        egui::Event::PointerButton { button: b, modifiers, .. } if *b == button => Some(*modifiers),
        _ => None,
    })
}

/// The line from the brush to the pointer while stroke smoothing lags (Pulled String Mode and
/// Stroke Catch-up, #3479): a dark underlay and a light line stay visible on any pixels, and a dot
/// marks the brush end.
pub(super) fn draw_smoothing_string(painter: &egui::Painter, xf: &ViewXform, (brush, pointer): ([f64; 2], [f64; 2])) {
    let (a, b) = (xf.to_screen(brush[0] as f32, brush[1] as f32), xf.to_screen(pointer[0] as f32, pointer[1] as f32));
    if a.distance(b) < 2.0 {
        return;
    }
    painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(110)));
    painter.line_segment([a, b], Stroke::new(1.0, Color32::WHITE));
    painter.circle_filled(a, 2.5, Color32::WHITE);
}

/// Commit a Brush, Pencil or Eraser drag. The live stroke already rendered it: its pixels are
/// offered to the commit (`Session::offer_live_stroke`), so release costs no second render, and
/// the canvas then refreshes only what it hadn't shown yet (`CommittedStroke`).
pub(super) fn commit_brush_stroke(app: &mut PhotocraftApp, d: &Drag) {
    if let Some(mut g) = app.gpu_stroke.take() {
        // Large Brush Mode: the stroke joins the queue of strokes to land, rendered once at full
        // precision with the seed its overlay's dabs used, off the UI thread; its overlay keeps
        // showing it meanwhile, and the next stroke can start at once. The overlay gets the
        // release frame's points and the stroke's finishing dabs (smoothing's catch-up) first,
        // so it shows every dab the commit paints.
        let rest = drag_points(app, d, g.fed);
        let mut tail = g.preview.push(&rest);
        tail.extend(g.preview.finish());
        if let Some(gpu) = &app.gpu {
            gpu.stroke_dabs(g.slot, &tail);
        }
        if g.in_layer {
            live_damage(app, g.doc, &tail);
        }
        g.dabs.extend(tail);
        let mut p = stroke_params(app, d, &app.stylus.stroke_points(&d.points));
        p["seed"] = json!(g.preview.seed);
        // Its layer too: selecting another before it lands mustn't move it there.
        p["layer"] = json!(g.preview.layer.0);
        let mut c = QueuedCommit {
            slot: g.slot,
            cmd: stroke_command(app, d.tool),
            p,
            doc: g.doc,
            rx: None,
            queued: crate::gpu_canvas::now_ms(),
            dabs: std::mem::take(&mut g.dabs),
            paint: g.preview.color,
            opacity: g.preview.opacity,
            in_layer: g.in_layer.then_some((g.preview.layer, g.preview.lock, g.preview.brush_opacity)),
            tools: app.session.tools.clone(),
        };
        // The first in line starts rendering now; later ones when those before them land.
        if app.gpu_commits.is_empty() {
            start_commit(app, &mut c);
        }
        app.gpu_commits.push_back(c);
        return;
    }
    let live = app.live_stroke.take();
    let mut p = stroke_params(app, d, &app.stylus.stroke_points(&d.points));
    if let Some(seed) = live.as_ref().and_then(|l| l.seed) {
        p["seed"] = json!(seed);
    }
    let Some(l) = live else {
        let _ = app.run(stroke_command(app, d.tool), p);
        return;
    };
    // Points the live stroke hasn't rendered yet (the release frame's last moves).
    let t0 = app.stylus.point_ms(0);
    let rest: Vec<_> = (l.fed..d.points.len())
        .filter_map(|i| {
            let q = d.points.get(i)?;
            let t = app.stylus.stroke.get(i).or(app.stylus.stroke.last()).copied().unwrap_or_default();
            let mut sp = photocraft_engine::paint::StrokePoint::new(q[0], q[1], q[2] as f32);
            (sp.tilt_x, sp.tilt_y, sp.rotation) = (t[0], t[1], t[2]);
            sp.time = app.stylus.point_ms(i) - t0;
            Some(sp)
        })
        .collect();
    // What the live stroke's thread was still painting, then those points.
    let LiveStroke { paint, doc, key, mut damage, .. } = l;
    let (steps, stroke) = paint.finish();
    let painted = stroke.and_then(|mut stroke| {
        for step in steps.into_iter().chain((!rest.is_empty()).then(|| stroke.push(&rest))) {
            damage.push(step.ok()?);
        }
        Some(stroke)
    });
    // The live stroke can't show these points: the commit renders the stroke itself.
    let Some(stroke) = painted else {
        let _ = app.run(stroke_command(app, d.tool), p);
        return;
    };
    let before = app.session.documents().iter().find(|st| st.doc.id == doc).map(|st| st.doc.clone());
    if let EngineStroke::Brush(stroke) = stroke
        && app.session.offer_live_stroke(*stroke).is_err()
    {
        app.session.clear_live_stroke();
    }
    let started = crate::gpu_canvas::now_ms();
    let result = app.run(stroke_command(app, d.tool), p);
    app.perf.stage.stroke_ms += crate::gpu_canvas::now_ms() - started;
    app.session.clear_live_stroke();
    let Ok(r) = result else { return };
    let after = app.session.documents().iter().find(|st| st.doc.id == doc).map(|st| (st.revision, st.doc.clone()));
    if r.get("fromLive").and_then(serde_json::Value::as_bool) == Some(true)
        && let Some((revision, after)) = after
    {
        let relayout = before.map_or(DRect::EMPTY, |b| photocraft_compose::relayout_damage(&b, &after));
        app.committed_stroke = Some(CommittedStroke { doc, revision, key, damage, relayout });
    } else {
        // Rendered again from scratch: the commit's damage rect refreshes the stroke.
        let shown = CommittedStroke { doc, revision: 0, key, damage, relayout: DRect::EMPTY };
        // Raw preview key 0 = the document itself (its colour display folded in).
        shown_as_document(app, doc, |k| shown.since(k).is_some());
    }
}

/// Pencil (and a Pencil-mode Eraser) dabs sit on the pixel grid. Clicks anywhere inside one
/// pixel must produce the same point, or a ⇧-click line changes with the sub-pixel position.
pub(super) fn snap_aliased_tip(app: &PhotocraftApp, tool: Tool, x: f64, y: f64) -> (f64, f64) {
    let pencil = tool == Tool::Pencil || crate::eraser_ui::pencil_mode(app, tool);
    if !pencil {
        return (x, y);
    }
    let (cx, cy) = photocraft_engine::paint::grid_center(x, y, app.session.tools.brush.size);
    (f64::from(cx), f64::from(cy))
}

/// "mask" when the Layers panel targets (or the canvas shows) the active layer's mask, or the layer
/// is an adjustment layer with one (it has no pixels: #4298), else "pixels".
pub fn paint_target(app: &PhotocraftApp) -> serde_json::Value {
    use photocraft_engine::channel_cmds::ChannelTarget;
    let Some(st) = app.session.active() else { return json!("pixels") };
    // A targeted alpha channel (Channels panel) or Quick Mask mode wins over the layer.
    match st.channel_view.target {
        ChannelTarget::Alpha(i) if i < st.doc.channels.len() => return json!({ "channel": i }),
        ChannelTarget::Composite if st.doc.quick_mask.is_some() => return json!("quickMask"),
        ChannelTarget::LayerMask => return json!("mask"),
        ChannelTarget::Color(_) => {
            if let Some(view) = photocraft_engine::mask_view_cmds::current(st) {
                return json!(if view.filter { "filterMask" } else { "mask" });
            }
            // A colour set edits a raster layer's own pixels. The rows stay selected across layer
            // switches, so an adjustment layer's mask and a layer that needs rasterizing first
            // keep the layer's own target below.
            let raster = st.active_layer.and_then(|id| st.doc.layer(id)).is_some_and(|l| matches!(l.content, photocraft_doc::LayerContent::Raster(_)));
            if raster && let Some(colors) = photocraft_engine::color_channels::selected(&app.session) {
                return colors.params();
            }
        }
        _ => {}
    }
    // A smart object's targeted (or shown) filter mask takes the paint (FILE-215-3).
    if app.filter_mask_targeted() {
        return json!("filterMask");
    }
    let has_mask = st.active_layer.and_then(|id| st.doc.layer(id)).is_some_and(|l| l.mask.is_some());
    // Viewing the mask (⌥-click its thumbnail, #196) paints the mask.
    let viewing = photocraft_engine::mask_view_cmds::current(st).is_some_and(|v| !v.filter);
    let mask = app.ui.mask_target || viewing || crate::adjustment_layer_active(st);
    json!(if mask && has_mask { "mask" } else { "pixels" })
}

/// Colour rows can remain selected behind a viewed mask; restrictions follow what is edited.
/// Several rows stop Move and transforms on any layer, which the engine refuses there too.
pub(crate) fn multiple_color_target(app: &PhotocraftApp) -> bool {
    let Some(st) = app.session.active() else { return false };
    matches!(st.channel_view.target, photocraft_engine::channel_cmds::ChannelTarget::Color(_))
        && photocraft_engine::mask_view_cmds::current(st).is_none()
        && photocraft_engine::color_channels::selected(&app.session).is_some_and(|colors| colors.len() > 1)
}

#[cfg(test)]
mod band_tests {
    use super::{BAND_TILES, DRect, next_band, tile_span};
    use photocraft_geom::TILE_SIZE as T;

    /// Bands run down the landing region in whole tile rows of about `BAND_TILES` tiles, the
    /// first from the region's own top, and together cover it exactly.
    #[test]
    fn landing_bands_cover_the_region_in_tile_rows() {
        let region = DRect::new(100, 300, 100 + 20 * T, 300 + 30 * T);
        let (cols, _) = tile_span(region);
        assert_eq!(cols, 21, "a region off the tile grid spans one more column");
        let mut next = region.y0;
        let mut bands = Vec::new();
        while next < region.y1 {
            let b = next_band(region, next);
            assert!(b.y1 > b.y0 && b.y0 == next && (b.x0, b.x1) == (region.x0, region.x1));
            assert!(b.y1 == region.y1 || b.y1 % T == 0, "bands end on tile rows: {b:?}");
            let (c, r) = tile_span(b);
            assert!(c * r <= BAND_TILES.max(c), "{b:?}: {} tiles", c * r);
            next = b.y1;
            bands.push(b);
        }
        assert_eq!(bands.first().map(|b| b.y0), Some(region.y0));
        assert_eq!(bands.last().map(|b| b.y1), Some(region.y1));
        assert!(bands.len() > 5, "{}", bands.len());
        // A region wider than a band's worth of tiles still goes a tile row at a time.
        let wide = DRect::new(0, 0, (BAND_TILES as i32 + 10) * T, 4 * T);
        assert_eq!(next_band(wide, 0), DRect::new(0, 0, wide.x1, T));
    }

    /// An ⌥-click pick before a stroke lands reads the dab coverage the overlay draws: full in
    /// the core, falling off over the shoulder, nothing past the reach.
    #[test]
    fn pending_paint_reads_the_overlays_dab_coverage() {
        use photocraft_engine::brush_cmds::gpu_preview::GpuDab;
        let d = GpuDab { center: [50.0, 50.0], angle: 0.3, roundness: 1.0, core: 10.0, shoulder: 5.0, reach: 23.0, alpha: 0.8, ceil: 0.5 };
        assert!((super::dab_value(&d, 50.5, 50.5) - 0.8).abs() < 1e-6);
        let mid = super::dab_value(&d, 64.0, 50.0);
        assert!(mid > 0.0 && mid < 0.8, "{mid}");
        assert_eq!(super::dab_value(&d, 80.0, 50.0), 0.0);
        assert_eq!(super::dab_value(&d, 50.0, 200.0), 0.0);
        assert!((super::soft_exp(0.0) - 1.0).abs() < 1e-6 && super::soft_exp(64.0) == 0.0);
    }
}
