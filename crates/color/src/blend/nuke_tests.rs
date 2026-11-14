use super::*;

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

#[test]
fn nuke_hard_light_matches_nuke_merge_math_and_keeps_float() {
    // A = source (top), B = backdrop. Multiply below A = 0.5, screen above, continuous at 0.5.
    let hl = |a: f32, b: f32| blend_channel(BlendMode::NukeHardLight, b, a);
    assert!(close(hl(0.25, 0.4), 0.2));
    // A > 0.5 over a dark backdrop: 1 - 2(1-A)(1-B), not A (the old max() fallback bug).
    assert!(close(hl(0.6, 0.1), 0.28));
    assert!(close(hl(0.9, 0.2), 0.84));
    for b in [0.0f32, 0.1, 0.4, 0.5, 0.9, 1.0, 3.0] {
        assert!(close(hl(0.5, b), b), "neutral at A = 0.5 for B = {b}");
        assert!((hl(0.5 - 1e-4, b) - hl(0.5 + 1e-4, b)).abs() < 1e-3, "continuous at 0.5 for B = {b}");
    }
    // 32-bit: nothing is clamped to 1. The screen branch is screen(2A - 1, B), and Nuke's screen
    // takes A + B - AB whenever either input is at most 1; 1 - 2(1-A)(1-B) agrees. Only a
    // max(A, B) fallback would give 2 and 5 here.
    assert!(close(hl(0.75, 2.0), 1.5), "{}", hl(0.75, 2.0));
    assert!(close(hl(3.0, 0.5), 3.0), "{}", hl(3.0, 0.5));
    // Overlay is hard light with A and B swapped (decided by B).
    assert!(close(blend_channel(BlendMode::NukeOverlay, 0.6, 0.1), hl(0.6, 0.1)));
}

#[test]
fn nuke_soft_light_matches_nuke_merge_math_and_keeps_float() {
    // Foundry's documented algorithm, A = source (top), B = backdrop:
    // B(2A + B(1 - AB)) where AB < 1, else 2AB.
    let sl = |a: f32, b: f32| blend_channel(BlendMode::NukeSoftLight, b, a);
    // Unlike Photoshop's soft light it lightens at A = 0.5 too.
    assert!(close(sl(0.5, 0.5), 0.6875), "{}", sl(0.5, 0.5));
    // The B(1 - AB) term, not B(1 - 2A): an earlier version had 0.896 here.
    assert!(close(sl(0.8, 0.8), 1.5104), "{}", sl(0.8, 0.8));
    assert!(close(sl(0.0, 0.6), 0.36));
    // Continuous where it switches to 2AB.
    assert!(close(sl(1.0, 1.0), 2.0));
    // Above 1 nothing is clipped first: a hot A over a dark B stays in the AB < 1 branch...
    assert!(close(sl(4.0, 0.1), 0.806), "{}", sl(4.0, 0.1));
    // ...and a bright plate takes 2AB.
    assert!(close(sl(2.0, 1.0), 4.0));
}

#[test]
fn photoshop_modes_keep_highlights_in_32_bit() {
    // Screen of a 50.0 sun over itself is the sun, not 100 - 2500.
    for mode in [BlendMode::Screen, BlendMode::Overlay, BlendMode::HardLight, BlendMode::SoftLight, BlendMode::ColorDodge, BlendMode::Exclusion] {
        assert!(close(blend_channel_hdr(mode, 50.0, 50.0), 50.0), "{mode:?}");
        assert!(blend_channel_hdr(mode, 50.0, 0.3) >= 50.0, "{mode:?} keeps a bright backdrop");
        // Inside 0..1 the formula is untouched, and so is every integer depth.
        assert_eq!(blend_channel_hdr(mode, 0.4, 0.7), blend_channel(mode, 0.4, 0.7), "{mode:?}");
    }
    assert!(close(blend_channel(BlendMode::Screen, 0.5, 0.5), 0.75));
}

#[test]
fn photoshop_modes_are_photoshops_and_nuke_modes_are_nukes() {
    // Photoshop's Subtract is backdrop − source, clipped at 0; Nuke's minus is A − B.
    assert!(close(blend_channel(BlendMode::Subtract, 0.75, 0.25), 0.5));
    assert!(close(blend_channel(BlendMode::Subtract, 0.25, 0.75), 0.0));
    assert!(close(blend_channel(BlendMode::NukeMinus, 0.25, 0.75), 0.5));
    assert!(close(blend_channel(BlendMode::NukeFrom, 0.75, 0.25), 0.5));
    // Photoshop's Linear Light; Nuke's hypot.
    assert!(close(blend_channel(BlendMode::LinearLight, 0.25, 0.5), 0.25));
    assert!(close(blend_channel(BlendMode::NukeHypot, 0.3, 0.4), 0.5));
    // Multiply leaves the backdrop where the source is transparent in both.
    let bg = [0.2, 0.4, 0.6, 1.0];
    for m in [BlendMode::Multiply, BlendMode::NukeMultiply] {
        assert_eq!(composite(m, bg, [1.0, 0.0, 0.0, 0.0], 1.0), bg, "{m:?}");
    }
    // Nuke's min takes the backdrop to black where the source is transparent; Darken doesn't.
    let min = composite(BlendMode::NukeMin, bg, [1.0, 1.0, 1.0, 0.0], 1.0);
    assert!(close(min[3], 0.0), "{min:?}");
    assert_eq!(composite(BlendMode::Darken, bg, [1.0, 1.0, 1.0, 0.0], 1.0), bg);
}

#[test]
fn menus_list_photoshops_modes_and_nuke_ones_only_when_asked() {
    set_nuke_modes_in_menus(false);
    assert_eq!(BlendMode::menu_modes().collect::<Vec<_>>(), BlendMode::LAYER_MODES.to_vec());
    assert!(!BlendMode::NukeMultiply.is_menu_mode());
    set_nuke_modes_in_menus(true);
    let menu: Vec<_> = BlendMode::menu_modes().collect();
    assert_eq!(menu.len(), 27 + 18);
    assert!(BlendMode::NukeMultiply.is_menu_mode() && !BlendMode::NukeAtop.is_menu_mode());
    set_nuke_modes_in_menus(false);
    // Every mode is in exactly one list, and the Nuke ones all have an operation.
    let all: Vec<_> = BlendMode::all_modes().collect();
    assert_eq!(all.len(), 33 + 28);
    for (i, m) in all.iter().enumerate() {
        assert!(!all[..i].contains(m), "{m:?} twice");
        assert_eq!(m.is_nuke(), m.nuke_op().is_some() && *m != BlendMode::Normal, "{m:?}");
        assert!(!m.is_nuke() || m.label().ends_with("(Nuke)"), "{m:?}");
    }
    // Labels are unique, so a name finds one mode.
    let mut labels: Vec<_> = all.iter().map(|m| m.label().to_ascii_lowercase()).collect();
    labels.sort();
    labels.dedup();
    assert_eq!(labels.len(), all.len());
}

#[test]
fn psd_keys_read_as_photoshop_modes_and_nuke_ones_write_their_nearest() {
    for key in [b"mul ", b"scrn", b"over", b"lbrn", b"fsub", b"fdiv", b"hue ", b"vLit"] {
        let m = BlendMode::from_psd_key(*key).expect("a Photoshop mode");
        assert!(!m.is_nuke(), "{m:?}");
    }
    assert_eq!(BlendMode::NukeMultiply.psd_key(), *b"mul ");
    assert_eq!(BlendMode::NukeFrom.psd_key(), *b"fsub");
    assert_eq!(BlendMode::NukeHypot.psd_key(), *b"norm");
    assert!(!BlendMode::NukeMultiply.has_psd_equivalent());
    // Nuke scripts: Photoshop modes map to the closest Merge operation, inexactly.
    assert_eq!(BlendMode::Multiply.nearest_nuke_op(), ("multiply", false));
    assert_eq!(BlendMode::Subtract.nearest_nuke_op(), ("from", false));
    assert_eq!(BlendMode::NukeMultiply.nearest_nuke_op(), ("multiply", true));
    assert_eq!(BlendMode::Normal.nearest_nuke_op(), ("over", true));
    assert_eq!(BlendMode::Hue.nearest_nuke_op(), ("over", false));
}

/// Foundry's Merge table, literally (learn.foundry.com, "Merge Operations"): one channel of the
/// premultiplied result, `ca`/`cb` the A and B colours, `a`/`b` their alphas. For the alpha
/// channel itself pass the alphas as the colours. `None` where Foundry gives no formula
/// (color-burn, color-dodge) or the value is undefined.
fn foundry(op: BlendMode, ca: f32, cb: f32, a: f32, b: f32) -> Option<f32> {
    use BlendMode::*;
    let screen = |x: f32, y: f32| if x <= 1.0 || y <= 1.0 { x + y - x * y } else { x.max(y) };
    let light = |x: f32, y: f32| if x < 0.5 { 2.0 * x * y } else { screen(2.0 * x - 1.0, y) };
    Some(match op {
        NukePlus => ca + cb,
        NukeMinus => ca - cb,
        NukeFrom => cb - ca,
        NukeMultiply => if ca < 0.0 && cb < 0.0 { ca } else { ca * cb },
        NukeDivide if cb.abs() < 1e-12 => return None,
        NukeDivide => if ca < 0.0 && cb < 0.0 { 0.0 } else { ca / cb },
        NukeScreen => screen(ca, cb),
        NukeMin => ca.min(cb),
        NukeMax => ca.max(cb),
        NukeDifference => (ca - cb).abs(),
        NukeExclusion => ca + cb - 2.0 * ca * cb,
        NukeHypot => (ca * ca + cb * cb).sqrt(),
        NukeGeometric if (ca + cb).abs() < 1e-12 => return None,
        NukeGeometric => 2.0 * ca * cb / (ca + cb),
        NukeAverage => (ca + cb) / 2.0,
        NukeOverlay => light(cb, ca),
        NukeHardLight => light(ca, cb),
        NukeSoftLight => if ca * cb < 1.0 { cb * (2.0 * ca + cb * (1.0 - ca * cb)) } else { 2.0 * ca * cb },
        NukeUnder => ca * (1.0 - b) + cb,
        NukeIn => ca * b,
        NukeOut => ca * (1.0 - b),
        NukeAtop => ca * b + cb * (1.0 - a),
        NukeXor => ca * (1.0 - b) + cb * (1.0 - a),
        NukeMask => cb * a,
        NukeStencil => cb * (1.0 - a),
        NukeConjointOver if a > b => ca,
        NukeConjointOver if b <= 0.0 => return None,
        NukeConjointOver => ca + cb * (1.0 - a / b),
        NukeDisjointOver if a + b < 1.0 => ca + cb,
        NukeDisjointOver if b <= 0.0 => return None,
        NukeDisjointOver => ca + cb * (1.0 - a) / b,
        _ => return None,
    })
}

const FORMULA_OPS: [BlendMode; 25] = {
    use BlendMode::*;
    [
        NukePlus, NukeMinus, NukeFrom, NukeMultiply, NukeDivide, NukeScreen, NukeMin, NukeMax, NukeDifference, NukeExclusion,
        NukeHypot, NukeGeometric, NukeAverage, NukeOverlay, NukeHardLight, NukeSoftLight, NukeUnder, NukeIn, NukeOut, NukeAtop,
        NukeXor, NukeMask, NukeStencil, NukeConjointOver, NukeDisjointOver,
    ]
};

/// Merge operations that combine coverage themselves; the export never turns alpha masking on
/// for them. Matte is left out of the partial-alpha check: see the note on its test below.
const PORTER_DUFF: [BlendMode; 9] = {
    use BlendMode::*;
    [NukeUnder, NukeIn, NukeOut, NukeAtop, NukeXor, NukeMask, NukeStencil, NukeConjointOver, NukeDisjointOver]
};

/// What alpha the export makes Nuke compute for `op` over opaque pixels: a Porter-Duff op or one
/// written without alpha masking runs its own formula on the alphas; one with alpha masking
/// (`screen_alpha`, written wherever the canvas's alpha is a + b - ab) gives a + b - ab = 1.
fn opaque_alpha(op: BlendMode) -> Option<f32> {
    let masked = !PORTER_DUFF.contains(&op) && (composite(op, [0.4, 0.4, 0.4, 0.3], [0.6, 0.6, 0.6, 0.5], 1.0)[3] - 0.65).abs() < 1e-5;
    if masked { Some(1.0) } else { foundry(op, 1.0, 1.0, 1.0, 1.0).map(|v| v.clamp(0.0, 1.0)) }
}

fn check_opaque(op: BlendMode, ca: f32, cb: f32) {
    let got = composite_hdr(op, [cb, cb, cb, 1.0], [ca, ca, ca, 1.0], 1.0);
    let Some(alpha) = opaque_alpha(op) else { return };
    assert!((got[3] - alpha).abs() < 1e-5, "{op:?} A={ca} B={cb}: alpha {} for Nuke's {alpha}", got[3]);
    let Some(want) = foundry(op, ca, cb, 1.0, 1.0) else { return };
    if alpha > 0.0 {
        let want = want / alpha;
        assert!((got[0] - want).abs() <= 1e-4 * want.abs().max(1.0), "{op:?} A={ca} B={cb}: {} for Nuke's {want}", got[0]);
    }
}

#[test]
fn every_nuke_operation_matches_foundrys_formula_on_opaque_pixels() {
    // Including light above 1 and the small negatives ACEScg keeps.
    let values = [-0.25f32, 0.0, 0.1, 0.25, 0.5, 0.75, 1.0, 1.5, 3.0];
    for op in FORMULA_OPS {
        for ca in values {
            for cb in values {
                check_opaque(op, ca, cb);
            }
        }
    }
}

#[test]
fn opaque_layers_never_go_transparent_under_the_subtracting_operations() {
    // Their own formulas on the alphas give 0 for two opaque inputs (|1 - 1|, 1 + 1 - 2, 1 - 1),
    // which a plain Merge leaves as colour with no alpha and straight alpha cannot hold at all.
    // With alpha masking, as the export writes them, the result covers.
    for op in [BlendMode::NukeDifference, BlendMode::NukeExclusion, BlendMode::NukeMinus, BlendMode::NukeFrom] {
        let got = composite_hdr(op, [0.2, 0.2, 0.2, 1.0], [0.7, 0.7, 0.7, 1.0], 1.0);
        assert!((got[3] - 1.0).abs() < 1e-6, "{op:?} went transparent: {got:?}");
    }
}

fn check_porter_duff(op: BlendMode, a: f32, b: f32, ca: f32, cb: f32) {
    let (pa, pb) = (ca * a, cb * b);
    let (Some(c), Some(alpha)) = (foundry(op, pa, pb, a, b), foundry(op, a, b, a, b)) else { return };
    let alpha = alpha.clamp(0.0, 1.0);
    let got = composite_hdr(op, [cb, cb, cb, b], [ca, ca, ca, a], 1.0);
    assert!((got[3] - alpha).abs() < 1e-4, "{op:?} a={a} b={b}: alpha {} for Nuke's {alpha}", got[3]);
    if alpha > 1e-6 {
        let want = c / alpha;
        assert!((got[0] - want).abs() <= 1e-3 * want.abs().max(1.0), "{op:?} a={a} b={b} A={ca} B={cb}: {} for {want}", got[0]);
    }
}

#[test]
fn porter_duff_operations_match_foundry_at_every_coverage() {
    // These are defined by alpha, so Foundry's table pins them at partial coverage too, the alpha
    // channel included. An empty A is a case: `out` gives nothing there, not the backdrop.
    let alphas = [0.0f32, 0.3, 0.7, 1.0];
    let colours = [0.2f32, 0.8, 2.0];
    for op in PORTER_DUFF {
        for a in alphas {
            for b in alphas {
                for ca in colours {
                    for cb in colours {
                        check_porter_duff(op, a, b, ca, cb);
                    }
                }
            }
        }
    }
}

#[test]
fn masked_operations_leave_each_input_alone_where_the_other_is_empty() {
    // Merge's alpha masking: "the input images are unchanged where the other image has zero alpha,
    // and the output alpha is set to a+b-a*b". Multiply used to black the source out where the
    // backdrop was empty.
    use BlendMode::*;
    for op in [NukeMultiply, NukeDivide, NukeMinus, NukeFrom, NukeDifference, NukeExclusion, NukeOverlay, NukeSoftLight, NukeAverage] {
        let over_empty = composite_hdr(op, [0.9, 0.9, 0.9, 0.0], [0.3, 0.6, 2.0, 0.7], 1.0);
        assert!((over_empty[0] - 0.3).abs() < 1e-5 && (over_empty[2] - 2.0).abs() < 1e-4, "{op:?} over nothing: {over_empty:?}");
        assert!((over_empty[3] - 0.7).abs() < 1e-6);
        let empty = composite_hdr(op, [0.3, 0.6, 2.0, 0.6], [0.9, 0.9, 0.9, 0.0], 1.0);
        assert!((empty[0] - 0.3).abs() < 1e-5 && (empty[3] - 0.6).abs() < 1e-6, "{op:?} from nothing: {empty:?}");
        let both = composite_hdr(op, [0.5, 0.5, 0.5, 0.4], [0.5, 0.5, 0.5, 0.5], 1.0);
        assert!((both[3] - 0.7).abs() < 1e-6, "{op:?}: alpha {} for a + b - ab = 0.7", both[3]);
    }
}

#[test]
fn nuke_screen_multiply_and_divide_follow_foundrys_conditions() {
    let ch = |op: BlendMode, a: f32, b: f32| blend_channel_hdr(op, b, a);
    // Screen takes the formula when either input is at most 1, with no lower bound...
    assert!(close(ch(BlendMode::NukeScreen, 2.0, 0.5), 1.5));
    assert!(close(ch(BlendMode::NukeScreen, -0.25, -0.25), -0.5625));
    // ...and max(A, B) only when both are above 1, so a hot highlight never goes negative.
    assert!(close(ch(BlendMode::NukeScreen, 4.0, 3.0), 4.0));
    // Two negatives don't make a positive.
    assert!(close(ch(BlendMode::NukeMultiply, -0.25, -0.5), -0.25));
    assert!(close(ch(BlendMode::NukeDivide, -0.25, -0.5), 0.0));
    assert!(close(ch(BlendMode::NukeDivide, 0.5, -0.25), -2.0));
}

/// Matte is Foundry's "Aa + B(1-a)", the over for an *unpremultiplied* A: PhotoCraft's layer
/// colour is straight, so A·a is the premultiplied colour and the result is an over. What Nuke
/// does to matte's alpha isn't in Foundry's table; Natron forces alpha masking (a + b - ab), the
/// canvas's too. Pinned here so a change is deliberate.
#[test]
fn nuke_matte_is_an_over_of_the_straight_colour() {
    let got = composite_hdr(BlendMode::NukeMatte, [0.8, 0.8, 0.8, 0.5], [0.2, 0.2, 0.2, 0.3], 1.0);
    let alpha = 0.3 + 0.5 * 0.7;
    assert!((got[3] - alpha).abs() < 1e-6);
    assert!((got[0] - (0.2 * 0.3 + 0.8 * 0.5 * 0.7) / alpha).abs() < 1e-5, "{got:?}");
}
