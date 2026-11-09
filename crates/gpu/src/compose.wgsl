struct Chunk {
    origin: vec2<i32>,
    size: vec2<i32>,
};

struct Op {
    mode: i32,
    kind: i32,
    flags: u32,
    _pad0: u32,
    opacity: f32,
    mask_density: f32,
    mask_default: f32,
    fill: f32,
    tex_origin: vec2<i32>,
    tex_size: vec2<i32>,
    mask_origin: vec2<i32>,
    mask_size: vec2<i32>,
    color: vec4<f32>,
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
    p3: vec4<f32>,
    map_origin: vec2<i32>,
    map_size: vec2<i32>,
    p4: vec4<f32>,
    q0: vec4<f32>,
    q1: vec4<f32>,
    q2: vec4<f32>,
    q3: vec4<f32>,
    q4: vec4<f32>,
};

const F_MASK: u32 = 1u;
const F_MASK_TEX: u32 = 2u;
const F_TEX: u32 = 4u;
const F_GRADIENT: u32 = 8u;
const F_KNOCKOUT: u32 = 16u;
const F_NO_LAYER: u32 = 32u;
const F_OUTLINE: u32 = 131072u;
const F_VECTOR: u32 = 64u;
const F_ATOP: u32 = 128u;
const F_GATE: u32 = 256u;
const F_REL: u32 = 512u;
const F_STROKE_OUT: u32 = 1024u;
const F_FIRST: u32 = 2048u;
const F_CHANNELS: u32 = 4096u;
const F_LAB: u32 = 65536u;
const F_HDR: u32 = 262144u;
const F_QUANT: u32 = 32768u;
const F_ADD_DIFF: u32 = 16384u;
const F_BLEND_IF: u32 = 524288u;
const F_TEXT_GAMMA: u32 = 8192u;
const F_LAYER_SHAPE: u32 = 1048576u;

@group(0) @binding(0) var<uniform> chunk: Chunk;
@group(0) @binding(1) var<uniform> op: Op;
@group(1) @binding(0) var tex_a: texture_2d<f32>;
@group(1) @binding(1) var tex_b: texture_2d<f32>;
@group(1) @binding(2) var layer_tex: texture_2d<f32>;
@group(1) @binding(3) var mask_tex: texture_2d<f32>;
@group(1) @binding(4) var lut_tex: texture_2d<f32>;
@group(1) @binding(5) var tex_c: texture_2d<f32>;
@group(1) @binding(6) var map_tex: texture_2d<f32>;
@group(1) @binding(7) var pat_tex: texture_2d<f32>;

struct VOut { @builtin(position) pos: vec4<f32> };

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    let uv = vec2(f32((vi << 1u) & 2u), f32(vi & 2u));
    return VOut(vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0));
}

fn local(pos: vec4<f32>) -> vec2<i32> { return vec2<i32>(floor(pos.xy)); }
fn doc_px(p: vec2<i32>) -> vec2<i32> { return chunk.origin + p; }

fn inside(p: vec2<i32>, size: vec2<i32>) -> bool {
    return p.x >= 0 && p.y >= 0 && p.x < size.x && p.y < size.y;
}

fn round_half_up(x: f32) -> f32 { return floor(x + 0.5); }
fn rem_euclid(x: f32, m: f32) -> f32 { return x - floor(x / m) * m; }

fn mask_value(d: vec2<i32>) -> f32 {
    if ((op.flags & F_MASK) == 0u) { return 1.0; }
    var v = op.mask_default;
    if ((op.flags & F_MASK_TEX) != 0u) {
        let m = d - op.mask_origin;
        if (inside(m, op.mask_size)) { v = textureLoad(mask_tex, m, 0).r; }
    }
    return 1.0 - op.mask_density * (1.0 - v);
}

fn lut(row: i32, v: f32) -> f32 {
    let x = clamp(v, 0.0, 1.0) * 4095.0;
    let i = i32(floor(x));
    let j = min(i + 1, 4095);
    let f = x - f32(i);
    return textureLoad(lut_tex, vec2(i, row), 0).r * (1.0 - f) + textureLoad(lut_tex, vec2(j, row), 0).r * f;
}

const F32_MAX: f32 = 0x1.fffffep+127f;

// Mode indices the shader tests by name (gpu::plan::lower::mode_index: BlendMode order). Modes
// 0..33 are Photoshop's and Paint.NET's (photocraft_color::blend::channel); the Nuke Merge
// operations start at NUKE_BASE (photocraft_color::blend::nuke_channel / nuke_composite).
const M_NORMAL: i32 = 1;
const M_DISSOLVE: i32 = 2;
const NUKE_BASE: i32 = 34;

// ---- Photoshop and Paint.NET modes (photocraft_color::blend, 32-bit values kept: f32::MAX) ----

fn color_burn(cb: f32, cs: f32) -> f32 {
    if (cb >= 1.0) { return 1.0; }
    if (cs <= 0.0) { return 0.0; }
    return 1.0 - min((1.0 - cb) / cs, 1.0);
}
fn color_dodge(cb: f32, cs: f32) -> f32 {
    if (cb <= 0.0) { return 0.0; }
    if (cs >= 1.0) { return 1.0; }
    return min(cb / (1.0 - cs), 1.0);
}
fn hard_light(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) { return cb * 2.0 * cs; }
    let s = 2.0 * cs - 1.0;
    return cb + s - cb * s;
}
// The W3C curve, as Photoshop: D(cb) is the cubic below a quarter, sqrt above it (#8804).
fn soft_light_ps(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) { return 2.0 * cb * cs + cb * cb * (1.0 - 2.0 * cs); }
    let b = max(cb, 0.0);
    var d = sqrt(b);
    if (b <= 0.25) { d = ((16.0 * b - 12.0) * b + 4.0) * b; }
    return 2.0 * cb * (1.0 - cs) + d * (2.0 * cs - 1.0);
}
// Vivid Light as Hard Mix thresholds it (backdrop extremes within 1e-4 of 0 / 1 count as exact).
fn vivid_light_hard_mix(cb: f32, cs: f32) -> f32 {
    let edge = 1e-4;
    if (cs <= 0.5) {
        if (cb >= 1.0 - edge) { return 1.0; }
        if (cs <= 0.0) { return 0.0; }
        return 1.0 - min((1.0 - cb) / (2.0 * cs), 1.0);
    }
    if (cb <= edge) { return 0.0; }
    if (cs >= 1.0) { return 1.0; }
    return min(cb / (2.0 * (1.0 - cs)), 1.0);
}

fn blend_channel(mode: i32, cb: f32, cs: f32) -> f32 {
    if (mode >= NUKE_BASE) { return nuke_channel(mode, cb, cs); }
    switch mode {
        case 3: { return min(cb, cs); }                         // Darken
        case 4: { return cb * cs; }                             // Multiply
        case 5: { return color_burn(cb, cs); }                  // Color Burn
        case 6: { return max(cb + cs - 1.0, 0.0); }             // Linear Burn
        case 8: { return max(cb, cs); }                         // Lighten
        case 9: { return cb + cs - cb * cs; }                   // Screen
        case 10: { return color_dodge(cb, cs); }                // Color Dodge
        case 11: { return min(cb + cs, F32_MAX); }              // Linear Dodge (Add)
        case 13: { return hard_light(cs, cb); }                 // Overlay
        case 14: { return soft_light_ps(cb, cs); }              // Soft Light
        case 15: { return hard_light(cb, cs); }                 // Hard Light
        case 16: {                                              // Vivid Light
            if (cs <= 0.0) { return 0.0; }
            if (cs >= 1.0) { return 1.0; }
            if (cs <= 0.5) { return color_burn(cb, 2.0 * cs); }
            return color_dodge(cb, 2.0 * (cs - 0.5));
        }
        case 17: { return clamp(cb + 2.0 * cs - 1.0, 0.0, 1.0); } // Linear Light
        case 18: {                                              // Pin Light
            if (cs <= 0.5) { return min(cb, 2.0 * cs); }
            return max(cb, 2.0 * cs - 1.0);
        }
        case 19: { return select(0.0, 1.0, vivid_light_hard_mix(cb, cs) >= 0.5 - 1e-6); } // Hard Mix
        case 20: { return abs(cb - cs); }                       // Difference
        case 21: { return cb + cs - 2.0 * cb * cs; }            // Exclusion
        case 22: { return max(cb - cs, 0.0); }                  // Subtract
        case 23: {                                              // Divide
            if (cs <= 0.0) { return select(1.0, 0.0, cb <= 0.0); }
            return min(cb / cs, F32_MAX);
        }
        case 28: {                                              // Reflect
            if (cs >= 1.0) { return 1.0; }
            return min(cb * cb / (1.0 - cs), 1.0);
        }
        case 29: {                                              // Glow
            if (cb >= 1.0) { return 1.0; }
            return min(cs * cs / (1.0 - cb), 1.0);
        }
        case 30: { return 1.0 - abs(1.0 - cb - cs); }           // Negation
        case 31: {                                              // XOR (8-bit channel operation)
            let b = u32(floor(clamp(cb, 0.0, 1.0) * 255.0 + 0.5));
            let s = u32(floor(clamp(cs, 0.0, 1.0) * 255.0 + 0.5));
            return f32(b ^ s) / 255.0;
        }
        case 32: {                                              // Color Burn (Paint.NET)
            if (cs <= 0.0) { return 0.0; }
            return max(1.0 - (1.0 - cb) / cs, 0.0);
        }
        case 33: {                                              // Color Dodge (Paint.NET)
            if (cs >= 1.0) { return 1.0; }
            return min(cb / (1.0 - cs), 1.0);
        }
        default: { return cs; }                                 // Normal, Dissolve, Pass Through
    }
}

// ---- Nuke Merge operations (A = source, B = backdrop; Nuke's 32-bit maths, nothing clamped) ----

fn nuke_screen(a: f32, b: f32) -> f32 {
    if (a >= 0.0 && a <= 1.0 && b >= 0.0 && b <= 1.0) { return a + b - a * b; }
    return max(a, b);
}

// Nuke overlay / hard-light core (photocraft_color::blend::nuke_light): multiply below 0.5 of
// the deciding input x, screen(2x - 1, y) above it. No clamping, so float values above 1 survive.
fn nuke_light(x: f32, y: f32) -> f32 {
    if (x < 0.5) { return 2.0 * x * y; }
    return nuke_screen(2.0 * x - 1.0, y);
}
fn nuke_overlay(a: f32, b: f32) -> f32 { return nuke_light(b, a); }    // A over B, decided by B
fn nuke_hard_light(a: f32, b: f32) -> f32 { return nuke_light(a, b); } // decided by A

fn nuke_soft_light(a: f32, b: f32) -> f32 {
    let ac = clamp(a, 0.0, 1.0);
    let bc = clamp(b, 0.0, 1.0);
    if (ac * bc < 1.0) { return bc * (2.0 * ac + (bc * (1.0 - ac * 2.0))); }
    return 2.0 * bc * ac;
}

fn nuke_divide(a: f32, b: f32) -> f32 {
    if (abs(b) <= 1e-7) { return 0.0; }
    return a / b;
}

fn nuke_channel(mode: i32, cb: f32, cs: f32) -> f32 {
    switch mode {
        case 34, 38: { return min(cb, cs); }             // min / in
        case 35: { return cb * cs; }                     // multiply
        case 60: { return cb - cs; }                     // from
        case 37, 61: { return cb; }                      // under / atop (RGBA in nuke_composite)
        case 39, 43: { return max(cb, cs); }             // max / out
        case 40: { return nuke_screen(cs, cb); }         // screen
        case 41: {                                       // color-dodge
            let denom = 1.0 - cs;
            if (abs(denom) <= 1e-6) { return cb; }
            return cb / denom;
        }
        case 36: {                                       // color-burn
            if (abs(cs) <= 1e-6) { return 0.0; }
            return 1.0 - (1.0 - cb) / cs;
        }
        case 42: { return cs + cb; }                     // plus
        case 44: { return nuke_overlay(cs, cb); }        // overlay
        case 45: { return nuke_soft_light(cs, cb); }     // soft-light
        case 46: { return nuke_hard_light(cs, cb); }     // hard-light
        case 47: {                                       // geometric
            let sum = cs + cb;
            if (abs(sum) <= 1e-7) { return 0.0; }
            return (2.0 * cs * cb) / sum;
        }
        case 48: { return sqrt(cs * cs + cb * cb); }     // hypot
        case 51: { return abs(cs - cb); }                // difference
        case 52: { return cs + cb - 2.0 * cs * cb; }     // exclusion
        case 53: { return cs - cb; }                     // minus
        case 54: { return nuke_divide(cs, cb); }         // divide
        case 56: { return 0.5 * (cs + cb); }             // average
        default: { return cs; }
    }
}

// ---- non-separable helpers (W3C Compositing Level 1; photocraft_color::blend::lum) ----

fn lum(c: vec3<f32>) -> f32 { return 0.3 * c.r + 0.59 * c.g + 0.11 * c.b; }

// photocraft_color::blend::clip_color: towards grey until within 0..top; a luminosity outside
// that range has no in-gamut colour (and max − lum is rounding noise), so it gives its grey.
fn clip_color(c: vec3<f32>, top: f32) -> vec3<f32> {
    let l = lum(c);
    let n = min(min(c.r, c.g), c.b);
    let x = max(max(c.r, c.g), c.b);
    var out = c;
    if (n < 0.0) {
        let d = l - n;
        if (l > 0.0 && d > 0.0) { out = l + (out - l) * l / d; } else { out = vec3(l); }
    }
    if (x > top) {
        let d = x - l;
        if (l < top && d > 0.0) { out = l + (out - l) * (top - l) / d; } else { out = vec3(l); }
    }
    return out;
}

fn set_lum(c: vec3<f32>, l: f32, top: f32) -> vec3<f32> { return clip_color(c + (l - lum(c)), top); }
fn sat(c: vec3<f32>) -> f32 { return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b); }
fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    if (mx > mn) { return (c - mn) * s / (mx - mn); }
    return vec3(0.0);
}

// photocraft_color::blend::blend_rgb_hdr: 32-bit values above 1 are kept (top f32::MAX).
fn blend_rgb(mode: i32, cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    let top = F32_MAX;
    switch mode {
        case 24: { return set_lum(set_sat(cs, sat(cb)), lum(cb), top); } // Hue
        case 25: { return set_lum(set_sat(cb, sat(cs)), lum(cb), top); } // Saturation
        case 26: { return set_lum(cs, lum(cb), top); }                  // Color
        case 27: { return set_lum(cb, lum(cs), top); }                  // Luminosity
        case 7: { return select(cb, cs, lum(cs) < lum(cb)); }          // Darker Color
        case 12: { return select(cb, cs, lum(cs) > lum(cb)); }         // Lighter Color
        default: {
            return vec3(blend_channel(mode, cb.r, cs.r), blend_channel(mode, cb.g, cs.g), blend_channel(mode, cb.b, cs.b));
        }
    }
}

// photocraft_color::blend::composite: the W3C general formula for Photoshop's and Paint.NET's
// modes; Nuke's premultiplied maths for the Nuke Merge operations.
fn composite(mode: i32, b: vec4<f32>, s: vec4<f32>, opacity: f32) -> vec4<f32> {
    if (mode >= NUKE_BASE) { return nuke_composite(mode, b, s, opacity); }
    let ab = b.a;
    let as_ = s.a * opacity;
    if (as_ <= 0.0) { return b; }
    let bl = blend_rgb(mode, b.rgb, s.rgb);
    let ao = as_ + ab * (1.0 - as_);
    if (ao <= 0.0) { return vec4(0.0); }
    let rgb = ((1.0 - as_) * ab * b.rgb + (1.0 - ab) * as_ * s.rgb + as_ * ab * bl) / ao;
    return vec4(rgb, ao);
}

fn nuke_composite(mode: i32, b_in: vec4<f32>, s_in: vec4<f32>, opacity: f32) -> vec4<f32> {
    let mix = clamp(opacity, 0.0, 1.0);
    if (mix <= 0.0) { return b_in; }

    let ba = b_in.a;
    let b = vec4(b_in.rgb * ba, ba);
    let sa = s_in.a;
    let a = vec4(s_in.rgb * sa, sa);

    // in, mask and min change the backdrop even where the source is empty.
    if (sa <= 0.0 && mode != 38 && mode != 49 && mode != 34) {
        return b_in;
    }

    var m = vec4(0.0);
    switch mode {
        case 55: { // matte
            m = vec4(a.rgb + b.rgb * (1.0 - sa), sa + ba * (1.0 - sa));
        }
        case 37: { // under
            m = vec4(a.rgb * (1.0 - ba) + b.rgb, sa * (1.0 - ba) + ba);
        }
        case 42: { // plus
            m = vec4(a.rgb + b.rgb, min(sa + ba, 1.0));
        }
        case 53: { // minus
            m = vec4(a.rgb - b.rgb, clamp(sa - ba, 0.0, 1.0));
        }
        case 60: { // from
            m = vec4(b.rgb - a.rgb, clamp(ba - sa, 0.0, 1.0));
        }
        case 35: { // multiply
            m = vec4(a.rgb * b.rgb + b.rgb * (1.0 - sa), sa + ba * (1.0 - sa));
        }
        case 40: { // screen
            m = vec4(nuke_screen(a.r, b.r), nuke_screen(a.g, b.g), nuke_screen(a.b, b.b), nuke_screen(sa, ba));
        }
        case 34: { // min
            m = vec4(min(a.rgb, b.rgb), min(sa, ba));
        }
        case 39: { // max
            m = vec4(max(a.rgb, b.rgb), max(sa, ba));
        }
        case 51: { // difference
            m = vec4(abs(a.rgb - b.rgb), abs(sa - ba));
        }
        case 52: { // exclusion
            m = vec4(a.rgb + b.rgb - 2.0 * a.rgb * b.rgb, sa + ba - 2.0 * sa * ba);
        }
        case 54: { // divide
            m = vec4(nuke_divide(a.r, b.r), nuke_divide(a.g, b.g), nuke_divide(a.b, b.b), sa + ba * (1.0 - sa));
        }
        case 48: { // hypot
            m = vec4(sqrt(a.rgb * a.rgb + b.rgb * b.rgb), min(sqrt(sa * sa + ba * ba), 1.0));
        }
        case 38: { // in
            m = vec4(a.rgb * ba, sa * ba);
        }
        case 43: { // out
            m = vec4(a.rgb * (1.0 - ba), sa * (1.0 - ba));
        }
        case 49: { // mask
            m = vec4(b.rgb * sa, ba * sa);
        }
        case 50: { // stencil
            m = vec4(b.rgb * (1.0 - sa), ba * (1.0 - sa));
        }
        case 61: { // atop
            m = vec4(a.rgb * ba + b.rgb * (1.0 - sa), ba);
        }
        case 36: { // color-burn
            m = vec4(
                select(1.0 - (1.0 - b.r) / a.r, 0.0, a.r <= 1e-6),
                select(1.0 - (1.0 - b.g) / a.g, 0.0, a.g <= 1e-6),
                select(1.0 - (1.0 - b.b) / a.b, 0.0, a.b <= 1e-6),
                sa + ba * (1.0 - sa)
            );
        }
        case 41: { // color-dodge
            m = vec4(
                select(b.r / (1.0 - a.r), b.r, 1.0 - a.r <= 1e-6),
                select(b.g / (1.0 - a.g), b.g, 1.0 - a.g <= 1e-6),
                select(b.b / (1.0 - a.b), b.b, 1.0 - a.b <= 1e-6),
                sa + ba * (1.0 - sa)
            );
        }
        case 59: { // xor
            m = vec4(a.rgb * (1.0 - ba) + b.rgb * (1.0 - sa), clamp(sa * (1.0 - ba) + ba * (1.0 - sa), 0.0, 1.0));
        }
        case 57: { // conjoint-over
            if (sa > ba || ba <= 1e-7) {
                m = vec4(a.rgb, sa);
            } else {
                let k = 1.0 - sa / ba;
                m = vec4(a.rgb + b.rgb * k, sa + ba * k);
            }
        }
        case 58: { // disjoint-over
            if (sa + ba < 1.0 || ba <= 1e-7) {
                m = vec4(a.rgb + b.rgb, min(sa + ba, 1.0));
            } else {
                let k = (1.0 - sa) / ba;
                m = vec4(a.rgb + b.rgb * k, 1.0);
            }
        }
        default: { // overlay, soft-light, hard-light, geometric, average: the general formula
            let blended = blend_rgb(mode, b_in.rgb, s_in.rgb);
            let ao = sa + ba * (1.0 - sa);
            m = vec4((1.0 - sa) * b.rgb + (1.0 - ba) * a.rgb + sa * ba * blended, ao);
        }
    }

    let out_pm = vec4(b.rgb + (m.rgb - b.rgb) * mix, clamp(b.a + (m.a - b.a) * mix, 0.0, 1.0));

    if (out_pm.a <= 1e-7) { return vec4(0.0); }
    return vec4(out_pm.rgb / out_pm.a, out_pm.a);
}

fn text_enc(c: vec3<f32>, g: f32) -> vec3<f32> { return c; }
fn text_dec(v: vec3<f32>, g: f32) -> vec3<f32> { return v; }

fn composite_g(mode: i32, b: vec4<f32>, s: vec4<f32>, opacity: f32, gamma_on: bool) -> vec4<f32> {
    return composite(mode, b, s, opacity);
}

fn fill_is_special(mode: i32) -> bool { return false; }

fn composite_fill(mode: i32, b: vec4<f32>, s: vec4<f32>, opacity: f32, fill: f32, gamma_on: bool) -> vec4<f32> {
    return composite(mode, b, s, opacity * clamp(fill, 0.0, 1.0));
}

fn composite_normal_fill(b: vec4<f32>, s: vec4<f32>, opacity: f32, fill: f32) -> vec4<f32> {
    let ab = b.a;
    let over = s.a * opacity * clamp(fill, 0.0, 1.0);
    if (over <= 0.0) { return b; }
    let ao = ab + (1.0 - ab) * over;
    if (ao <= 0.0) { return vec4(0.0); }
    let wb = ab * (1.0 - over) / ao;
    let wbs = ab * over / ao;
    let ws = (1.0 - ab) * over / ao;
    return vec4(wb * b.rgb + wbs * s.rgb + ws * s.rgb, ao);
}

const D50: vec3<f32> = vec3(0.96422, 1.0, 0.82521);
fn lab_f(t: f32) -> f32 {
    if (t > 0.008856452) { return pow(t, 1.0 / 3.0); }
    return t / 0.12841855 + 0.13793103;
}
fn lab_finv(t: f32) -> f32 {
    if (t > 0.20689656) { return t * t * t; }
    return 0.12841855 * (t - 0.13793103);
}
fn srgb_to_lab(c: vec3<f32>) -> vec3<f32> { return c; }
fn lab_to_srgb(lab: vec3<f32>) -> vec3<f32> { return lab; }

// Dither for 8-bit output: a hash of the document pixel in 0..1.
fn dither_noise(d: vec2<i32>) -> f32 {
    var h = (bitcast<u32>(d.x) * 0x9E3779B1u) ^ (bitcast<u32>(d.y) * 0x85EBCA77u);
    h = h ^ (h >> 15u);
    h = h * 0x2C1B3C6Du;
    h = h ^ (h >> 12u);
    return f32(h & 0xFFFFu) / 65535.0;
}

fn dissolve_noise(d: vec2<i32>) -> f32 {
    var h = (bitcast<u32>(d.x) * 0x8da6b343u) ^ (bitcast<u32>(d.y) * 0xd8163841u) ^ 0x9e3779b9u;
    h = h ^ (h >> 15u);
    h = h * 0x2c1b3c6du;
    h = h ^ (h >> 12u);
    return f32(h & 0xffffu) / 65536.0;
}

fn srgb_to_linear(v: f32) -> f32 {
    if (v <= 0.04045) { return v / 12.92; }
    return pow((v + 0.055) / 1.055, 2.4);
}
fn linear_to_srgb(v: f32) -> f32 {
    if (v <= 0.0031308) { return v * 12.92; }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}
fn t_decode(v: f32, g: f32) -> f32 {
    if (g <= 0.0) { return srgb_to_linear(max(v, 0.0)); }
    return pow(max(v, 0.0), g);
}
fn t_encode(v: f32, g: f32) -> f32 {
    if (g <= 0.0) { return linear_to_srgb(max(v, 0.0)); }
    return pow(max(v, 0.0), 1.0 / g);
}

fn vibrance_lin(c: vec3<f32>, v: f32, b0: vec4<f32>, b1: vec3<f32>) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    if (mx <= 0.0 || mx - mn <= 1e-9) { return c; }
    let t = 1.0 - mn / mx;
    let shape = t * (1.0 - t) * (2.0 - t + t * t) * mx * max(1.0 - mx, 0.0);
    var t2: f32;
    var v2: f32;
    if (v < 0.0) {
        let a = -v;
        t2 = t * (1.0 - a / 4.0) * (1.0 - a + a * t * (1.0 + t) / 2.0);
        v2 = mx - a * shape;
    } else {
        let d = mx - mn;
        var h: f32;
        if (mx == c.r) { h = rem_euclid((c.g - c.b) / d, 6.0); }
        else if (mx == c.g) { h = (c.b - c.r) / d + 2.0; }
        else { h = (c.r - c.g) / d + 4.0; }
        h = h * 60.0;
        var w = 0.0;
        if (h <= 30.0) { w = 1.0; }
        else if (h < 45.0) { w = (45.0 - h) / 15.0; }
        else if (h >= 300.0) { w = (h - 300.0) / 60.0; }
        let a = v * (1.0 - w) + 0.64 * pow(v, 1.6) * w;
        let boost = (b0.x * a + b0.y * a * a) * pow(t, b0.z) * pow(1.0 - t, b0.w)
            * exp((b1.x + b1.y * a + b1.z * (1.0 - t)) * max(1.0 - mx, 0.0));
        t2 = min(t * (1.0 + boost), 1.0);
        v2 = mx + v / 4.0 * shape;
    }
    let m2 = v2 * (1.0 - t2);
    let k = (v2 - m2) / (mx - mn);
    return vec3(m2) + (c - vec3(mn)) * k;
}

fn rgb_to_hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    let l = (mx + mn) / 2.0;
    if (abs(mx - mn) < 1e-7) { return vec3(0.0, 0.0, l); }
    let d = mx - mn;
    var s: f32;
    if (l > 0.5) { s = d / (2.0 - mx - mn); } else { s = d / (mx + mn); }
    var h: f32;
    if (mx == c.r) {
        h = rem_euclid((c.g - c.b) / d, 6.0);
    } else if (mx == c.g) {
        h = (c.b - c.r) / d + 2.0;
    } else {
        h = (c.r - c.g) / d + 4.0;
    }
    return vec3(h / 6.0, s, l);
}

fn hue_ch(p: f32, q: f32, t0: f32) -> f32 {
    let t = rem_euclid(t0, 1.0);
    if (t < 1.0 / 6.0) { return p + (q - p) * 6.0 * t; }
    if (t < 0.5) { return q; }
    if (t < 2.0 / 3.0) { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
    return p;
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> vec3<f32> {
    if (s <= 0.0) { return vec3(l); }
    var q: f32;
    if (l < 0.5) { q = l * (1.0 + s); } else { q = l + s - l * s; }
    let p = 2.0 * l - q;
    return vec3(hue_ch(p, q, h + 1.0 / 3.0), hue_ch(p, q, h), hue_ch(p, q, h - 1.0 / 3.0));
}

fn posterize(v: f32, n: f32) -> f32 {
    let x = round_half_up(clamp(v, 0.0, 1.0) * 255.0);
    let bin = min(floor(x * n / 256.0), n - 1.0);
    return floor(bin * 255.0 / (n - 1.0)) / 255.0;
}

fn posterize16(v: f32, n: f32) -> f32 {
    let x = floor(clamp(v, 0.0, 1.0) * 32768.0 + 0.5);
    let bin = min(floor(x * n / 32768.0), n - 1.0);
    let step = floor(bin * 32768.0 / (n - 1.0));
    return floor((step * 255.0 + 16384.0) / 32768.0) / 255.0;
}

var<private> adj_px: vec2<i32>;

fn lut_at(i: i32) -> f32 { return textureLoad(lut_tex, vec2(i % 4096, i / 4096), 0).r; }

fn lut3(n: i32, r: i32, g: i32, b: i32) -> vec3<f32> {
    let i = ((b * n + g) * n + r) * 3;
    return vec3(lut_at(i), lut_at(i + 1), lut_at(i + 2));
}

fn bayer4(p: vec2<i32>) -> f32 {
    var m = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
    return (m[(p.y & 3) * 4 + (p.x & 3)] + 0.5) / 16.0 - 0.5;
}

fn color_lookup(c: vec3<f32>, n: i32, tetra: bool) -> vec3<f32> {
    let m = f32(n - 1);
    let pos = clamp(c, vec3(0.0), vec3(1.0)) * m;
    let i0 = min(vec3<i32>(floor(pos)), vec3(n - 2));
    let f = pos - vec3<f32>(i0);
    let r = i0.x;
    let g = i0.y;
    let b = i0.z;
    if (tetra) {
        let c000 = lut3(n, r, g, b);
        let c111 = lut3(n, r + 1, g + 1, b + 1);
        if (f.x > f.y) {
            if (f.y > f.z) { return (1.0 - f.x) * c000 + (f.x - f.y) * lut3(n, r + 1, g, b) + (f.y - f.z) * lut3(n, r + 1, g + 1, b) + f.z * c111; } 
            else if (f.x > f.z) { return (1.0 - f.x) * c000 + (f.x - f.z) * lut3(n, r + 1, g, b) + (f.z - f.y) * lut3(n, r + 1, g, b + 1) + f.y * c111; }
            return (1.0 - f.z) * c000 + (f.z - f.x) * lut3(n, r, g, b + 1) + (f.x - f.y) * lut3(n, r + 1, g, b + 1) + f.y * c111;
        }
        if (f.z > f.y) { return (1.0 - f.z) * c000 + (f.z - f.y) * lut3(n, r, g, b + 1) + (f.y - f.x) * lut3(n, r, g + 1, b + 1) + f.x * c111; } 
        else if (f.z > f.x) { return (1.0 - f.y) * c000 + (f.y - f.z) * lut3(n, r, g + 1, b) + (f.z - f.x) * lut3(n, r, g + 1, b + 1) + f.x * c111; }
        return (1.0 - f.y) * c000 + (f.y - f.x) * lut3(n, r, g + 1, b) + (f.x - f.z) * lut3(n, r + 1, g + 1, b) + f.z * c111;
    }
    let c00 = mix(lut3(n, r, g, b), lut3(n, r + 1, g, b), f.x);
    let c10 = mix(lut3(n, r, g + 1, b), lut3(n, r + 1, g + 1, b), f.x);
    let c01 = mix(lut3(n, r, g, b + 1), lut3(n, r + 1, g, b + 1), f.x);
    let c11 = mix(lut3(n, r, g + 1, b + 1), lut3(n, r + 1, g + 1, b + 1), f.x);
    return mix(mix(c00, c10, f.y), mix(c01, c11, f.y), f.z);
}

fn selective_row(r: i32, w: f32, c: vec3<f32>, relative: bool) -> vec3<f32> {
    let k = lut_at(r * 4 + 3) / 100.0;
    var d = vec3(lut_at(r * 4), lut_at(r * 4 + 1), lut_at(r * 4 + 2)) / 100.0 + k;
    if (relative) { d = d * (1.0 - c); } else { d = d * select(min(2.0 * (1.0 - c), vec3(1.0)), min(2.0 * c, vec3(1.0)), d > vec3(0.0)); }
    return select(vec3(0.0), d * w, w > 0.0);
}

fn selective_color(c: vec3<f32>, relative: bool) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    let md = c.r + c.g + c.b - mx - mn;
    var delta = selective_row(0, select(0.0, mx - md, c.r >= mx), c, relative);
    delta += selective_row(1, select(0.0, md - mn, c.b <= mn), c, relative);
    delta += selective_row(2, select(0.0, mx - md, c.g >= mx), c, relative);
    delta += selective_row(3, select(0.0, md - mn, c.r <= mn), c, relative);
    delta += selective_row(4, select(0.0, mx - md, c.b >= mx), c, relative);
    delta += selective_row(5, select(0.0, md - mn, c.g <= mn), c, relative);
    delta += selective_row(6, max((mn - 0.5) * 2.0, 0.0), c, relative);
    delta += selective_row(7, clamp(1.0 - abs(mx - 0.5) - abs(mn - 0.5), 0.0, 1.0), c, relative);
    delta += selective_row(8, max((0.5 - mx) * 2.0, 0.0), c, relative);
    return clamp(c - delta, vec3(0.0), vec3(1.0));
}

fn exposure(v: f32, p: vec4<f32>) -> f32 {
    let lin = v * p.x + p.y;
    if (p.z == 1.0) { return lin; }
    return sign(lin) * pow(abs(lin), 1.0 / p.z);
}

// Nuke Grade (compose::adjust::GradeCoefficients): p0 = a, p1 = b, p2 = 1/gamma per channel,
// p3 = (black clamp, white clamp, tone-curve exponent, float document). Integer documents are
// decoded to linear light first and re-encoded after, as on the CPU.
fn grade(c: vec3<f32>, p0: vec4<f32>, p1: vec4<f32>, p2: vec4<f32>, p3: vec4<f32>) -> vec3<f32> {
    let float_doc = p3.w > 0.5;
    var x = c;
    if (!float_doc) { x = vec3(t_decode(c.r, p3.z), t_decode(c.g, p3.z), t_decode(c.b, p3.z)); }
    var v = p0.xyz * x + p1.xyz;
    // Gamma on positive values only (negatives pass through, as in Nuke).
    if (p2.x != 1.0 && v.x > 0.0) { v.x = pow(v.x, p2.x); }
    if (p2.y != 1.0 && v.y > 0.0) { v.y = pow(v.y, p2.y); }
    if (p2.z != 1.0 && v.z > 0.0) { v.z = pow(v.z, p2.z); }
    if (p3.x > 0.5) { v = max(v, vec3(0.0)); }
    if (p3.y > 0.5) { v = min(v, vec3(1.0)); }
    if (!float_doc) {
        v = max(v, vec3(0.0));
        v = clamp(vec3(t_encode(v.r, p3.z), t_encode(v.g, p3.z), t_encode(v.b, p3.z)), vec3(0.0), vec3(1.0));
    }
    return v;
}

fn levels_float(record: i32, v: f32) -> f32 {
    let base = record * 5;
    let t = (v - lut_at(base)) / max(lut_at(base + 1) - lut_at(base), 0.000001);
    let mapped = sign(t) * pow(abs(t), 1.0 / lut_at(base + 2));
    return lut_at(base + 3) + mapped * (lut_at(base + 4) - lut_at(base + 3));
}

fn tone_channel(row: i32, v: f32, identity: f32, extrapolate: f32) -> f32 {
    if (identity > 0.5) { return v; }
    if (extrapolate > 0.5 && abs(v) <= 3.0e38) {
        if (v < 0.0) { return textureLoad(lut_tex, vec2(0, row), 0).r + v * (textureLoad(lut_tex, vec2(16, row), 0).r - textureLoad(lut_tex, vec2(0, row), 0).r) * (4095.0 / 16.0); }
        if (v > 1.0) { return textureLoad(lut_tex, vec2(4095, row), 0).r + (v - 1.0) * (textureLoad(lut_tex, vec2(4095, row), 0).r - textureLoad(lut_tex, vec2(4079, row), 0).r) * (4095.0 / 16.0); }
    }
    return lut(row, v);
}

// compose::adjust::lookup_sample: a row tabulated over 0..range, held or continued along the
// end slopes (over 16 table steps) outside it.
fn lookup_channel(row: i32, v: f32, range: f32, hold: bool) -> f32 {
    let t = v / range;
    if (!hold && abs(v) <= 3.0e38) {
        let per_unit = 4095.0 / (16.0 * range);
        let first = textureLoad(lut_tex, vec2(0, row), 0).r;
        let last = textureLoad(lut_tex, vec2(4095, row), 0).r;
        if (t < 0.0) { return first + v * (textureLoad(lut_tex, vec2(16, row), 0).r - first) * per_unit; }
        if (t > 1.0) { return last + (v - range) * (last - textureLoad(lut_tex, vec2(4079, row), 0).r) * per_unit; }
    }
    return lut(row, t);
}

fn adjust(c: vec3<f32>) -> vec3<f32> {
    let hdr = (op.flags & F_HDR) != 0u;
    let p0 = op.p0;
    let p1 = op.p1;
    let p2 = op.p2;
    let p3 = op.p3;
    switch op.kind {
        case 1: { return 1.0 - c; }
        case 2: {
            var white = lum(c) * 255.0 >= p0.x;
            if p0.y > 0.5 {
                let q = select(32768.0, 255.0, p0.y < 1.5);
                let v = floor(clamp(c, vec3(0.0), vec3(1.0)) * q + 0.5);
                let s = 30.0 * v.r + 59.0 * v.g + 11.0 * v.b;
                white = select(s >= 100.0 * p0.z, s > 100.0 * p0.x - 50.0, p0.y < 1.5);
            }
            return vec3(select(0.0, 1.0, white));
        }
        case 3: {
            if p0.y > 0.5 { return vec3(posterize16(c.r, p0.x), posterize16(c.g, p0.x), posterize16(c.b, p0.x)); }
            return vec3(posterize(c.r, p0.x), posterize(c.g, p0.x), posterize(c.b, p0.x));
        }
        case 4: {
            let v = (c + p0.x - p0.z) * p0.y + p0.z + p0.w;
            if (hdr) { return v; }
            return clamp(v, vec3(0.0), vec3(1.0));
        }
        case 6: { return vec3(exposure(c.r, p0), exposure(c.g, p0), exposure(c.b, p0)); }
        case 18: { return grade(c, p0, p1, p2, p3); }
        case 19: {
            // Nuke ColorLookup-style Curves: p0 = (range, hold ends, float document).
            let hold = p0.y > 0.5;
            let o = vec3(lookup_channel(0, c.r, p0.x, hold), lookup_channel(1, c.g, p0.x, hold), lookup_channel(2, c.b, p0.x, hold));
            if (p0.z > 0.5) { return o; }
            return clamp(o, vec3(0.0), vec3(1.0));
        }
        case 7: { return vec3(tone_channel(0, c.r, p0.x, p0.w), tone_channel(1, c.g, p0.y, p0.w), tone_channel(2, c.b, p0.z, p0.w)); }
        case 17: { return vec3(levels_float(0, levels_float(1, c.r)), levels_float(0, levels_float(2, c.g)), levels_float(0, levels_float(3, c.b))); }
        case 8: {
            let hsl = rgb_to_hsl(c);
            var hh: f32;
            var ss: f32;
            var dh = 0.0;
            var ds = 0.0;
            var dl = 0.0;
            if (p1.x > 0.5) {
                dh = lut(0, hsl.x);
                ds = lut(1, hsl.x);
                dl = lut(2, hsl.x);
            }
            let sat = clamp(p0.y + ds, -1.0, 1.0);
            let light = clamp(p0.z, -1.0, 1.0);
            if (p0.w > 0.5) {
                var ll = hsl.z * (1.0 + light);
                if (light > 0.0) { ll = hsl.z + (1.0 - hsl.z) * light; }
                return hsl_to_rgb(rem_euclid(p0.x, 360.0) / 360.0, min(abs(sat), 1.0), ll);
            }
            var lc = c * (1.0 + light);
            if (light > 0.0) { lc = c + (1.0 - c) * light; }
            let lh = rgb_to_hsl(lc);
            hh = rem_euclid(lh.x + (p0.x + dh) / 360.0, 1.0);
            var rgb: vec3<f32>;
            if (hdr) {
                var k: f32;
                if (sat <= 0.0) {
                    k = 1.0 - floor(-sat * 256.0 + 1e-4) / 256.0;
                } else {
                    let steps = min(u32(floor(sat * 254.0 + 1e-4)), 254u);
                    k = f32(min(65280u / (255u - steps), 32767u)) / 256.0;
                }
                let d = (max(max(lc.r, lc.g), lc.b) - min(min(lc.r, lc.g), lc.b)) * k;
                let lo = lh.z - d / 2.0;
                let hi = lh.z + d / 2.0;
                rgb = vec3(hue_ch(lo, hi, hh + 1.0 / 3.0), hue_ch(lo, hi, hh), hue_ch(lo, hi, hh - 1.0 / 3.0));
            } else {
                if (sat > 0.0) {
                    ss = clamp(lh.y / max(1.0 - sat, 1.0 / 512.0), 0.0, 1.0);
                } else {
                    ss = clamp(lh.y * (1.0 + sat), 0.0, 1.0);
                }
                rgb = hsl_to_rgb(hh, ss, lh.z);
            }
            let rl = clamp(dl, -1.0, 1.0);
            if (rl > 0.0) { rgb = rgb + (vec3(max(rgb.r, max(rgb.g, rgb.b))) - rgb) * rl; } else if (rl < 0.0) { rgb = rgb + (vec3(min(rgb.r, min(rgb.g, rgb.b))) - rgb) * -rl; }
            return rgb;
        }
        case 9: {
            var l = vec3(t_decode(c.r, p0.z), t_decode(c.g, p0.z), t_decode(c.b, p0.z));
            if (p0.x != 0.0) { l = vibrance_lin(l, p0.x, p1, p2.xyz); }
            if (p0.y != 0.0) {
                let g = 0.288 * l.r + 0.712 * l.g;
                l = clamp(vec3(g) + (1.0 + p0.y) * (l - vec3(g)), vec3(0.0), vec3(p0.w));
            }
            l = clamp(l, vec3(0.0), vec3(p0.w));
            return vec3(t_encode(l.r, p0.z), t_encode(l.g, p0.z), t_encode(l.b, p0.z));
        }
        case 10: {
            let lo = select(0.0, -F32_MAX, hdr);
            let hi = select(1.0, F32_MAX, hdr);
            let r = clamp(dot(p0.xyz, c) + p0.w, lo, hi);
            if (p3.x > 0.5) { return vec3(r); }
            return vec3(r, clamp(dot(p1.xyz, c) + p1.w, lo, hi), clamp(dot(p2.xyz, c) + p2.w, lo, hi));
        }
        case 11: {
            if (hdr) { return vec3(dot(p0.xyz, c), dot(p1.xyz, c), dot(p2.xyz, c)); }
            let g = p3.y;
            let lin = vec3(t_decode(c.r, g), t_decode(c.g, g), t_decode(c.b, g));
            var f = vec3(t_encode(dot(p0.xyz, lin), g), t_encode(dot(p1.xyz, lin), g), t_encode(dot(p2.xyz, lin), g));
            if (p3.x > 0.5) { f = set_lum(f, lum(c), 1.0); }
            return clamp(f, vec3(0.0), vec3(1.0));
        }
        case 12: {
            var w = array<f32, 6>(p0.x, p0.y, p0.z, p0.w, p1.x, p1.y);
            let mx = max(c.r, max(c.g, c.b));
            let mn = min(c.r, min(c.g, c.b));
            let mid = c.r + c.g + c.b - mx - mn;
            var primary = 4;
            if (c.r >= c.g && c.r >= c.b) { primary = 0; } else if (c.g >= c.b) { primary = 2; }
            var secondary = 5;
            if (c.b <= c.r && c.b <= c.g) { secondary = 1; } else if (c.r <= c.g) { secondary = 3; }
            let g = clamp(mn + (mid - mn) * w[secondary] / 100.0 + (mx - mid) * w[primary] / 100.0, 0.0, select(1.0, 16.0, hdr));
            if (p1.z > 0.5) {
                if (hdr) { return set_lum(p2.rgb, g, F32_MAX); }
                return clamp(set_lum(p2.rgb, g, 1.0), vec3(0.0), vec3(1.0));
            }
            return vec3(g);
        }
        case 13: {
            var t = lum(c);
            if (p0.x > 0.5) { t = 1.0 - t; }
            var o = vec3(lut(0, t), lut(1, t), lut(2, t));
            if (p0.y > 0.5) { o = clamp(o + bayer4(adj_px) / 255.0, vec3(0.0), vec3(1.0)); }
            return o;
        }
        case 15: { return selective_color(c, p0.x > 0.5); }
        case 16: {
            var o = color_lookup(c, i32(p0.x), p0.y > 0.5);
            if (p0.z > 0.5) { o = clamp(o + bayer4(adj_px) / 255.0, vec3(0.0), vec3(1.0)); }
            return o;
        }
        default: { return c; }
    }
}

fn gradient_t(d: vec2<i32>) -> f32 {
    let w = max(op.p1.z, 1.0);
    let h = max(op.p1.w, 1.0);
    let cx = op.p1.x + w / 2.0 + op.p2.x * w;
    let cy = op.p1.y + h / 2.0 + op.p2.y * h;
    let a = radians(op.p0.x);
    let s = sin(a);
    let c = cos(a);
    let dx = f32(d.x) + 0.5 - cx;
    let dy = f32(d.y) + 0.5 - cy;
    let along = dx * c - dy * s;
    let across = dx * s + dy * c;
    let len = max(sqrt((c * w) * (c * w) + (s * h) * (s * h)), 1.0) * max(op.p0.y, 1e-3);
    let chord = max(min(w / max(abs(c), 1e-6), h / max(abs(s), 1e-6)), 1.0) * max(op.p0.y, 1e-3);
    let corner = 0.5 * (c - s);
    var t: f32;
    switch i32(op.p0.w) {
        case 1: { t = sqrt(dx * dx + dy * dy) / (len / 2.0); }
        case 2: { t = rem_euclid((a - atan2(-dy, dx)) / 6.28318530718, 1.0); }
        case 3: { t = abs((along - corner) / (chord / 2.0)); }
        case 4: { t = (abs(along) + abs(across)) / (len / 2.0); }
        default: { t = (along - corner) / chord + 0.5; }
    }
    t = clamp(t, 0.0, 1.0);
    if (op.p0.z > 0.5) { t = 1.0 - t; }
    return t;
}

fn layer_texel(d: vec2<i32>) -> vec4<f32> {
    if ((op.flags & F_GRADIENT) != 0u) {
        let t = gradient_t(d);
        var c = vec4(lut(0, t), lut(1, t), lut(2, t), lut(3, t));
        if (op.p2.w > 0.5) {
            var rgb = clamp(c.rgb + (dither_noise(d) - 0.5) / 255.0, vec3(0.0), vec3(1.0));
            if (op.p2.z > 0.0) { rgb = floor(rgb * op.p2.z + 0.5) / op.p2.z; }
            c = vec4(rgb, c.a);
        }
        return c;
    }
    if ((op.flags & F_TEX) != 0u) {
        let q = d - op.tex_origin;
        if (inside(q, op.tex_size)) { return textureLoad(layer_tex, q, 0); }
    }
    return op.color;
}

@fragment
fn fs_content(in: VOut) -> @location(0) vec4<f32> {
    let d = doc_px(local(in.pos));
    var c = layer_texel(d);
    c.a = c.a * mask_value(d);
    return c;
}

@fragment
fn fs_mask(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    var c = textureLoad(tex_a, p, 0);
    c.a = c.a * mask_value(doc_px(p));
    return c;
}

fn blend_px(p: vec2<i32>, b: vec4<f32>, src: vec4<f32>) -> vec4<f32> {
    var s = src;
    if (s.a <= 0.0) { return b; }
    if (op.mode == M_DISSOLVE) {
        s.a = select(0.0, 1.0, dissolve_noise(doc_px(p)) < s.a * op.opacity * op.fill);
        return composite(M_NORMAL, b, s, 1.0);
    }
    return composite_fill(op.mode, b, s, op.opacity, op.fill, (op.flags & F_TEXT_GAMMA) != 0u);
}

@fragment
fn fs_blend(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    return blend_px(p, textureLoad(tex_a, p, 0), textureLoad(tex_b, p, 0));
}

@fragment
fn fs_layerblend(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let d = doc_px(p);
    var s = layer_texel(d);
    s.a = s.a * mask_value(d);
    return blend_px(p, textureLoad(tex_a, p, 0), s);
}

fn batch_texel(t: i32, d: vec2<i32>, geo: vec4<f32>, color: vec4<f32>, prm: vec4<f32>) -> vec4<f32> {
    let q = d - vec2<i32>(geo.xy);
    if (prm.w > 0.5 && inside(q, vec2<i32>(geo.zw))) {
        if (t == 0) { return textureLoad(tex_b, q, 0); } else if (t == 1) { return textureLoad(tex_c, q, 0); }
        return textureLoad(map_tex, q, 0);
    }
    return color;
}

fn batch_layer(t: i32, d: vec2<i32>, b: vec4<f32>, geo: vec4<f32>, color: vec4<f32>, prm: vec4<f32>) -> vec4<f32> {
    let s = batch_texel(t, d, geo, color, prm);
    if (s.a <= 0.0) { return b; }
    return composite_fill(i32(prm.z), b, s, prm.x, prm.y, false);
}

fn batch_layer_normal(t: i32, d: vec2<i32>, b: vec4<f32>, geo: vec4<f32>, color: vec4<f32>, prm: vec4<f32>) -> vec4<f32> {
    let s = batch_texel(t, d, geo, color, prm);
    if (s.a <= 0.0) { return b; }
    return composite_normal_fill(b, s, prm.x, prm.y);
}

@fragment
fn fs_layerbatch(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let d = doc_px(p);
    var s = layer_texel(d);
    s.a = s.a * mask_value(d);
    var b = blend_px(p, textureLoad(tex_a, p, 0), s);
    if (op.kind >= 1) { b = batch_layer(0, d, b, op.p0, op.p1, op.p2); }
    if (op.kind >= 2) { b = batch_layer(1, d, b, op.p3, op.q0, op.q1); }
    if (op.kind >= 3) { b = batch_layer(2, d, b, op.q2, op.q3, op.q4); }
    return b;
}

@fragment
fn fs_layerbatch_normal(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let d = doc_px(p);
    var s = layer_texel(d);
    s.a = s.a * mask_value(d);
    var b = textureLoad(tex_a, p, 0);
    if (s.a > 0.0) { b = composite_normal_fill(b, s, op.opacity, op.fill); }
    if (op.kind >= 1) { b = batch_layer_normal(0, d, b, op.p0, op.p1, op.p2); }
    if (op.kind >= 2) { b = batch_layer_normal(1, d, b, op.p3, op.q0, op.q1); }
    if (op.kind >= 3) { b = batch_layer_normal(2, d, b, op.q2, op.q3, op.q4); }
    return b;
}

@fragment
fn fs_atop(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let base = textureLoad(tex_a, p, 0);
    if (base.a <= 0.0) { return base; }
    let s = textureLoad(tex_b, p, 0);
    if (op.mode == M_DISSOLVE) { return select(base, vec4(s.rgb, base.a), dissolve_noise(doc_px(p)) < s.a * op.opacity); }
    let r = composite_g(op.mode, vec4(base.rgb, 1.0), s, op.opacity, (op.flags & F_TEXT_GAMMA) != 0u);
    return vec4(r.rgb, base.a);
}

@fragment
fn fs_adjust(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let c = textureLoad(tex_a, p, 0);
    if (c.a <= 0.0) { return c; }
    adj_px = doc_px(p);
    return vec4(adjust(c.rgb), c.a);
}

@fragment
fn fs_adjmix(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let b = textureLoad(tex_a, p, 0);
    let k = op.opacity * mask_value(doc_px(p));
    if (k <= 0.0) { return b; }
    let a = textureLoad(tex_b, p, 0);
    if (op.mode == M_DISSOLVE) { return select(b, vec4(a.rgb, b.a), dissolve_noise(doc_px(p)) < k); }
    let f = clamp(op.fill, 0.0, 1.0);
    var cs = a.rgb;
    var bl = blend_rgb(op.mode, b.rgb, cs);
    return vec4(b.rgb * (1.0 - k) + bl * k, b.a);
}

@fragment
fn fs_lerp(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let a = textureLoad(tex_a, p, 0);
    let b = textureLoad(tex_b, p, 0);
    if ((op.flags & F_QUANT) != 0u) { return floor(a * op.p0.x + 0.5) / op.p0.x; }
    if ((op.flags & F_CHANNELS) != 0u) { return a + (b - a) * op.p0; }
    if ((op.flags & F_ADD_DIFF) != 0u) {
        let c = textureLoad(tex_c, p, 0);
        var pm = vec4(a.rgb * a.a, a.a) + vec4(b.rgb * b.a, b.a) - vec4(c.rgb * c.a, c.a);
        let al = clamp(pm.a, 0.0, 1.0);
        if (al <= 0.0) { return vec4(0.0); }
        return vec4(clamp(pm.rgb / al, vec3(0.0), vec3(1.0)), al);
    }
    if ((op.flags & F_BLEND_IF) != 0u) {
        var this_px = b;
        if (op.p4.y < 0.5) { this_px = textureLoad(tex_c, p, 0); }
        let k = blend_if_weight(this_px, a);
        if (k >= 1.0) { return b; }
        let alpha = a.a + (b.a - a.a) * k;
        if (alpha <= 0.0) { return vec4(0.0); }
        return vec4(clamp((a.rgb * a.a + (b.rgb * b.a - a.rgb * a.a) * k) / alpha, vec3(0.0), vec3(1.0)), alpha);
    }
    let k = op.opacity * mask_value(doc_px(p));
    let wa = a.a * (1.0 - k);
    let wb = b.a * k;
    let alpha = wa + wb;
    if (alpha <= 0.0) { return vec4(0.0); }
    return vec4((a.rgb * wa + b.rgb * wb) / alpha, alpha);
}

fn bi_ramp(lo: f32, hi: f32, x: f32) -> f32 {
    if (hi <= lo) { return select(0.0, 1.0, x >= lo); }
    return clamp((x - lo) / (hi - lo), 0.0, 1.0);
}

fn bi_weight(e: u32, s: u32, v: f32) -> f32 {
    var f = array<vec4<f32>, 4>(op.p0, op.p1, op.p2, op.p3);
    let i = (e * 2u + s) * 2u;
    let black = f[i / 4u][i % 4u];
    let white = f[(i + 1u) / 4u][(i + 1u) % 4u];
    let b_lo = floor(black / 256.0);
    let b_hi = black - b_lo * 256.0;
    let w_lo = floor(white / 256.0);
    let w_hi = white - w_lo * 256.0;
    return bi_ramp(b_lo, b_hi, v) * bi_ramp(255.0 - w_hi, 255.0 - w_lo, 255.0 - v);
}

fn blend_if_weight(this_px: vec4<f32>, under: vec4<f32>) -> f32 {
    var k = 1.0;
    for (var s = 0u; s < 2u; s = s + 1u) {
        let px = select(this_px, under, s == 1u);
        if (px.a <= 0.0) { continue; }
        let x = clamp(px.rgb, vec3(0.0), vec3(1.0));
        let raw = x * 255.0;
        let level = round(raw);
        let v = select(raw, level, round(x * 32768.0) == round(level * 32768.0 / 255.0));
        if (op.p4.x > 0.5) {
            k = k * bi_weight(0u, s, v.r) * bi_weight(1u, s, v.r);
        } else {
            let sum = 30.0 * v.r + 59.0 * v.g + 11.0 * v.b;
            let gray = select(sum / 100.0, floor((sum + 49.0) / 100.0), all(v == round(v)));
            k = k * bi_weight(0u, s, gray) * bi_weight(1u, s, v.r) * bi_weight(2u, s, v.g) * bi_weight(3u, s, v.b);
        }
        if (k <= 0.0) { return 0.0; }
    }
    return k;
}

const INSIDE_EPS: f32 = 0.5 / 255.0;

fn map_value(d: vec2<i32>, outside: f32) -> f32 {
    let q = d - op.map_origin;
    if (inside(q, op.map_size)) { return textureLoad(map_tex, q, 0).r; }
    return outside;
}

fn wrap(i: i32, n: i32) -> i32 { return ((i % n) + n) % n; }

fn pattern_tap(u0: f32, v0: f32) -> vec4<f32> {
    let u = u0 - 0.5;
    let v = v0 - 0.5;
    let x0 = floor(u);
    let y0 = floor(v);
    let fx = u - x0;
    let fy = v - y0;
    let tw = i32(op.p4.y);
    let th = i32(op.p4.z);
    let ix = i32(x0);
    let iy = i32(y0);
    var acc = vec4(0.0);
    let wy = array<f32, 2>(1.0 - fy, fy);
    let wx = array<f32, 2>(1.0 - fx, fx);
    for (var j = 0; j < 2; j++) {
        if (wy[j] == 0.0) { continue; }
        for (var i = 0; i < 2; i++) {
            if (wx[i] == 0.0) { continue; }
            acc += textureLoad(pat_tex, vec2(wrap(ix + i, tw), wrap(iy + j, th)), 0) * (wx[i] * wy[j]);
        }
    }
    return acc;
}

fn pattern_sample(d: vec2<i32>) -> vec4<f32> {
    var n = 1;
    if (op.p4.x > 1.0) { n = i32(min(ceil(op.p4.x), 8.0)); }
    let c = op.p3.z;
    let s = op.p3.w;
    var acc = vec4(0.0);
    for (var j = 0; j < n; j++) {
        for (var i = 0; i < n; i++) {
            let ox = (f32(i) + 0.5) / f32(n) - 0.5;
            let oy = (f32(j) + 0.5) / f32(n) - 0.5;
            let dx = f32(d.x) + 0.5 + ox - op.p3.x;
            let dy = f32(d.y) + 0.5 + oy - op.p3.y;
            acc += pattern_tap((dx * c - dy * s) * op.p4.x, (dx * s + dy * c) * op.p4.x);
        }
    }
    acc = acc / f32(n * n);
    if (acc.a <= 0.0) { return vec4(0.0); }
    return vec4(acc.rgb / acc.a, min(acc.a, 1.0));
}

fn fx_color(d: vec2<i32>) -> vec4<f32> {
    let kind = i32(op.p2.z);
    if (kind == 1) {
        let t = gradient_t(d);
        return vec4(lut(0, t), lut(1, t), lut(2, t), lut(3, t));
    }
    if (kind == 2) { return pattern_sample(d); }
    return op.color;
}

fn fx_composite(d: vec2<i32>, dst: vec4<f32>, s: vec4<f32>) -> vec4<f32> {
    if (op.mode == M_DISSOLVE) {
        if (dissolve_noise(d) < s.a) { return composite(M_NORMAL, dst, vec4(s.rgb, 1.0), 1.0); }
        return dst;
    }
    return composite(op.mode, dst, s, 1.0);
}

@fragment
fn fs_fxinit(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let c = textureLoad(tex_a, p, 0);
    switch op.kind {
        case 1: { return vec4(c.rgb, 1.0); }
        case 2: { return vec4(c.rgb, select(0.0, op.opacity, c.a > INSIDE_EPS)); }
        case 3: { return vec4(c.rgb, c.a * min(textureLoad(tex_b, p, 0).a, 1.0)); }
        case 4: {
            let a = textureLoad(tex_b, p, 0).a;
            let s = textureLoad(tex_c, p, 0);
            if (s.a <= 0.0 || a <= INSIDE_EPS) { return c; }
            return fx_composite(doc_px(p), c, vec4(s.rgb, op.opacity * min(s.a / a, 1.0)));
        }
        case 5: {
            let a = textureLoad(tex_b, p, 0).a;
            if (a <= INSIDE_EPS) { return vec4(c.rgb, 0.0); }
            return vec4(c.rgb, op.opacity * min(c.a / a, 1.0));
        }
        case 6: { return vec4(0.0, c.a, 0.0, 0.0); }
        case 7: { return vec4(c.rgb, max(c.a, map_value(doc_px(p), 0.0))); }
        default: { return vec4(c.rgb, c.a * op.opacity); }
    }
}

fn outline_share(cov: f32, l: f32) -> f32 {
    let outside = max(1.0 - clamp(cov, 0.0, 1.0), 0.0);
    if (outside <= 1e-6) { return 0.0; }
    let rest = 1.0 - clamp(l, 0.0, 1.0);
    if (rest <= 1e-6) { return 1.0; }
    return min(outside / rest, 1.0);
}

@fragment
fn fs_fxpaint(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let d = doc_px(p);
    let dst = textureLoad(tex_a, p, 0);
    let a = textureLoad(tex_b, p, 0).a;
    let inside_shape = a > INSIDE_EPS;
    var m = 1.0;
    if (op.kind == 0) { m = map_value(d, op.p2.w); }
    if ((op.flags & F_KNOCKOUT) != 0u) { m = m * (1.0 - a * op.p4.w); }
    if ((op.flags & F_GATE) != 0u && !inside_shape) { m = 0.0; }
    if ((op.flags & F_REL) != 0u) {
        if (inside_shape) { m = min(m / a, 1.0); } else { m = 0.0; }
    }
    if ((op.flags & F_STROKE_OUT) != 0u && inside_shape) { m = select(1.0, 0.0, (op.flags & F_VECTOR) != 0u); }
    if (i32(op.p2.z) == 4) {
        let k = min(m * op.p0.x, 1.0) * op.opacity;
        if (k <= 0.0) { return dst; }
        let t = 1.0 - clamp(m, 0.0, 1.0);
        return fx_composite(d, dst, vec4(lut(0, t), lut(1, t), lut(2, t), lut(3, t) * k));
    }
    let k = m * op.opacity;
    if (k <= 0.0) { return dst; }
    let c = fx_color(d);
    return fx_composite(d, dst, vec4(c.rgb, c.a * k));
}

@fragment
fn fs_fxstroke(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let d = doc_px(p);
    let a = textureLoad(tex_b, p, 0).a;
    let outline = (op.flags & F_OUTLINE) != 0u;
    let first = (op.flags & F_FIRST) != 0u;
    var cover = 0.0;
    var lay_a = 0.0;
    let vector = (op.flags & F_VECTOR) != 0u;
    if (!first || outline || !vector) {
        let cv = textureLoad(tex_c, p, 0);
        cover = cv.r;
        lay_a = cv.g;
    }
    var k = map_value(d, 0.0);
    if (outline) {
        k = k * outline_share(a, lay_a);
    } else if (a > INSIDE_EPS) {
        if (vector) { k = 0.0; } else { k = k * outline_share(a, lay_a); }
    }
    var cov = k * op.opacity;
    if (op.mode == M_DISSOLVE) { cov = select(0.0, 1.0, cov > 0.0 && dissolve_noise(d) < cov); }
    let share = cov * (1.0 - cover);
    if (op.kind == 1) { return vec4(cover + share, lay_a, 0.0, 0.0); }
    var acc = vec4(0.0);
    if (!first) { acc = textureLoad(layer_tex, p, 0); }
    if (share > 0.0) {
        let base = textureLoad(tex_a, p, 0);
        var bl = base;
        if (i32(op.p2.z) != 3) {
            let c = fx_color(d);
            bl = composite(op.mode, base, vec4(c.rgb, c.a), 1.0);
        }
        acc += share * vec4(bl.rgb * bl.a, bl.a);
    }
    return acc;
}

@fragment
fn fs_fxstrokeend(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let w = textureLoad(tex_a, p, 0);
    let cover = textureLoad(tex_c, p, 0).r;
    if (cover <= 0.0) { return w; }
    let acc = textureLoad(tex_b, p, 0);
    let k = 1.0 - min(cover, 1.0);
    let alpha = w.a * k + acc.a;
    if (alpha <= 0.0) { return w; }
    return vec4((w.rgb * w.a * k + acc.rgb) / alpha, min(alpha, 1.0));
}

fn mix_premul(a: vec4<f32>, b: vec4<f32>, k: f32) -> vec4<f32> {
    let alpha = a.a + (b.a - a.a) * k;
    if (alpha <= 0.0) { return vec4(0.0); }
    return vec4((a.rgb * a.a + (b.rgb * b.a - a.rgb * a.a) * k) / alpha, alpha);
}

@fragment
fn fs_fxmerge(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    var w = textureLoad(tex_a, p, 0);
    var l = textureLoad(tex_b, p, 0);
    let c = textureLoad(tex_c, p, 0);
    if ((op.flags & F_LAYER_SHAPE) != 0u) { l = vec4(l.rgb, select(0.0, op.p0.x, l.a > INSIDE_EPS) * min(l.a, 1.0)); }
    if (l.a > 0.0 && (op.flags & F_NO_LAYER) == 0u) { w = composite_g(op.mode, w, l, 1.0, (op.flags & F_TEXT_GAMMA) != 0u); }
    let atop = (op.flags & F_ATOP) != 0u;
    var before = c;
    if (atop) { before = vec4(c.rgb, 1.0); }
    var o: vec4<f32>;
    if (op.mode == M_DISSOLVE) { o = select(before, w, dissolve_noise(doc_px(p)) < op.opacity); } else { o = mix_premul(before, w, op.opacity); }
    if (atop) {
        if (c.a > 0.0) { return vec4(o.rgb, c.a); }
        return c;
    }
    return o;
}

fn ra(p: vec2<i32>) -> f32 {
    if (inside(p, chunk.size)) { return textureLoad(tex_a, p, 0).r; }
    return 0.0;
}
fn mout(v: f32) -> vec4<f32> { return vec4(v, 0.0, 0.0, 1.0); }

@fragment
fn fs_mshift(in: VOut) -> @location(0) vec4<f32> {
    let s = local(in.pos) - vec2<i32>(i32(op.p0.x), i32(op.p0.y));
    if (!inside(s, chunk.size)) { return mout(op.p0.z); }
    var v = textureLoad(tex_a, s, 0).r;
    if (op.p0.w > 0.5) { v = 1.0 - v; }
    return mout(v);
}

@fragment
fn fs_mdilate(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let a = textureLoad(tex_a, p, 0).r;
    let d = textureLoad(tex_b, p, 0).r;
    if (d < 0.0) { return mout(min(a + op.p0.x, 1.0)); }
    return mout(max(a, clamp(op.p0.x + 0.5 - d, 0.0, 1.0)));
}

@fragment
fn fs_mblur(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let r = i32(op.p0.y);
    var step = vec2(1, 0);
    var n = chunk.size.x;
    var c = p.x;
    if (op.p0.x > 0.5) {
        step = vec2(0, 1);
        n = chunk.size.y;
        c = p.y;
    }
    var acc = 0.0;
    for (var k = 0; k <= 2 * r; k++) {
        let i = c + k - r;
        if (i >= 0 && i < n) { acc += textureLoad(tex_a, p + step * (k - r), 0).r * textureLoad(lut_tex, vec2(k, 0), 0).r; }
    }
    return mout(acc);
}

@fragment
fn fs_mglow(in: VOut) -> @location(0) vec4<f32> {
    let d = textureLoad(tex_a, local(in.pos), 0).r + 0.5;
    var m = 0.5 * clamp(1.0 - (d - op.p0.x) / op.p0.y, 0.0, 1.0);
    if (d <= op.p0.x) { m = 1.0; }
    return mout(m);
}

@fragment
fn fs_mfinish(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    var v = textureLoad(tex_a, p, 0).r;
    if (op.p0.x > 0.5) { v = abs(v - textureLoad(tex_b, p, 0).r); }
    if (op.p0.y > 0.5 && op.p0.y < 1.5) { v = 1.0 - v; }
    if (op.p0.z > 0.5) { v = lut(0, v); }
    if (op.p0.y > 1.5) { v = 1.0 - v; }
    if (op.p0.w > 0.5) { v = v * textureLoad(layer_tex, p, 0).r; }
    return mout(v);
}

@fragment
fn fs_mbevelh(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let din = textureLoad(tex_a, p, 0).r;
    let dout = textureLoad(tex_b, p, 0).r;
    let size = op.p0.y;
    var h: f32;
    let ramp = size + 1.0;
    let soft = op.p0.z > 0.5;
    switch i32(op.p0.x) {
        case 0: {
            if (soft) { h = select((size + 2.0) / ramp, 1.0 - clamp((dout + 0.5) / ramp, 0.0, 1.0), dout > 0.0); } 
            else { let top = (size + 2.0) / ramp; h = select(top, clamp((size + 1.5 - dout) / ramp, 0.0, top), dout > 0.0); }
        }
        case 1: {
            let half = size / 2.0 + 0.5;
            if (din > 0.0) { h = 0.5 + 0.5 * clamp(din / half, 0.0, 1.0); } else { h = 0.5 - 0.5 * clamp(dout / half, 0.0, 1.0); }
        }
        default: {
            if (soft) { h = select(0.0, clamp((din + 1.5) / ramp, 0.0, (size + 2.0) / ramp), din > 0.0); } 
            else { h = select(0.0, clamp((din + 0.5) / ramp, 0.0, 1.0), din > 0.0); }
        }
    }
    return mout(h);
}

@fragment
fn fs_mbevelshade(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let s = textureLoad(layer_tex, p, 0).r;
    var hmax = ra(p);
    if (op.p1.y > 1.5 || op.p3.x > 0.5) { hmax = max(max(hmax, max(ra(p + vec2(1, 0)), ra(p - vec2(1, 0)))), max(ra(p + vec2(0, 1)), ra(p - vec2(0, 1)))); }
    let outside = select(0.0, 1.0, s < 1.0 - INSIDE_EPS && hmax > 1e-5);
    if (op.p1.y < 0.5) { return mout(bevel_part(p, op.p1.x, s)); }
    if (op.p1.y < 1.5) { return mout(bevel_part(p, op.p1.x, outside)); }
    var inner = 0.0;
    if (s > INSIDE_EPS) { inner = bevel_part(p, op.p1.x, s); }
    let od = select(op.p1.x, -op.p1.x, op.p1.y > 2.5);
    return mout(inner + bevel_part(p, od, outside) * (1.0 - min(s, 1.0)));
}

fn bevel_part(p: vec2<i32>, depth: f32, region: f32) -> f32 {
    let gx = (ra(p + vec2(1, 0)) - ra(p - vec2(1, 0))) * 0.5 * depth;
    let gy = (ra(p + vec2(0, 1)) - ra(p - vec2(0, 1))) * 0.5 * depth;
    let n = vec3(-gx, -gy, 1.0);
    let len = sqrt(n.x * n.x + n.y * n.y + 1.0);
    let shade = (n.x * op.p0.x + n.y * op.p0.y + n.z * op.p0.z) / len;
    let se = op.p0.w;
    var k = shade - se;
    if (op.p1.w > 0.5) { k = lut(0, shade) - lut(0, se); }
    var v = 0.0;
    if (op.p1.z > 0.5) {
        if (k <= 0.0) { v = clamp(-k / max(se, 1e-3), 0.0, 1.0) * region; }
    } else if (k > 0.0) {
        v = clamp(k / max(1.0 - se, 1e-3), 0.0, 1.0) * region;
    }
    return v;
}

@fragment
fn fs_mbeveltex(in: VOut) -> @location(0) vec4<f32> {
    let p = local(in.pos);
    let c = pattern_sample(doc_px(p));
    var l = (0.299 * c.r + 0.587 * c.g + 0.114 * c.b) * c.a;
    if (op.p0.y > 0.5) { l = 1.0 - l; }
    return mout(textureLoad(tex_a, p, 0).r + op.p0.x * l);
}

@fragment
fn fs_mstroke(in: VOut) -> @location(0) vec4<f32> {
    let d = textureLoad(tex_a, local(in.pos), 0).r;
    return mout(clamp(op.p0.x + 0.5 - d, 0.0, 1.0));
}