//! Image › Input Colour Space…: what colour space the active document's pixels are in, for the
//! OCIO viewer (`photocraft_engine::ocio_viewer::OcioViewer::input`), as the colorspace knob of
//! Nuke's Read. It describes the image, so it lives here rather than on the viewer bar, which
//! shows a short `IN:` tag while it isn't automatic. With explicit params the engine runs directly.

use egui::{Align2, RichText, Sense, pos2, vec2};
use photocraft_engine::ocio_viewer::INPUT_DISPLAY;
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

pub const COMMAND: &str = "image.inputColorSpace";
const MARK: &str = "__inputSpace";

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key(MARK)
}

/// The short tag the viewer bar shows for a non-automatic `input`.
pub fn tag(input: &str) -> String {
    match input {
        "" => String::new(),
        INPUT_DISPLAY => tl!("IN: Display-referred").to_string(),
        s => crate::i18n::fmt(tl!("IN: {space}"), &[("space", s)]),
    }
}

pub fn open(app: &mut PhotocraftApp) -> Result<u64, String> {
    let (v, cfg) = photocraft_engine::ocio_viewer::menus(&app.session)?;
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__label".into(), json!("Input Colour Space…"));
    f.insert("__enabled".into(), json!(v.enabled));
    f.insert("__spaces".into(), json!(cfg.colorspaces().iter().map(|c| c.name.clone()).collect::<Vec<_>>()));
    f.insert("input".into(), json!(v.input));
    Ok(app.ui.open_dialog(crate::state::DialogKind::Command, f))
}

pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let input = f.get("input").and_then(Value::as_str).unwrap_or("").to_string();
    app.run(COMMAND, json!({"input": input}))
}

fn note(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(t.text_dim)).wrap());
}

pub fn body(ui: &mut egui::Ui, f: &mut Map<String, Value>) {
    ui.set_max_width(420.0);
    ui.spacing_mut().item_spacing.y = 6.0;
    let spaces: Vec<String> = f.get("__spaces").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
    let mut input = f.get("input").and_then(Value::as_str).unwrap_or("").to_string();
    let mut opts: Vec<(String, &str)> = vec![(String::new(), tl!("Automatic (from the document's profile)")), (INPUT_DISPLAY.to_string(), tl!("Display-referred (graded image)"))];
    opts.extend(spaces.iter().map(|s| (s.clone(), s.as_str())));
    ui.horizontal(|ui| {
        let t = Tokens::get(ui.ctx());
        let (r, _) = ui.allocate_exact_size(vec2(96.0, 22.0), Sense::hover());
        ui.painter().text(pos2(r.right() - 6.0, r.center().y), Align2::RIGHT_CENTER, tl!("Colour space:"), egui::FontId::proportional(12.0), t.text_dim);
        if crate::widgets::dropdown_literal(ui, "input-space", tl!("Input Colour Space"), &mut input, &opts, 280.0) {
            f.insert("input".into(), json!(input.clone()));
        }
    });
    note(
        ui,
        match input.as_str() {
            "" => tl!("sRGB images are decoded to linear light and linear documents are used as they are, then shown through the viewer's view. A graded sRGB image looks darker and more contrasty under an ACES view, as it does in Nuke."),
            INPUT_DISPLAY => tl!("The image is taken as already graded for the screen: the viewer's view is inverted first, so it looks as it does without OCIO, and a look or LUT applies on top. Use it for sRGB photos, textures and matte paintings."),
            _ => tl!("The pixel values are taken as this colour space of the OCIO config (for example a log plate) and converted to the scene-linear space before the view."),
        },
    );
    if f.get("__enabled").and_then(Value::as_bool) == Some(false) {
        note(ui, tl!("Only the OCIO viewer uses this; it is off for this document."));
    }
    note(ui, tl!("Display only: the pixels never change."));
}
