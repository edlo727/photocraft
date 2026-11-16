//! Large Brush Mode strokes painted into their layers on the GPU.
//!
//! While a stroke is drawn (and until its full-precision write lands), the app keeps its dab
//! coverage in a texture of its own. The compositor lays that paint into a copy of each page of
//! the stroke's layer (`Resident::painted`) and composites the copy in the layer's place, so the
//! stroke shows where it belongs in the stack: under the layers above it, through the layer's
//! mask, blend mode and opacity, clipped layers over it. Nothing on the CPU changes until the
//! write lands; the copy is rebuilt from the layer's own texels wherever those or the coverage
//! change, so it never drifts from them.
//!
//! The paint is laid as the CPU's paint over straight layer values: `a = coverage × opacity`,
//! colour and alpha "over" the layer pixel, or the colour alone where the layer's transparency is
//! locked. A brush in a blend mode ([`LiveSlot::mode`]) blends its colour with the layer pixel
//! first, by `photocraft_color::blend`'s formulas (`composite_for` / `composite_atop_for`, what
//! `paint.stroke` writes). Layer values are the document's own (linear for a linear document),
//! like the paint colour, so no encoding is involved.

use photocraft_doc::{DocId, LayerId};
use photocraft_geom::Rect;

use super::Compositor;
use super::textures::{ResKey, TexKind};

/// At most this many strokes paint into layers at once (the app's overlay slots).
pub const LIVE_SLOTS: usize = 4;

/// One stroke painting into a layer.
#[derive(Clone, Debug)]
pub struct LiveSlot {
    /// Stable for the stroke's life: a slot that goes away makes the painted copies rebuild.
    pub id: u64,
    pub layer: LayerId,
    /// The stroke's dab coverage (`R32Float` or `R16Float`, read with `textureLoad`), `scale`
    /// texels per document pixel.
    pub coverage: wgpu::TextureView,
    pub coverage_size: [u32; 2],
    pub scale: f32,
    /// Paint colour in the document's values (straight), and the brush opacity.
    pub color: [f32; 3],
    pub opacity: f32,
    /// The layer's transparency is locked: the paint changes colour only.
    pub lock: bool,
    /// The brush's blend mode: Normal, or one of Photoshop's the paint pass blends
    /// ([`live_mode`]; any other paints as Normal).
    pub mode: photocraft_color::BlendMode,
    /// The document is 32-bit float: the blend keeps values above 1 (`composite_hdr`).
    pub float: bool,
    /// Document rect (x0, y0, x1, y1) the layer's own pixels already hold this stroke in (its
    /// write landed there): no paint is laid there.
    pub hide: [f32; 4],
    /// Everything the stroke has painted so far (document pixels).
    pub bounds: Rect,
}

/// The strokes painting into layers of one document, oldest first.
#[derive(Clone, Debug)]
pub struct LivePaint {
    pub doc: DocId,
    pub slots: Vec<LiveSlot>,
}

/// A layer page with the live strokes laid in.
pub(crate) struct Painted {
    pub(crate) _texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
    /// The live slots it holds, by id: any other set rebuilds it whole.
    pub(crate) slots: Vec<u64>,
    /// Its size in bytes (it counts against [`Compositor::live_budget`]).
    pub(crate) bytes: u64,
}

/// Pipelines and fixed resources of the paint pass, made on first use.
pub(crate) struct LiveKit {
    bgl: wgpu::BindGroupLayout,
    pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    sampler: wgpu::Sampler,
    dummy: wgpu::TextureView,
    _dummy: wgpu::Texture,
}

/// Uniform: origin of the page (x, y), slot count; then per slot colour + opacity, scale,
/// coverage width and height, lock; hide rect; blend mode ([`live_mode`]) and float document.
const UNIFORM_BYTES: u64 = 16 + LIVE_SLOTS as u64 * 64;

/// The shader's index of brush mode `mode` (`plan::mode_index`'s, 3..=30: Photoshop's modes), or
/// `None` where the pass paints Normal: Normal itself, and the modes it has no formula for
/// (Dissolve, XOR, Paint.NET's and the Nuke operations; the app keeps those strokes on the
/// normal live path).
pub(crate) fn live_mode(mode: photocraft_color::BlendMode) -> Option<i32> {
    let m = super::plan::mode_index(mode);
    (3..=30).contains(&m).then_some(m)
}

const LIVE_WGSL: &str = r#"
struct Slot { c: vec4<f32>, s: vec4<f32>, h: vec4<f32>, m: vec4<f32> };
struct U { o: vec4<f32>, slots: array<Slot, 4> };
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var cov0: texture_2d<f32>;
@group(0) @binding(2) var cov1: texture_2d<f32>;
@group(0) @binding(3) var cov2: texture_2d<f32>;
@group(0) @binding(4) var cov3: texture_2d<f32>;
@group(0) @binding(5) var samp: sampler;
@group(0) @binding(6) var<uniform> u: U;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

// Coverage texture `t` at texel-space point `q`, bilinear by hand (32-bit float coverage isn't
// filterable everywhere), clamped at the edges: as the canvas's overlay reads it.
fn cov_bilinear(t: texture_2d<f32>, q: vec2<f32>) -> f32 {
    let dim = vec2<i32>(textureDimensions(t)) - 1;
    let x = q - 0.5;
    let i = vec2<i32>(floor(x));
    let f = x - floor(x);
    let a = textureLoad(t, clamp(i, vec2(0), dim), 0).r;
    let b = textureLoad(t, clamp(i + vec2(1, 0), vec2(0), dim), 0).r;
    let c = textureLoad(t, clamp(i + vec2(0, 1), vec2(0), dim), 0).r;
    let e = textureLoad(t, clamp(i + vec2(1, 1), vec2(0), dim), 0).r;
    return mix(mix(a, b, f.x), mix(c, e, f.x), f.y);
}

fn coverage(k: i32, d: vec2<f32>) -> f32 {
    let q = d * u.slots[k].s.x;
    switch k {
        case 0: { return cov_bilinear(cov0, q); }
        case 1: { return cov_bilinear(cov1, q); }
        case 2: { return cov_bilinear(cov2, q); }
        default: { return cov_bilinear(cov3, q); }
    }
}

// ---- brush blend modes: photocraft_color::blend's formulas (channel, blend_rgb_within), which
// `paint.stroke` writes with; `hdr` is a 32-bit float document (composite_hdr). Mode numbers are
// plan::mode_index's. As in compose.wgsl, each switch case assigns and the function returns once
// (FXC, the DX12 compiler, can't follow a large switch whose cases all return).

const F32_MAX: f32 = 0x1.fffffep+127f;
const EDGE: f32 = 1e-4;

fn lp_color_burn(cb: f32, cs: f32) -> f32 {
    if (cb >= 1.0) { return 1.0; }
    if (cs <= 0.0) { return 0.0; }
    return 1.0 - min((1.0 - cb) / cs, 1.0);
}
fn lp_color_dodge(cb: f32, cs: f32) -> f32 {
    if (cb <= 0.0) { return 0.0; }
    if (cs >= 1.0) { return 1.0; }
    return min(cb / (1.0 - cs), 1.0);
}
fn lp_hard_light(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) { return cb * 2.0 * cs; }
    let s = 2.0 * cs - 1.0;
    return cb + s - cb * s;
}
fn lp_soft_light(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) { return 2.0 * cb * cs + cb * cb * (1.0 - 2.0 * cs); }
    let b = max(cb, 0.0);
    var d = sqrt(b);
    if (b <= 0.25) { d = ((16.0 * b - 12.0) * b + 4.0) * b; }
    return 2.0 * cb * (1.0 - cs) + d * (2.0 * cs - 1.0);
}
fn lp_vivid_light(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.0) { return 0.0; }
    if (cs >= 1.0) { return 1.0; }
    if (cs <= 0.5) { return lp_color_burn(cb, 2.0 * cs); }
    return lp_color_dodge(cb, 2.0 * (cs - 0.5));
}
// blend::vivid_light_hard_mix: Hard Mix's Vivid Light, exact at the backdrop's extremes.
fn lp_vivid_hard_mix(cb: f32, cs: f32) -> f32 {
    if (cs <= 0.5) {
        if (cb >= 1.0 - EDGE) { return 1.0; }
        if (cs <= 0.0) { return 0.0; }
        return 1.0 - min((1.0 - cb) / (2.0 * cs), 1.0);
    }
    if (cb <= EDGE) { return 0.0; }
    if (cs >= 1.0) { return 1.0; }
    return min(cb / (2.0 * (1.0 - cs)), 1.0);
}
fn lp_reflect(cb: f32, cs: f32) -> f32 {
    if (cs >= 1.0) { return 1.0; }
    return min(cb * cb / (1.0 - cs), 1.0);
}
// blend::range_bound: the modes whose formula only means something on 0..1 (Screen, Color Dodge,
// Overlay, Soft Light, Hard Light, Vivid Light, Linear Light, Exclusion, Reflect, Glow, Negation).
fn lp_range_bound(mode: i32) -> bool {
    return mode == 9 || mode == 10 || (mode >= 13 && mode <= 17) || mode == 21 || (mode >= 28 && mode <= 30);
}

fn lp_channel(mode: i32, cb: f32, cs: f32, hdr: bool) -> f32 {
    // Linear Dodge / Divide clip at 1 at integer depths, at f32::MAX in 32-bit documents.
    let hi = select(1.0, F32_MAX, hdr);
    var r = cs;
    let in_range = cb >= 0.0 && cb <= 1.0 && cs >= 0.0 && cs <= 1.0;
    if (hdr && lp_range_bound(mode) && !in_range) {
        // 32-bit: outside 0..1 the brighter input stands.
        r = max(cb, cs);
    } else {
        switch mode {
            case 3: { r = min(cb, cs); }                            // Darken
            case 4: { r = clamp(cb * cs, -F32_MAX, F32_MAX); }      // Multiply
            case 5: { r = lp_color_burn(cb, cs); }                  // ColorBurn
            case 6: { r = max(cb + cs - 1.0, 0.0); }                // LinearBurn
            case 8: { r = max(cb, cs); }                            // Lighten
            case 9: { r = cb + cs - cb * cs; }                      // Screen
            case 10: { r = lp_color_dodge(cb, cs); }                // ColorDodge
            case 11: { r = min(cb + cs, hi); }                      // LinearDodge
            case 13: { r = lp_hard_light(cs, cb); }                 // Overlay
            case 14: { r = lp_soft_light(cb, cs); }                 // SoftLight
            case 15: { r = lp_hard_light(cb, cs); }                 // HardLight
            case 16: { r = lp_vivid_light(cb, cs); }                // VividLight
            case 17: { r = clamp(cb + 2.0 * cs - 1.0, 0.0, 1.0); }  // LinearLight
            case 18: {                                              // PinLight
                if (cs <= 0.5) { r = min(cb, 2.0 * cs); } else { r = max(cb, 2.0 * cs - 1.0); }
            }
            case 19: { r = select(0.0, 1.0, lp_vivid_hard_mix(cb, cs) >= 0.5 - 1e-6); } // HardMix
            case 20: { r = abs(cb - cs); }                          // Difference
            case 21: { r = cb + cs - 2.0 * cb * cs; }               // Exclusion
            case 22: { r = max(cb - cs, 0.0); }                     // Subtract
            case 23: {                                              // Divide
                if (cs <= 0.0) { r = select(1.0, 0.0, cb <= 0.0); } else { r = min(cb / cs, hi); }
            }
            case 28: { r = lp_reflect(cb, cs); }                    // Reflect
            case 29: { r = lp_reflect(cs, cb); }                    // Glow
            case 30: { r = 1.0 - abs(1.0 - cb - cs); }              // Negation
            default: { r = cs; }
        }
    }
    return r;
}

fn lp_lum(c: vec3<f32>) -> f32 { return 0.3 * c.r + 0.59 * c.g + 0.11 * c.b; }
// blend::clip_color: towards grey until within 0..top; a luminosity outside it gives its grey.
fn lp_clip_color(c: vec3<f32>, top: f32) -> vec3<f32> {
    let l = lp_lum(c);
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
fn lp_set_lum(c: vec3<f32>, l: f32, top: f32) -> vec3<f32> { return lp_clip_color(c + (l - lp_lum(c)), top); }
fn lp_sat(c: vec3<f32>) -> f32 { return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b); }
fn lp_set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    var out = vec3(0.0);
    if (mx > mn) { out = (c - mn) * s / (mx - mn); }
    return out;
}

// B(cb, cs) of brush mode `mode` (blend::blend_rgb_hdr / blend_rgb).
fn lp_blend(mode: i32, cb: vec3<f32>, cs: vec3<f32>, hdr: bool) -> vec3<f32> {
    let top = select(1.0, F32_MAX, hdr);
    var r = cs;
    switch mode {
        case 24: { r = lp_set_lum(lp_set_sat(cs, lp_sat(cb)), lp_lum(cb), top); } // Hue
        case 25: { r = lp_set_lum(lp_set_sat(cb, lp_sat(cs)), lp_lum(cb), top); } // Saturation
        case 26: { r = lp_set_lum(cs, lp_lum(cb), top); }                         // Color
        case 27: { r = lp_set_lum(cb, lp_lum(cs), top); }                         // Luminosity
        case 7: { r = select(cb, cs, lp_lum(cs) < lp_lum(cb)); }                  // DarkerColor
        case 12: { r = select(cb, cs, lp_lum(cs) > lp_lum(cb)); }                 // LighterColor
        default: {
            r = vec3(lp_channel(mode, cb.r, cs.r, hdr), lp_channel(mode, cb.g, cs.g, hdr), lp_channel(mode, cb.b, cs.b, hdr));
        }
    }
    return r;
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let t = vec2<i32>(pos.xy);
    var col = textureLoad(src, t, 0);
    // The texel centre in document pixels, as the overlay and the CPU sample them.
    let d = pos.xy + u.o.xy;
    let n = i32(u.o.z);
    for (var k = 0; k < n; k++) {
        let h = u.slots[k].h;
        if (d.x >= h.x && d.x < h.z && d.y >= h.y && d.y < h.w) {
            continue;
        }
        let a = clamp(coverage(k, d), 0.0, 1.0) * u.slots[k].c.a;
        if (a <= 0.0) {
            continue;
        }
        let paint = u.slots[k].c.rgb;
        let mode = i32(u.slots[k].m.x);
        let lock = u.slots[k].s.w > 0.5;
        if (mode > 2) {
            // A brush in a blend mode: blend::composite_with_blend, or composite_atop_with_blend
            // where the layer's transparency is locked.
            let bl = lp_blend(mode, col.rgb, paint, u.slots[k].m.y > 0.5);
            if (lock) {
                col = vec4(mix(col.rgb, bl, a), col.a);
            } else {
                let out_a = a + col.a * (1.0 - a);
                col = vec4(((1.0 - a) * col.a * col.rgb + (1.0 - col.a) * a * paint + a * col.a * bl) / max(out_a, 1e-6), out_a);
            }
        } else if (lock) {
            col = vec4(mix(col.rgb, paint, a), col.a);
        } else {
            let out_a = a + col.a * (1.0 - a);
            col = vec4((paint * a + col.rgb * col.a * (1.0 - a)) / max(out_a, 1e-6), out_a);
        }
    }
    return col;
}
"#;

impl LiveKit {
    fn new(device: &wgpu::Device) -> Self {
        let tex = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                multisampled: false,
                // Coverage is read with textureLoad (it may be 32-bit float).
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let mut entries: Vec<wgpu::BindGroupLayoutEntry> = (0..5).map(tex).collect();
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 5,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 6,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: std::num::NonZeroU64::new(UNIFORM_BYTES) },
            count: None,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("pc_live_paint"), entries: &entries });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pc_live_paint"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let dummy_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pc_live_paint_none"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy = dummy_tex.create_view(&Default::default());
        LiveKit { bgl, pipelines: Vec::new(), sampler, dummy, _dummy: dummy_tex }
    }

    fn pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        if let Some(i) = self.pipelines.iter().position(|(f, _)| *f == format) {
            return &self.pipelines[i].1;
        }
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("pc_live_paint"), source: wgpu::ShaderSource::Wgsl(LIVE_WGSL.into()) });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("pc_live_paint"), bind_group_layouts: &[Some(&self.bgl)], immediate_size: 0 });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pc_live_paint"),
            layout: Some(&layout),
            vertex: wgpu::VertexState { module: &module, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.push((format, pipeline));
        let last = self.pipelines.len() - 1;
        &self.pipelines[last].1
    }
}

/// The paint pass's uniform for `slots` over a page at `origin`.
fn uniform(origin: [i32; 2], slots: &[&LiveSlot]) -> Vec<u8> {
    let mut f: Vec<f32> = vec![origin[0] as f32, origin[1] as f32, slots.len().min(LIVE_SLOTS) as f32, 0.0];
    for k in 0..LIVE_SLOTS {
        match slots.get(k) {
            Some(s) => {
                let ok = |v: f32| if v.is_finite() { v } else { 0.0 };
                f.extend([ok(s.color[0]), ok(s.color[1]), ok(s.color[2]), ok(s.opacity).clamp(0.0, 1.0)]);
                f.extend([ok(s.scale), s.coverage_size[0] as f32, s.coverage_size[1] as f32, if s.lock { 1.0 } else { 0.0 }]);
                f.extend(s.hide);
                f.extend([live_mode(s.mode).unwrap_or(1) as f32, if s.float { 1.0 } else { 0.0 }, 0.0, 0.0]);
            }
            None => f.extend([0.0; 16]),
        }
    }
    f.iter().flat_map(|v| v.to_le_bytes()).collect()
}

impl Compositor {
    /// Paint `live`'s strokes into their layers for the renders that follow (`None`: none).
    /// A layer page the strokes reach composites as a copy with them laid in (see the module
    /// docs); the layer's planned area grows to where they paint, so a stroke on an empty layer
    /// shows too. Set before every render of the document while strokes are live.
    pub fn set_live_paint(&mut self, live: Option<LivePaint>) {
        let live = live.filter(|l| !l.slots.is_empty());
        if live.is_none() {
            for r in self.residents.values_mut() {
                r.painted = None;
            }
        }
        self.live = live.map(|mut l| {
            l.slots.truncate(LIVE_SLOTS);
            l
        });
    }

    /// Whether a painted copy didn't fit [`Self::live_budget`] since the last call: the strokes
    /// it was for don't show in their layer on that page, so the app lays them over the canvas
    /// instead. Cleared by the call.
    pub fn live_over_budget(&mut self) -> bool {
        std::mem::take(&mut self.live_over)
    }

    /// GPU memory the painted copies hold.
    pub fn live_bytes(&self) -> u64 {
        self.residents.values().filter_map(|r| r.painted.as_ref().map(|p| p.bytes)).sum()
    }

    /// GPU memory painted copies may take: a quarter of the layer pages' budget (they copy
    /// pages), so a huge stroke on a smaller card falls back instead of running out of memory.
    fn live_budget(&self) -> u64 {
        self.resident_budget / 4
    }

    /// The layers live strokes paint into in `doc`, with where (for [`crate::plan::plan_live`]).
    pub(crate) fn live_rects(&self, doc: DocId) -> Vec<(LayerId, Rect)> {
        let Some(live) = self.live.as_ref().filter(|l| l.doc == doc) else { return Vec::new() };
        let mut out: Vec<(LayerId, Rect)> = Vec::new();
        for s in &live.slots {
            let r = s.bounds;
            match out.iter_mut().find(|(id, _)| *id == s.layer) {
                Some((_, b)) => *b = b.union(&r),
                None => out.push((s.layer, r)),
            }
        }
        out
    }

    /// Bring resident `key`'s painted copy up to date for this render (or drop it where no live
    /// stroke reaches): rebuilt whole when it is new or its strokes changed, else where the
    /// render draws and where tiles were uploaded since.
    pub(crate) fn paint_resident(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, key: ResKey) {
        let (layer, _, _) = key;
        let region_now = self.render_region;
        // What the other painted copies take, for the budget.
        let others: u64 = self.residents.iter().filter(|(k, _)| **k != key).filter_map(|(_, r)| r.painted.as_ref().map(|p| p.bytes)).sum();
        let budget = self.live_budget();
        let Some(r) = self.residents.get_mut(&key) else { return };
        let dirty = std::mem::replace(&mut r.dirty, Rect::EMPTY);
        let slots: Vec<&super::live::LiveSlot> = match &self.live {
            Some(l) if l.doc == r.doc => l.slots.iter().filter(|s| s.layer == layer && !s.bounds.intersect(&r.region).is_empty()).collect(),
            _ => Vec::new(),
        };
        if slots.is_empty() || !matches!(r.kind, TexKind::Rgba16F | TexKind::Rgba8 | TexKind::Rgba8Direct) {
            r.painted = None;
            return;
        }
        let ids: Vec<u64> = slots.iter().map(|s| s.id).collect();
        let full = r.painted.as_ref().is_none_or(|p| p.slots != ids);
        let rect = if full { r.region } else { region_now.union(&dirty).intersect(&r.region) };
        if full {
            if others.saturating_add(r.bytes()) > budget {
                r.painted = None;
                self.live_over = true;
                return;
            }
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pc_live_painted"),
                size: wgpu::Extent3d { width: r.region.width().max(1), height: r.region.height().max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: r.kind.format(),
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            r.painted = Some(super::live::Painted { _texture: texture, view, slots: ids, bytes: r.bytes() });
        }
        if rect.is_empty() {
            return;
        }
        let kit = self.live_kit.get_or_insert_with(|| LiveKit::new(device));
        let format = r.kind.format();
        let data = uniform([r.region.x0, r.region.y0], &slots);
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pc_live_paint"),
            size: data.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ubuf, 0, &data);
        let cov = |k: usize| slots.get(k).map_or(&kit.dummy, |s| &s.coverage);
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pc_live_paint"),
            layout: &kit.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&r.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(cov(0)) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(cov(1)) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(cov(2)) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(cov(3)) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::Sampler(&kit.sampler) },
                wgpu::BindGroupEntry { binding: 6, resource: ubuf.as_entire_binding() },
            ],
        });
        let Some(painted) = r.painted.as_ref() else { return };
        let pipeline = kit.pipeline(device, format);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pc_live_paint"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &painted.view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_scissor_rect((rect.x0 - r.region.x0) as u32, (rect.y0 - r.region.y0) as u32, rect.width(), rect.height());
        pass.draw(0..3, 0..1);
        drop(pass);
        self.recorded += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_paint_shader_validates() {
        let module = wgpu::naga::front::wgsl::parse_str(LIVE_WGSL).unwrap_or_else(|e| panic!("{}", e.emit_to_string(LIVE_WGSL)));
        wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }

    /// The pass blends Photoshop's modes; the rest paint as Normal (the app doesn't send them).
    #[test]
    fn photoshop_brush_modes_have_a_shader_index() {
        use photocraft_color::BlendMode;
        assert_eq!(live_mode(BlendMode::Normal), None);
        assert_eq!(live_mode(BlendMode::Dissolve), None);
        assert_eq!(live_mode(BlendMode::HardLight), Some(15));
        assert_eq!(live_mode(BlendMode::LinearDodge), Some(11));
        assert_eq!(live_mode(BlendMode::Luminosity), Some(27));
        assert_eq!(live_mode(BlendMode::Xor), None);
        assert_eq!(live_mode(BlendMode::NukePlus), None);
    }

    #[test]
    fn the_uniform_matches_the_shader_layout() {
        assert_eq!(uniform([0, 0], &[]).len() as u64, UNIFORM_BYTES);
    }
}
