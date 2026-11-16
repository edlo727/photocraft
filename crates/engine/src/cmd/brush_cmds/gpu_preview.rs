//! Large Brush Mode (options bar): while the pen is down the Brush's dabs are drawn on the GPU as
//! a coverage overlay over the canvas, and nothing is rendered into the document; on release the
//! stroke is committed by `paint.stroke` with the same points and seed, at full precision. Each
//! frame then costs the dabs alone, whatever the brush size or document depth, instead of
//! rasterising, compositing and uploading every touched 32-bit tile.
//!
//! Only strokes the overlay can show faithfully take this path: a round computed tip on a visible
//! RGB layer's pixels, with no selection, symmetry, colour-channel restriction, dual brush,
//! texture, wet edges, noise, colour dynamics or erasing. A brush in one of Photoshop's blend
//! modes ([`mode_previewable`]) takes it where the stroke is painted into its layer on the GPU
//! (the overlay over the canvas can only show Normal paint). Anything else keeps the normal live
//! stroke, and [`DabPreview::begin`] says why.

use photocraft_color::{BlendMode, ColorMode, SampleType};
use photocraft_paint::{BrushSettings, Dab, StrokePoint, StrokeRenderer};
use serde_json::Value;

use crate::{EngineError, Result, Session};

/// One dab as the GPU overlay draws it (document pixels).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuDab {
    pub center: [f32; 2],
    /// Radians, counter-clockwise (the CPU tip's rotation).
    pub angle: f32,
    /// Roundness (minor / major axis), as the CPU tip applies it.
    pub roundness: f32,
    /// Solid core radius and soft shoulder width of the tip profile (`photocraft_paint::tip_shape`).
    pub core: f32,
    pub shoulder: f32,
    /// How far from the centre the tip still paints.
    pub reach: f32,
    /// Flow: how far each dab builds the coverage towards its ceiling.
    pub alpha: f32,
    /// The dab's opacity ceiling relative to the stroke's (Transfer › Opacity, e.g. pen
    /// pressure): coverage builds up to it and no further, as the CPU stroke buffer does
    /// (`c + v·(ceiling − c)` where the ceiling is above `c`).
    pub ceil: f32,
}

/// Why the overlay can't show strokes of `b` faithfully, or `None` when it can (see the module
/// docs).
pub fn not_previewable(b: &BrushSettings) -> Option<&'static str> {
    Some(if b.tip.bitmap().is_some() {
        "the brush has a sampled tip (use a round brush)"
    } else if b.dual_brush.enabled {
        "the brush uses Dual Brush"
    } else if b.texture.enabled {
        "the brush uses Texture"
    } else if b.wet_edges {
        "the brush uses Wet Edges"
    } else if b.noise {
        "the brush uses Noise"
    } else if b.aliased || b.square {
        "the brush is aliased"
    } else if b.color_dynamics.enabled {
        "the brush uses Color Dynamics"
    } else if b.erase || b.behind {
        "erasing or Behind mode"
    } else if !mode_previewable(b.mode) {
        "the brush mode is Dissolve, XOR, a Paint.NET mode or a Nuke operation"
    } else {
        return None;
    })
}

/// The brush modes the GPU blends as `paint.stroke` writes them (`photocraft_gpu`'s live paint
/// pass): Normal and Photoshop's blend modes but Dissolve.
pub fn mode_previewable(mode: BlendMode) -> bool {
    !mode.is_nuke()
        && !matches!(mode, BlendMode::PassThrough | BlendMode::Dissolve | BlendMode::Xor | BlendMode::PaintNetColorBurn | BlendMode::PaintNetColorDodge)
}

/// Whether the overlay can show strokes of `b` faithfully.
pub fn previewable(b: &BrushSettings) -> bool {
    not_previewable(b).is_none()
}

/// The dabs of a Large Brush Mode stroke, generated exactly as the commit will generate them.
pub struct DabPreview {
    renderer: StrokeRenderer,
    hardness: f32,
    /// The paint colour in the document's own encoding (straight RGB, 0..1 or linear).
    pub color: [f32; 3],
    /// Brush opacity times the layer's (the stroke's ceiling as it shows).
    pub opacity: f32,
    /// The document is linear light (a 32-bit linear profile): blend the overlay in linear.
    pub linear: bool,
    /// The jitter seed the commit must reuse.
    pub seed: u64,
    /// The layer the stroke paints, whether its transparency is locked, and the brush's own
    /// opacity: what painting the stroke into the layer on the GPU needs.
    pub layer: photocraft_doc::LayerId,
    pub lock: bool,
    pub brush_opacity: f32,
    /// The stroke can be laid over the whole canvas faithfully (the layer blends Normal, and so
    /// does the brush): where it can't be painted into its layer, that is how it shows.
    pub on_top: bool,
    /// The brush's blend mode, and whether the document is 32-bit float (the blend keeps values
    /// above 1): how the stroke blends with its layer's pixels when painted into it.
    pub mode: BlendMode,
    pub float: bool,
    /// The stroke can be painted into its layer on the GPU (the layer has no layer effects:
    /// those are built from the layer's own pixels on the CPU).
    pub in_layer: bool,
    pub doc: photocraft_doc::DocId,
    pub revision: u64,
}

impl DabPreview {
    /// A preview of the `paint.stroke` the params `p` would commit, or `None` when the overlay
    /// can't show it faithfully (the caller then uses the normal live stroke).
    pub fn begin(s: &Session, p: &Value) -> Result<Option<Self>> {
        Ok(Self::begin_or_why(s, p)?.ok())
    }

    /// [`begin`](Self::begin), with the reason when the overlay can't show the stroke (for the
    /// status bar: "Large Brush Mode isn't used for this stroke: …").
    pub fn begin_or_why(s: &Session, p: &Value) -> Result<std::result::Result<Self, &'static str>> {
        const CMD: &str = "paint.stroke";
        super::has_paintable(s).map_err(EngineError::Other)?;
        let captured = crate::channel_cmds::inject_target(s, CMD, p.clone());
        let p = &captured;
        if crate::channel_cmds::target_of(p) != crate::channel_cmds::Target::Pixels {
            return Ok(Err("painting a mask or channel"));
        }
        if crate::color_channels::target(s, p)?.is_some() {
            return Ok(Err("painting selected colour channels"));
        }
        let brush = super::with_blend_mode(super::resolve_brush(s, p, CMD)?, p);
        let (layer, brush, zoom) = super::stroke_target(s, p, brush)?;
        if let Some(why) = not_previewable(&brush) {
            return Ok(Err(why));
        }
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let doc = &st.doc;
        let Some(id) = layer else { return Ok(Err("painting a channel")) };
        if doc.selection.is_some() {
            return Ok(Err("a selection is active"));
        }
        if st.symmetry.is_some() {
            return Ok(Err("painting symmetry is on"));
        }
        if crate::mode_cmds::paint_color_table(doc, p).is_some() {
            return Ok(Err("the document is Indexed Color"));
        }
        let Some(l) = doc.layer(id) else { return Ok(Err("no layer")) };
        if !l.visible {
            return Ok(Err("the layer is hidden"));
        }
        // Paint in a blend mode needs the layer's pixels under it: only painting into the layer
        // shows it.
        let on_top = l.blend == BlendMode::Normal && brush.mode == BlendMode::Normal;
        let in_layer = !photocraft_compose::effects::has_effects(l);
        if !on_top && !in_layer {
            return Ok(Err(if brush.mode == BlendMode::Normal {
                "the layer has layer effects and its blend mode isn't Normal"
            } else {
                "the layer has layer effects and the brush mode isn't Normal"
            }));
        }
        let Some(surf) = l.surface() else { return Ok(Err("not a pixel layer")) };
        let fmt = surf.format();
        if fmt.mode != ColorMode::Rgb {
            return Ok(Err("the document isn't RGB"));
        }
        // Locked transparency (the Background) changes nothing a Normal stroke shows where the
        // layer is opaque, so it previews too; where it is transparent the commit keeps it so.
        let mut native = [0.0f32; 8];
        photocraft_raster::from_rgba_into(&fmt, brush.color, &mut native);
        let linear = crate::cmd::image_cmds::is_linear_rgb_profile(&crate::color_cmds::composite_profile(doc));
        let layer_opacity = if l.opacity.is_finite() { (l.opacity * l.fill_opacity.clamp(0.0, 1.0)).clamp(0.0, 1.0) } else { 1.0 };
        Ok(Ok(Self {
            renderer: StrokeRenderer::new(&brush, Some(fmt), zoom),
            hardness: brush.hardness,
            color: [native[0], native[1], native[2]],
            opacity: brush.opacity.clamp(0.0, 1.0) * layer_opacity,
            linear,
            seed: brush.seed,
            layer: id,
            lock: doc.effective_locks(id).transparency,
            brush_opacity: brush.opacity.clamp(0.0, 1.0),
            on_top,
            mode: brush.mode,
            float: fmt.sample == SampleType::F32,
            in_layer,
            doc: doc.id,
            revision: st.revision,
        }))
    }

    /// The dabs `pts` add.
    pub fn push(&mut self, pts: &[StrokePoint]) -> Vec<GpuDab> {
        let dabs = self.renderer.push_dabs(pts);
        dabs.iter().map(|d| self.gpu_dab(d)).collect()
    }

    /// The dabs finishing the stroke adds (smoothing catch-up, a lone first dab).
    pub fn finish(&mut self) -> Vec<GpuDab> {
        let dabs = self.renderer.finish_dabs();
        dabs.iter().map(|d| self.gpu_dab(d)).collect()
    }

    /// The tip geometry `BrushContext::rasterize_into` uses for a round computed dab.
    fn gpu_dab(&self, d: &Dab) -> GpuDab {
        let r = if d.radius.is_finite() { d.radius.max(0.0) } else { 0.0 };
        let ro = if r > 0.0 { d.roundness.max(0.5 / r).min(1.0) } else { 1.0 };
        let rm = (r * ro).max(0.5);
        let (core, shoulder) = photocraft_paint::tip_shape(rm, self.hardness);
        GpuDab {
            center: [d.center.x as f32, d.center.y as f32],
            angle: if d.angle.is_finite() { d.angle } else { 0.0 },
            roundness: ro,
            core,
            shoulder,
            reach: (core + 2.6 * shoulder).max(rm),
            alpha: d.alpha.clamp(0.0, 1.0),
            ceil: d.opacity.clamp(0.0, 1.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s
    }

    /// A plain round brush previews; its dabs match what the commit renders, in number and
    /// place, and a selection hands the stroke back to the normal live path.
    #[test]
    fn round_brush_strokes_preview_on_the_gpu() {
        let mut s = session();
        let p = json!({"points": [[8.0, 8.0, 1.0], [40.0, 30.0, 1.0]], "zoom": 1.0, "target": "pixels", "seed": 7});
        let mut prev = DabPreview::begin(&s, &p).unwrap().expect("a round brush previews");
        assert_eq!(prev.seed, 7);
        assert!(prev.in_layer && prev.on_top && !prev.lock, "a plain layer paints in place and over the canvas alike");
        assert!((prev.brush_opacity - prev.opacity).abs() < 1e-6, "the layer is at full opacity");
        let pts = [StrokePoint::new(8.0, 8.0, 1.0), StrokePoint::new(40.0, 30.0, 1.0)];
        let mut dabs = prev.push(&pts);
        dabs.extend(prev.finish());
        assert!(dabs.len() > 2, "{dabs:?}");
        assert!(dabs.iter().all(|d| d.reach > 0.0 && d.alpha > 0.0 && d.core <= d.reach));
        let first = dabs[0].center;
        assert!((first[0] - 8.0).abs() < 1.0 && (first[1] - 8.0).abs() < 1.0, "{first:?}");
        s.execute("select.all", json!({})).unwrap();
        assert!(DabPreview::begin(&s, &p).unwrap().is_none(), "a selection keeps the normal path");
        assert_eq!(DabPreview::begin_or_why(&s, &p).unwrap().err(), Some("a selection is active"));
        s.execute("select.deselect", json!({})).unwrap();
        assert!(DabPreview::begin(&s, &p).unwrap().is_some());
    }

    /// Photoshop's blend modes preview (painted into the layer, never laid over the canvas);
    /// Dissolve, XOR, Paint.NET's modes and the Nuke operations keep the normal path.
    #[test]
    fn brush_blend_modes_preview_in_the_layer_only() {
        for mode in [BlendMode::Multiply, BlendMode::Screen, BlendMode::LinearDodge, BlendMode::HardLight, BlendMode::Color, BlendMode::Luminosity] {
            assert!(mode_previewable(mode), "{mode:?}");
        }
        for mode in [BlendMode::Dissolve, BlendMode::Xor, BlendMode::PaintNetColorDodge, BlendMode::NukePlus, BlendMode::PassThrough] {
            assert!(!mode_previewable(mode), "{mode:?}");
        }
        let s = session();
        let p = json!({"points": [[8.0, 8.0, 1.0], [40.0, 30.0, 1.0]], "zoom": 1.0, "target": "pixels", "seed": 7, "mode": "hardLight"});
        let prev = DabPreview::begin(&s, &p).unwrap().expect("a Hard Light brush previews");
        assert_eq!(prev.mode, BlendMode::HardLight);
        assert!(prev.in_layer && !prev.on_top && !prev.float, "blended with the layer's pixels, in an 8-bit document");
        let p = json!({"points": [[8.0, 8.0, 1.0], [40.0, 30.0, 1.0]], "zoom": 1.0, "target": "pixels", "seed": 7, "mode": "dissolve"});
        assert_eq!(DabPreview::begin_or_why(&s, &p).unwrap().err(), Some("the brush mode is Dissolve, XOR, a Paint.NET mode or a Nuke operation"));
    }
}
