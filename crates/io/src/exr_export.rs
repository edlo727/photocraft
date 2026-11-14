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
    /// The EXR layer holding this layer's mask (pixel mask × vector mask, with density and
    /// feather, as the canvas applies them) in every channel: a Merge `mask` for pixel layers
    /// and groups, the node's own mask channel for adjustments.
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
/// A Nuke blend mode is its own Merge operation, so the script runs the maths the canvas renders.
/// A Photoshop mode becomes the closest operation (Multiply `multiply`, Subtract `from`, …) and
/// one Nuke has nothing close to (Linear Burn, Vivid Light, Hue, …) becomes `over`, both flagged
/// with `~` in the breakdown. Pass-through is `over` (its children continue the comp). The EXR's
/// beauty pass holds the true composite.
pub fn nuke_merge_operation(mode: BlendMode) -> (&'static str, bool) {
    mode.nearest_nuke_op()
}

/// Extra Merge2 knobs the Merge for `mode` needs to match the canvas.
///
/// Where a layer is transparent the canvas leaves the comp beneath unchanged, and its alpha is
/// a+b−ab, for every mode whose maths doesn't use alpha itself (multiply, average, divide,
/// overlay, geometric…). Nuke's plain operation doesn't: multiply blacks B out where A is empty,
/// average halves it. Merge's "alpha masking" (`screen_alpha`) is exactly the canvas's: "the input
/// images are unchanged where the other image has zero alpha, and the output alpha is set to
/// a+b-a*b". It is turned on wherever the canvas's own maths gives that alpha; never for the
/// Porter-Duff operations, which use alpha themselves (Nuke greys the option out for them).
fn merge_knobs(mode: BlendMode) -> &'static str {
    if needs_alpha_masking(mode) { " screen_alpha true\n" } else { "" }
}

fn needs_alpha_masking(mode: BlendMode) -> bool {
    use photocraft_color::blend::composite;
    const PORTER_DUFF: [&str; 12] = ["over", "under", "in", "out", "atop", "xor", "mask", "stencil", "matte", "conjoint-over", "disjoint-over", "copy"];
    // A Photoshop mode written as its closest operation (Multiply as `multiply`) needs it as much
    // as the Nuke one: its maths leaves the comp beneath unchanged where the layer is empty too.
    let (op, _) = nuke_merge_operation(mode);
    if PORTER_DUFF.contains(&op) {
        return false;
    }
    const SAMPLES: [[f32; 4]; 4] = [[0.2, 0.5, 0.9, 1.0], [0.8, 0.3, 0.1, 0.6], [0.5, 0.5, 0.5, 0.25], [0.9, 0.7, 0.05, 0.9]];
    SAMPLES.iter().all(|&b| {
        SAMPLES.iter().all(|&a| {
            let out = composite(mode, b, a, 1.0);
            (out[3] - (a[3] + b[3] - a[3] * b[3])).abs() < 1e-4
        })
    })
}

/// The mode whose maths the script's Merge for `mode` runs: Normal where it is written as `over`.
fn merged_as(mode: BlendMode) -> BlendMode {
    if nuke_merge_operation(mode).0 == "over" { BlendMode::Normal } else { mode }
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
const COL_W: i32 = 250; // column pitch: a layer's Shuffle and, beside it, its mask's
const BD_W: i32 = 230; // layer backdrop width
const DOT_DX: i32 = 34; // centres a Dot over an 80-wide node
const MASK_DX: i32 = 110; // a mask's Shuffle sits right of its layer's
const READ_Y: i32 = -230;
const BUS_Y: i32 = -100;
const BD_Y: i32 = -50; // layer backdrop top
const BD_H: i32 = 190;
const SHUFFLE_Y: i32 = 40;
const PREMULT_Y: i32 = 100;
/// Under a layer's Premult, its mask and its clipped layers stack one slot each.
const STACK_Y: i32 = 150;
const STACK_DY: i32 = 130;
/// Group branch rows, above the main COMP row.
const ROW_DY: i32 = 120;

fn walk<'a>(layers: &'a [ExportLayer], f: &mut dyn FnMut(&'a ExportLayer)) {
    for l in layers {
        f(l);
        if let Some(children) = &l.children {
            walk(children, f);
        }
        walk(&l.clipped, f);
    }
}

/// How many group branch rows sit above the main comp row.
fn branch_depth(layers: &[ExportLayer]) -> i32 {
    layers
        .iter()
        .map(|l| {
            let clip = branch_depth(&l.clipped);
            match &l.children {
                Some(c) => 1 + branch_depth(c).max(clip),
                None => clip,
            }
        })
        .max()
        .unwrap_or(0)
}

/// The most slots any layer's column stacks under its Premult (its mask, its clipped layers).
fn stack_slots(layers: &[ExportLayer]) -> i32 {
    let mut most = 0;
    walk(layers, &mut |l| {
        if l.is_pixels() {
            most = most.max(i32::from(l.mask.is_some()) + l.clipped.len() as i32);
        }
    });
    most
}

/// Where a clipped node goes: stacked in its base's column, or along a group's branch row.
#[derive(Clone, Copy)]
enum At {
    Stack { x: i32, slot: i32 },
    Row,
}

/// The `.nk` text as it is built: nodes in `nk`, backdrops collected apart (written last, behind).
struct Script {
    nk: String,
    backdrops: String,
    col: i32,
    prev_bus: String,
    max_depth: i32,
    /// Where the stacks under the columns end.
    stack_bottom: i32,
}

impl Script {
    fn row_y(&self, depth: i32) -> i32 {
        self.stack_bottom + 60 + (self.max_depth - depth).max(0) * ROW_DY
    }

    fn slot_top(slot: i32) -> i32 {
        STACK_Y + slot * STACK_DY
    }

    fn slot_node_y(slot: i32) -> i32 {
        Self::slot_top(slot) + 60
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

    #[allow(clippy::too_many_arguments)]
    fn backdrop(&mut self, id: &str, title: &str, sub: &str, color: &str, x: i32, y: i32, h: i32) {
        self.backdrops.push_str(&format!(
            "BackdropNode {{\n inputs 0\n name BD_{id}\n tile_color {color}\n label \"<center><b>{title}</b>\\n<font size=2>{sub}</font></center>\"\n note_font_size 18\n xpos {}\n ypos {y}\n bdwidth {BD_W}\n bdheight {h}\n z_order 2\n}}\n",
            x - 50
        ));
    }

    /// The mask's EXR layer off the bus, and `stream` (premultiplied) masked by it with a Merge
    /// `mask` (B × A's alpha): the mask goes into the layer's coverage, as the canvas applies it,
    /// so it is exact in every blend mode.
    #[allow(clippy::too_many_arguments)]
    fn mask(&mut self, stream: &str, mask: &str, name: &str, bus: &str, x: i32, shuffle_y: i32, merge_y: i32) -> String {
        self.nk.push_str(&format!(
            "push ${bus}\nShuffle {{\n in {mask}\n out rgba\n name ShuffleMask_{name}\n xpos {}\n ypos {shuffle_y}\n}}\nset NShuffleMask_{name} [stack 0]\n",
            x + MASK_DX
        ));
        // Merge2 inputs: 0 = B (top of the stack), 1 = A: push A, then B.
        self.nk.push_str(&format!(
            "push $NShuffleMask_{name}\npush ${stream}\nMerge2 {{\n inputs 2\n operation mask\n name Mask_{name}\n xpos {x}\n ypos {merge_y}\n}}\nset NMask_{name} [stack 0]\n"
        ));
        format!("NMask_{name}")
    }

    /// A pixel layer's column: bus Dot → Shuffle → Premult, then its mask. Returns the stream.
    fn column(&mut self, l: &ExportLayer, x: i32, is_base: bool) -> String {
        let name = &l.name;
        let bus = self.bus(name, x);
        // Shuffle -> Premult (EXR holds straight alpha; Merge wants premultiplied).
        self.nk.push_str(&format!("Shuffle {{\n in {name}\n out rgba\n name Shuffle_{name}\n xpos {x}\n ypos {SHUFFLE_Y}\n}}\n"));
        self.nk.push_str(&format!("Premult {{\n name Premult_{name}\n xpos {x}\n ypos {PREMULT_Y}\n}}\nset NPremult_{name} [stack 0]\n"));
        let mut a = format!("NPremult_{name}");
        let mut h = BD_H;
        if let Some(m) = &l.mask {
            a = self.mask(&a, m, name, &bus, x, PREMULT_Y, Self::slot_node_y(0));
            h += STACK_DY;
        }
        let (op, _) = nuke_merge_operation(l.blend_mode);
        let sub = Self::sub_label(l, is_base);
        self.backdrop(name, name, &sub, backdrop_color(op, is_base), x, BD_Y, h);
        a
    }

    /// The mask's EXR layer Shuffled off the bus at (`x`, `y`) into rgba.
    fn mask_shuffle(&mut self, mask: &str, name: &str, x: i32, y: i32) -> String {
        let bus = self.bus(&format!("{name}_mask"), x);
        self.nk.push_str(&format!(
            "push ${bus}\nShuffle {{\n in {mask}\n out rgba\n name ShuffleMask_{name}\n xpos {x}\n ypos {y}\n}}\nset NShuffleMask_{name} [stack 0]\n"
        ));
        format!("NShuffleMask_{name}")
    }

    /// `a` mixed over `b` by `mask`'s alpha times `mix` (a Keymix: its mask is an ordinary third
    /// input, so it stays wired and visible in Nuke).
    #[allow(clippy::too_many_arguments)]
    fn keymix(&mut self, b: &str, a: &str, mask: &str, mix: f32, disable: bool, id: &str, x: i32, y: i32) -> String {
        // Keymix inputs: 0 = B (top of the stack), 1 = A, 2 = mask: push mask, A, then B.
        self.nk.push_str(&format!(
            "push ${mask}\npush ${a}\npush ${b}\nKeymix {{\n inputs 3\n channels rgba\n maskChannel rgba.alpha\n mix {mix}\n{} name {id}\n xpos {x}\n ypos {y}\n}}\nset N{id} [stack 0]\n",
            if disable { " disable true\n" } else { "" }
        ));
        format!("N{id}")
    }

    /// An adjustment node on `comp` at (`x`, `y`). Its mask, like a layer's, is Shuffled off the
    /// bus beside it and wired straight into the node's mask input: the node mixes by it, as the
    /// canvas mixes an adjustment by its mask.
    fn adjustment(&mut self, l: &ExportLayer, adj: &photocraft_doc::Adjustment, comp: &str, x: i32, y: i32) -> (String, &'static str) {
        let name = &l.name;
        let (class, knobs, op) = match adj {
            photocraft_doc::Adjustment::Grade { .. } => ("Grade", nk_grade_knobs(adj), "grade"),
            photocraft_doc::Adjustment::OcioColorSpace { input, output } => (
                "OCIOColorSpace",
                format!(" in_colorspace \"{}\"\n out_colorspace \"{}\"\n unpremult rgba.alpha\n", tcl_escape(input), tcl_escape(output)),
                "ocio",
            ),
            _ => ("ColorLookup", nk_lookup_knobs(adj), "colorlookup"),
        };
        // As Nuke saves a node with its mask input connected: `inputs 1+1`, the image on top of
        // the stack and the mask under it (push the mask, then the image), and the mask channel
        // in `maskChannelMask`, after the node's own knobs and before `unpremult`.
        let pushes = match &l.mask {
            Some(m) => {
                let shuffle = self.mask_shuffle(m, name, x + MASK_DX, y);
                format!("push ${shuffle}\npush ${comp}\n")
            }
            None => format!("push ${comp}\n"),
        };
        let (inputs, mask_knob) = if l.mask.is_some() { (" inputs 1+1\n", " maskChannelMask rgba.alpha\n") } else { ("", "") };
        const UNPREMULT: &str = " unpremult rgba.alpha\n";
        let knobs = match knobs.strip_suffix(UNPREMULT) {
            Some(own) => format!("{own}{mask_knob}{UNPREMULT}"),
            None => format!("{knobs}{mask_knob}"),
        };
        self.nk.push_str(&format!(
            "{pushes}{class} {{\n{inputs}{knobs}{} mix {}\n name {class}_{name}\n xpos {x}\n ypos {y}\n}}\nset N{class}_{name} [stack 0]\n",
            if l.visible { "" } else { " disable true\n" },
            l.opacity.clamp(0.0, 1.0)
        ));
        (format!("N{class}_{name}"), op)
    }

    fn adjustment_label(l: &ExportLayer, op: &str) -> String {
        if !l.visible {
            return "hidden".to_string();
        }
        let mut s = op.to_string();
        let pct = (l.opacity.clamp(0.0, 1.0) * 100.0).round() as i32;
        if pct < 100 {
            s.push_str(&format!("  {pct}%"));
        }
        if l.mask.is_some() {
            s.push_str(" masked");
        }
        s
    }

    /// `a` (this layer's premultiplied stream) merged onto `comp` at column `x` with its blend
    /// mode and opacity, or starting the comp when there is none yet.
    fn place(&mut self, l: &ExportLayer, a: String, comp: Option<String>, depth: i32, x: i32) -> String {
        let name = &l.name;
        let y = self.row_y(depth);
        let mix = l.opacity.clamp(0.0, 1.0);
        let Some(comp) = comp else {
            // The base starts the comp. Over an empty backdrop every mode is plain over, so only
            // its opacity (0 when hidden) applies.
            let mut a = a;
            let base_mix = if l.visible { mix } else { 0.0 };
            if base_mix < 1.0 {
                self.nk.push_str(&format!(
                    "push ${a}\nMultiply {{\n channels rgba\n value {base_mix}\n name Opacity_{name}\n xpos {x}\n ypos {}\n}}\nset NOpacity_{name} [stack 0]\n",
                    y - 40
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
        self.nk.push_str(&format!(
            "push ${a}\npush ${comp}\nMerge2 {{\n inputs 2\n operation {op}\n{} mix {mix}\n{} name Merge_{name}\n xpos {x}\n ypos {y}\n}}\nset NMerge_{name} [stack 0]\n",
            merge_knobs(l.blend_mode),
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

    /// The layers clipped to `base` (a premultiplied stream), composited onto it: a clipped
    /// adjustment grades only the base; a clipped layer goes atop it (only inside the base's
    /// alpha, which it keeps). Stacked under the base's column, or along a group's branch row.
    fn clip(&mut self, base: String, clipped: &[ExportLayer], at: At, depth: i32) -> String {
        let mut base = base;
        for (i, c) in clipped.iter().enumerate() {
            let name = &c.name;
            if let Some(adj) = &c.adjustment {
                let (x, y) = match at {
                    At::Stack { x, slot } => (x, Self::slot_node_y(slot + i as i32)),
                    At::Row => (self.next_x(), self.row_y(depth)),
                };
                let (node, op) = self.adjustment(c, adj, &base, x, y);
                let (top, h) = match at {
                    At::Stack { slot, .. } => (Self::slot_top(slot + i as i32), STACK_DY - 10),
                    At::Row => (BD_Y, BD_H),
                };
                self.backdrop(name, name, &Self::adjustment_label(c, op), backdrop_color(op, false), x, top, h);
                base = node;
                continue;
            }
            let Some((a, cx)) = self.stream(c, depth, false) else { continue };
            let (x, y) = match at {
                At::Stack { x, slot } => (x, Self::slot_node_y(slot + i as i32)),
                At::Row => (cx, self.row_y(depth)),
            };
            if let At::Stack { slot, .. } = at {
                let (op, _) = nuke_merge_operation(c.blend_mode);
                self.backdrop(&format!("Clip_{name}"), name, &format!("clipped  {op}"), "0x3a3a3aff", x, Self::slot_top(slot + i as i32), STACK_DY - 10);
            }
            let mix = c.opacity.clamp(0.0, 1.0);
            let disable = if c.visible { "" } else { " disable true\n" };
            if merged_as(c.blend_mode) == BlendMode::Normal {
                // Normal clipped: atop, exact.
                self.nk.push_str(&format!(
                    "push ${a}\npush ${base}\nMerge2 {{\n inputs 2\n operation atop\n mix {mix}\n{disable} name Clip_{name}\n xpos {x}\n ypos {y}\n}}\nset NClip_{name} [stack 0]\n"
                ));
            } else {
                // Another mode: merged in its mode (keeping the base's alpha), then kept inside
                // the base by a Keymix on the base's own alpha. Exact where the base is opaque or
                // empty; its soft edges are close.
                let (op, _) = nuke_merge_operation(c.blend_mode);
                self.nk.push_str(&format!(
                    "push ${a}\npush ${base}\nMerge2 {{\n inputs 2\n operation {op}\n{} output {{rgba.red rgba.green rgba.blue -rgba.alpha}}\n mix {mix}\n{disable} name ClipOp_{name}\n xpos {x}\n ypos {y}\n}}\nset NClipOp_{name} [stack 0]\n",
                    merge_knobs(c.blend_mode)
                ));
                let merged = format!("NClipOp_{name}");
                self.keymix(&base, &merged, &base, 1.0, !c.visible, &format!("Clip_{name}"), x, y + 40);
            }
            base = format!("NClip_{name}");
        }
        base
    }

    /// A pixel layer's or an isolated group's premultiplied stream, masked and with its own
    /// clipped layers composited on, ready to merge: (stream, x of its merge).
    fn stream(&mut self, l: &ExportLayer, depth: i32, is_base: bool) -> Option<(String, i32)> {
        let name = &l.name;
        if let Some(children) = &l.children {
            let start = self.col;
            let mut sub = self.chain(children, None, depth + 1)?;
            if !l.clipped.is_empty() {
                sub = self.clip(sub, &l.clipped, At::Row, depth + 1);
            }
            let x = self.last_x();
            if let Some(m) = &l.mask {
                let bus = self.bus(&format!("{name}_mask"), x);
                let y = self.row_y(depth) - ROW_DY / 2;
                sub = self.mask(&sub, m, name, &bus, x, y, y);
            }
            self.group_backdrop(l, start, depth);
            return Some((sub, x));
        }
        let x = self.next_x();
        let a = self.column(l, x, is_base);
        if l.clipped.is_empty() {
            return Some((a, x));
        }
        let slot = i32::from(l.mask.is_some());
        let a = self.clip(a, &l.clipped, At::Stack { x, slot }, depth);
        Some((a, x))
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
                    let x = self.next_x();
                    let y = self.row_y(depth);
                    let (node, op) = self.adjustment(l, adj, c, x, y);
                    self.backdrop(&l.name, &l.name, &Self::adjustment_label(l, op), backdrop_color(op, false), x, BD_Y, BD_H);
                    comp = Some(node);
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
                    let x = self.last_x();
                    comp = Some(match comp {
                        None => {
                            let after = match &l.mask {
                                Some(m) => {
                                    let bus = self.bus(&format!("{}_mask", l.name), x);
                                    let y = self.row_y(depth) - ROW_DY / 2;
                                    self.mask(&after, m, &l.name, &bus, x, y, y)
                                }
                                None => after,
                            };
                            self.place(l, after, None, depth, x)
                        }
                        Some(before) => self.mix_copy(l, after, before, depth, x),
                    });
                }
                self.group_backdrop(l, start, depth);
                continue;
            }
            let Some((a, x)) = self.stream(l, depth, comp.is_none()) else { continue };
            comp = Some(self.place(l, a, comp, depth, x));
        }
        comp
    }

    /// `after` mixed over `before` by the layer's opacity and mask: a Keymix with the mask, a
    /// Merge2 copy without.
    fn mix_copy(&mut self, l: &ExportLayer, after: String, before: String, depth: i32, x: i32) -> String {
        let name = &l.name;
        let y = self.row_y(depth);
        let mix = if l.visible { l.opacity.clamp(0.0, 1.0) } else { 0.0 };
        if let Some(m) = &l.mask {
            let shuffle = self.mask_shuffle(m, name, x + MASK_DX, y - ROW_DY / 2);
            return self.keymix(&before, &after, &shuffle, mix, !l.visible, &format!("Merge_{name}"), x, y);
        }
        self.nk.push_str(&format!(
            "push ${after}\npush ${before}\nMerge2 {{\n inputs 2\n operation copy\n mix {mix}\n{} name Merge_{name}\n xpos {x}\n ypos {y}\n}}\nset NMerge_{name} [stack 0]\n",
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
///   Premult Premult Premult Premult     -> Merge mask (its mask) -> its clipped layers
///   [ group branch rows ]
///  [ o----->Merge-->Merge-->Merge ]   COMP row, B runs left to right
///
/// A layer's mask is an EXR layer of its own (`<Layer>_mask`, the mask in every channel),
/// Shuffled out beside the layer and applied with a Merge `mask`.
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
        let pixels = l.is_pixels().then_some(&l.name);
        for n in pixels.into_iter().chain(l.mask.as_ref()) {
            registered.push_str(&format!("add_layer {{{n} {n}.red {n}.green {n}.blue {n}.alpha}}\n"));
        }
    });
    nk.push_str(&registered);

    nk.push_str(&format!(
        "Read {{\n inputs 0\n file_type exr\n file \"[python {{nuke.script_directory()}}]/{}\"\n format \"{fmt}\"\n origset true\n colorspace {}\n name Read_MultiChannel\n xpos 0\n ypos {READ_Y}\n}}\nset NRead_MultiChannel [stack 0]\n",
        tcl_escape(exr_filename),
        // The EXR is scene-linear: Nuke's `linear`, or the OCIO config's scene_linear role.
        if ocio.is_some() { "scene_linear" } else { "linear" }
    ));

    let mut s = Script {
        nk,
        backdrops: String::new(),
        col: 0,
        prev_bus: "NRead_MultiChannel".into(),
        max_depth: branch_depth(layers),
        stack_bottom: Script::slot_top(stack_slots(layers)),
    };
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
    buf.into_px()
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
    /// Progress: layers read so far of `total`, reported into `0.0..span`.
    report: Progress<'a>,
    done: usize,
    total: usize,
    span: f32,
    /// The export was cancelled: stop reading layers.
    cancelled: bool,
}

/// Progress and cancellation for an export: called with the fraction done (`0.0..=1.0`) and
/// what it is doing; returns `false` to cancel.
pub type Progress<'a> = &'a (dyn Fn(f32, &str) -> bool + Sync);

/// Never cancels, ignores progress.
pub const NO_PROGRESS: Progress<'static> = &|_, _| true;

/// Every layer in `layers` and below (the work the export reads).
fn count_layers(layers: &[DocLayer]) -> usize {
    layers.iter().map(|l| 1 + l.children().map_or(0, count_layers)).sum()
}

impl<'a> Collector<'a> {
    fn new(doc: &'a Document, groups: GroupExport, report: Progress<'a>, span: f32) -> Self {
        let mut names = HashSet::new();
        names.insert(BEAUTY.to_string());
        Collector {
            doc,
            bounds: doc.bounds(),
            groups,
            names,
            pixels: Vec::new(),
            masks: Vec::new(),
            report,
            done: 0,
            total: count_layers(&doc.layers).max(1),
            span,
            cancelled: false,
        }
    }

    /// One more layer read; `false` once cancelled.
    fn tick(&mut self, name: &str) -> bool {
        if self.cancelled {
            return false;
        }
        self.done += 1;
        let f = self.done as f32 / self.total as f32 * self.span;
        if !(self.report)(f, &format!("Reading layer {} of {}: {name}", self.done.min(self.total), self.total)) {
            self.cancelled = true;
        }
        !self.cancelled
    }

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
        photocraft_compose::render_layer_in(self.doc, &solo, self.bounds).into_px()
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
                        photocraft_compose::surface_to_buffer(surface, self.bounds).into_px()
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
            if !self.tick(&l.name) {
                return out;
            }
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

/// Everything an EXR + `.nk` export decides, apart from the document and where it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NukeExportOptions {
    /// EXR compression, applied to every part of the file.
    pub compression: NukeCompression,
    /// Also write the flattened result as the file's main (beauty) image.
    pub include_flat_beauty: bool,
    /// Full float channels rather than half.
    pub use_32bit: bool,
    /// Write the matching `.nk` comp beside the EXR.
    pub write_nuke_script: bool,
    /// How groups go into the file and the script.
    pub groups: GroupExport,
}

impl Default for NukeExportOptions {
    fn default() -> Self {
        Self { compression: NukeCompression::ZipScanline, include_flat_beauty: true, use_32bit: true, write_nuke_script: false, groups: GroupExport::Branches }
    }
}

pub fn export_multichannel_exr_with_options(
    doc: &Document,
    path: &Path,
    options: NukeExportOptions,
    progress: Progress<'_>,
) -> std::result::Result<(), String> {
    let NukeExportOptions { compression, include_flat_beauty, use_32bit, write_nuke_script, groups } = options;
    const CANCELLED: &str = "cancelled";
    let step = |f: f32, m: &str| if progress(f, m) { Ok(()) } else { Err(CANCELLED.to_string()) };
    let width = doc.size.width as usize;
    let height = doc.size.height as usize;

    if width == 0 || height == 0 {
        return Err("Document dimensions must be greater than zero.".into());
    }

    // Reading layers: 0–60%, the beauty pass to 75%, channels to 80%, writing to 98%.
    let mut c = Collector::new(doc, groups, progress, 0.6);
    let nuke_layers = c.list(&doc.layers, true, 0);
    if c.cancelled {
        return Err(CANCELLED.into());
    }
    let Collector { pixels: mut extracted, masks, .. } = c;

    if extracted.is_empty() {
        return Err("No layers available for export.".into());
    }

    if include_flat_beauty {
        // The canvas's own composite: every layer, mode, mask, clipping group, group and
        // adjustment exactly as shown, whether or not the script can rebuild it.
        step(0.6, "Compositing the beauty pass")?;
        let flat = photocraft_compose::flatten(doc).into_px();
        if flat.len() != width * height {
            return Err("The document is too large to flatten for the beauty pass.".into());
        }
        extracted.push((BEAUTY.to_string(), flat));
    }

    step(0.75, "Building channels")?;
    let to_samples = |vals: Vec<f32>| -> FlatSamples {
        if use_32bit {
            FlatSamples::F32(vals)
        } else {
            FlatSamples::F16(vals.into_iter().map(f16::from_f32).collect())
        }
    };

    // Single-part EXR, fully qualified channel names: "<Layer>.R/G/B/A",
    // beauty as bare R/G/B/A. This is what Nuke's Shuffle `in <Layer>` expects.
    let mut chans: SmallVec<[AnyChannel<FlatSamples>; 4]> = SmallVec::new();
    for (name, data) in &extracted {
        let prefix = if name == BEAUTY { String::new() } else { format!("{name}.") };
        for (idx, ch) in ["R", "G", "B", "A"].iter().enumerate() {
            let label = format!("{prefix}{ch}");
            let vals: Vec<f32> = data.iter().map(|p| p[idx]).collect();
            chans.push(AnyChannel::new(label.as_str(), to_samples(vals)));
        }
    }
    // Masks in every channel ("<Layer>_mask.R/G/B/A"): they view as grey in Nuke, and the
    // script's Merge `mask` takes their alpha.
    for (name, data) in masks {
        for ch in ["R", "G", "B", "A"] {
            chans.push(AnyChannel::new(format!("{name}.{ch}").as_str(), to_samples(data.clone())));
        }
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

    // The last chance to cancel: nothing is written before this.
    step(0.8, "Writing EXR")?;
    // Writing can't stop halfway (a cancelled export would leave a broken file): progress only.
    image
        .write()
        .on_progress(|p| {
            let _ = progress(0.8 + 0.18 * p as f32, "");
        })
        .to_file(path)
        .map_err(|e| e.to_string())?;

    if write_nuke_script {
        let _ = progress(0.98, "Writing Nuke script");
        let exr_filename = path.file_name().unwrap_or_default().to_string_lossy();
        let nk_path = path.with_extension("nk");
        let nk_content =
            generate_nuke_script(&exr_filename, &nuke_layers, width as u32, height as u32);
        std::fs::write(&nk_path, nk_content).map_err(|e| e.to_string())?;
    }
    let _ = progress(1.0, "Done");

    Ok(())
}

pub fn export_multichannel_exr(doc: &Document, path: &Path) -> std::result::Result<(), String> {
    export_multichannel_exr_with_options(doc, path, NukeExportOptions::default(), NO_PROGRESS)
}

#[cfg(test)]
#[path = "exr_export_tests.rs"]
mod tests;
