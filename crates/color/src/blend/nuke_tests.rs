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
    // 32-bit: values above 1 are not clamped to 1.
    assert!(hl(0.75, 2.0) > 1.9);
    assert!(hl(3.0, 0.5) > 4.9);
    // Overlay is hard light with A and B swapped (decided by B).
    assert!(close(blend_channel(BlendMode::NukeOverlay, 0.6, 0.1), hl(0.6, 0.1)));
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
