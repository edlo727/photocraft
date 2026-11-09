//! View › OCIO Viewer: the canvas shown through an OpenColorIO display / view (and optional
//! look), as Nuke's viewer process shows it. Display only, per document: the pixels never
//! change. The config is Preferences › File Handling › OCIO Config (empty: `$OCIO`, else OCIO's
//! built-in ACES 1.3 studio config), shared with the OCIO Colorspace adjustment.
//!
//! The GPU canvas applies it as a 3D LUT ([`viewer_lut`]) over a log2 shaper of scene-linear
//! light ([`SHAPER_MIN_STOPS`] .. + [`SHAPER_STOPS`]), so values far above 1.0 tone-map through
//! the view instead of clipping.

use std::sync::{Arc, Mutex};

use photocraft_cms::{Builtin, Lut3d, Profile, math};
use photocraft_doc::{DocId, Document};
use photocraft_ocio::Config;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::color_cmds::ColorState;
use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// The shaper's darkest value, in stops from 1.0 (2^-12 ≈ 0.00024); darker is shown as it.
pub const SHAPER_MIN_STOPS: f32 = -12.0;
/// The shaper's range in stops (up to 2^12 = 4096).
pub const SHAPER_STOPS: f32 = 24.0;
/// The viewer LUT's lattice size.
pub const VIEWER_LUT: usize = 49;

/// One document's viewer process.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OcioViewer {
    pub enabled: bool,
    /// Empty: the config's default display.
    pub display: String,
    /// Empty (or not one of the display's): its first view.
    pub view: String,
    /// Empty: none (the view's own looks still apply).
    pub look: String,
    /// A LUT file in the viewer (.cube, .3dl, .csp, .spi1d/.spi3d, .clf, .cdl …); empty: none.
    pub lut: String,
    /// The colour space the LUT expects (a DP's LUT usually ARRI LogC3 or ACEScct). Empty: the
    /// config's `color_timing` / `compositing_log` role, else its scene-linear space.
    pub lut_space: String,
    /// Always [`LutMode::Look`] once resolved: like Nuke's viewer input process, the LUT runs
    /// before the display / view. Kept so older settings and scripts still load.
    pub lut_mode: LutMode,
    /// The colour space the document's pixel values are in, as the colorspace knob of Nuke's
    /// Read: empty is automatic (the document's own profile: sRGB images are decoded to linear,
    /// linear documents taken as they are); [`INPUT_DISPLAY`] takes them as already graded for the
    /// display (the view is inverted, so an sRGB image looks as it does without OCIO); anything
    /// else names a colour space of the config.
    pub input: String,
}

/// [`OcioViewer::input`] for display-referred pixels (an sRGB image, a matte painting graded on a
/// monitor): taken back through the chosen display / view inverted, so the view gives them back
/// as they are (the config's own display colour spaces invert its default view transform, which
/// in the ACES configs is Un-tone-mapped, not the ACES view).
pub const INPUT_DISPLAY: &str = "@display";

/// The view display-referred documents get when none was chosen: the display encoding alone, no
/// tone curve (an ACES view would grade an already graded image a second time and darken it).
fn untonemapped_view(views: &[String]) -> Option<String> {
    let find = |needle: &str| views.iter().find(|v| v.to_lowercase().replace(['-', ' '], "").contains(needle)).cloned();
    find("untonemapped")
}

/// Whether `doc` holds display-referred pixels: anything but a linear-light document.
pub fn display_referred(doc: &Document) -> bool {
    !crate::cmd::image_cmds::is_linear_rgb_profile(&crate::color_cmds::composite_profile(doc))
}

/// How a viewer LUT is used. The viewer always applies it as [`LutMode::Look`], before the
/// display / view, as Nuke's viewer input process does; `Replace` is only read from older
/// settings and is treated as `Look`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LutMode {
    /// Formerly: the LUT replaced the display / view. No longer used (see [`OcioViewer::resolved`]).
    Replace,
    /// The LUT is applied, then the display / view.
    #[default]
    Look,
}

impl LutMode {
    pub fn id(self) -> &'static str {
        match self {
            LutMode::Replace => "replace",
            LutMode::Look => "look",
        }
    }
    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "replace" => Some(LutMode::Replace),
            "look" => Some(LutMode::Look),
            _ => None,
        }
    }
}

/// The LUT input space a viewer LUT gets when none was chosen: the config's colour-timing or
/// compositing log space (ACEScct in ACES configs), else its scene-linear space.
pub fn default_lut_space(cfg: &Config) -> String {
    ["color_timing", "compositing_log", "scene_linear"].iter().find_map(|r| cfg.role(r)).unwrap_or_else(|| scene_linear(cfg).0)
}

impl OcioViewer {
    /// With display and view resolved against `cfg` (its default display; the display's first
    /// view, or for a `display_referred` document an un-tone-mapped view when the config has one).
    pub fn resolved(&self, cfg: &Config, display_referred: bool) -> OcioViewer {
        let display = if cfg.displays().iter().any(|d| d.name == self.display) { self.display.clone() } else { cfg.default_display().unwrap_or_default().to_string() };
        let views = cfg.views(&display);
        let view = if views.contains(&self.view) {
            self.view.clone()
        } else {
            display_referred.then(|| untonemapped_view(views)).flatten().or_else(|| views.first().cloned()).unwrap_or_default()
        };
        let input = if self.input == INPUT_DISPLAY || cfg.has(&self.input) { self.input.clone() } else { String::new() };
        let look = if cfg.looks().contains(&self.look) { self.look.clone() } else { String::new() };
        let lut_space = if self.lut.is_empty() {
            String::new()
        } else if cfg.has(&self.lut_space) {
            self.lut_space.clone()
        } else {
            default_lut_space(cfg)
        };
        // The LUT always runs before the display / view, as in Nuke's viewer.
        OcioViewer { enabled: self.enabled, display, view, look, lut: self.lut.clone(), lut_space, lut_mode: LutMode::Look, input }
    }
}

/// The config Preferences name (`path`; empty: `$OCIO`, else the built-in default), loaded when
/// it changes and made OCIO's process-wide config. A config that fails to load reports why each
/// time it is asked for, until the preference changes.
pub fn sync_config(path: &str) -> std::result::Result<Arc<Config>, String> {
    type Loaded = Option<(String, std::result::Result<Arc<Config>, String>)>;
    static LOADED: Mutex<Loaded> = Mutex::new(None);
    let key = path.trim().to_string();
    let mut slot = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, r)) = slot.as_ref()
        && *k == key
    {
        return r.clone();
    }
    let r = if !key.is_empty() {
        Config::load(&key)
    } else if std::env::var_os("OCIO").is_some_and(|v| !v.is_empty()) {
        Config::from_env()
    } else {
        Config::load(photocraft_ocio::DEFAULT_CONFIG)
    };
    if let Ok(c) = &r {
        photocraft_ocio::set_current(c.clone());
    }
    *slot = Some((key, r.clone()));
    r
}

impl ColorState {
    /// `doc`'s viewer process as set (unresolved; off by default).
    pub fn ocio_viewer(&self, doc: DocId) -> OcioViewer {
        self.ocio_viewer.get(&doc).cloned().unwrap_or_default()
    }
}

/// The scene-linear colour space the viewer feeds the display / view, and whether its primaries
/// are ACES AP1 (ACEScg) rather than Rec.709: the config's `scene_linear` role (ACEScg in ACES
/// configs, linear Rec.709 in most others).
fn scene_linear(cfg: &Config) -> (String, bool) {
    let name = cfg.role("scene_linear").unwrap_or_else(|| "ACEScg".to_string());
    let ap1 = name.contains("ACEScg") || name.contains("AP1");
    (name, ap1)
}

/// Linear RGB in `doc`'s primaries → linear RGB in `target`'s (both D50-adapted matrix profiles);
/// identity when either has no matrix (the document is then taken as already in the target).
fn primaries_matrix(doc: &Profile, target: &Profile) -> math::Mat3 {
    match (doc.matrix, target.matrix.and_then(|m| math::invert(&m))) {
        (Some(src), Some(inv)) => math::mul(&inv, &src),
        _ => math::IDENTITY,
    }
}

/// The viewer as a `size`³ display LUT over the log2 shaper: lattice point `u` (per channel) is
/// linear light `2^(SHAPER_MIN_STOPS + u · SHAPER_STOPS)` as the canvas decodes the document
/// (`doc_profile`'s primaries). With the automatic input it is converted to the config's
/// scene-linear space; with a chosen input colour space it is first re-encoded to the document's
/// pixel values (sRGB-encoded unless the document is linear) and converted from that space. Then
/// the display / view (and look). Output is display code values, 0..1.
pub fn viewer_lut(cfg: &Config, doc_profile: &Profile, v: &OcioViewer, size: usize) -> std::result::Result<Lut3d, String> {
    let doc_linear = crate::cmd::image_cmds::is_linear_rgb_profile(doc_profile);
    let v = v.resolved(cfg, !doc_linear);
    let replace = !v.lut.is_empty() && v.lut_mode == LutMode::Replace;
    if !replace && (v.display.is_empty() || v.view.is_empty()) {
        return Err(format!("the OCIO config `{}` has no displays", cfg.name()));
    }
    let (scene, ap1) = scene_linear(cfg);
    let target = if ap1 { Builtin::AcesCg.profile() } else { Builtin::LinearSrgb.profile() };
    let input = (!v.input.is_empty()).then(|| v.input.clone());
    let input_proc = match input.as_deref() {
        // Out of the view the LUT ends in (a Replace LUT has none: the display's own colour space).
        Some(INPUT_DISPLAY) if !replace => Some(cfg.inverse_view_processor(&scene, &v.display, &v.view)?),
        Some(INPUT_DISPLAY) => Some(cfg.processor(&v.display, &scene)?),
        Some(src) if src != scene => Some(cfg.processor(src, &scene)?),
        _ => None,
    };
    let m = if input.is_some() { math::IDENTITY } else { primaries_matrix(doc_profile, target) };
    let encode = |x: f64| if doc_linear || input.is_none() { x } else if x <= 0.003_130_8 { x * 12.92 } else { (1.055 * x.powf(1.0 / 2.4) - 0.055).min(1.0) };
    let look = (!v.look.is_empty()).then_some(v.look.as_str());
    let proc = if v.lut.is_empty() {
        cfg.display_view_processor(&scene, &v.display, &v.view, look)?
    } else {
        let then_view = (!replace).then_some((v.display.as_str(), v.view.as_str()));
        cfg.lut_processor(&scene, &v.lut, &v.lut_space, then_view)?
    };
    let n = size.max(2);
    let step = 1.0 / (n - 1) as f32;
    let lin = |i: usize| f64::from(2f32.powf(SHAPER_MIN_STOPS + i as f32 * step * SHAPER_STOPS));
    let mut px = Vec::with_capacity(n * n * n);
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let c = math::apply(&m, [encode(lin(r)), encode(lin(g)), encode(lin(b))]);
                px.push([c[0] as f32, c[1] as f32, c[2] as f32, 1.0]);
            }
        }
    }
    if let Some(p) = &input_proc {
        p.apply_rgba(&mut px);
    }
    proc.apply_rgba(&mut px);
    let data = px.into_iter().map(|p| [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0), p[2].clamp(0.0, 1.0), 1.0]).collect();
    Ok(Lut3d { size: n, data })
}

/// The GPU canvas's viewer LUT for `doc` (RGBA8, [`VIEWER_LUT`]³) and a signature of what made
/// it; `Ok(None)` when the viewer is off.
pub fn gpu_viewer_lut(c: &ColorState, doc: &Document, config_path: &str) -> Result<Option<(Vec<u8>, u64)>> {
    let v = c.ocio_viewer(doc.id);
    if !v.enabled {
        return Ok(None);
    }
    let cfg = sync_config(config_path).map_err(|e| EngineError::Other(format!("OCIO config: {e}")))?;
    let profile = crate::color_cmds::composite_profile(doc);
    let lut = viewer_lut(&cfg, &profile, &v, VIEWER_LUT).map_err(|e| EngineError::Other(format!("OCIO viewer: {e}")))?;
    Ok(Some((lut.to_rgba8(), signature(c, doc, config_path))))
}

/// Changes whenever `doc`'s viewer LUT would (its settings, the config, the document profile).
pub fn signature(c: &ColorState, doc: &Document, config_path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    c.ocio_viewer(doc.id).hash(&mut h);
    config_path.trim().hash(&mut h);
    crate::color_cmds::composite_profile(doc).content_hash().hash(&mut h);
    h.finish()
}

/// The active document's viewer (resolved) and the loaded config, for the viewer bar's menus.
pub fn menus(s: &Session) -> std::result::Result<(OcioViewer, Arc<Config>), String> {
    let st = s.active().ok_or_else(|| "no document is open".to_string())?;
    let cfg = sync_config(&s.prefs().file_handling.ocio_config)?;
    Ok((s.color.ocio_viewer(st.doc.id).resolved(&cfg, display_referred(&st.doc)), cfg))
}

// ------------------------------------------------------------------ commands

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document is open".to_string())
}

/// The viewer of the active document with the config's choices, as the viewer bar lists them.
fn describe(s: &Session) -> Result<Value> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let v = s.color.ocio_viewer(st.doc.id);
    let cfg = sync_config(&s.prefs().file_handling.ocio_config).map_err(|e| EngineError::Other(format!("OCIO config: {e}")))?;
    let r = v.resolved(&cfg, display_referred(&st.doc));
    let displays: Vec<Value> = cfg.displays().iter().map(|d| json!({"name": d.name, "views": d.views})).collect();
    Ok(json!({
        "enabled": r.enabled,
        "display": r.display,
        "view": r.view,
        "look": r.look,
        "lut": r.lut,
        "lutSpace": r.lut_space,
        "lutMode": r.lut_mode.id(),
        "input": r.input,
        "colorspaces": cfg.colorspaces().iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        "displays": displays,
        "looks": cfg.looks(),
        "recentLuts": s.color.recent_luts,
        "config": {"name": cfg.name(), "uri": cfg.uri()},
    }))
}

/// `view.ocioViewer`: change any of `enabled`, `display`, `view`, `look` for the active document.
fn ocio_viewer_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "view.ocioViewer";
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (id, dr) = (st.doc.id, display_referred(&st.doc));
    let cfg = sync_config(&s.prefs().file_handling.ocio_config).map_err(|e| EngineError::Other(format!("OCIO config: {e}")))?;
    let mut v = s.color.ocio_viewer(id).resolved(&cfg, dr);
    let bad = |msg: String| EngineError::BadParams { cmd: CMD.into(), msg };
    if let Some(e) = p.get("enabled").and_then(Value::as_bool) {
        v.enabled = e;
    }
    if let Some(d) = p.get("display").and_then(Value::as_str) {
        if !cfg.displays().iter().any(|x| x.name == d) {
            return Err(bad(format!("no display `{d}` in the OCIO config")));
        }
        if d != v.display {
            v.display = d.to_string();
            v.view.clear();
        }
    }
    if let Some(view) = p.get("view").and_then(Value::as_str) {
        let display = v.resolved(&cfg, dr).display;
        if !cfg.views(&display).iter().any(|x| x == view) {
            return Err(bad(format!("no view `{view}` for display `{display}`")));
        }
        v.view = view.to_string();
    }
    if let Some(look) = p.get("look").and_then(Value::as_str) {
        if !look.is_empty() && !cfg.looks().iter().any(|x| x == look) {
            return Err(bad(format!("no look `{look}` in the OCIO config")));
        }
        v.look = look.to_string();
    }
    if let Some(input) = p.get("input").and_then(Value::as_str) {
        if !input.is_empty() && input != INPUT_DISPLAY && !cfg.has(input) {
            return Err(bad(format!("no colour space `{input}` in the OCIO config")));
        }
        v.input = input.to_string();
    }
    if let Some(m) = p.get("lutMode").and_then(Value::as_str) {
        v.lut_mode = LutMode::from_id(m).ok_or_else(|| bad(format!("lutMode `{m}` (replace|look)")))?;
    }
    if let Some(space) = p.get("lutSpace").and_then(Value::as_str) {
        if !cfg.has(space) {
            return Err(bad(format!("no colour space `{space}` in the OCIO config")));
        }
        v.lut_space = space.to_string();
    }
    if let Some(lut) = p.get("lut").and_then(Value::as_str) {
        v.lut = lut.trim().to_string();
        if !v.lut.is_empty() {
            v.enabled = true;
        }
    }
    let v = v.resolved(&cfg, dr);
    if !v.lut.is_empty() {
        // Load the LUT now, so a bad file is reported here rather than as a blank viewer.
        let (scene, _) = scene_linear(&cfg);
        let then_view = (v.lut_mode == LutMode::Look).then_some((v.display.as_str(), v.view.as_str()));
        cfg.lut_processor(&scene, &v.lut, &v.lut_space, then_view).map_err(|e| bad(format!("LUT `{}`: {e}", v.lut)))?;
        let recent = &mut s.color.recent_luts;
        recent.retain(|r| *r != v.lut);
        recent.insert(0, v.lut.clone());
        recent.truncate(8);
    }
    if p.as_object().is_none_or(|o| o.is_empty()) {
        let mut v = v;
        v.enabled = true;
        s.color.ocio_viewer.insert(id, v);
    } else {
        s.color.ocio_viewer.insert(id, v);
    }
    describe(s)
}

/// Image › Input Colour Space…: the colour space of the active document's pixels for the OCIO
/// viewer (`input`: empty automatic, [`INPUT_DISPLAY`], or a config colour space), as the
/// colorspace knob of Nuke's Read. Without params it reports the viewer.
fn input_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    match p.get("input").and_then(Value::as_str) {
        Some(input) => ocio_viewer_cmd(s, &json!({"input": input})),
        None => describe(s),
    }
}

/// View › OCIO Viewer (on/off).
fn toggle(s: &mut Session, _p: &Value) -> Result<Value> {
    let id = s.active().ok_or(EngineError::NoDocument)?.doc.id;
    let on = !s.color.ocio_viewer(id).enabled;
    ocio_viewer_cmd(s, &json!({"enabled": on}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "view.ocioViewer",
            label: "OCIO Viewer Settings",
            menu: &[],
            shortcut: None,
            params: r##"{"enabled":bool,"display":str,"view":str,"look":str (empty: none),"lut":"path to .cube/.3dl/.csp/.spi1d/.spi3d/.clf/.cdl… (empty: none)","lutSpace":"the colour space the LUT expects","lutMode":"look (the LUT runs before the display / view; replace is accepted and treated as look)","input":"the colour space of the document's pixels (empty: automatic, from its profile; @display: display-referred, the view inverted)"} → {enabled,display,view,look,lut,lutSpace,lutMode,input,colorspaces,displays:[{name,views}],looks,recentLuts,config} (the active document's viewer process through the OCIO config: display only; no params turns it on)"##,
            enabled: has_doc,
            run: ocio_viewer_cmd,
            journal: false,
        },
        CommandSpec {
            id: "image.inputColorSpace",
            label: "Input Colour Space…",
            menu: &["Image"],
            shortcut: None,
            params: r##"{"input":"the colour space of the document's pixels for the OCIO viewer (empty: automatic, from its profile; @display: display-referred, the view inverted; else a config colour space)"} → as view.ocioViewer"##,
            enabled: has_doc,
            run: input_cmd,
            journal: false,
        },
        CommandSpec {
            id: "view.ocioViewer.toggle",
            label: "OCIO Viewer",
            menu: &["View"],
            shortcut: None,
            params: "{} (turns the active document's OCIO viewer on or off)",
            enabled: has_doc,
            run: toggle,
            journal: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primaries_matrix_is_identity_within_a_space_and_maps_white_to_white() {
        let lin = Builtin::LinearSrgb.profile();
        let aces = Builtin::AcesCg.profile();
        let id = primaries_matrix(lin, lin);
        for (i, row) in id.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                assert!((v - if i == j { 1.0 } else { 0.0 }).abs() < 1e-9, "{id:?}");
            }
        }
        // D50-adapted on both sides: Rec.709 white is AP1 white (1, 1, 1).
        let w = math::apply(&primaries_matrix(lin, aces), [1.0, 1.0, 1.0]);
        assert!(w.iter().all(|c| (c - 1.0).abs() < 1e-4), "{w:?}");
        // A pure Rec.709 red is inside AP1: all channels non-negative, red dominant.
        let r = math::apply(&primaries_matrix(lin, aces), [1.0, 0.0, 0.0]);
        assert!(r[0] > 0.5 && r[1] >= -1e-6 && r[2] >= -1e-6, "{r:?}");
    }

    /// An sRGB image (display-referred) under an ACES view: the default view is un-tone-mapped,
    /// and with the input set to display-referred the ACES view gives the image back as it is
    /// (the view inverted, then applied) instead of grading it a second time.
    #[test]
    fn display_referred_images_round_trip_through_the_view() {
        // A build without OpenColorIO (the `ocio` crate's stub) has no config to test.
        if !photocraft_ocio::available() {
            return;
        }
        let cfg = Config::load(photocraft_ocio::DEFAULT_CONFIG).unwrap();
        let auto = OcioViewer { enabled: true, ..Default::default() };
        let views = cfg.views(&auto.resolved(&cfg, true).display).to_vec();
        if let Some(u) = untonemapped_view(&views) {
            assert_eq!(auto.resolved(&cfg, true).view, u, "{views:?}");
        }
        let sdr = views.iter().find(|v| v.starts_with("ACES 1.0 - SDR")).cloned().expect("an ACES SDR view");
        let srgb = Builtin::Srgb.profile();
        let shown = |input: &str| {
            let v = OcioViewer { enabled: true, display: "sRGB - Display".into(), view: sdr.clone(), input: input.into(), ..Default::default() };
            viewer_lut(&cfg, srgb, &v, VIEWER_LUT).unwrap()
        };
        let (graded, kept) = (shown(""), shown(INPUT_DISPLAY));
        let n = VIEWER_LUT;
        let mut darker = false;
        for i in 0..n {
            let lin = 2f64.powf(f64::from(SHAPER_MIN_STOPS) + i as f64 / (n - 1) as f64 * f64::from(SHAPER_STOPS));
            if !(0.02..=0.8).contains(&lin) {
                continue;
            }
            let want = 1.055 * lin.powf(1.0 / 2.4) - 0.055;
            let at = i + i * n + i * n * n;
            let got = f64::from(kept.data[at][0]);
            assert!((got - want).abs() < 0.03, "lin {lin}: shown {got}, image {want}");
            darker |= f64::from(graded.data[at][0]) < want - 0.03;
        }
        assert!(darker, "the automatic input grades the image through the ACES tone curve");
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let r = s.execute("image.inputColorSpace", json!({"input": INPUT_DISPLAY})).unwrap();
        assert_eq!(r["input"], INPUT_DISPLAY);
        assert_eq!(s.execute("image.inputColorSpace", json!({})).unwrap()["input"], INPUT_DISPLAY, "no params reports it");
        assert!(s.execute("image.inputColorSpace", json!({"input": "No Such Space"})).is_err());
    }

    #[test]
    fn the_viewer_lut_tone_maps_hdr_and_is_monotonic() {
        // A build without OpenColorIO (the `ocio` crate's stub) has no config to test.
        if !photocraft_ocio::available() {
            return;
        }
        let cfg = Config::load(photocraft_ocio::DEFAULT_CONFIG).unwrap();
        let v = OcioViewer { enabled: true, ..Default::default() }.resolved(&cfg, false);
        assert!(!v.display.is_empty() && !v.view.is_empty(), "{v:?}");
        let lut = viewer_lut(&cfg, Builtin::AcesCg.profile(), &v, 17).unwrap();
        // Along the neutral axis: never decreasing, black near 0, the top within display range.
        let n = lut.size;
        let grey: Vec<f32> = (0..n).map(|i| lut.data[i + i * n + i * n * n][0]).collect();
        assert!(grey.windows(2).all(|w| w[1] >= w[0] - 1e-4), "{grey:?}");
        assert!(grey[0] < 0.02 && grey[n - 1] <= 1.0, "{grey:?}");
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8, "depth": 32})).unwrap();
        let r = s.execute("view.ocioViewer", json!({})).unwrap();
        assert_eq!(r["enabled"], true);
        assert!(r["displays"].as_array().is_some_and(|d| !d.is_empty()));
        assert!(s.execute("view.ocioViewer", json!({"display": "No Such Display"})).is_err());
        let d = s.active().unwrap().doc.clone();
        assert!(gpu_viewer_lut(&s.color, &d, "").unwrap().is_some());
        s.execute("view.ocioViewer.toggle", json!({})).unwrap();
        assert!(gpu_viewer_lut(&s.color, &d, "").unwrap().is_none(), "toggled off");
        // A LUT file: loaded and checked when chosen, remembered, its space defaulting to ACEScct.
        let dir = std::env::temp_dir().join(format!("pc-viewer-lut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("grade.cube");
        let mut cube = String::from("LUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    cube.push_str(&format!("{r} {g} {b}\n"));
                }
            }
        }
        std::fs::write(&path, cube).unwrap();
        let r = s.execute("view.ocioViewer", json!({"lut": path.to_string_lossy()})).unwrap();
        assert_eq!(r["enabled"], true, "choosing a LUT turns the viewer on");
        assert_eq!(r["lutSpace"], "ACEScct");
        assert_eq!(r["lutMode"], "look", "the LUT runs before the view, as in Nuke");
        // An old "replace" setting is still accepted, and still runs before the view.
        let r = s.execute("view.ocioViewer", json!({"lutMode": "replace"})).unwrap();
        assert_eq!(r["lutMode"], "look");
        assert_eq!(r["recentLuts"][0], path.to_string_lossy().as_ref());
        assert!(gpu_viewer_lut(&s.color, &d, "").unwrap().is_some());
        assert!(s.execute("view.ocioViewer", json!({"lut": dir.join("missing.cube").to_string_lossy()})).is_err(), "a bad file is reported");
        let r = s.execute("view.ocioViewer", json!({"lut": ""})).unwrap();
        assert_eq!(r["lut"], "");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
