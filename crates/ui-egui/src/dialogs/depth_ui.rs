//! Image › Mode › 16 / 8 Bits/Channel on a 32-bit document: a dialog that says what the
//! conversion does and how values above 1.0 are handled (the role of Photoshop's HDR Toning).
//! The look of values up to 1.0 is always kept: the pixels are sRGB-encoded and a linear profile
//! becomes its sRGB-curve twin (`photocraft_engine::image_cmds::convert_depth_keeping_look`).
//! With explicit params (CLI, MCP, Actions) the engine runs directly.

use std::cell::RefCell;
use std::sync::Arc;

use egui::{Align2, Color32, Rect, RichText, Sense, pos2, vec2};
use photocraft_color::{ColorMode, SampleType};
use photocraft_engine::display_color::CanvasDisplay;
use photocraft_engine::image_cmds::{NarrowMethod, narrow_linear};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

const MARK: &str = "__depth";
const PREF: &str = "image.mode.narrowFrom32";
const KEYS: [&str; 3] = ["method", "exposure", "gamma"];
/// The preview's longer side, in pixels.
const PREVIEW_SIDE: u32 = 420;

/// The dialog's preview: the 32-bit composite reduced to [`PREVIEW_SIDE`] (linear light, taken
/// once when the dialog opens) and the canvas's display transform, so the preview shows what the
/// canvas will. Each option change re-tones this small copy only; the document is untouched
/// until OK.
struct Preview {
    token: u64,
    src: photocraft_compose::Buffer,
    display: Arc<CanvasDisplay>,
    shown: Option<(String, egui::TextureHandle)>,
}

thread_local! {
    static PREVIEW: RefCell<Option<Preview>> = const { RefCell::new(None) };
}

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key(MARK)
}

/// The commands the dialog fronts: 16 and 8 Bits/Channel, while the document is 32-bit.
pub fn fronts(app: &PhotocraftApp, id: &str) -> bool {
    matches!(id, "image.mode.bits16" | "image.mode.bits8") && app.session.active().is_some_and(|st| st.doc.depth == SampleType::F32)
}

pub fn open(app: &mut PhotocraftApp, command: &str) -> Option<u64> {
    let st = app.session.active()?;
    let doc = &st.doc;
    // The composite as the 32-bit canvas shows it, reduced (a proxy for large documents).
    let src = photocraft_compose::thumbnail_buffer(doc, PREVIEW_SIDE);
    let token = PREVIEW.with(|p| p.borrow().as_ref().map_or(1, |p| p.token.wrapping_add(1)));
    if let Ok(display) = app.session.color.canvas_display(doc) {
        PREVIEW.with(|p| *p.borrow_mut() = Some(Preview { token, src, display, shown: None }));
    }
    let profile = photocraft_engine::color_cmds::document_profile(doc);
    let linear = doc.mode == ColorMode::Rgb && photocraft_engine::image_cmds::is_linear_rgb_profile(&profile);
    let sixteen = command == "image.mode.bits16";
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__command".into(), json!(command));
    f.insert("__label".into(), json!(if sixteen { "Convert to 16 Bits/Channel…" } else { "Convert to 8 Bits/Channel…" }));
    f.insert("__profile".into(), json!(profile.description.clone()));
    f.insert("__linear".into(), json!(linear));
    f.insert("__keepLinear".into(), json!(linear && sixteen));
    f.insert("__preview".into(), json!(token));
    f.insert("__clipped".into(), json!(false));
    f.insert("method".into(), json!("clip"));
    f.insert("exposure".into(), json!(0.0));
    f.insert("gamma".into(), json!(1.0));
    if let Some(Value::Object(saved)) = app.session.prefs().dialogs.get(PREF) {
        for key in KEYS {
            if let Some(v) = saved.get(key) {
                f.insert(key.into(), v.clone());
            }
        }
    }
    // Keep Linear only where it applies.
    if f.get("method").and_then(Value::as_str) == Some("keepLinear") && !(linear && sixteen) {
        f.insert("method".into(), json!("clip"));
    }
    Some(app.ui.open_dialog(crate::state::DialogKind::Command, f))
}

pub fn params(f: &Map<String, Value>) -> Value {
    let method = f.get("method").and_then(Value::as_str).unwrap_or("clip");
    let mut p = Map::new();
    p.insert("method".into(), json!(method));
    if method == "exposureGamma" {
        p.insert("exposure".into(), f.get("exposure").cloned().unwrap_or(json!(0.0)));
        p.insert("gamma".into(), f.get("gamma").cloned().unwrap_or(json!(1.0)));
    }
    Value::Object(p)
}

pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let command = f.get("__command").and_then(Value::as_str).unwrap_or("image.mode.bits16").to_string();
    let p = params(f);
    let result = app.run(&command, p);
    if result.is_ok() {
        let remembered: Map<String, Value> = KEYS.iter().filter_map(|k| f.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
        app.session.prefs.edit(|pr| pr.dialogs.insert(PREF.into(), Value::Object(remembered)));
    }
    result
}

fn label(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(112.0, 22.0), Sense::hover());
    ui.painter().text(pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, text, egui::FontId::proportional(12.0), t.text_dim);
}

fn note(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(t.text_dim)).wrap());
}

pub fn body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    ui.set_max_width(420.0);
    ui.spacing_mut().item_spacing.y = 6.0;
    let profile = f.get("__profile").and_then(Value::as_str).unwrap_or("").to_string();
    let linear = f.get("__linear").and_then(Value::as_bool).unwrap_or(false);
    let keep_linear = f.get("__keepLinear").and_then(Value::as_bool).unwrap_or(false);

    preview(ui, f);

    crate::widgets::section_label(ui, tl!("What happens"));
    crate::widgets::hairline(ui);
    note(ui, tl!("32-bit documents hold linear light. 8- and 16-bit documents hold gamma-encoded values, so the pixels are encoded with the sRGB curve."));
    if linear {
        note(
            ui,
            &crate::i18n::fmt(
                tl!("The document's profile, {profile}, becomes the same colour space with the sRGB curve, so the image looks the same. Values above 1.0 can't be stored at this depth: choose how to handle them below."),
                &[("profile", &profile)],
            ),
        );
    } else {
        note(ui, tl!("The image looks the same up to 1.0. Values above 1.0 can't be stored at this depth: choose how to handle them below."));
    }

    ui.add_space(4.0);
    crate::widgets::section_label(ui, tl!("Highlights above 1.0"));
    crate::widgets::hairline(ui);
    let mut method = f.get("method").and_then(Value::as_str).unwrap_or("clip").to_string();
    let mut opts: Vec<(String, &str)> = vec![
        ("clip".into(), tl!("Clip (exact match below 1.0)")),
        ("highlightCompression".into(), tl!("Highlight Compression")),
        ("exposureGamma".into(), tl!("Exposure and Gamma")),
    ];
    if keep_linear {
        opts.push(("keepLinear".into(), tl!("Keep Linear (no change)")));
    }
    ui.horizontal(|ui| {
        label(ui, tl!("Method:"));
        if crate::widgets::dropdown(ui, "depth-method", tl!("Method"), &mut method, &opts, 230.0) {
            f.insert("method".into(), json!(method.clone()));
        }
    });
    match method.as_str() {
        "highlightCompression" => note(ui, tl!("Rolls values above 0.8 smoothly towards 1.0, keeping hue, so bright highlights keep detail instead of clipping. Values below 0.8 are unchanged.")),
        "exposureGamma" => {
            note(ui, tl!("Scales the image in linear light before converting: lower the exposure to bring highlights under 1.0. Exposure 0 and gamma 1.0 is the same as Clip."));
            ui.horizontal(|ui| {
                label(ui, tl!("Exposure:"));
                let mut e = f.get("exposure").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                if crate::widgets::value_field(ui, &mut e, -20.0..=20.0, "stops", 84.0).accessible_name(tl!("Exposure:")).changed() {
                    f.insert("exposure".into(), json!(e));
                }
            });
            ui.horizontal(|ui| {
                label(ui, tl!("Gamma:"));
                let mut g = f.get("gamma").and_then(Value::as_f64).unwrap_or(1.0) as f32;
                if crate::widgets::value_field(ui, &mut g, 0.1..=10.0, "", 84.0).accessible_name(tl!("Gamma:")).changed() {
                    f.insert("gamma".into(), json!(g));
                }
            });
        }
        "keepLinear" => note(ui, tl!("Keeps the pixels and the profile linear: nothing changes below 1.0, values above it are clipped. Best for round-trips to Nuke; 16-bit linear has less precision in deep shadows than 32-bit.")),
        _ => note(ui, tl!("Identical below 1.0; brighter values are clipped to 1.0.")),
    }
}

/// The preview's pixels for `method`: the source re-toned as the conversion will (values then
/// clipped at 1.0, as integer samples are), the source untouched for `original`, and with
/// `clipped` the pixels that still exceed 1.0 after the method in magenta.
fn retone(src: &photocraft_compose::Buffer, method: NarrowMethod, original: bool, clipped: bool) -> photocraft_compose::Buffer {
    let px = src
        .px
        .iter()
        .map(|p| {
            if original {
                return *p;
            }
            let c = narrow_linear(method, [p[0], p[1], p[2]]);
            if clipped && c.iter().any(|v| *v > 1.0) {
                return [1.0, 0.0, 1.0, p[3]];
            }
            [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0), p[3]]
        })
        .collect();
    photocraft_compose::Buffer { rect: src.rect, px }
}

fn preview(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    let token = f.get("__preview").and_then(Value::as_u64).unwrap_or(0);
    let Some(size) = PREVIEW.with(|cell| cell.borrow().as_ref().filter(|p| p.token == token).map(|p| vec2(p.src.rect.width() as f32, p.src.rect.height() as f32)))
    else {
        return;
    };
    // Lay the box out first, so a press on it this frame shows the original this frame.
    let scale = (PREVIEW_SIDE as f32 / size.x.max(size.y).max(1.0)).clamp(f32::MIN_POSITIVE, 1.0);
    let box_size = vec2(PREVIEW_SIDE as f32, (size.y * scale).min(PREVIEW_SIDE as f32));
    let (r, resp) = ui.allocate_exact_size(box_size, Sense::click_and_drag());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tl!("Press and hold to see the original"));
    // Click and hold: the 32-bit original, as Camera Raw and Nuke's A/B compare do.
    let original = resp.is_pointer_button_down_on();
    let method = NarrowMethod::from_params(&params(f)).unwrap_or(NarrowMethod::Clip);
    let mut clipped = f.get("__clipped").and_then(Value::as_bool).unwrap_or(false);
    let key = format!("{method:?}/{original}/{clipped}");
    let tex = PREVIEW.with(|cell| {
        let mut cell = cell.borrow_mut();
        let p = cell.as_mut().filter(|p| p.token == token)?;
        if p.shown.as_ref().is_none_or(|(k, _)| *k != key) {
            let img = p.display.to_rgba8(&retone(&p.src, method, original, clipped && !original));
            let px_size = [img.width as usize, img.height as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(px_size, &img.pixels);
            match p.shown.as_mut() {
                Some((k, tex)) => {
                    tex.set(color, egui::TextureOptions::LINEAR);
                    *k = key.clone();
                }
                None => p.shown = Some((key.clone(), ui.ctx().load_texture("depth-preview", color, egui::TextureOptions::LINEAR))),
            }
        }
        p.shown.as_ref().map(|(_, tex)| tex.id())
    });
    let Some(tex) = tex else { return };
    ui.painter().rect_filled(r, 2.0, t.canvas);
    let img = Rect::from_center_size(r.center(), size * scale);
    ui.painter().image(tex, img, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    // Which image is showing, in the corner.
    let tag = if original { tl!("Original (32-bit)") } else { tl!("Converted") };
    let galley = ui.painter().layout_no_wrap(tag.to_string(), egui::FontId::proportional(11.0), Color32::WHITE);
    let tag_rect = Rect::from_min_size(img.left_top() + vec2(6.0, 6.0), galley.size() + vec2(10.0, 4.0));
    ui.painter().rect_filled(tag_rect, 3.0, Color32::from_black_alpha(160));
    ui.painter().galley(tag_rect.min + vec2(5.0, 2.0), galley, Color32::WHITE);
    note(ui, tl!("Press and hold the preview to compare with the original."));
    if crate::widgets::checkbox(ui, &mut clipped, tl!("Show clipped highlights (magenta)")).changed() {
        f.insert("__clipped".into(), json!(clipped));
    }
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_carry_only_what_the_method_uses() {
        let mut f = Map::new();
        f.insert("method".into(), json!("clip"));
        f.insert("exposure".into(), json!(-2.0));
        assert_eq!(params(&f), json!({"method": "clip"}));
        f.insert("method".into(), json!("exposureGamma"));
        f.insert("gamma".into(), json!(1.2));
        assert_eq!(params(&f), json!({"method": "exposureGamma", "exposure": -2.0, "gamma": 1.2}));
    }

    #[test]
    fn the_preview_retones_as_the_conversion_does() {
        let src = photocraft_compose::Buffer { rect: photocraft_geom::Rect::new(0, 0, 2, 1), px: vec![[0.5, 2.0, 0.1, 1.0], [0.2, 0.2, 0.2, 0.5]] };
        let clip = retone(&src, NarrowMethod::Clip, false, false);
        assert_eq!(clip.px[0], [0.5, 1.0, 0.1, 1.0]);
        assert_eq!(clip.px[1], [0.2, 0.2, 0.2, 0.5], "alpha and in-range values untouched");
        assert_eq!(retone(&src, NarrowMethod::Clip, true, false).px[0], src.px[0], "original");
        assert_eq!(retone(&src, NarrowMethod::Clip, false, true).px[0], [1.0, 0.0, 1.0, 1.0], "clipped in magenta");
        let rolled = retone(&src, NarrowMethod::HighlightCompression, false, true).px[0];
        assert!(rolled[1] < 1.0 && rolled[0] < 0.5, "compression leaves nothing to clip: {rolled:?}");
    }
}
