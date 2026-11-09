//! Photoshop and Paint.NET blend modes, plus Nuke Merge operations (scalar reference
//! implementation).
//!
//! The Photoshop and Paint.NET modes follow the public PDF / ISO 32000-2 and W3C Compositing
//! definitions, with Photoshop's variants where they differ (Hard Mix, Darker/Lighter Color,
//! Linear Burn/Dodge, Vivid/Linear/Pin Light, Subtract, Divide). They are what layers get by
//! default, and what PSD and the other layered formats read and write.
//!
//! The `Nuke*` modes are Nuke Merge operations with Nuke's own 32-bit maths (premultiplied, no
//! clamping). Blend menus list them only when "Nuke blend modes" is turned on in the Layers panel
//! ([`set_nuke_modes_in_menus`]); a layer keeps its mode either way, so a document renders the
//! same whatever the switch says. This is the CPU reference the GPU kernels must match.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlendMode {
    /// Groups only: children blend directly into the group's backdrop.
    PassThrough,
    Normal,
    Dissolve,
    Darken,
    Multiply,
    ColorBurn,
    // The aliases read documents saved while these Photoshop modes were called `Ps…`.
    #[serde(alias = "PsLinearBurn")]
    LinearBurn,
    #[serde(alias = "PsDarkerColor")]
    DarkerColor,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    #[serde(alias = "PsLighterColor")]
    LighterColor,
    Overlay,
    SoftLight,
    HardLight,
    #[serde(alias = "PsVividLight")]
    VividLight,
    #[serde(alias = "PsLinearLight")]
    LinearLight,
    #[serde(alias = "PsPinLight")]
    PinLight,
    #[serde(alias = "PsHardMix")]
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    #[serde(alias = "PsDivide")]
    Divide,
    #[serde(alias = "PsHue")]
    Hue,
    Saturation,
    Color,
    Luminosity,
    Reflect,
    Glow,
    Negation,
    Xor,
    PaintNetColorBurn,
    PaintNetColorDodge,
    // ---- Nuke Merge operations (`over` is Normal) ----
    NukeMin,
    NukeMultiply,
    NukeColorBurn,
    NukeUnder,
    NukeIn,
    NukeMax,
    NukeScreen,
    NukeColorDodge,
    NukePlus,
    NukeOut,
    NukeOverlay,
    NukeSoftLight,
    NukeHardLight,
    NukeGeometric,
    NukeHypot,
    NukeMask,
    NukeStencil,
    NukeDifference,
    NukeExclusion,
    NukeMinus,
    NukeDivide,
    NukeMatte,
    NukeAverage,
    NukeConjointOver,
    NukeDisjointOver,
    NukeXor,
    NukeFrom,
    NukeAtop,
}

/// Whether blend menus list the Nuke Merge operations ([`BlendMode::NUKE_MODES`]) after
/// Photoshop's modes. The Layers panel's "Nuke blend modes" switch; off by default.
static NUKE_MENU: AtomicBool = AtomicBool::new(false);

/// Show (or hide) the Nuke Merge operations in blend menus ([`BlendMode::menu_modes`]).
pub fn set_nuke_modes_in_menus(on: bool) {
    NUKE_MENU.store(on, Ordering::Relaxed);
}

/// Whether blend menus list the Nuke Merge operations.
pub fn nuke_modes_in_menus() -> bool {
    NUKE_MENU.load(Ordering::Relaxed)
}

impl BlendMode {
    /// Paint.NET modes without an exact Photoshop equivalent.
    pub const PAINT_NET_MODES: [BlendMode; 6] = [Self::Reflect, Self::Glow, Self::Negation, Self::Xor, Self::PaintNetColorBurn, Self::PaintNetColorDodge];

    /// The Photoshop and Paint.NET modes, with Paint.NET modes after Photoshop's menu order.
    pub fn layer_modes() -> impl Iterator<Item = Self> {
        Self::LAYER_MODES.into_iter().chain(Self::PAINT_NET_MODES)
    }

    /// Every mode a layer can have but Pass Through: [`Self::layer_modes`], then the Nuke Merge
    /// operations ([`Self::NUKE_MODES`], [`Self::NUKE_HIDDEN_MODES`]).
    pub fn all_modes() -> impl Iterator<Item = Self> {
        Self::layer_modes().chain(Self::NUKE_MODES).chain(Self::NUKE_HIDDEN_MODES)
    }

    /// The modes blend menus list, in menu order: Photoshop's ([`Self::LAYER_MODES`]), then the
    /// Nuke Merge colour operations ([`Self::NUKE_MODES`]) when they are turned on.
    pub fn menu_modes() -> impl Iterator<Item = Self> {
        let nuke: &'static [BlendMode] = if nuke_modes_in_menus() { &Self::NUKE_MODES } else { &[] };
        Self::LAYER_MODES.into_iter().chain(nuke.iter().copied())
    }

    /// Whether blend menus list this mode ([`Self::menu_modes`]).
    pub fn is_menu_mode(self) -> bool {
        Self::LAYER_MODES.contains(&self) || (nuke_modes_in_menus() && Self::NUKE_MODES.contains(&self))
    }

    /// One of the Nuke Merge operations (a `Nuke…` mode).
    pub fn is_nuke(self) -> bool {
        Self::NUKE_MODES.contains(&self) || Self::NUKE_HIDDEN_MODES.contains(&self)
    }

    /// Kept for older call sites: a mode that is not a Nuke Merge operation (or Normal, `over`).
    pub fn is_photoshop_only(self) -> bool {
        self.nuke_op().is_none() && self != BlendMode::PassThrough
    }

    pub fn has_psd_equivalent(self) -> bool {
        !Self::PAINT_NET_MODES.contains(&self) && !self.is_nuke()
    }

    /// Photoshop modes except PassThrough, in Photoshop menu order.
    pub const LAYER_MODES: [BlendMode; 27] = [
        BlendMode::Normal,
        BlendMode::Dissolve,
        BlendMode::Darken,
        BlendMode::Multiply,
        BlendMode::ColorBurn,
        BlendMode::LinearBurn,
        BlendMode::DarkerColor,
        BlendMode::Lighten,
        BlendMode::Screen,
        BlendMode::ColorDodge,
        BlendMode::LinearDodge,
        BlendMode::LighterColor,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::VividLight,
        BlendMode::LinearLight,
        BlendMode::PinLight,
        BlendMode::HardMix,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Subtract,
        BlendMode::Divide,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    /// The Nuke Merge colour operations blend menus list when they are turned on, grouped like
    /// Photoshop's menu (darken, lighten, contrast, comparative, Nuke-only). `over` is Normal.
    pub const NUKE_MODES: [BlendMode; 18] = [
        BlendMode::NukeMin,
        BlendMode::NukeMultiply,
        BlendMode::NukeColorBurn,
        BlendMode::NukeMax,
        BlendMode::NukeScreen,
        BlendMode::NukeColorDodge,
        BlendMode::NukePlus,
        BlendMode::NukeOverlay,
        BlendMode::NukeSoftLight,
        BlendMode::NukeHardLight,
        BlendMode::NukeDifference,
        BlendMode::NukeExclusion,
        BlendMode::NukeFrom,
        BlendMode::NukeMinus,
        BlendMode::NukeDivide,
        BlendMode::NukeAverage,
        BlendMode::NukeGeometric,
        BlendMode::NukeHypot,
    ];

    /// Nuke Merge operations that are compositing (Porter-Duff / matte) ops rather than colour
    /// blends: in a layer stack B is everything below, so they act on the whole stack (mask,
    /// stencil, in, out, xor), reorder it (under), duplicate clipping masks (atop) or equal over
    /// for straight-alpha layers (matte, conjoint-over, disjoint-over). They still render, so
    /// scripts and documents that use them work, but menus list one only on a layer that has it.
    pub const NUKE_HIDDEN_MODES: [BlendMode; 10] = [
        BlendMode::NukeAtop,
        BlendMode::NukeConjointOver,
        BlendMode::NukeDisjointOver,
        BlendMode::NukeIn,
        BlendMode::NukeMask,
        BlendMode::NukeMatte,
        BlendMode::NukeOut,
        BlendMode::NukeStencil,
        BlendMode::NukeUnder,
        BlendMode::NukeXor,
    ];

    /// Kept for older call sites: [`Self::NUKE_HIDDEN_MODES`].
    pub const HIDDEN_MODES: [BlendMode; 10] = Self::NUKE_HIDDEN_MODES;

    /// The Nuke Merge2 `operation` this mode is: Normal / Pass Through are `over`, a `Nuke…` mode
    /// its own operation; `None` for the Photoshop and Paint.NET modes.
    pub fn nuke_op(self) -> Option<&'static str> {
        Some(match self {
            BlendMode::Normal | BlendMode::PassThrough => "over",
            BlendMode::NukeMin => "min",
            BlendMode::NukeMultiply => "multiply",
            BlendMode::NukeColorBurn => "color-burn",
            BlendMode::NukeUnder => "under",
            BlendMode::NukeIn => "in",
            BlendMode::NukeMax => "max",
            BlendMode::NukeScreen => "screen",
            BlendMode::NukeColorDodge => "color-dodge",
            BlendMode::NukePlus => "plus",
            BlendMode::NukeOut => "out",
            BlendMode::NukeOverlay => "overlay",
            BlendMode::NukeSoftLight => "soft-light",
            BlendMode::NukeHardLight => "hard-light",
            BlendMode::NukeGeometric => "geometric",
            BlendMode::NukeHypot => "hypot",
            BlendMode::NukeMask => "mask",
            BlendMode::NukeStencil => "stencil",
            BlendMode::NukeDifference => "difference",
            BlendMode::NukeExclusion => "exclusion",
            BlendMode::NukeMinus => "minus",
            BlendMode::NukeDivide => "divide",
            BlendMode::NukeMatte => "matte",
            BlendMode::NukeAverage => "average",
            BlendMode::NukeConjointOver => "conjoint-over",
            BlendMode::NukeDisjointOver => "disjoint-over",
            BlendMode::NukeXor => "xor",
            BlendMode::NukeFrom => "from",
            BlendMode::NukeAtop => "atop",
            _ => return None,
        })
    }

    /// The Nuke Merge operation closest to a Photoshop mode, for scripts written for Nuke:
    /// (operation, exact). `over` (inexact) where Nuke has nothing close.
    pub fn nearest_nuke_op(self) -> (&'static str, bool) {
        if let Some(op) = self.nuke_op() {
            return (op, true);
        }
        let op = match self {
            BlendMode::Darken => "min",
            BlendMode::Multiply => "multiply",
            BlendMode::ColorBurn => "color-burn",
            BlendMode::Lighten => "max",
            BlendMode::Screen => "screen",
            BlendMode::ColorDodge => "color-dodge",
            BlendMode::LinearDodge => "plus",
            BlendMode::Overlay => "overlay",
            BlendMode::SoftLight => "soft-light",
            BlendMode::HardLight => "hard-light",
            BlendMode::Difference => "difference",
            BlendMode::Exclusion => "exclusion",
            BlendMode::Subtract => "from",
            _ => "over",
        };
        (op, false)
    }

    pub fn label(self) -> &'static str {
        match self {
            BlendMode::PassThrough => "Pass Through",
            BlendMode::Normal => "Normal",
            BlendMode::Dissolve => "Dissolve",
            BlendMode::Darken => "Darken",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::LinearBurn => "Linear Burn",
            BlendMode::DarkerColor => "Darker Color",
            BlendMode::Lighten => "Lighten",
            BlendMode::Screen => "Screen",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::LinearDodge => "Linear Dodge (Add)",
            BlendMode::LighterColor => "Lighter Color",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::VividLight => "Vivid Light",
            BlendMode::LinearLight => "Linear Light",
            BlendMode::PinLight => "Pin Light",
            BlendMode::HardMix => "Hard Mix",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Subtract => "Subtract",
            BlendMode::Divide => "Divide",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Color",
            BlendMode::Luminosity => "Luminosity",
            BlendMode::Reflect => "Reflect",
            BlendMode::Glow => "Glow",
            BlendMode::Negation => "Negation",
            BlendMode::Xor => "XOR",
            BlendMode::PaintNetColorBurn => "Color Burn (Paint.NET)",
            BlendMode::PaintNetColorDodge => "Color Dodge (Paint.NET)",
            BlendMode::NukeMin => "min (Nuke)",
            BlendMode::NukeMultiply => "multiply (Nuke)",
            BlendMode::NukeColorBurn => "color-burn (Nuke)",
            BlendMode::NukeUnder => "under (Nuke)",
            BlendMode::NukeIn => "in (Nuke)",
            BlendMode::NukeMax => "max (Nuke)",
            BlendMode::NukeScreen => "screen (Nuke)",
            BlendMode::NukeColorDodge => "color-dodge (Nuke)",
            BlendMode::NukePlus => "plus (Nuke)",
            BlendMode::NukeOut => "out (Nuke)",
            BlendMode::NukeOverlay => "overlay (Nuke)",
            BlendMode::NukeSoftLight => "soft-light (Nuke)",
            BlendMode::NukeHardLight => "hard-light (Nuke)",
            BlendMode::NukeGeometric => "geometric (Nuke)",
            BlendMode::NukeHypot => "hypot (Nuke)",
            BlendMode::NukeMask => "mask (Nuke)",
            BlendMode::NukeStencil => "stencil (Nuke)",
            BlendMode::NukeDifference => "difference (Nuke)",
            BlendMode::NukeExclusion => "exclusion (Nuke)",
            BlendMode::NukeMinus => "minus (Nuke)",
            BlendMode::NukeDivide => "divide (Nuke)",
            BlendMode::NukeMatte => "matte (Nuke)",
            BlendMode::NukeAverage => "average (Nuke)",
            BlendMode::NukeConjointOver => "conjoint-over (Nuke)",
            BlendMode::NukeDisjointOver => "disjoint-over (Nuke)",
            BlendMode::NukeXor => "xor (Nuke)",
            BlendMode::NukeFrom => "from (Nuke)",
            BlendMode::NukeAtop => "atop (Nuke)",
        }
    }

    /// PSD 4-character blend key. Modes without an equivalent fall back to Normal, except the
    /// Nuke colour operations, written as the Photoshop mode closest to them; exporters must
    /// report the loss using [`Self::has_psd_equivalent`].
    pub fn psd_key(self) -> [u8; 4] {
        *match self {
            BlendMode::PassThrough => b"pass",
            BlendMode::Normal => b"norm",
            BlendMode::Dissolve => b"diss",
            BlendMode::Darken | BlendMode::NukeMin => b"dark",
            BlendMode::Multiply | BlendMode::NukeMultiply => b"mul ",
            BlendMode::ColorBurn | BlendMode::NukeColorBurn => b"idiv",
            BlendMode::LinearBurn => b"lbrn",
            BlendMode::DarkerColor => b"dkCl",
            BlendMode::Lighten | BlendMode::NukeMax => b"lite",
            BlendMode::Screen | BlendMode::NukeScreen => b"scrn",
            BlendMode::ColorDodge | BlendMode::NukeColorDodge => b"div ",
            BlendMode::LinearDodge | BlendMode::NukePlus => b"lddg",
            BlendMode::LighterColor => b"lgCl",
            BlendMode::Overlay | BlendMode::NukeOverlay => b"over",
            BlendMode::SoftLight | BlendMode::NukeSoftLight => b"sLit",
            BlendMode::HardLight | BlendMode::NukeHardLight => b"hLit",
            BlendMode::VividLight => b"vLit",
            BlendMode::LinearLight => b"lLit",
            BlendMode::PinLight => b"pLit",
            BlendMode::HardMix => b"hMix",
            BlendMode::Difference | BlendMode::NukeDifference => b"diff",
            BlendMode::Exclusion | BlendMode::NukeExclusion => b"smud",
            BlendMode::Subtract | BlendMode::NukeFrom => b"fsub",
            BlendMode::Divide => b"fdiv",
            BlendMode::Hue => b"hue ",
            BlendMode::Saturation => b"sat ",
            BlendMode::Color => b"colr",
            BlendMode::Luminosity => b"lum ",
            BlendMode::Reflect | BlendMode::Glow | BlendMode::Negation | BlendMode::Xor | BlendMode::PaintNetColorBurn | BlendMode::PaintNetColorDodge => {
                b"norm"
            }
            BlendMode::NukeUnder
            | BlendMode::NukeIn
            | BlendMode::NukeOut
            | BlendMode::NukeGeometric
            | BlendMode::NukeHypot
            | BlendMode::NukeMask
            | BlendMode::NukeStencil
            | BlendMode::NukeMinus
            | BlendMode::NukeDivide
            | BlendMode::NukeMatte
            | BlendMode::NukeAverage
            | BlendMode::NukeConjointOver
            | BlendMode::NukeDisjointOver
            | BlendMode::NukeXor
            | BlendMode::NukeAtop => b"norm",
        }
    }

    /// The mode a Photoshop key reads as: always Photoshop's own (never a Nuke operation).
    pub fn from_psd_key(key: [u8; 4]) -> Option<BlendMode> {
        std::iter::once(BlendMode::PassThrough).chain(Self::LAYER_MODES).find(|m| m.psd_key() == key)
    }

    pub fn is_separable(self) -> bool {
        !matches!(self, BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity | BlendMode::DarkerColor | BlendMode::LighterColor)
    }
}

/// Separable per-channel blend function `B(cb, cs)`.
#[inline]
pub fn blend_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    channel(mode, cb, cs, 1.0)
}

/// [`blend_channel`] for 32-bit float documents, whose values may exceed 1: Linear Dodge (Add)
/// and Divide don't clip at 1 there (Photoshop's 32-bit Add is how light is added in HDR), only
/// at `f32::MAX`, so they stay finite. Every other mode is [`blend_channel`].
#[inline]
pub fn blend_channel_hdr(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    channel(mode, cb, cs, f32::MAX)
}

/// `B(cb, cs)` with Linear Dodge and Divide clipped at `hi` (1 at integer depths).
#[inline]
fn channel(mode: BlendMode, cb: f32, cs: f32, hi: f32) -> f32 {
    match mode {
        BlendMode::Normal | BlendMode::Dissolve | BlendMode::PassThrough => cs,
        BlendMode::Darken => cb.min(cs),
        BlendMode::Multiply => cb * cs,
        BlendMode::ColorBurn => color_burn(cb, cs),
        BlendMode::LinearBurn => (cb + cs - 1.0).max(0.0),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::ColorDodge => color_dodge(cb, cs),
        BlendMode::LinearDodge => (cb + cs).min(hi),
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::SoftLight => soft_light_ps(cb, cs),
        BlendMode::HardLight => hard_light(cb, cs),
        BlendMode::VividLight if cs <= 0.0 => 0.0,
        BlendMode::VividLight if cs >= 1.0 => 1.0,
        BlendMode::VividLight => {
            if cs <= 0.5 {
                color_burn(cb, 2.0 * cs)
            } else {
                color_dodge(cb, 2.0 * (cs - 0.5))
            }
        }
        BlendMode::LinearLight => (cb + 2.0 * cs - 1.0).clamp(0.0, 1.0),
        BlendMode::PinLight => {
            if cs <= 0.5 {
                cb.min(2.0 * cs)
            } else {
                cb.max(2.0 * cs - 1.0)
            }
        }
        BlendMode::HardMix => {
            if vivid_light_hard_mix(cb, cs) >= 0.5 - 1e-6 {
                1.0
            } else {
                0.0
            }
        }
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
        BlendMode::Subtract => (cb - cs).max(0.0),
        BlendMode::Divide => {
            if cs <= 0.0 {
                if cb <= 0.0 { 0.0 } else { 1.0 }
            } else {
                (cb / cs).min(hi)
            }
        }
        BlendMode::Reflect => reflect(cb, cs),
        BlendMode::Glow => reflect(cs, cb),
        BlendMode::Negation => 1.0 - (1.0 - cb - cs).abs(),
        BlendMode::Xor => {
            // Paint.NET's XOR is defined on 8-bit channels even in a floating-point compositor.
            let b = (cb.clamp(0.0, 1.0) * 255.0).round() as u8;
            let s = (cs.clamp(0.0, 1.0) * 255.0).round() as u8;
            f32::from(b ^ s) / 255.0
        }
        BlendMode::PaintNetColorBurn => {
            if cs <= 0.0 {
                0.0
            } else {
                (1.0 - (1.0 - cb) / cs).max(0.0)
            }
        }
        BlendMode::PaintNetColorDodge => {
            if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        // Non-separable modes are handled by `blend_rgb`; fall back to source.
        BlendMode::DarkerColor | BlendMode::LighterColor | BlendMode::Hue | BlendMode::Saturation | BlendMode::Color | BlendMode::Luminosity => cs,
        BlendMode::NukeMin
        | BlendMode::NukeMultiply
        | BlendMode::NukeColorBurn
        | BlendMode::NukeUnder
        | BlendMode::NukeIn
        | BlendMode::NukeMax
        | BlendMode::NukeScreen
        | BlendMode::NukeColorDodge
        | BlendMode::NukePlus
        | BlendMode::NukeOut
        | BlendMode::NukeOverlay
        | BlendMode::NukeSoftLight
        | BlendMode::NukeHardLight
        | BlendMode::NukeGeometric
        | BlendMode::NukeHypot
        | BlendMode::NukeMask
        | BlendMode::NukeStencil
        | BlendMode::NukeDifference
        | BlendMode::NukeExclusion
        | BlendMode::NukeMinus
        | BlendMode::NukeDivide
        | BlendMode::NukeMatte
        | BlendMode::NukeAverage
        | BlendMode::NukeConjointOver
        | BlendMode::NukeDisjointOver
        | BlendMode::NukeXor
        | BlendMode::NukeFrom
        | BlendMode::NukeAtop => nuke_channel(mode, cb, cs),
    }
}

fn reflect(cb: f32, cs: f32) -> f32 {
    if cs >= 1.0 { 1.0 } else { (cb * cb / (1.0 - cs)).min(1.0) }
}

#[inline]
fn vivid_light_hard_mix(cb: f32, cs: f32) -> f32 {
    const EDGE: f32 = 1e-4;
    if cs <= 0.5 {
        if cb >= 1.0 - EDGE {
            1.0
        } else if cs <= 0.0 {
            0.0
        } else {
            1.0 - ((1.0 - cb) / (2.0 * cs)).min(1.0)
        }
    } else if cb <= EDGE {
        0.0
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (2.0 * (1.0 - cs))).min(1.0)
    }
}

#[inline]
fn color_burn(cb: f32, cs: f32) -> f32 {
    if cb >= 1.0 {
        1.0
    } else if cs <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - cb) / cs).min(1.0)
    }
}

#[inline]
fn color_dodge(cb: f32, cs: f32) -> f32 {
    if cb <= 0.0 {
        0.0
    } else if cs >= 1.0 {
        1.0
    } else {
        (cb / (1.0 - cs)).min(1.0)
    }
}

#[inline]
fn hard_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb * 2.0 * cs
    } else {
        let s = 2.0 * cs - 1.0;
        cb + s - cb * s
    }
}

/// Photoshop's Soft Light: the W3C/PDF curve. In the upper half (`cs > 0.5`) the backdrop moves
/// towards `D(cb)`, which is `sqrt(cb)` above a quarter and the cubic `((16·cb − 12)·cb + 4)·cb`
/// below it; with `sqrt` throughout, dark backdrops came out up to 20 levels too light (#8804,
/// measured on psd-tools `blend-modes/rgb-blend-modes.psd`).
#[inline]
fn soft_light_ps(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        2.0 * cb * cs + cb * cb * (1.0 - 2.0 * cs)
    } else {
        let b = cb.max(0.0);
        let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
        2.0 * cb * (1.0 - cs) + d * (2.0 * cs - 1.0)
    }
}

// ---- non-separable helpers (W3C Compositing Level 1) ----

#[inline]
pub fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// ClipColor: moves `c` towards its grey ([`lum`]) until every channel is within `0..=top`,
/// keeping the luminosity. `top` is the brightest in-gamut value: 1, or `f32::MAX` in 32-bit
/// documents (values above 1 are valid light there, so only negatives are pulled in).
///
/// The scale factors `l / (l − min)` and `(top − l) / (max − l)` lie in 0..1 only while the
/// luminosity itself is in range. Outside it no colour of that luminosity is in gamut: the plain
/// formula turned colours inside out (a grey of 2 as the Luminosity of (0.5, 0.25, 0.125) gave
/// (1.0, 2.32, 2.99)) and, for greys, divided by the rounding noise of `max − l`, so results were
/// arbitrary and differed between the CPU and the GPU. Such colours become their grey instead
/// (the formula's value as the luminosity reaches the bound).
fn clip_color(c: [f32; 3], top: f32) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 {
        let d = l - n;
        for v in &mut out {
            *v = if l > 0.0 && d > 0.0 { l + (*v - l) * l / d } else { l };
        }
    }
    if x > top {
        let d = x - l;
        for v in &mut out {
            *v = if l < top && d > 0.0 { l + (*v - l) * (top - l) / d } else { l };
        }
    }
    out
}

/// `c` with its luminosity ([`lum`]) moved to `l`, clipped back into gamut towards grey (the
/// Luminosity blend mode's SetLum + ClipColor).
pub fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    set_lum_within(c, l, 1.0)
}

/// [`set_lum`] clipping at `top` instead of 1 (see [`blend_rgb_within`]).
fn set_lum_within(c: [f32; 3], l: f32, top: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d], top)
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut idx = [0usize, 1, 2];
    idx.sort_by(|&a, &b| c[a].partial_cmp(&c[b]).unwrap_or(std::cmp::Ordering::Equal));
    let (imin, imid, imax) = (idx[0], idx[1], idx[2]);
    let mut out = [0.0f32; 3];
    if c[imax] > c[imin] {
        out[imid] = (c[imid] - c[imin]) * s / (c[imax] - c[imin]);
        out[imax] = s;
    }
    out[imin] = 0.0;
    out
}

/// Blend a whole RGB triple: `B(Cb, Cs)` including non-separable modes.
pub fn blend_rgb(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    blend_rgb_within(mode, cb, cs, 1.0)
}

/// [`blend_rgb`] with Hue / Saturation / Color / Luminosity clipped at `top` instead of 1:
/// `f32::MAX` keeps the values above 1 of 32-bit documents (their gamut has no upper bound).
pub fn blend_rgb_within(mode: BlendMode, cb: [f32; 3], cs: [f32; 3], top: f32) -> [f32; 3] {
    match mode {
        BlendMode::Hue => set_lum_within(set_sat(cs, sat(cb)), lum(cb), top),
        BlendMode::Saturation => set_lum_within(set_sat(cb, sat(cs)), lum(cb), top),
        BlendMode::Color => set_lum_within(cs, lum(cb), top),
        BlendMode::Luminosity => set_lum_within(cb, lum(cs), top),
        BlendMode::DarkerColor => {
            if lum(cs) < lum(cb) {
                cs
            } else {
                cb
            }
        }
        BlendMode::LighterColor => {
            if lum(cs) > lum(cb) {
                cs
            } else {
                cb
            }
        }
        m => [blend_channel(m, cb[0], cs[0]), blend_channel(m, cb[1], cs[1]), blend_channel(m, cb[2], cs[2])],
    }
}

/// [`blend_rgb`] for 32-bit float documents: separable modes use [`blend_channel_hdr`], and the
/// non-separable ones keep values above 1 ([`blend_rgb_within`] at `f32::MAX`).
pub fn blend_rgb_hdr(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    if mode.is_separable() { std::array::from_fn(|i| blend_channel_hdr(mode, cb[i], cs[i])) } else { blend_rgb_within(mode, cb, cs, f32::MAX) }
}


// ---- Nuke Merge operations (the `Nuke…` modes): Nuke's 32-bit maths, A = source, B = backdrop ----

#[inline]
pub fn nuke_screen(a: f32, b: f32) -> f32 {
    if (0.0..=1.0).contains(&a) && (0.0..=1.0).contains(&b) {
        a + b - a * b
    } else {
        a.max(b)
    }
}

/// Shared core of Nuke's `overlay` and `hard-light`: multiply below 0.5 of the deciding input
/// `x`, screen above it, with `x` (not the other input) rescaled to 0..1 in the screen branch:
/// `x < 0.5 → 2·x·y`, else `screen(2·x − 1, y)`. Both branches give `y` at `x = 0.5`, so the
/// result is continuous. Nothing is clamped: 32-bit values above 1 (and below 0) pass through,
/// and [`nuke_screen`]'s `max` fallback handles them as Nuke does.
#[inline]
pub fn nuke_light(x: f32, y: f32) -> f32 {
    if x < 0.5 {
        2.0 * x * y
    } else {
        nuke_screen(2.0 * x - 1.0, y)
    }
}

/// Nuke `overlay` (A = `a`, B = `b`): multiply if B < 0.5, screen if B > 0.5.
#[inline]
pub fn nuke_overlay(a: f32, b: f32) -> f32 {
    nuke_light(b, a)
}

/// Nuke `hard-light` (A = `a`, B = `b`): multiply if A < 0.5, screen if A > 0.5.
#[inline]
pub fn nuke_hard_light(a: f32, b: f32) -> f32 {
    nuke_light(a, b)
}

#[inline]
pub fn nuke_soft_light(a: f32, b: f32) -> f32 {
    let a_c = a.clamp(0.0, 1.0);
    let b_c = b.clamp(0.0, 1.0);
    if a_c * b_c < 1.0 {
        b_c * (2.0 * a_c + (b_c * (1.0 - a_c * 2.0)))
    } else {
        2.0 * b_c * a_c
    }
}

#[inline]
pub fn nuke_divide(a: f32, b: f32) -> f32 {
    if b.abs() <= 1e-7 {
        0.0
    } else {
        a / b
    }
}

/// `B(cb, cs)` of a Nuke colour operation (A = `cs`, B = `cb`); the compositing ones (in, out,
/// mask, stencil, atop, matte, xor, conjoint / disjoint over, under) are composited whole in
/// [`composite`], and give the source here.
#[inline]
fn nuke_channel(mode: BlendMode, cb: f32, cs: f32) -> f32 {
    match mode {
        BlendMode::NukeUnder | BlendMode::NukeAtop => cb,
        BlendMode::NukePlus => cs + cb,
        BlendMode::NukeMultiply => cs * cb,
        BlendMode::NukeScreen => nuke_screen(cs, cb),
        BlendMode::NukeMinus => cs - cb,
        BlendMode::NukeFrom => cb - cs,
        BlendMode::NukeDivide => nuke_divide(cs, cb),
        BlendMode::NukeMin | BlendMode::NukeIn => cs.min(cb),
        BlendMode::NukeMax | BlendMode::NukeOut => cs.max(cb),
        BlendMode::NukeHypot => (cs * cs + cb * cb).sqrt(),
        BlendMode::NukeGeometric => {
            let sum = cs + cb;
            if sum.abs() <= 1e-7 { 0.0 } else { (2.0 * cs * cb) / sum }
        }
        BlendMode::NukeDifference => (cs - cb).abs(),
        BlendMode::NukeExclusion => cs + cb - 2.0 * cs * cb,
        BlendMode::NukeOverlay => nuke_overlay(cs, cb),
        BlendMode::NukeHardLight => nuke_hard_light(cs, cb),
        BlendMode::NukeSoftLight => nuke_soft_light(cs, cb),
        BlendMode::NukeAverage => 0.5 * (cs + cb),
        BlendMode::NukeColorDodge => {
            let denom = 1.0 - cs;
            if denom.abs() <= 1e-6 { cb } else { cb / denom }
        }
        BlendMode::NukeColorBurn => {
            if cs.abs() <= 1e-6 { 0.0 } else { 1.0 - (1.0 - cb) / cs }
        }
        _ => cs,
    }
}

/// Composite a straight-alpha source over a straight-alpha backdrop with blend mode and opacity.
///
/// Uses the W3C/PDF general formula:
/// `αo = αs + αb(1−αs)`,
/// `αo·Co = (1−αs)·αb·Cb + (1−αb)·αs·Cs + αs·αb·B(Cb,Cs)`.
/// Dissolve must be resolved by the caller (per-pixel coverage decision) before calling.
pub fn composite(mode: BlendMode, backdrop: [f32; 4], source: [f32; 4], opacity: f32) -> [f32; 4] {
    if mode.is_nuke() {
        return nuke_composite(mode, backdrop, source, opacity);
    }
    let ab = backdrop[3];
    let as_ = source[3] * opacity;
    if as_ <= 0.0 {
        return backdrop;
    }
    let cb = [backdrop[0], backdrop[1], backdrop[2]];
    let cs = [source[0], source[1], source[2]];
    // 32-bit values above 1 are kept, as the GPU compositor keeps them.
    let b = blend_rgb_hdr(mode, cb, cs);
    let ao = as_ + ab * (1.0 - as_);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0f32; 4];
    for i in 0..3 {
        let premul = (1.0 - as_) * ab * cb[i] + (1.0 - ab) * as_ * cs[i] + as_ * ab * b[i];
        out[i] = premul / ao;
    }
    out[3] = ao;
    out
}

/// [`composite`] with the backdrop's alpha kept (Lock Transparent Pixels, Fill's Preserve
/// Transparency): "source atop", `Co = (1−αs)·Cb + αs·B(Cb,Cs)`, `αo = αb`. Unlike source-over
/// followed by restoring the alpha, a half-transparent backdrop takes the new colour by `αs` alone.
pub fn composite_atop(mode: BlendMode, backdrop: [f32; 4], source: [f32; 4], opacity: f32) -> [f32; 4] {
    let as_ = source[3] * opacity;
    if as_ <= 0.0 {
        return backdrop;
    }
    let b = blend_rgb_hdr(mode, [backdrop[0], backdrop[1], backdrop[2]], [source[0], source[1], source[2]]);
    let mut out = backdrop;
    for i in 0..3 {
        out[i] = (1.0 - as_) * backdrop[i] + as_ * b[i];
    }
    out
}

/// [`composite`] for the Nuke Merge operations: Nuke's premultiplied maths, then mixed by
/// `opacity` (Merge's `mix`). The compositing operations are composited whole here; the colour
/// ones use the general formula with [`nuke_channel`].
fn nuke_composite(mode: BlendMode, backdrop: [f32; 4], source: [f32; 4], opacity: f32) -> [f32; 4] {
    let mix = opacity.clamp(0.0, 1.0);
    if mix <= 0.0 {
        return backdrop;
    }

    let ba = backdrop[3];
    let b = [backdrop[0] * ba, backdrop[1] * ba, backdrop[2] * ba, ba];

    let sa = source[3];
    let a = [source[0] * sa, source[1] * sa, source[2] * sa, sa];

    if sa <= 0.0
        && !matches!(
            mode,
            BlendMode::NukeIn | BlendMode::NukeMask | BlendMode::NukeMin
        )
    {
        return backdrop;
    }

    let m: [f32; 4] = match mode {
        BlendMode::NukeMatte => [ // matte
            a[0] + b[0] * (1.0 - sa),
            a[1] + b[1] * (1.0 - sa),
            a[2] + b[2] * (1.0 - sa),
            sa + ba * (1.0 - sa),
        ],
        BlendMode::NukeUnder => [ // under
            a[0] * (1.0 - ba) + b[0],
            a[1] * (1.0 - ba) + b[1],
            a[2] * (1.0 - ba) + b[2],
            sa * (1.0 - ba) + ba,
        ],
        BlendMode::NukePlus => [ // plus
            a[0] + b[0],
            a[1] + b[1],
            a[2] + b[2],
            (sa + ba).min(1.0),
        ],
        BlendMode::NukeMinus => [ // minus
            a[0] - b[0],
            a[1] - b[1],
            a[2] - b[2],
            (sa - ba).clamp(0.0, 1.0),
        ],
        BlendMode::NukeFrom => [ // from
            b[0] - a[0],
            b[1] - a[1],
            b[2] - a[2],
            (ba - sa).clamp(0.0, 1.0),
        ],
        BlendMode::NukeMultiply => [ // multiply
            a[0] * b[0] + b[0] * (1.0 - sa),
            a[1] * b[1] + b[1] * (1.0 - sa),
            a[2] * b[2] + b[2] * (1.0 - sa),
            sa + ba * (1.0 - sa),
        ],
        BlendMode::NukeScreen => [ // screen
            nuke_screen(a[0], b[0]),
            nuke_screen(a[1], b[1]),
            nuke_screen(a[2], b[2]),
            nuke_screen(sa, ba),
        ],
        BlendMode::NukeMin => [ // min
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].min(b[2]),
            sa.min(ba),
        ],
        BlendMode::NukeMax => [ // max
            a[0].max(b[0]),
            a[1].max(b[1]),
            a[2].max(b[2]),
            sa.max(ba),
        ],
        BlendMode::NukeDifference => [ // difference
            (a[0] - b[0]).abs(),
            (a[1] - b[1]).abs(),
            (a[2] - b[2]).abs(),
            (sa - ba).abs(),
        ],
        BlendMode::NukeExclusion => [ // exclusion
            a[0] + b[0] - 2.0 * a[0] * b[0],
            a[1] + b[1] - 2.0 * a[1] * b[1],
            a[2] + b[2] - 2.0 * a[2] * b[2],
            sa + ba - 2.0 * sa * ba,
        ],
        BlendMode::NukeDivide => [ // divide
            nuke_divide(a[0], b[0]),
            nuke_divide(a[1], b[1]),
            nuke_divide(a[2], b[2]),
            sa + ba * (1.0 - sa),
        ],
        BlendMode::NukeHypot => [ // hypot
            (a[0] * a[0] + b[0] * b[0]).sqrt(),
            (a[1] * a[1] + b[1] * b[1]).sqrt(),
            (a[2] * a[2] + b[2] * b[2]).sqrt(),
            (sa * sa + ba * ba).sqrt().min(1.0),
        ],
        BlendMode::NukeIn => [a[0] * ba, a[1] * ba, a[2] * ba, sa * ba], // in
        BlendMode::NukeOut => [ // out
            a[0] * (1.0 - ba),
            a[1] * (1.0 - ba),
            a[2] * (1.0 - ba),
            sa * (1.0 - ba),
        ],
        BlendMode::NukeMask => [b[0] * sa, b[1] * sa, b[2] * sa, ba * sa], // mask
        BlendMode::NukeStencil => [ // stencil
            b[0] * (1.0 - sa),
            b[1] * (1.0 - sa),
            b[2] * (1.0 - sa),
            ba * (1.0 - sa),
        ],
        BlendMode::NukeAtop => [ // atop
            a[0] * ba + b[0] * (1.0 - sa),
            a[1] * ba + b[1] * (1.0 - sa),
            a[2] * ba + b[2] * (1.0 - sa),
            ba,
        ],
        BlendMode::NukeColorBurn => [ // color-burn
            if a[0] <= 1e-6 { 0.0 } else { 1.0 - (1.0 - b[0]) / a[0] },
            if a[1] <= 1e-6 { 0.0 } else { 1.0 - (1.0 - b[1]) / a[1] },
            if a[2] <= 1e-6 { 0.0 } else { 1.0 - (1.0 - b[2]) / a[2] },
            sa + ba * (1.0 - sa),
        ],
        BlendMode::NukeColorDodge => [ // color-dodge
            if 1.0 - a[0] <= 1e-6 { b[0] } else { b[0] / (1.0 - a[0]) },
            if 1.0 - a[1] <= 1e-6 { b[1] } else { b[1] / (1.0 - a[1]) },
            if 1.0 - a[2] <= 1e-6 { b[2] } else { b[2] / (1.0 - a[2]) },
            sa + ba * (1.0 - sa),
        ],
        BlendMode::NukeXor => [ // xor
            a[0] * (1.0 - ba) + b[0] * (1.0 - sa),
            a[1] * (1.0 - ba) + b[1] * (1.0 - sa),
            a[2] * (1.0 - ba) + b[2] * (1.0 - sa),
            (sa * (1.0 - ba) + ba * (1.0 - sa)).clamp(0.0, 1.0),
        ],
        BlendMode::NukeConjointOver => { // conjoint-over
            if sa > ba || ba <= 1e-7 {
                a
            } else {
                let k = 1.0 - sa / ba;
                [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k, sa + ba * k]
            }
        }
        BlendMode::NukeDisjointOver => { // disjoint-over
            if sa + ba < 1.0 || ba <= 1e-7 {
                [a[0] + b[0], a[1] + b[1], a[2] + b[2], (sa + ba).min(1.0)]
            } else {
                let k = (1.0 - sa) / ba;
                [a[0] + b[0] * k, a[1] + b[1] * k, a[2] + b[2] * k, 1.0]
            }
        }
        other => {
            let blended = blend_rgb_hdr(
                other,
                [backdrop[0], backdrop[1], backdrop[2]],
                [source[0], source[1], source[2]],
            );
            let ao = sa + ba * (1.0 - sa);
            [
                (1.0 - sa) * b[0] + (1.0 - ba) * a[0] + sa * ba * blended[0],
                (1.0 - sa) * b[1] + (1.0 - ba) * a[1] + sa * ba * blended[1],
                (1.0 - sa) * b[2] + (1.0 - ba) * a[2] + sa * ba * blended[2],
                ao,
            ]
        }
    };

    let out_pm = [
        b[0] + (m[0] - b[0]) * mix,
        b[1] + (m[1] - b[1]) * mix,
        b[2] + (m[2] - b[2]) * mix,
        (b[3] + (m[3] - b[3]) * mix).clamp(0.0, 1.0),
    ];

    let ao = out_pm[3];
    if ao <= 1e-7 {
        [0.0; 4]
    } else {
        let inv_a = 1.0 / ao;
        [out_pm[0] * inv_a, out_pm[1] * inv_a, out_pm[2] * inv_a, ao]
    }
}



#[cfg(test)]
mod tests;

#[cfg(test)]
mod nuke_tests;
