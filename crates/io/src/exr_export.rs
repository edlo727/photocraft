use exr::prelude::*;
use photocraft_color::blend::BlendMode;
use photocraft_doc::{Document, Layer as DocLayer, LayerContent, MAX_GROUP_DEPTH};
use photocraft_geom::Rect;
use std::collections::HashSet;
use std::path::Path;

const BEAUTY: &str = "rgba";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NukeCompression {
    None,
    Rle,
    ZipScanline,
    ZipBlock,
    Piz,
}

impl NukeCompression {
    fn to_exr_compression(self) -> Compression {
        match self {
            NukeCompression::None => Compression::Uncompressed,
            NukeCompression::Rle => Compression::RLE,
            NukeCompression::ZipScanline => Compression::ZIP1,
            NukeCompression::ZipBlock => Compression::ZIP16,
            NukeCompression::Piz => Compression::PIZ,
        }
    }
}

/// How groups go into the EXR and the `.nk` script.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupExport {
    /// Every layer keeps its own EXR layer, and each group is rebuilt in the script: a
    /// pass-through group continues the comp it sits in, any other group composites its
    /// children on their own and merges the result with the group's blend mode, opacity and mask.
    #[default]
    Branches,
    /// Each outermost group is merged into one EXR layer (its contents as they show, as
    /// Layer › Merge Group renders them) that keeps the group's blend mode, opacity and mask.
    /// Pass Through becomes over, as Merge Group makes it Normal.
    Merged,
}

/// One entry of the exported layer tree.
///
/// `name` must already be sanitized (see `sanitize_layer_name`); it is used verbatim as the EXR
/// layer prefix, the Shuffle `in`, and in node names.
#[derive(Clone, Debug)]
pub struct ExportLayer {
    pub name: String,
    pub blend_mode: BlendMode,
    /// Layer opacity × fill opacity: the Merge `mix`.
    pub opacity: f32,
    /// Hidden layers are still written to the EXR, but their nodes are disabled, as the Layers
    /// panel hides them.
    pub visible: bool,
    /// A Grade, RGB Curves or OCIO Colorspace adjustment layer: no pixels in the EXR, a Nuke
    /// `Grade`, `ColorLookup` or `OCIOColorSpace` node instead of a Shuffle → Premult → Merge
    /// column (`opacity` is its mix). On the comp it sits in, or on its base's branch when clipped.
    pub adjustment: Option<photocraft_doc::Adjustment>,
    /// The EXR layer whose `.alpha` holds this layer's mask (pixel mask × vector mask, with
    /// density and feather, as the canvas applies them). It drives the node's mask input.
    pub mask: Option<String>,
    /// A group exported as a branch: its children, bottom first.
    pub children: Option<Vec<ExportLayer>>,
    /// The layers clipped to this one, bottom first: composited onto it in its own branch, so
    /// a clipped Grade or Curves only changes this layer.
    pub clipped: Vec<ExportLayer>,
    /// Known not to match the canvas exactly (flagged with `~` in the breakdown).
    pub approx: bool,
}

impl ExportLayer {
    /// A pixel layer with no mask, children or clipped layers.
    pub fn pixels(name: impl Into<String>, blend_mode: BlendMode, opacity: f32, visible: bool) -> Self {
        Self { name: name.into(), blend_mode, opacity, visible, adjustment: None, mask: None, children: None, clipped: Vec::new(), approx: false }
    }

    fn is_pixels(&self) -> bool {
        self.adjustment.is_none() && self.children.is_none()
    }
}

/// A Grade knob as `.nk` text: one value when the channels agree, else `{r g b a}` with
/// alpha at the knob's neutral value (the node grades rgb only).
fn nk_knob(v: &[f32; 3], neutral: f32) -> String {
    if v[0] == v[1] && v[1] == v[2] { format!("{}", v[0]) } else { format!("{{{} {} {} {neutral}}}", v[0], v[1], v[2]) }
}

/// The body of a Nuke `Grade` node for a Grade adjustment (knobs at their defaults omitted).
fn nk_grade_knobs(adj: &photocraft_doc::Adjustment) -> String {
    let photocraft_doc::Adjustment::Grade { blackpoint, whitepoint, lift, gain, multiply, offset, gamma, black_clamp, white_clamp } = adj else {
        return String::new();
    };
    let mut s = String::from(" channels rgb\n");
    for (key, v, neutral) in [
        ("blackpoint", blackpoint, 0.0),
        ("whitepoint", whitepoint, 1.0),
        ("black", lift, 0.0),
        ("white", gain, 1.0),
        ("multiply", multiply, 1.0),
        ("add", offset, 0.0),
        ("gamma", gamma, 1.0),
    ] {
        if v.iter().any(|c| *c != neutral) {
            s.push_str(&format!(" {key} {}\n", nk_knob(v, neutral)));
        }
    }
    if !black_clamp {
        s.push_str(" black_clamp false\n");
    }
    if *white_clamp {
        s.push_str(" white_clamp true\n");
    }
    // PhotoCraft grades the composite's straight colour; the comp pipe is premultiplied.
    s.push_str(" unpremult rgba.alpha\n");
    s
}

/// Only ASCII alphanumerics and '_' survive. No dots (they create sub-layers),
/// no spaces, no leading digit, never the reserved beauty name.
fn sanitize_layer_name(raw: &str) -> String {
    let mut s: String = raw
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if s.is_empty() {
        s = "Layer".into();
    }
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s = format!("L_{}", s);
    }
    if s.eq_ignore_ascii_case(BEAUTY) {
        s = format!("{}_layer", s);
    }
    s
}

/// Returns (Nuke Merge2 operation, is_exact_match).
///
/// Photocraft's layer modes are Nuke Merge operations, and each mode's label is the Merge2
/// `operation` name, so the script uses the same maths the canvas renders. Only pass-through
/// (a pass-through group's children continue the comp, so the group itself is plain over) and
/// the Photoshop-only modes ([`BlendMode::PS_MODES`]: no Merge equivalent) fall back to over,
/// flagged with `~` in the breakdown. The EXR's beauty pass holds the true composite.
pub fn nuke_merge_operation(mode: BlendMode) -> (&'static str, bool) {
    match mode {
        BlendMode::PassThrough => ("over", true),
        m if m.is_photoshop_only() => ("over", false),
        m => (m.label(), true),
    }
}

/// The mode whose maths the script's Merge for `mode` runs.
fn merged_as(mode: BlendMode) -> BlendMode {
    if mode == BlendMode::PassThrough || mode.is_photoshop_only() { BlendMode::Normal } else { mode }
}

/// Whether a Merge's mask input reproduces the canvas for `mode`. The canvas multiplies a
/// layer's alpha by its mask before blending; a Merge2 mask input mixes the merged result back
/// towards B by the mask. They agree when the mode's maths is linear in the layer's coverage
/// (over, plus, screen, the Photoshop-style separable forms…). Checked on the compositor's own
/// maths, so a mode whose maths changes is re-judged; otherwise the mask goes into the layer's
/// alpha in the script, which is exact for every mode.
fn mask_input_exact(mode: BlendMode) -> bool {
    use photocraft_color::blend::composite;
    const SAMPLES: [[f32; 4]; 4] = [[0.2, 0.5, 0.9, 1.0], [0.8, 0.3, 0.1, 0.6], [0.5, 0.5, 0.5, 0.25], [1.5, 0.7, 0.05, 0.9]];
    let pm = |p: [f32; 4]| [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]];
    let mode = merged_as(mode);
    SAMPLES.iter().all(|&b| {
        SAMPLES.iter().all(|&a| {
            [0.3_f32, 0.7].iter().all(|&k| {
                let masked = pm(composite(mode, b, [a[0], a[1], a[2], a[3] * k], 1.0));
                let full = pm(composite(mode, b, a, 1.0));
                let bp = pm(b);
                (0..4).all(|c| (masked[c] - (bp[c] + (full[c] - bp[c]) * k)).abs() < 1e-4)
            })
        })
    })
}

fn tcl_escape(s: &str) -> String {
    s.replace('\\', "/")
        .replace('"', "\\\"")
        .replace('[', "\\[")
        .replace('$', "\\$")
}

/// Backdrop colour by blend family, so the breakdown reads at a glance.
fn backdrop_color(op: &str, is_base: bool) -> &'static str {
    if is_base {
        return "0x2e2e2eff";
    }
    match op {
        "multiply" | "min" | "color-burn" => "0x34465aff",            // darken: blue
        "screen" | "plus" | "max" | "color-dodge" => "0x5a4a32ff",    // lighten: amber
        "overlay" | "soft-light" | "hard-light" => "0x4c3a5aff",      // contrast: purple
        "difference" | "exclusion" | "from" | "minus" | "divide" | "average" | "geometric" | "hypot" => {
            "0x335a4aff" // math: green
        }
        "grade" | "colorlookup" => "0x5a3a3aff",                       // colour correction: red
        "ocio" => "0x3a4a5aff",                                        // colour space: slate
        _ => "0x404040ff",                                            // over: grey
    }
}

/// Layout grid (Nuke nodes are 80x18, Dots 12x12).
const COL_W: i32 = 200; // column pitch: room for long node names + Merge autolabel
const DOT_DX: i32 = 34; // centres a Dot over an 80-wide node
const READ_Y: i32 = -230;
const BUS_Y: i32 = -100;
const BD_Y: i32 = -50; // layer backdrop top
const BD_H: i32 = 190;
const SHUFFLE_Y: i32 = 40;
const PREMULT_Y: i32 = 100;
/// The deepest branch row; shallower rows sit `ROW_DY` lower each, the main comp lowest.
const MERGE_Y: i32 = 220;
const ROW_DY: i32 = 90;

fn walk<'a>(layers: &'a [ExportLayer], f: &mut dyn FnMut(&'a ExportLayer)) {
    for l in layers {
        f(l);
        if let Some(children) = &l.children {
            walk(children, f);
        }
        walk(&l.clipped, f);
    }
}

/// How many branch rows sit above the main comp row.
fn branch_depth(layers: &[ExportLayer]) -> i32 {
    layers
        .iter()
        .map(|l| {
            let clip = if l.clipped.is_empty() { 0 } else { 1 + branch_depth(&l.clipped) };
            let group = l.children.as_deref().map_or(0, |c| 1 + branch_depth(c));
            clip.max(group)
        })
        .max()
        .unwrap_or(0)
}

/// The `.nk` text as it is built: nodes in `nk`, backdrops collected apart (written last, behind).
struct Script {
    nk: String,
    backdrops: String,
    col: i32,
    prev_bus: String,
    max_depth: i32,
}

impl Script {
    fn row_y(&self, depth: i32) -> i32 {
        MERGE_Y + (self.max_depth - depth).max(0) * ROW_DY
    }

    fn next_x(&mut self) -> i32 {
        let x = self.col * COL_W;
        self.col += 1;
        x
    }

    fn last_x(&self) -> i32 {
        (self.col - 1).max(0) * COL_W
    }

    /// A Dot on the Read bus at `x`, chained off the previous one (the first off the Read).
    fn bus(&mut self, id: &str, x: i32) -> String {
        self.nk.push_str(&format!(
            "push ${}\nDot {{\n name Dot_{id}\n xpos {}\n ypos {BUS_Y}\n}}\nset NDot_{id} [stack 0]\n",
            self.prev_bus,
            x + DOT_DX
        ));
        self.prev_bus = format!("NDot_{id}");
        self.prev_bus.clone()
    }

    fn layer_backdrop(&mut self, name: &str, x: i32, color: &str, sub: &str) {
        self.backdrops.push_str(&format!(
            "BackdropNode {{\n inputs 0\n name BD_{name}\n tile_color {color}\n label \"<center><b>{name}</b>\\n<font size=2>{sub}</font></center>\"\n note_font_size 18\n xpos {}\n ypos {BD_Y}\n bdwidth {}\n bdheight {BD_H}\n z_order 2\n}}\n",
            x - 50,
            COL_W - 20
        ));
    }

    /// A pixel layer's column: bus Dot → Shuffle → Premult. Returns the Premult and the bus Dot.
    fn column(&mut self, name: &str, x: i32) -> (String, String) {
        let bus = self.bus(name, x);
        // Shuffle -> Premult (EXR holds straight alpha; Merge wants premultiplied).
        self.nk.push_str(&format!("Shuffle {{\n in {name}\n out rgba\n name Shuffle_{name}\n xpos {x}\n ypos {SHUFFLE_Y}\n}}\n"));
        self.nk.push_str(&format!("Premult {{\n name Premult_{name}\n xpos {x}\n ypos {PREMULT_Y}\n}}\nset NPremult_{name} [stack 0]\n"));
        (format!("NPremult_{name}"), bus)
    }

    /// `stream` (premultiplied) times `mask`: the mask goes into the layer's coverage, as the
    /// canvas applies it. Every stream carries the Read's channels, so the mask is at hand.
    fn mask_multiply(&mut self, stream: &str, mask: &str, name: &str, x: i32, y: i32) -> String {
        let m = format!("{mask}.alpha");
        self.nk.push_str(&format!(
            "push ${stream}\nExpression {{\n expr0 \"r*{m}\"\n expr1 \"g*{m}\"\n expr2 \"b*{m}\"\n expr3 \"a*{m}\"\n name Mask_{name}\n xpos {x}\n ypos {y}\n}}\nset NMask_{name} [stack 0]\n"
        ));
        format!("NMask_{name}")
    }

    /// An adjustment node on `comp`, its mask (if any) on the node's mask input.
    fn adjustment(&mut self, l: &ExportLayer, adj: &photocraft_doc::Adjustment, comp: &str, depth: i32) -> String {
        let name = &l.name;
        let x = self.next_x();
        let y = self.row_y(depth);
        let (class, knobs, op) = match adj {
            photocraft_doc::Adjustment::Grade { .. } => ("Grade", nk_grade_knobs(adj), "grade"),
            photocraft_doc::Adjustment::OcioColorSpace { input, output } => (
                "OCIOColorSpace",
                format!(" in_colorspace \"{}\"\n out_colorspace \"{}\"\n unpremult rgba.alpha\n", tcl_escape(input), tcl_escape(output)),
                "ocio",
            ),
            _ => ("ColorLookup", nk_lookup_knobs(adj), "colorlookup"),
        };
        let mask = match &l.mask {
            Some(m) => {
                let bus = self.bus(&format!("{name}_mask"), x);
                self.nk.push_str(&format!("push ${bus}\n"));
                format!(" inputs 1+1\n maskChannelInput {m}.alpha\n")
            }
            None => String::new(),
        };
        self.nk.push_str(&format!(
            "push ${comp}\n{class} {{\n{mask}{knobs}{} mix {}\n name {class}_{name}\n xpos {x}\n ypos {y}\n}}\nset N{class}_{name} [stack 0]\n",
            if l.visible { "" } else { " disable true\n" },
            l.opacity.clamp(0.0, 1.0)
        ));
        let pct = (l.opacity.clamp(0.0, 1.0) * 100.0).round() as i32;
        let sub = if !l.visible {
            "hidden".to_string()
        } else {
            let mut s = op.to_string();
            if pct < 100 {
                s.push_str(&format!("  {pct}%"));
            }
            if l.mask.is_some() {
                s.push_str("  masked");
            }
            s
        };
        self.layer_backdrop(name, x, backdrop_color(op, false), &sub);
        format!("N{class}_{name}")
    }

    /// `a` (this layer's premultiplied stream, at column `x`) merged onto `comp` with its blend
    /// mode, opacity and mask, or starting the comp when there is none yet. `bus` is a Dot on the
    /// Read bus at `x`, for the mask input (one is made when `None`).
    fn place(&mut self, l: &ExportLayer, a: String, comp: Option<String>, depth: i32, x: i32, bus: Option<String>) -> String {
        let name = &l.name;
        let y = self.row_y(depth);
        let mix = l.opacity.clamp(0.0, 1.0);
        let Some(comp) = comp else {
            // The base starts the comp. Over an empty backdrop every mode is plain over, so only
            // its mask and opacity (0 when hidden) apply.
            let mut a = a;
            if let Some(m) = &l.mask {
                a = self.mask_multiply(&a, m, name, x, y - 60);
            }
            let base_mix = if l.visible { mix } else { 0.0 };
            if base_mix < 1.0 {
                self.nk.push_str(&format!(
                    "push ${a}\nMultiply {{\n channels rgba\n value {base_mix}\n name Opacity_{name}\n xpos {x}\n ypos {}\n}}\nset NOpacity_{name} [stack 0]\n",
                    y - 30
                ));
                a = format!("NOpacity_{name}");
            }
            self.nk.push_str(&format!(
                "push ${a}\nDot {{\n name Dot_Comp_{name}\n xpos {}\n ypos {}\n}}\nset NDot_Comp_{name} [stack 0]\n",
                x + DOT_DX,
                y + 3
            ));
            return format!("NDot_Comp_{name}");
        };
        let (op, _) = nuke_merge_operation(l.blend_mode);
        let mut a = a;
        let mut mask_knob = String::new();
        let mut inputs = "2";
        if let Some(m) = &l.mask {
            if mask_input_exact(l.blend_mode) {
                let bus = match bus {
                    Some(b) => b,
                    None => self.bus(&format!("{name}_mask"), x),
                };
                // Merge2 inputs: 0 = B (top of the stack), 1 = A, then the mask: push mask, A, B.
                self.nk.push_str(&format!("push ${bus}\n"));
                mask_knob = format!(" maskChannelInput {m}.alpha\n");
                inputs = "2+1";
            } else {
                a = self.mask_multiply(&a, m, name, x, y - 60);
            }
        }
        self.nk.push_str(&format!(
            "push ${a}\npush ${comp}\nMerge2 {{\n inputs {inputs}\n operation {op}\n{mask_knob} mix {mix}\n{} name Merge_{name}\n xpos {x}\n ypos {y}\n}}\nset NMerge_{name} [stack 0]\n",
            if l.visible { "" } else { " disable true\n" }
        ));
        format!("NMerge_{name}")
    }

    /// The label under a layer's name in its backdrop.
    fn sub_label(l: &ExportLayer, is_base: bool) -> String {
        if !l.visible {
            return "hidden".to_string();
        }
        let (op, exact) = nuke_merge_operation(l.blend_mode);
        let mut s = if is_base { "base".to_string() } else { op.to_string() };
        if !exact || l.approx {
            s.push_str(" ~");
        }
        let pct = (l.opacity.clamp(0.0, 1.0) * 100.0).round() as i32;
        if pct < 100 {
            s.push_str(&format!("  {pct}%"));
        }
        if l.mask.is_some() {
            s.push_str("  masked");
        }
        if !l.clipped.is_empty() {
            s.push_str(&format!("  +{} clipped", l.clipped.len()));
        }
        s
    }

    /// The layers clipped to `base` (a premultiplied stream), composited onto it in a branch:
    /// a clipped adjustment grades only the base; a clipped layer goes atop it (only inside the
    /// base's alpha, which it keeps).
    fn clip(&mut self, base: String, clipped: &[ExportLayer], depth: i32) -> String {
        let mut base = base;
        for c in clipped {
            if let Some(adj) = &c.adjustment {
                base = self.adjustment(c, adj, &base, depth);
                continue;
            }
            let Some((a, x, bus)) = self.stream(c, depth, false) else { continue };
            let name = &c.name;
            let y = self.row_y(depth);
            let mix = c.opacity.clamp(0.0, 1.0);
            let disable = if c.visible { "" } else { " disable true\n" };
            if merged_as(c.blend_mode) == BlendMode::Normal {
                // Normal clipped: atop, exact (its mask on the mask input is exact too).
                let mut mask_knob = String::new();
                let mut inputs = "2";
                if let Some(m) = &c.mask {
                    let bus = match bus {
                        Some(b) => b,
                        None => self.bus(&format!("{name}_mask"), x),
                    };
                    self.nk.push_str(&format!("push ${bus}\n"));
                    mask_knob = format!(" maskChannelInput {m}.alpha\n");
                    inputs = "2+1";
                }
                self.nk.push_str(&format!(
                    "push ${a}\npush ${base}\nMerge2 {{\n inputs {inputs}\n operation atop\n{mask_knob} mix {mix}\n{disable} name Clip_{name}\n xpos {x}\n ypos {y}\n}}\nset NClip_{name} [stack 0]\n"
                ));
            } else {
                // Another mode: merged in its mode, kept inside the base by the base's own
                // alpha on the mask input, and the base's alpha kept. Exact where the base is
                // opaque or empty; its soft edges are close.
                let (op, _) = nuke_merge_operation(c.blend_mode);
                let a = match &c.mask {
                    Some(m) => self.mask_multiply(&a, m, name, x, y - 60),
                    None => a,
                };
                self.nk.push_str(&format!(
                    "push ${base}\npush ${a}\npush ${base}\nMerge2 {{\n inputs 2+1\n operation {op}\n output {{rgba.red rgba.green rgba.blue -rgba.alpha}}\n maskChannelInput rgba.alpha\n mix {mix}\n{disable} name Clip_{name}\n xpos {x}\n ypos {y}\n}}\nset NClip_{name} [stack 0]\n"
                ));
            }
            base = format!("NClip_{name}");
        }
        base
    }

    /// A pixel layer's or an isolated group's premultiplied stream, with its own clipped layers
    /// composited on, ready to merge: (stream, x of its merge, bus Dot at that x if any).
    fn stream(&mut self, l: &ExportLayer, depth: i32, is_base: bool) -> Option<(String, i32, Option<String>)> {
        if let Some(children) = &l.children {
            let start = self.col;
            let sub = self.chain(children, None, depth + 1)?;
            let sub = if l.clipped.is_empty() { sub } else { self.clip(sub, &l.clipped, depth + 1) };
            self.group_backdrop(l, start, depth);
            return Some((sub, self.last_x(), None));
        }
        let x = self.next_x();
        let (a, bus) = self.column(&l.name, x);
        let (op, _) = nuke_merge_operation(l.blend_mode);
        let sub = Self::sub_label(l, is_base);
        self.layer_backdrop(&l.name, x, backdrop_color(op, is_base), &sub);
        if l.clipped.is_empty() {
            return Some((a, x, Some(bus)));
        }
        // The clipped layers take the next columns; the merge goes after the last of them.
        let a = self.clip(a, &l.clipped, depth + 1);
        Some((a, self.last_x(), None))
    }

    fn group_backdrop(&mut self, l: &ExportLayer, start_col: i32, depth: i32) {
        let cols = (self.col - start_col).max(1);
        let top = BD_Y - 40 - 10 * (self.max_depth - depth).max(0);
        let bottom = self.row_y(depth) + 50;
        let inset = 6 * depth;
        let mut sub = Self::sub_label(l, false);
        if l.blend_mode == BlendMode::PassThrough {
            sub = sub.replacen("over", "pass-through", 1);
        }
        self.backdrops.push_str(&format!(
            "BackdropNode {{\n inputs 0\n name BD_{}\n tile_color 0x30363cff\n label \"<b>{}</b>  <font size=2>{sub}</font>\"\n note_font_size 18\n xpos {}\n ypos {top}\n bdwidth {}\n bdheight {}\n z_order 1\n}}\n",
            l.name,
            l.name,
            start_col * COL_W - 60 + inset,
            cols * COL_W - 2 * inset,
            bottom - top
        ));
    }

    /// `items` (bottom first) composited onto `comp` on row `depth`; returns the new comp.
    fn chain(&mut self, items: &[ExportLayer], comp: Option<String>, depth: i32) -> Option<String> {
        let mut comp = comp;
        for l in items {
            if let Some(adj) = &l.adjustment {
                // Nothing to adjust before the base.
                if let Some(c) = &comp {
                    comp = Some(self.adjustment(l, adj, c, depth));
                }
                continue;
            }
            if let Some(children) = &l.children
                && l.blend_mode == BlendMode::PassThrough
                && l.clipped.is_empty()
            {
                // Pass-through: the children continue this comp.
                let start = self.col;
                if l.opacity >= 1.0 && l.mask.is_none() {
                    comp = self.chain(children, comp, depth);
                } else if let Some(after) = self.chain(children, comp.clone(), depth + 1) {
                    // Opacity or mask: the comp with the children mixed with the comp without
                    // them, as the canvas mixes a pass-through group.
                    comp = Some(match comp {
                        None => self.place(l, after, None, depth, self.last_x(), None),
                        Some(before) => {
                            let x = self.last_x();
                            let mut copy = l.clone();
                            copy.blend_mode = BlendMode::Normal;
                            self.mix_copy(&copy, after, before, depth, x)
                        }
                    });
                }
                self.group_backdrop(l, start, depth);
                continue;
            }
            let Some((a, x, bus)) = self.stream(l, depth, comp.is_none()) else { continue };
            comp = Some(self.place(l, a, comp, depth, x, bus));
        }
        comp
    }

    /// `after` mixed over `before` by the layer's opacity and mask (Merge2 copy).
    fn mix_copy(&mut self, l: &ExportLayer, after: String, before: String, depth: i32, x: i32) -> String {
        let name = &l.name;
        let y = self.row_y(depth);
        let mut mask_knob = String::new();
        let mut inputs = "2";
        if let Some(m) = &l.mask {
            let bus = self.bus(&format!("{name}_mask"), x);
            self.nk.push_str(&format!("push ${bus}\n"));
            mask_knob = format!(" maskChannelInput {m}.alpha\n");
            inputs = "2+1";
        }
        self.nk.push_str(&format!(
            "push ${after}\npush ${before}\nMerge2 {{\n inputs {inputs}\n operation copy\n{mask_knob} mix {}\n{} name Merge_{name}\n xpos {x}\n ypos {y}\n}}\nset NMerge_{name} [stack 0]\n",
            if l.visible { l.opacity.clamp(0.0, 1.0) } else { 0.0 },
            if l.visible { "" } else { " disable true\n" }
        ));
        format!("NMerge_{name}")
    }
}

/// Generates a Nuke script laid out like a PSD breakdown:
///
///   Read
///    o-------o-------o-------o        bus (one Dot per layer)
///   [Layer] [Layer] [Layer] [Layer]   one backdrop column each:
///   Shuffle Shuffle Shuffle Shuffle     Shuffle -> Premult
///   Premult Premult Premult Premult
///   [ branch rows: clipped layers and groups, composited on their own ]
///  [ o----->Merge-->Merge-->Merge ]   COMP row, B runs left to right
///
/// Masks are EXR layers of their own (`<Layer>_mask.alpha`) feeding the node's mask input.
pub fn generate_nuke_script(
    exr_filename: &str,
    layers: &[ExportLayer],
    width: u32,
    height: u32,
) -> String {
    let (w, h) = (width, height);
    let fmt = format!("{w} {h} 0 0 {w} {h} 1 custom");
    // A script with OCIOColorSpace nodes uses PhotoCraft's OCIO config, so their colour space
    // names resolve; otherwise Nuke's default colour management, as before.
    let mut has_ocio = false;
    walk(layers, &mut |l| has_ocio |= matches!(l.adjustment, Some(photocraft_doc::Adjustment::OcioColorSpace { .. })));
    let ocio = has_ocio.then(|| photocraft_ocio::current().ok().map(|c| c.uri().to_string())).flatten();
    let mut nk = String::new();

    nk.push_str(&format!(
        "Root {{\n inputs 0\n format \"{fmt}\"\n project_directory \"[python {{nuke.script_directory()}}]\"\n{}}}\n",
        root_color(&ocio)
    ));

    // Register every layer before any node references it, otherwise a Shuffle's
    // `in <layer>` resolves to `none` for names Nuke hasn't seen this session.
    let mut registered = String::new();
    walk(layers, &mut |l| {
        if l.is_pixels() {
            let n = &l.name;
            registered.push_str(&format!("add_layer {{{n} {n}.red {n}.green {n}.blue {n}.alpha}}\n"));
        }
        if let Some(m) = &l.mask {
            registered.push_str(&format!("add_layer {{{m} {m}.alpha}}\n"));
        }
    });
    nk.push_str(&registered);

    nk.push_str(&format!(
        "Read {{\n inputs 0\n file_type exr\n file \"[python {{nuke.script_directory()}}]/{}\"\n format \"{fmt}\"\n origset true\n colorspace {}\n name Read_MultiChannel\n xpos 0\n ypos {READ_Y}\n}}\nset NRead_MultiChannel [stack 0]\n",
        tcl_escape(exr_filename),
        // The EXR is scene-linear: Nuke's `linear`, or the OCIO config's scene_linear role.
        if ocio.is_some() { "scene_linear" } else { "linear" }
    ));

    let mut s = Script { nk, backdrops: String::new(), col: 0, prev_bus: "NRead_MultiChannel".into(), max_depth: branch_depth(layers) };
    let out = s.chain(layers, None, 0);
    let main_y = s.row_y(0);
    let cols = s.col.max(1);

    // Output Dot at the end of the comp chain, ready for a Write/Viewer.
    if let Some(out) = out {
        s.nk.push_str(&format!("push ${out}\n"));
    }
    s.nk.push_str(&format!(
        "Dot {{\n name Dot_OUT\n label OUT\n note_font_size 18\n xpos {}\n ypos {}\n}}\n",
        cols * COL_W - COL_W / 2 + DOT_DX,
        main_y + 3
    ));

    // One long COMP backdrop under all columns, behind the layer and group backdrops.
    s.nk.push_str(&format!(
        "BackdropNode {{\n inputs 0\n name BD_COMP\n tile_color 0x262626ff\n label \"<b>COMP</b>\"\n note_font_size 18\n xpos -50\n ypos {}\n bdwidth {}\n bdheight 90\n z_order 0\n}}\n",
        main_y - 50,
        cols * COL_W
    ));
    let mut nk = s.nk;
    nk.push_str(&s.backdrops);
    nk
}

/// The Root node's colour management: OCIO with `config` (a `.ocio` path or `ocio://` URI), or
/// Nuke's default with a linear working space.
fn root_color(config: &Option<String>) -> String {
    match config {
        Some(uri) => format!(" colorManagement OCIO\n OCIO_config custom\n customOCIOConfigPath \"{}\"\n", tcl_escape(uri)),
        None => " colorManagement Nuke\n workingSpaceLUT linear\n".to_string(),
    }
}

/// `px` (straight linear RGBA) through `adj` as a 32-bit linear document applies it.
fn adjusted_px(adj: &photocraft_doc::Adjustment, px: &[[f32; 4]]) -> Vec<[f32; 4]> {
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_compose::adjust::{Transfer, apply_depth};
    let mut buf = photocraft_compose::Buffer { rect: photocraft_geom::Rect::from_xywh(0, 0, px.len().max(1) as u32, 1), px: px.to_vec() };
    apply_depth(adj, &mut buf, Transfer::for_document(ColorMode::Rgb, SampleType::F32), Some(SampleType::F32));
    buf.px
}

/// The body of a Nuke `ColorLookup` node reproducing a Curves adjustment as PhotoCraft applies
/// it in 32-bit: each channel's response (master and channel curves composed in the layer's
/// order) sampled at 129 points over the curves' range with linear keys, plus a key far below
/// and far above so its ends (held or continued along their slopes) carry over whatever
/// extrapolation Nuke uses. The master and alpha curves are identity.
fn nk_lookup_knobs(adj: &photocraft_doc::Adjustment) -> String {
    let range = match adj {
        photocraft_doc::Adjustment::Curves { lookup, pencil: false, .. } => lookup.span(),
        _ => 1.0,
    };
    let mut xs = vec![-100.0 * range];
    xs.extend((0..=128).map(|k| k as f32 / 128.0 * range));
    xs.push(100.0 * range);
    let px: Vec<[f32; 4]> = xs.iter().map(|&x| [x, x, x, 1.0]).collect();
    let ys = adjusted_px(adj, &px);
    let curve = |c: usize| {
        let keys: Vec<String> = xs.iter().zip(&ys).map(|(x, y)| format!("x{x} {}", y[c])).collect();
        format!("{{curve L {}}}", keys.join(" "))
    };
    let identity = "{curve L x-100000 -100000 x100000 100000}";
    format!(" channels rgb\n lut {{master {identity}\n  red {}\n  green {}\n  blue {}\n  alpha {identity}}}\n unpremult rgba.alpha\n", curve(0), curve(1), curve(2))
}

fn is_exported_adjustment(adj: &photocraft_doc::Adjustment) -> bool {
    matches!(
        adj,
        photocraft_doc::Adjustment::Grade { .. }
            | photocraft_doc::Adjustment::Curves { space: photocraft_doc::adjust::ToneSpace::Rgb, .. }
            | photocraft_doc::Adjustment::OcioColorSpace { .. }
    )
}

/// Whether a pass-through group holds anything that blends with the layers beneath it (an
/// adjustment, a mode other than Normal), which merging it on its own changes.
fn reaches_below(layers: &[DocLayer]) -> bool {
    layers.iter().any(|l| {
        l.visible
            && (matches!(l.content, LayerContent::Adjustment(_))
                || !matches!(l.blend, BlendMode::Normal | BlendMode::PassThrough)
                || (l.blend == BlendMode::PassThrough && l.children().is_some_and(reaches_below)))
    })
}

/// Walks the document into the export tree, collecting the EXR's pixel and mask layers.
struct Collector<'a> {
    doc: &'a Document,
    bounds: Rect,
    groups: GroupExport,
    names: HashSet<String>,
    pixels: Vec<(String, Vec<[f32; 4]>)>,
    masks: Vec<(String, Vec<f32>)>,
}

impl Collector<'_> {
    /// `base`, or `base_2`, `base_3`, … when an earlier layer or mask already has that name (EXR
    /// layer prefixes and node names must be unique).
    fn unique(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        let mut suffix = 2usize;
        while self.names.contains(&name) || name.eq_ignore_ascii_case(BEAUTY) {
            name = format!("{base}_{suffix}");
            suffix += 1;
        }
        self.names.insert(name.clone());
        name
    }

    /// The pixel count every layer and mask holds.
    fn pixel_count(&self) -> usize {
        self.bounds.width() as usize * self.bounds.height() as usize
    }

    /// The layer's effective mask as an EXR layer of its own, or `None` without an enabled one.
    fn mask(&mut self, l: &DocLayer, name: &str) -> Option<String> {
        let pixel = l.mask.as_ref().is_some_and(|m| m.enabled);
        let vector = l.vector_mask.as_ref().is_some_and(|v| v.enabled);
        if !pixel && !vector {
            return None;
        }
        let mut v = Vec::new();
        if let Some(s) = photocraft_compose::masks::combined_mask(l, self.bounds) {
            s.read_region_into(self.bounds, &mut v);
        } else {
            l.mask.as_ref()?.values_into(self.bounds, &mut v);
        }
        if v.len() != self.pixel_count() {
            return None;
        }
        let name = self.unique(&format!("{name}_mask"));
        self.masks.push((name.clone(), v));
        Some(name)
    }

    /// `l` alone as the canvas draws it (content, effects; no blend mode, opacity or mask: the
    /// script applies those).
    fn render_alone(&self, l: &DocLayer) -> Vec<[f32; 4]> {
        let mut solo = l.clone();
        solo.blend = BlendMode::Normal;
        solo.opacity = 1.0;
        solo.fill_opacity = 1.0;
        solo.mask = None;
        solo.vector_mask = None;
        solo.clipped = false;
        solo.visible = true;
        photocraft_compose::render_layer_in(self.doc, &solo, self.bounds).px
    }

    fn layer(&mut self, l: &DocLayer, parent_visible: bool, depth: usize) -> Option<ExportLayer> {
        let visible = l.visible && parent_visible;
        let opacity = l.opacity * l.fill_opacity;
        match &l.content {
            LayerContent::Adjustment(adj) if is_exported_adjustment(adj) => {
                let raw = if l.name.trim().is_empty() { adj.label().to_string() } else { l.name.clone() };
                let name = self.unique(&sanitize_layer_name(&raw));
                let mask = self.mask(l, &name);
                Some(ExportLayer { adjustment: Some(adj.clone()), mask, ..ExportLayer::pixels(name, BlendMode::Normal, opacity, visible) })
            }
            // Other adjustments have no node; the beauty pass still holds them.
            LayerContent::Adjustment(_) => None,
            LayerContent::Group(g) if self.groups == GroupExport::Branches => {
                let raw = if l.name.trim().is_empty() { "Group".to_string() } else { l.name.clone() };
                let name = self.unique(&sanitize_layer_name(&raw));
                let children = self.list(&g.children, visible, depth + 1);
                if children.is_empty() {
                    return None;
                }
                let mask = self.mask(l, &name);
                // Artboards clip their contents to the board; the branch does not.
                let approx = g.artboard.is_some();
                Some(ExportLayer { mask, children: Some(children), approx, ..ExportLayer::pixels(name, l.blend, opacity, visible) })
            }
            _ => {
                let fallback = if l.is_group() { "Group" } else { "Layer" };
                let raw = if l.name.trim().is_empty() { format!("{fallback}_{}", self.pixels.len() + 1) } else { l.name.clone() };
                let name = self.unique(&sanitize_layer_name(&raw));
                let px = match &l.content {
                    LayerContent::Raster(surface) if !photocraft_compose::effects::has_effects(l) => {
                        photocraft_compose::surface_to_buffer(surface, self.bounds).px
                    }
                    // Type, shapes, fills, smart objects, layer styles and merged groups: as drawn.
                    _ => self.render_alone(l),
                };
                if px.len() != self.pixel_count() {
                    return None;
                }
                self.pixels.push((name.clone(), px));
                let mask = self.mask(l, &name);
                let (blend, approx) = match &l.content {
                    LayerContent::Group(g) if l.blend == BlendMode::PassThrough => (BlendMode::Normal, reaches_below(&g.children)),
                    _ => (l.blend, false),
                };
                Some(ExportLayer { mask, approx, ..ExportLayer::pixels(name, blend, opacity, visible) })
            }
        }
    }

    /// A sibling list (bottom first), clipped layers gathered onto their bases.
    fn list(&mut self, layers: &[DocLayer], parent_visible: bool, depth: usize) -> Vec<ExportLayer> {
        let mut out: Vec<ExportLayer> = Vec::new();
        // The clipping base: the latest unclipped pixel layer or group in `out`.
        let mut base: Option<usize> = None;
        if depth > MAX_GROUP_DEPTH {
            return out;
        }
        for l in layers {
            let item = self.layer(l, parent_visible, depth);
            match item {
                Some(item) if l.clipped && base.is_some() => {
                    if let Some(b) = base.and_then(|i| out.get_mut(i)) {
                        b.clipped.push(item);
                    }
                }
                Some(item) => {
                    // A layer clipped to an adjustment or to nothing exported goes on unclipped.
                    if !l.clipped {
                        base = item.adjustment.is_none().then_some(out.len());
                    }
                    out.push(item);
                }
                None if !l.clipped => base = None,
                None => {}
            }
        }
        out
    }
}

pub fn export_multichannel_exr_with_options(
    doc: &Document,
    path: &Path,
    compression: NukeCompression,
    include_flat_beauty: bool,
    use_32bit: bool,
    write_nuke_script: bool,
    groups: GroupExport,
) -> std::result::Result<(), String> {
    let width = doc.size.width as usize;
    let height = doc.size.height as usize;

    if width == 0 || height == 0 {
        return Err("Document dimensions must be greater than zero.".into());
    }

    let mut c = Collector { doc, bounds: doc.bounds(), groups, names: HashSet::new(), pixels: Vec::new(), masks: Vec::new() };
    c.names.insert(BEAUTY.to_string());
    let nuke_layers = c.list(&doc.layers, true, 0);
    let Collector { pixels: mut extracted, masks, .. } = c;

    if extracted.is_empty() {
        return Err("No layers available for export.".into());
    }

    if include_flat_beauty {
        // The canvas's own composite: every layer, mode, mask, clipping group, group and
        // adjustment exactly as shown, whether or not the script can rebuild it.
        let flat = photocraft_compose::flatten(doc).px;
        if flat.len() != width * height {
            return Err("The document is too large to flatten for the beauty pass.".into());
        }
        extracted.push((BEAUTY.to_string(), flat));
    }

    let to_samples = |vals: Vec<f32>| -> FlatSamples {
        if use_32bit {
            FlatSamples::F32(vals)
        } else {
            FlatSamples::F16(vals.into_iter().map(f16::from_f32).collect())
        }
    };

    // Single-part EXR, fully qualified channel names: "<Layer>.R/G/B/A",
    // beauty as bare R/G/B/A. This is what Nuke's Shuffle `in <Layer>` expects.
    // Masks are "<Layer>_mask.A", which Nuke reads as `<Layer>_mask.alpha`.
    let mut chans: SmallVec<[AnyChannel<FlatSamples>; 4]> = SmallVec::new();
    for (name, data) in &extracted {
        let prefix = if name == BEAUTY { String::new() } else { format!("{name}.") };
        for (idx, ch) in ["R", "G", "B", "A"].iter().enumerate() {
            let label = format!("{prefix}{ch}");
            let vals: Vec<f32> = data.iter().map(|p| p[idx]).collect();
            chans.push(AnyChannel::new(label.as_str(), to_samples(vals)));
        }
    }
    for (name, data) in masks {
        chans.push(AnyChannel::new(format!("{name}.A").as_str(), to_samples(data)));
    }

    let layer = Layer::new(
        Vec2(width, height),
        LayerAttributes::default(),
        Encoding {
            compression: compression.to_exr_compression(),
            line_order: LineOrder::Increasing,
            blocks: Blocks::ScanLines,
        },
        AnyChannels::sort(chans),
    );
    let image = Image::from_layer(layer);

    image.write().to_file(path).map_err(|e| e.to_string())?;

    if write_nuke_script {
        let exr_filename = path.file_name().unwrap_or_default().to_string_lossy();
        let nk_path = path.with_extension("nk");
        let nk_content =
            generate_nuke_script(&exr_filename, &nuke_layers, width as u32, height as u32);
        std::fs::write(&nk_path, nk_content).map_err(|e| e.to_string())?;
    }

    Ok(())
}

pub fn export_multichannel_exr(doc: &Document, path: &Path) -> std::result::Result<(), String> {
    export_multichannel_exr_with_options(doc, path, NukeCompression::ZipScanline, true, true, false, GroupExport::Branches)
}

#[cfg(test)]
#[path = "exr_export_tests.rs"]
mod tests;
