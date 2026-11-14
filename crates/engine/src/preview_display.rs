//! The canvas display chain on the CPU, so a dialog preview shows document pixels the way the
//! canvas shows them.
//!
//! The canvas stores a linear composite sRGB-encoded ([`crate::display_color::CanvasDisplay`])
//! and its shader then applies, in order:
//!
//! 1. View › 32-bit Preview Options: exposure as a gain and gamma, in linear light.
//! 2. Either View › OCIO Viewer — a log2 shaper over
//!    [`SHAPER_MIN_STOPS`]..+[`SHAPER_STOPS`] into the baked display / view LUT, so values above
//!    1.0 tone-map instead of clipping — or colour management, Proof Colors and Gamut Warning
//!    as the document's display LUT.
//!
//! [`CanvasPreview`] is that chain for one pixel at a time, built from the same state. A preview
//! that runs it matches the canvas instead of quantising scene-linear values straight to eight
//! bits, which showed a 32-bit document crushed (mid grey 0.18 as 46/255 instead of 119) or, with
//! a viewer on, flat and washed out.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use craft_cms::Lut3d;
use photocraft_color::convert::{linear_to_srgb, srgb_to_linear};
use photocraft_doc::Document;

use crate::color_cmds::ColorState;
use crate::ocio_viewer::{SHAPER_MIN_STOPS, SHAPER_STOPS, VIEWER_LUT};

/// Lattice size of the CPU display LUT, as the canvas's.
const DISPLAY_LUT: usize = 33;

/// How the canvas puts one document's pixels on screen (see the module docs).
#[derive(Debug, Default)]
pub struct CanvasPreview {
    /// The canvas texture holds `srgb_encode(value)`: a linear composite.
    encode_srgb: bool,
    /// 2^exposure of View › 32-bit Preview Options; 0.0 when it does not apply.
    gain: f32,
    /// 1 / gamma of the same.
    inv_gamma: f32,
    /// The display / view LUT (`viewer`) or the display LUT; `None` when neither applies.
    lut: Option<Lut3d>,
    /// `lut` is the OCIO viewer's, read through the log2 shaper.
    viewer: bool,
}

impl CanvasPreview {
    /// Nothing to do: document values go to the screen as they are.
    pub fn is_identity(&self) -> bool {
        !self.encode_srgb && self.gain == 0.0 && self.lut.is_none()
    }

    /// One straight RGB document value as the canvas shows it, 0..1 display values.
    pub fn shown(&self, rgb: [f32; 3]) -> [f32; 3] {
        // The texture value: the canvas encodes a linear composite, values above 1.0 included.
        let enc = |v: f32| if self.encode_srgb { linear_to_srgb(v.clamp(0.0, 65504.0)) } else { v };
        let c = [enc(rgb[0]), enc(rgb[1]), enc(rgb[2])];
        if self.viewer {
            let Some(lut) = &self.lut else { return c };
            // Scene-linear light (the exposure as a gain, as Nuke's viewer does) through the
            // shaper, then the view; the gamma lands on the display values after it.
            let gain = if self.gain > 0.0 { self.gain } else { 1.0 };
            let u = c.map(|v| {
                let lin = srgb_to_linear(v.max(0.0)) * gain;
                ((lin.max(exp2(SHAPER_MIN_STOPS)).log2() - SHAPER_MIN_STOPS) / SHAPER_STOPS).clamp(0.0, 1.0)
            });
            let shown = lut.sample(u);
            return if self.gain > 0.0 { shown.map(|v| v.max(0.0).powf(self.inv_gamma)) } else { shown };
        }
        let c = if self.gain > 0.0 {
            c.map(|v| {
                let lin = srgb_to_linear(v.max(0.0)) * self.gain;
                linear_to_srgb(lin.max(1e-12).powf(self.inv_gamma).min(1.0))
            })
        } else {
            c
        };
        match &self.lut {
            Some(lut) => lut.sample(c.map(|v| v.clamp(0.0, 1.0))),
            None => c,
        }
    }

    /// One straight RGBA document pixel; alpha is coverage and passes through.
    pub fn pixel(&self, p: [f32; 4]) -> [f32; 4] {
        let c = self.shown([p[0], p[1], p[2]]);
        [c[0], c[1], c[2], p[3]]
    }
}

fn exp2(v: f32) -> f32 {
    2f32.powf(v)
}

/// What the canvas does to `doc` right now, cached: previews rebuild per frame and an OCIO view
/// costs a 49³ lattice through the config.
pub fn canvas_preview(c: &ColorState, doc: &Document, ocio_config: &str) -> Arc<CanvasPreview> {
    let key = preview_key(c, doc, ocio_config);
    if let Some(p) = cache().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return p.clone();
    }
    let p = Arc::new(build(c, doc, ocio_config));
    let mut m = cache().lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > 16 {
        m.clear();
    }
    m.insert(key, p.clone());
    p
}

fn cache() -> &'static Mutex<HashMap<u64, Arc<CanvasPreview>>> {
    static C: OnceLock<Mutex<HashMap<u64, Arc<CanvasPreview>>>> = OnceLock::new();
    C.get_or_init(Mutex::default)
}

/// Changes whenever anything feeding [`build`] does.
fn preview_key(c: &ColorState, doc: &Document, ocio_config: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    c.display_signature_for(doc, c.main_display).hash(&mut h);
    let viewer = c.ocio_viewer(doc.id);
    viewer.enabled.hash(&mut h);
    if viewer.enabled {
        crate::ocio_viewer::signature(c, doc, ocio_config).hash(&mut h);
    }
    h.finish()
}

fn build(c: &ColorState, doc: &Document, ocio_config: &str) -> CanvasPreview {
    let display = c.main_display;
    let cm = c.canvas_display_for(doc, display).ok();
    let (gain, inv_gamma) = match c.hdr_preview(doc) {
        Some(h) if h.exposure.is_finite() && h.gamma.is_finite() && h.gamma > 0.0 => (exp2(h.exposure.clamp(-20.0, 20.0)), 1.0 / h.gamma.max(0.01)),
        _ => (0.0, 1.0),
    };
    let mut p = CanvasPreview { encode_srgb: cm.as_ref().is_some_and(|d| d.encode_srgb), gain, inv_gamma, lut: None, viewer: false };
    // View › OCIO Viewer replaces the ICC display transform, as the canvas's mode 4 does.
    let v = c.ocio_viewer(doc.id);
    if v.enabled
        && let Ok(cfg) = crate::ocio_viewer::sync_config(ocio_config)
        && let Ok(lut) = crate::ocio_viewer::viewer_lut(&cfg, &crate::color_cmds::composite_profile(doc), &v, VIEWER_LUT)
    {
        p.lut = Some(lut);
        p.viewer = true;
        return p;
    }
    // Proof Colors / Gamut Warning / the 32-bit preview's own LUT, else plain colour management.
    if let Ok(Some(lut)) = crate::proof_sim::display_lut_with(c, doc, DISPLAY_LUT, false, display) {
        p.lut = Some(lut);
    } else if let Some(t) = cm.as_ref().and_then(|d| d.transform.clone())
        && t.inputs() == 3
        && (t.outputs() == 3 || t.outputs() == 1)
    {
        p.lut = Some(Lut3d::from_transform(&t, DISPLAY_LUT));
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear_doc() -> CanvasPreview {
        CanvasPreview { encode_srgb: true, ..CanvasPreview::default() }
    }

    #[test]
    fn a_linear_composite_is_srgb_encoded_as_the_canvas_texture_is() {
        // Mid grey previews as mid grey, not as the 46/255 a raw quantize gave.
        let v = linear_doc().shown([0.18, 0.18, 0.18])[0];
        assert!((v - 0.4613).abs() < 1e-3, "{v}");
        assert!(!linear_doc().is_identity());
    }

    #[test]
    fn an_integer_document_is_left_alone() {
        let p = CanvasPreview::default();
        assert!(p.is_identity());
        assert_eq!(p.pixel([0.25, 0.5, 0.75, 0.5]), [0.25, 0.5, 0.75, 0.5]);
    }

    #[test]
    fn the_32_bit_preview_exposure_doubles_the_light_and_holds_at_white() {
        let p = CanvasPreview { encode_srgb: true, gain: 2.0, inv_gamma: 1.0, ..CanvasPreview::default() };
        let one_stop_down = linear_doc().shown([0.09, 0.09, 0.09])[0];
        assert!((p.shown([0.09, 0.09, 0.09])[0] - linear_doc().shown([0.18, 0.18, 0.18])[0]).abs() < 1e-3);
        assert!(p.shown([0.09; 3])[0] > one_stop_down);
        assert!((p.shown([4.0; 3])[0] - 1.0).abs() < 1e-6, "clipped at white");
    }

    #[test]
    fn the_viewer_shaper_puts_scene_linear_light_on_the_lattice() {
        // An identity LUT over the shaper returns the shaper coordinate itself: 1.0 of light is
        // half way up a -12..+12 stop range.
        let n = 2;
        let at = |i: usize| f32::from(u8::try_from(i).unwrap_or(0));
        let data = (0..n * n * n).map(|i| [at(i % n), at((i / n) % n), at(i / (n * n)), 1.0]).collect();
        let lut = Lut3d { size: n, data };
        let p = CanvasPreview { encode_srgb: true, lut: Some(lut), viewer: true, ..CanvasPreview::default() };
        let v = p.shown([1.0, 1.0, 1.0])[0];
        assert!((v - 0.5).abs() < 1e-3, "{v}");
        // Four stops over white still has somewhere to go, instead of clipping.
        assert!(p.shown([16.0; 3])[0] > v);
    }
}
