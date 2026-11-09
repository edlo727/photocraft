use super::{PhotocraftApp, RichText, Tokens, label, vec2, widgets};

// ----------------------------------------------------------------------------- status bar

/// The status bar's height and the height of the controls in it (zoom field, Fit), which leaves
/// them an even margin above and below inside the bar (#2823).
pub fn status_bar_heights(t: &Tokens) -> (f32, f32) {
    if t.pro { (24.0, 18.0) } else { (30.0, 22.0) }
}

pub fn status_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    egui::Panel::bottom("status_bar")
        .exact_size(status_bar_heights(&t).0)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            let control_h = status_bar_heights(&t).1;
            // Read from the right in a right-to-left UI, with the trailing group at the left.
            crate::direction::horizontal_centered(ui, |ui| {
                if t.pro {
                    crate::chrome_ui::status_bar_pro(app, ui);
                    // Background job progress with Cancel, at the far end (#210).
                    ui.with_layout(crate::direction::trailing(), |ui| {
                        crate::jobs_ui::status_progress(app, ui);
                        crate::background_redraw::status(app, ui);
                        crate::chrome_ui::render_path_status(app, ui);
                    });
                    return;
                }
                if let (Some(st), Some(i)) = (app.session.active(), app.session.active_index()) {
                    let (w, h, mode, bits, layers) =
                        (st.doc.size.width, st.doc.size.height, crate::canvas::mode_label(&st.doc), st.doc.depth.bits(), st.doc.layer_count());
                    let mut pct = app.ui.views[i].zoom * 100.0;
                    let field =
                        widgets::compact_wrapping_value_field(ui, &mut pct, crate::zoom_levels::percent_range(&app.ui.views[i]), "%", vec2(78.0, control_h))
                            .accessible_name(tl!("Zoom"));
                    if field.changed() {
                        let animated = app.session.prefs().general.animated_zoom;
                        crate::zoom_levels::set_from_field(&mut app.ui.views[i], field.dragged() || field.drag_started(), pct, animated);
                    }
                    widgets::vline(ui, 16.0);
                    label(ui, &crate::i18n::fmt(tl!("{mode} Color · {bits} bit"), &[("mode", mode), ("bits", &bits.to_string())]));
                    widgets::vline(ui, 16.0);
                    label(ui, &format!("{w} × {h} px"));
                    widgets::vline(ui, 16.0);
                    label(ui, &crate::i18n::trn(crate::i18n::current(), layers as u64, "{n} layer", "{n} layers"));
                } else {
                    label(ui, tl!("No document"));
                }
                if !app.ui.status.is_empty() {
                    widgets::vline(ui, 16.0);
                    let is_err = app.ui.status_error || app.ui.status.starts_with("Couldn");
                    ui.label(RichText::new(&app.ui.status).color(if is_err { t.warning } else { t.text_faint }));
                }
                ui.with_layout(crate::direction::trailing(), |ui| {
                    if app.session.active().is_some()
                        && !t.pro
                        && widgets::compact_button(ui, tl!("Fit"), control_h).clicked()
                        && let Some(i) = app.session.active_index()
                    {
                        app.ui.views[i].fit_pending = true;
                    }
                    if app.session.has_jobs() {
                        ui.add_space(6.0);
                        crate::jobs_ui::status_progress(app, ui);
                    }
                    crate::background_redraw::status(app, ui);
                    crate::chrome_ui::render_path_status(app, ui);
                });
            });
        });
    viewer_bar(app, ui);
}

/// The viewer bar above the status bar (Nuke's viewer controls), for the active document: the
/// OCIO viewer (on/off, Display, View, Look from the config), the viewer channel, and for 32-bit
/// documents exposure (stops) and gamma. Every control is display only and kept per document.
fn viewer_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.session.active().is_none() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let (bar_h, control_h) = status_bar_heights(&t);
    egui::Panel::bottom("viewer_bar")
        .exact_size(bar_h)
        .frame(egui::Frame::NONE.fill(t.chrome).inner_margin(egui::Margin::symmetric(10, 0)))
        .show(ui, |ui| {
            crate::direction::horizontal_centered(ui, |ui| {
                let mut run: Option<(&'static str, serde_json::Value)> = None;
                match photocraft_engine::ocio_viewer::menus(&app.session) {
                    Ok((v, cfg)) => {
                        let mut on = v.enabled;
                        if widgets::checkbox(ui, &mut on, tl!("OCIO")).on_hover_text(cfg.name().to_string()).changed() {
                            run = Some(("view.ocioViewer", serde_json::json!({"enabled": on})));
                        }
                        ui.add_enabled_ui(on, |ui| {
                            // Display and view as one menu, as Nuke's viewer lists them
                            // ("ACES 1.0 - SDR Video (sRGB)"). The input colour space describes the
                            // image, so it is Image › Input Colour Space…; a tag shows it here
                            // while it isn't automatic.
                            const SEP: char = '\u{1}';
                            let short = |d: &str| d.trim_end_matches(" - Display").to_string();
                            let several = cfg.displays().len() > 1;
                            let keys: Vec<(String, String)> = cfg
                                .displays()
                                .iter()
                                .flat_map(|d| {
                                    d.views.iter().map(move |view| {
                                        let label = if several { format!("{view} ({})", short(&d.name)) } else { view.clone() };
                                        (format!("{}{SEP}{view}", d.name), label)
                                    })
                                })
                                .collect();
                            let views: Vec<(String, &str)> = keys.iter().map(|(k, l)| (k.clone(), l.as_str())).collect();
                            let mut current = format!("{}{SEP}{}", v.display, v.view);
                            if widgets::dropdown_literal(ui, "viewer-view", tl!("Viewer"), &mut current, &views, 240.0)
                                && let Some((display, view)) = current.split_once(SEP)
                            {
                                run = Some(("view.ocioViewer", serde_json::json!({"display": display, "view": view})));
                            }
                            let tag = crate::input_space_ui::tag(&v.input);
                            if !tag.is_empty()
                                && ui
                                    .add(egui::Label::new(RichText::new(tag).size(11.5).color(t.accent)).sense(egui::Sense::click()))
                                    .on_hover_text(tl!("Image › Input Colour Space… (click to change)"))
                                    .clicked()
                            {
                                run = Some((crate::input_space_ui::COMMAND, serde_json::json!({})));
                            }
                            if !cfg.looks().is_empty() {
                                let mut look = v.look.clone();
                                let mut looks: Vec<(String, &str)> = vec![(String::new(), tl!("No Look"))];
                                looks.extend(cfg.looks().iter().map(|x| (x.clone(), x.as_str())));
                                if widgets::dropdown_literal(ui, "viewer-look", tl!("Look"), &mut look, &looks, 130.0) {
                                    run = Some(("view.ocioViewer", serde_json::json!({"look": look})));
                                }
                            }
                            // Viewer LUT: none, a recent file, or Load… (any format OCIO reads).
                            const LOAD: &str = "\u{0}load";
                            let recent = app.session.color.recent_luts.clone();
                            let name = |p: &str| std::path::Path::new(p).file_name().map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned());
                            let names: Vec<String> = recent.iter().map(|p| name(p)).collect();
                            let mut luts: Vec<(String, &str)> = vec![(String::new(), tl!("No LUT"))];
                            luts.extend(recent.iter().cloned().zip(names.iter().map(String::as_str)));
                            let shown = name(&v.lut);
                            if !v.lut.is_empty() && !recent.contains(&v.lut) {
                                luts.push((v.lut.clone(), shown.as_str()));
                            }
                            luts.push((LOAD.to_string(), tl!("Load LUT…")));
                            let mut lut = v.lut.clone();
                            if widgets::dropdown_literal(ui, "viewer-lut", tl!("LUT"), &mut lut, &luts, 140.0) {
                                if lut == LOAD {
                                    #[cfg(not(target_arch = "wasm32"))]
                                    if let Some(path) = rfd::FileDialog::new()
                                        .add_filter("LUT", &["cube", "3dl", "csp", "spi1d", "spi3d", "spimtx", "clf", "ctf", "cc", "ccc", "cdl", "lut", "mga", "m3d", "itx", "look", "vf", "hdl", "cub", "1dl"])
                                        .pick_file()
                                    {
                                        run = Some(("view.ocioViewer", serde_json::json!({"lut": path.to_string_lossy()})));
                                    }
                                } else {
                                    run = Some(("view.ocioViewer", serde_json::json!({"lut": lut})));
                                }
                            }
                            if !v.lut.is_empty() {
                                let mut space = v.lut_space.clone();
                                let spaces: Vec<(String, &str)> = cfg.colorspaces().iter().map(|c| (c.name.clone(), c.name.as_str())).collect();
                                if widgets::dropdown_literal(ui, "viewer-lut-space", tl!("LUT input"), &mut space, &spaces, 170.0) {
                                    run = Some(("view.ocioViewer", serde_json::json!({"lutSpace": space})));
                                }
                            }
                        });
                    }
                    Err(e) => {
                        ui.label(RichText::new(crate::i18n::fmt(tl!("OCIO: {error}"), &[("error", &e)])).color(t.warning).size(11.5));
                    }
                }
                widgets::vline(ui, 16.0);
                let Some(st) = app.session.active() else { return };
                let (doc, float) = (st.doc.id, st.doc.depth == photocraft_color::SampleType::F32);
                let mut channel = app.session.color.viewer_channel(doc);
                let channels: Vec<(photocraft_engine::proof_sim::ViewerChannel, &str)> =
                    photocraft_engine::proof_sim::ViewerChannel::ALL.iter().map(|c| (*c, tl!(c.label()))).collect();
                if widgets::dropdown(ui, "viewer-channel", tl!("Channel"), &mut channel, &channels, 100.0) {
                    run = Some(("view.viewerChannel.rgb", serde_json::json!({"channel": channel.id()})));
                }
                if float {
                    widgets::vline(ui, 16.0);
                    let h = app.session.color.hdr.get(&doc).copied().unwrap_or_default();
                    label(ui, tl!("Exposure"));
                    let mut e = h.exposure;
                    if widgets::value_field(ui, &mut e, -20.0..=20.0, "", 56.0).accessible_name(tl!("Exposure")).changed() {
                        run = Some(("view.thirtyTwoBitPreviewOptions", serde_json::json!({"method": "exposureGamma", "exposure": e})));
                    }
                    label(ui, tl!("Gamma"));
                    let mut g = h.gamma;
                    if widgets::value_field(ui, &mut g, 0.1..=9.99, "", 56.0).accessible_name(tl!("Gamma")).changed() {
                        run = Some(("view.thirtyTwoBitPreviewOptions", serde_json::json!({"method": "exposureGamma", "gamma": g})));
                    }
                    if (h.exposure != 0.0 || h.gamma != 1.0) && widgets::compact_button(ui, tl!("Reset"), control_h).clicked() {
                        run = Some(("view.thirtyTwoBitPreviewOptions", serde_json::json!({"method": "exposureGamma", "exposure": 0.0, "gamma": 1.0})));
                    }
                }
                // The IN: tag opens Image › Input Colour Space… (its dialog, as the menu does).
                let result = match run {
                    Some((crate::input_space_ui::COMMAND, _)) => crate::input_space_ui::open(app).map(|_| serde_json::Value::Null),
                    Some((id, p)) => app.run(id, p),
                    None => Ok(serde_json::Value::Null),
                };
                if let Err(e) = result {
                    app.ui.status = e;
                    app.ui.status_error = true;
                }
            });
        });
}
