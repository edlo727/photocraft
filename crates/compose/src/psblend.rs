//! Nuke-style 32-bit floating-point linear compositing and Merge math.
//!
//! Delegates all blending and compositing math directly to `photocraft_color::blend`
//! so the entire engine shares a single source of truth for Nuke Merge operations,
//! while retaining the vectorized fast path for `Over` (`BlendMode::Normal`).

use photocraft_color::blend::{self as generic, BlendMode};

pub const EDGE: f32 = 1e-4;

thread_local! {
    /// Kept for API compatibility with the rest of the workspace; disabled in Nuke linear mode.
    pub static LAB_MIX: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Always defaults to true in the VFX pipeline so 32-bit float values never clamp at 1.0.
    pub static HDR: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

pub fn with_lab_mix<R>(lab: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            LAB_MIX.with(|l| l.set(self.0));
        }
    }
    let _restore = Restore(LAB_MIX.with(|l| l.replace(lab)));
    f()
}

pub fn with_hdr<R>(hdr: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            HDR.with(|h| h.set(self.0));
        }
    }
    let _restore = Restore(HDR.with(|h| h.replace(hdr)));
    f()
}

/// Whether the document being composited is 32-bit float, whose gamut has no upper bound.
/// [`with_hdr`] sets it per document (`render::flatten`); Photoshop clips the non-separable
/// modes and Add at 1 at the integer depths, and the PSD merged-image oracle holds us to that.
#[inline]
pub(crate) fn hdr() -> bool {
    HDR.with(std::cell::Cell::get)
}

#[inline]
pub fn blend_rgb(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    if hdr() { generic::blend_rgb_hdr(mode, cb, cs) } else { generic::blend_rgb(mode, cb, cs) }
}

/// The eight modes where Photoshop folds a Fill below 100 % into the blend itself rather than
/// treating it as opacity. A 32-bit document composites as Nuke does, where Fill is plain
/// opacity, so the rule applies only at the integer depths ([`hdr`]).
#[inline]
pub fn fill_is_special(mode: BlendMode) -> bool {
    !hdr()
        && matches!(
            mode,
            BlendMode::ColorBurn
                | BlendMode::LinearBurn
                | BlendMode::ColorDodge
                | BlendMode::LinearDodge
                | BlendMode::VividLight
                | BlendMode::LinearLight
                | BlendMode::HardMix
                | BlendMode::Difference
        )
}

/// [`blend_rgb`] with Fill folded in for [`fill_is_special`]: the source moves towards the mode's
/// neutral by `fill` before it blends.
#[inline]
pub fn blend_rgb_fill(mode: BlendMode, cb: [f32; 3], cs: [f32; 3], fill: f32) -> [f32; 3] {
    let f = fill.clamp(0.0, 1.0);
    if f >= 1.0 || !fill_is_special(mode) {
        return blend_rgb(mode, cb, cs);
    }
    let toward = |neutral: f32| -> [f32; 3] { std::array::from_fn(|i| neutral + (cs[i] - neutral) * f) };
    match mode {
        // Hard Mix at Fill f: (cb − f·(1 − cs)) / (1 − f), clamped. At f = 1 it is the hard
        // threshold (cb + cs ≥ 1), at f = 0 the backdrop.
        BlendMode::HardMix => std::array::from_fn(|i| ((cb[i] - f * (1.0 - cs[i])) / (1.0 - f)).clamp(0.0, 1.0)),
        BlendMode::ColorBurn | BlendMode::LinearBurn => blend_rgb(mode, cb, toward(1.0)),
        BlendMode::VividLight | BlendMode::LinearLight => blend_rgb(mode, cb, toward(0.5)),
        _ => blend_rgb(mode, cb, toward(0.0)),
    }
}

#[inline]
pub fn composite_fill(
    mode: BlendMode,
    backdrop: [f32; 4],
    source: [f32; 4],
    opacity: f32,
    fill: f32,
    _gamma: f32,
) -> [f32; 4] {
    composite(mode, backdrop, source, opacity * fill.clamp(0.0, 1.0))
}

#[inline]
pub fn composite(mode: BlendMode, backdrop: [f32; 4], source: [f32; 4], opacity: f32) -> [f32; 4] {
    if hdr() { generic::composite_hdr(mode, backdrop, source, opacity) } else { generic::composite(mode, backdrop, source, opacity) }
}

#[inline]
pub(crate) fn composite_normal_rgb(backdrop: [f32; 4], source: [f32; 4], opacity: f32) -> [f32; 4] {
    let ab = backdrop[3];
    let as_ = source[3] * opacity;
    if as_ <= 0.0 {
        return backdrop;
    }
    let ao = as_ + ab * (1.0 - as_);
    normal_rgb(backdrop, source, as_, ao)
}

pub(crate) fn composite_normal_rgb_span(dst: &mut [[f32; 4]], src: &[[f32; 4]], opacity: f32) {
    let n = dst.len().min(src.len());
    let (dc, dt) = dst[..n].as_chunks_mut::<LANES>();
    let (sc, st) = src[..n].as_chunks::<LANES>();
    for (b, s) in dc.iter_mut().zip(sc) {
        normal_lanes(b, s, opacity);
    }
    for (b, s) in dt.iter_mut().zip(st) {
        if s[3] <= 0.0 {
            continue;
        }
        *b = composite_normal_rgb(*b, *s, opacity);
    }
}

const LANES: usize = 8;

#[inline]
#[allow(clippy::needless_range_loop)]
fn normal_lanes(b: &mut [[f32; 4]; LANES], s: &[[f32; 4]; LANES], opacity: f32) {
    let lane = |p: &[[f32; 4]; LANES], c: usize| -> [f32; LANES] { std::array::from_fn(|i| p[i][c]) };
    let (b0, b1, b2, ab) = (lane(b, 0), lane(b, 1), lane(b, 2), lane(b, 3));
    let (s0, s1, s2, s3) = (lane(s, 0), lane(s, 1), lane(s, 2), lane(s, 3));
    let mut as_ = [0.0f32; LANES];
    let mut ao = [0.0f32; LANES];
    let mut kb = [0.0f32; LANES];
    let mut inv = [0.0f32; LANES];
    for i in 0..LANES {
        as_[i] = s3[i] * opacity;
        ao[i] = as_[i] + ab[i] * (1.0 - as_[i]);
        kb[i] = ab[i] * (1.0 - as_[i]);
        inv[i] = 1.0 / ao[i];
    }
    for i in 0..LANES {
        let mix = [
            (kb[i] * b0[i] + as_[i] * s0[i]) * inv[i],
            (kb[i] * b1[i] + as_[i] * s1[i]) * inv[i],
            (kb[i] * b2[i] + as_[i] * s2[i]) * inv[i],
            ao[i],
        ];
        let keep = (s3[i] <= 0.0) | (as_[i] <= 0.0);
        let gone = ao[i] <= 0.0;
        b[i] = if keep {
            b[i]
        } else if gone {
            [0.0; 4]
        } else {
            mix
        };
    }
}

#[inline]
fn normal_rgb(backdrop: [f32; 4], source: [f32; 4], as_: f32, ao: f32) -> [f32; 4] {
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let kb = backdrop[3] * (1.0 - as_);
    let inv = 1.0 / ao;
    [
        (kb * backdrop[0] + as_ * source[0]) * inv,
        (kb * backdrop[1] + as_ * source[1]) * inv,
        (kb * backdrop[2] + as_ * source[2]) * inv,
        ao,
    ]
}

pub const TEXT_GAMMA: f32 = 1.45;

static TEXT_GAMMA_SETTING: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3fb9_999a); // 1.45f32

pub fn set_text_gamma(gamma: f32) {
    let g = if gamma.is_finite() { gamma.clamp(1.0, 2.2) } else { TEXT_GAMMA };
    TEXT_GAMMA_SETTING.store(g.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

pub fn text_gamma() -> f32 {
    f32::from_bits(TEXT_GAMMA_SETTING.load(std::sync::atomic::Ordering::Relaxed))
}

/// Into the text blending space: linear light raised to `1 / gamma`.
#[inline]
pub fn text_encode(v: f32, gamma: f32) -> f32 {
    photocraft_color::convert::srgb_to_linear(v.max(0.0)).powf(1.0 / gamma)
}

#[inline]
pub fn text_decode(v: f32, gamma: f32) -> f32 {
    photocraft_color::convert::linear_to_srgb(v.max(0.0).powf(gamma))
}

#[inline]
pub fn composite_gamma(
    mode: BlendMode,
    backdrop: [f32; 4],
    source: [f32; 4],
    opacity: f32,
    gamma: f32,
) -> [f32; 4] {
    if gamma == 1.0 {
        return composite(mode, backdrop, source, opacity);
    }
    let ab = backdrop[3];
    let as_ = source[3] * opacity;
    if as_ <= 0.0 {
        return backdrop;
    }
    let ao = as_ + ab * (1.0 - as_);
    if ao <= 0.0 {
        return [0.0; 4];
    }
    let cb = [backdrop[0], backdrop[1], backdrop[2]];
    let cs = [source[0], source[1], source[2]];
    let b = blend_rgb(mode, cb, cs);
    let (wb, ws, wbs) = ((1.0 - as_) * ab / ao, (1.0 - ab) * as_ / ao, as_ * ab / ao);
    let g = |v: f32| text_encode(v, gamma);
    let ginv = |v: f32| text_decode(v, gamma);
    let mut out = [0.0f32; 4];
    for i in 0..3 {
        out[i] = ginv(wb * g(cb[i]) + ws * g(cs[i]) + wbs * g(b[i]));
    }
    out[3] = ao;
    out
}

pub fn composite_atop(mode: BlendMode, backdrop: [f32; 4], source: [f32; 4], opacity: f32) -> [f32; 4] {
    if hdr() { generic::composite_atop_hdr(mode, backdrop, source, opacity) } else { generic::composite_atop(mode, backdrop, source, opacity) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nuke_plus_preserves_hdr_above_one() {
        let b = [0.8, 0.5, 0.2, 1.0];
        let a = [0.7, 0.9, 1.4, 1.0];
        let out = composite(BlendMode::NukePlus, b, a, 1.0);
        assert!((out[0] - 1.5).abs() < 1e-5);
        assert!((out[1] - 1.4).abs() < 1e-5);
        assert!((out[2] - 1.6).abs() < 1e-5);
    }

    #[test]
    fn nuke_screen_handles_hdr_without_inverting() {
        let b = [2.0, 0.5, 0.2, 1.0];
        let a = [1.5, 0.5, 0.5, 1.0];
        let out = composite(BlendMode::NukeScreen, b, a, 1.0);
        assert!((out[0] - 2.0).abs() < 1e-5);
        assert!((out[1] - 0.75).abs() < 1e-5);
    }

    #[test]
    fn photoshop_add_keeps_32_bit_values_above_one() {
        let out = composite(BlendMode::LinearDodge, [0.8, 0.5, 0.2, 1.0], [0.7, 0.9, 1.4, 1.0], 1.0);
        assert!((out[0] - 1.5).abs() < 1e-5 && (out[2] - 1.6).abs() < 1e-5, "{out:?}");
    }

    #[test]
    fn nuke_geometric_and_conjoint_over_delegate_cleanly() {
        let b = [0.5, 0.5, 0.5, 0.8];
        let a = [0.5, 0.5, 0.5, 0.4];
        let geo = composite(BlendMode::NukeGeometric, b, a, 1.0);
        let conj = composite(BlendMode::NukeConjointOver, b, a, 1.0);
        assert!(geo[3] > 0.0 && conj[3] > 0.0);
    }
}