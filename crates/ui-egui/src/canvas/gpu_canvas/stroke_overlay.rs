//! Large Brush Mode's GPU stroke overlay (`photocraft_engine::brush_cmds::gpu_preview`).
//!
//! While the pen is down, the Brush's dabs are drawn into a one-channel coverage texture
//! (document space at up to 8192 px a side), building up as the CPU stroke buffer does: each dab
//! takes coverage `c` towards its own opacity ceiling (pen pressure through Transfer › Opacity),
//! `c + v·(ceiling − c)` where the ceiling is above `c`, never past it. Blending can't stop at a
//! ceiling, so a compute pass walks each frame's dabs in order per texel (`R32Float`, read-write
//! storage); a GPU without read-write storage textures draws instanced quads with plain build-up
//! (`c + v·ceiling·(1 − c)`, which ignores the ceiling: the pressure's opacity then shows when the
//! stroke lands). The canvas shader (bind group 3)
//! lays the paint colour over the document's display texture at that coverage times the stroke
//! opacity, before the display transform, so the viewer LUT and channels see it as they will
//! see the committed stroke. Nothing is rendered into the document until the pen lifts.
//!
//! There are [`SLOTS`] such strokes at once: a released stroke keeps showing from its slot while
//! its full-precision write runs in the background, and the next stroke starts at once in
//! another. The canvas lays them over the document in stroke order.

use super::doc_textures::init_buffer;
use super::resources::{Resources, f32_bytes};
use super::{GpuCanvas, wgpu};

/// The coverage texture's longest side at most (`R16Float`: 128 MiB at 8192²).
const MAX_SIDE: u32 = 8192;
/// Strokes the overlay shows at once (one drawn, the rest waiting for their writes).
pub(super) const SLOTS: usize = 4;
/// The overlay uniform: per slot, paint colour (texture encoding) and opacity, then scale, on,
/// linear, unused; then the drawing order (slot indices) and how many show; then per slot the
/// document rect where its landed write already shows (the overlay skips it, see `stroke_hide`).
const OVERLAY_UNIFORM: u64 = (SLOTS as u64) * 48 + 32;
/// The dab pass uniform: scale, texture width, height, unused.
const DAB_UNIFORM: u64 = 16;
/// Floats per dab instance: centre x, y, angle, roundness; core, shoulder, reach, alpha.
const DAB_FLOATS: usize = 8;
/// Floats per dab of the exact (compute) pass: those, then the ceiling and three unused.
const EXACT_DAB_FLOATS: usize = 12;
/// The exact pass's uniform: scale, width, height, unused (f32); the texel rect x0, y0, x1, y1
/// (i32); the dab count and three unused (u32).
const EXACT_UNIFORM: u64 = 48;

/// The exact dab pass: every texel the frame's dabs reach takes them in order, each building
/// the coverage up to that dab's ceiling, as `photocraft_paint`'s stroke buffer does.
pub(super) const EXACT_DAB_WGSL: &str = r#"
struct P { s: vec4<f32>, r: vec4<i32>, n: vec4<u32> };
struct Dab { a: vec4<f32>, b: vec4<f32>, c: vec4<f32> }; // centre, angle, roundness; core, shoulder, reach, flow; ceiling
@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> dabs: array<Dab>;
@group(0) @binding(2) var cov: texture_storage_2d<r32float, read_write>;

// exp(-u) as (1 - u/64)^64, as photocraft_paint's soft_exp.
fn soft_exp(u: f32) -> f32 {
    var g = clamp(1.0 - u / 64.0, 0.0, 1.0);
    g = g * g; g = g * g; g = g * g; g = g * g; g = g * g; g = g * g;
    return g;
}

@compute @workgroup_size(8, 8, 1)
fn cs(@builtin(global_invocation_id) g: vec3<u32>) {
    let t = p.r.xy + vec2<i32>(g.xy);
    if (t.x >= p.r.z || t.y >= p.r.w) {
        return;
    }
    // The texel centre in document pixels; the CPU samples pixel centres the same way.
    let d = (vec2<f32>(t) + 0.5) / p.s.x;
    let c0 = textureLoad(cov, t).r;
    var c = c0;
    for (var i = 0u; i < p.n.x; i = i + 1u) {
        let k = dabs[i];
        let reach = k.b.z + 0.5;
        if (abs(d.x - k.a.x) > reach + 1.0 || abs(d.y - k.a.y) > reach + 1.0) {
            continue;
        }
        let ux = d.x - k.a.x;
        let uy = -(d.y - k.a.y);
        let cs = cos(k.a.z);
        let sn = sin(k.a.z);
        let u = ux * cs + uy * sn;
        let v = -ux * sn + uy * cs;
        let dd = sqrt((u * k.a.w) * (u * k.a.w) + v * v);
        if (dd >= reach) {
            continue;
        }
        var val = 1.0;
        if (dd > k.b.x) {
            let x = (dd - k.b.x) / k.b.y;
            val = soft_exp(x * x);
        }
        let o = val * k.b.w;
        let ceil = k.c.x;
        if (o > 0.0 && ceil > c) {
            c = c + o * (ceil - c);
        }
    }
    if (c != c0) {
        textureStore(cov, t, vec4(c, 0.0, 0.0, 0.0));
    }
}
"#;

pub(super) const DAB_WGSL: &str = r#"
struct P { s: vec4<f32> }; // scale (texture px per doc px), texture w, h, unused
@group(0) @binding(0) var<uniform> p: P;

struct V {
    @builtin(position) pos: vec4<f32>,
    @location(0) a: vec4<f32>, // centre x, y (doc px), angle, roundness
    @location(1) b: vec4<f32>, // core, shoulder, reach, alpha
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, @location(0) a: vec4<f32>, @location(1) b: vec4<f32>) -> V {
    var c = array<vec2<f32>, 6>(vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0), vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0));
    let r = b.z + 1.5;
    let px = (a.xy + c[vi] * r) * p.s.x;
    let n = px / p.s.yz * 2.0 - 1.0;
    return V(vec4(n.x, -n.y, 0.0, 1.0), a, b);
}

// exp(-u) as (1 - u/64)^64, as photocraft_paint's soft_exp.
fn soft_exp(u: f32) -> f32 {
    var g = clamp(1.0 - u / 64.0, 0.0, 1.0);
    g = g * g; g = g * g; g = g * g; g = g * g; g = g * g; g = g * g;
    return g;
}

@fragment
fn fs(in: V) -> @location(0) vec4<f32> {
    // The texel centre in document pixels; the CPU samples pixel centres the same way.
    let d = in.pos.xy / p.s.x;
    let ux = d.x - in.a.x;
    let uy = -(d.y - in.a.y);
    let cs = cos(in.a.z);
    let sn = sin(in.a.z);
    let u = ux * cs + uy * sn;
    let v = -ux * sn + uy * cs;
    let dd = sqrt((u * in.a.w) * (u * in.a.w) + v * v);
    if (dd >= in.b.z + 0.5) {
        discard;
    }
    var val = 1.0;
    if (dd > in.b.x) {
        let x = (dd - in.b.x) / in.b.y;
        val = soft_exp(x * x);
    }
    let o = val * in.b.w;
    return vec4(o, 0.0, 0.0, o);
}
"#;

/// One stroke's coverage texture.
struct Slot {
    doc: u64,
    doc_size: [u32; 2],
    size: [u32; 2],
    scale: f32,
    color: [f32; 3],
    opacity: f32,
    linear: bool,
    view: wgpu::TextureView,
    _texture: wgpu::Texture,
    dab_bind_group: wgpu::BindGroup,
    _dab_uniform: wgpu::Buffer,
    /// Shown by the canvas (drawn, or waiting for its write to land).
    visible: bool,
    /// Document rect (x0, y0, x1, y1) the canvas already draws this stroke's landed write in:
    /// the overlay doesn't lay it over again there. Empty until it lands.
    hide: [f32; 4],
    /// Painted into this layer by the compositor (`photocraft_gpu::Compositor::set_live_paint`),
    /// in its place in the stack, with transparency locked or not; `None`: laid over the canvas.
    layer: Option<(photocraft_doc::LayerId, bool)>,
    /// The brush's blend mode, and whether the document is 32-bit float: how a stroke painted
    /// into its layer blends with the layer's pixels (`photocraft_gpu::LiveSlot`).
    blend: (photocraft_color::BlendMode, bool),
    /// Everything its dabs reach so far (document pixels).
    bounds: photocraft_geom::Rect,
    /// Unique per stroke.
    id: u64,
}

/// The overlay's GPU objects, kept in the canvas [`Resources`].
pub(super) struct StrokeGpu {
    /// Bind group 3 of the canvas pipeline: [`SLOTS`] coverage textures and the overlay uniform.
    pub(super) bgl: wgpu::BindGroupLayout,
    /// Bound when no stroke shows (1×1 empty coverage, nothing on).
    idle: wgpu::BindGroup,
    dummy_view: wgpu::TextureView,
    _dummy: wgpu::Texture,
    _idle_uniform: wgpu::Buffer,
    uniform: wgpu::Buffer,
    /// Bind group over the slots' textures, rebuilt when one changes.
    bind_group: Option<wgpu::BindGroup>,
    dab_bgl: wgpu::BindGroupLayout,
    dab_pipeline: wgpu::RenderPipeline,
    /// The exact dab pass, where the GPU can run it.
    exact: Option<(wgpu::ComputePipeline, wgpu::BindGroupLayout)>,
    slots: [Option<Slot>; SLOTS],
    /// Visible slots, oldest stroke first.
    order: Vec<usize>,
    /// The last stroke id given out.
    next_id: u64,
    /// Slots in use at most, and the coverage textures' largest side: smaller on cards with
    /// less video memory (`set_card`).
    max_slots: usize,
    max_side: u32,
}

/// A coverage texture: `R32Float` read-write storage for the exact pass, else `R16Float`
/// drawn into by the instanced one. The canvas reads either with `textureLoad` (bilinear by
/// hand: 32-bit float isn't filterable everywhere).
fn coverage_texture(device: &wgpu::Device, w: u32, h: u32, exact: bool) -> wgpu::Texture {
    let (format, extra) = if exact {
        (wgpu::TextureFormat::R32Float, wgpu::TextureUsages::STORAGE_BINDING)
    } else {
        (wgpu::TextureFormat::R16Float, wgpu::TextureUsages::empty())
    };
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pc_stroke_coverage"),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | extra,
        view_formats: &[],
    })
}

/// Whether `adapter` runs the exact dab pass: compute shaders and read-write `R32Float`
/// storage textures (every Vulkan, Metal and DirectX 12 GPU).
pub(crate) fn exact_dabs_supported(adapter: &wgpu::Adapter) -> bool {
    adapter.get_downlevel_capabilities().flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        && adapter
            .get_texture_format_features(wgpu::TextureFormat::R32Float)
            .flags
            .contains(wgpu::TextureFormatFeatureFlags::STORAGE_READ_WRITE)
}

/// The sRGB encoding the display texture stores for linear-light documents.
pub(super) fn srgb_encode(v: f32) -> f32 {
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// Document scale of the coverage texture for a `w`×`h` document viewed at `zoom` (screen
/// pixels per document pixel): the screen's resolution with a quarter to spare, so a zoomed-out
/// view draws (and the GPU fills) a fraction of the document's pixels per dab; at most 1, and
/// less to fit [`MAX_SIDE`] and the device's texture limit. The commit is full resolution anyway.
pub(super) fn coverage_scale(w: u32, h: u32, device_max: u32, zoom: f32) -> f32 {
    let side = w.max(h).max(1) as f32;
    let view = if zoom.is_finite() && zoom > 0.0 { (zoom * 1.25).max(1.0 / 64.0) } else { 1.0 };
    (MAX_SIDE.min(device_max.max(1)) as f32 / side).min(view).min(1.0)
}

/// Document rect the dabs reach (with a pixel to spare).
pub(super) fn dab_bounds(dabs: &[photocraft_engine::brush_cmds::gpu_preview::GpuDab]) -> photocraft_geom::Rect {
    dabs.iter().filter(|d| d.reach.is_finite() && d.center.iter().all(|v| v.is_finite())).fold(photocraft_geom::Rect::EMPTY, |b, d| {
        let r = d.reach + 2.0;
        let x0 = (d.center[0] - r).floor().clamp(-1e9, 1e9) as i32;
        let y0 = (d.center[1] - r).floor().clamp(-1e9, 1e9) as i32;
        let x1 = (d.center[0] + r).ceil().clamp(-1e9, 1e9) as i32;
        let y1 = (d.center[1] + r).ceil().clamp(-1e9, 1e9) as i32;
        b.union(&photocraft_geom::Rect::new(x0, y0, x1.max(x0 + 1), y1.max(y0 + 1)))
    })
}

/// Large Brush Mode's slots and coverage side for a card with `vram` bytes of video memory
/// (`None`: unknown or shared memory): each slot's coverage takes up to side² × 4 bytes
/// (`R32Float`), so 8 GB cards get 2 slots of 4096 (128 MB), 16 GB 3 of 6144 (450 MB), larger
/// cards 4 of 8192 (1 GB). A smaller side only softens the preview of a huge document seen
/// close up; the written stroke is full precision either way.
pub(super) fn card_tier(vram: Option<u64>) -> (usize, u32) {
    match vram {
        Some(v) if v > 18 << 30 => (SLOTS, MAX_SIDE),
        Some(v) if v > 10 << 30 => (3, 6144),
        _ => (2, 4096),
    }
}

impl StrokeGpu {
    /// Fit the slots and coverage size to a card with `vram` bytes ([`card_tier`]).
    pub(super) fn set_card(&mut self, vram: Option<u64>) {
        (self.max_slots, self.max_side) = card_tier(vram);
    }

    /// Lay every stroke of `doc` painted into a layer over the canvas instead.
    pub(super) fn lay_over_canvas(&mut self, doc: u64, queue: &wgpu::Queue) {
        for s in self.slots.iter_mut().flatten().filter(|s| s.doc == doc && s.layer.is_some()) {
            s.layer = None;
            // Its paint was in the document's values; over the canvas it is the display's.
            if s.linear {
                s.color = s.color.map(srgb_encode);
            }
        }
        self.sync(queue);
    }

    /// The strokes of `doc` painted into layers, oldest first, for the compositor.
    pub(super) fn live_paint(&self, doc: u64) -> Option<photocraft_gpu::LivePaint> {
        let slots: Vec<photocraft_gpu::LiveSlot> = self
            .order
            .iter()
            .filter_map(|&i| self.slots.get(i).and_then(Option::as_ref))
            .filter(|s| s.visible && s.doc == doc && !s.bounds.is_empty())
            .filter_map(|s| {
                let (layer, lock) = s.layer?;
                Some(photocraft_gpu::LiveSlot {
                    id: s.id,
                    layer,
                    coverage: s.view.clone(),
                    coverage_size: s.size,
                    scale: s.scale,
                    color: s.color,
                    opacity: s.opacity,
                    lock,
                    mode: s.blend.0,
                    float: s.blend.1,
                    hide: s.hide,
                    bounds: s.bounds,
                })
            })
            .collect();
        (!slots.is_empty()).then_some(photocraft_gpu::LivePaint { doc: photocraft_doc::DocId(doc), slots })
    }
}

/// The overlay uniform for `slots` shown in `order`.
fn overlay_uniform(slots: &[Option<Slot>; SLOTS], order: &[usize]) -> Vec<u8> {
    let mut f = Vec::with_capacity(SLOTS * 8 + 8);
    for s in slots {
        match s.as_ref().filter(|s| s.visible) {
            Some(s) => f.extend_from_slice(&[s.color[0], s.color[1], s.color[2], s.opacity]),
            None => f.extend_from_slice(&[0.0; 4]),
        }
    }
    for s in slots {
        match s.as_ref().filter(|s| s.visible) {
            Some(s) => f.extend_from_slice(&[s.scale, if s.layer.is_none() { 1.0 } else { 0.0 }, if s.linear { 1.0 } else { 0.0 }, 0.0]),
            None => f.extend_from_slice(&[1.0, 0.0, 0.0, 0.0]),
        }
    }
    let mut o = [0.0f32; 4];
    for (k, &i) in order.iter().take(SLOTS).enumerate() {
        o[k] = i as f32;
    }
    f.extend_from_slice(&o);
    f.extend_from_slice(&[order.len().min(SLOTS) as f32, 0.0, 0.0, 0.0]);
    for s in slots {
        f.extend_from_slice(&s.as_ref().map_or([0.0; 4], |s| s.hide));
    }
    f32_bytes(&f)
}

fn overlay_bind_group(device: &wgpu::Device, bgl: &wgpu::BindGroupLayout, views: [&wgpu::TextureView; SLOTS], uniform: &wgpu::Buffer) -> wgpu::BindGroup {
    let mut entries: Vec<wgpu::BindGroupEntry> =
        views.iter().enumerate().map(|(i, v)| wgpu::BindGroupEntry { binding: i as u32, resource: wgpu::BindingResource::TextureView(v) }).collect();
    entries.push(wgpu::BindGroupEntry { binding: SLOTS as u32, resource: uniform.as_entire_binding() });
    device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("pc_stroke_overlay"), layout: bgl, entries: &entries })
}

impl StrokeGpu {
    pub(super) fn new(device: &wgpu::Device, queue: &wgpu::Queue, exact: bool) -> Self {
        let mut entries: Vec<wgpu::BindGroupLayoutEntry> = (0..SLOTS as u32)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    // Read with textureLoad (`R32Float` isn't filterable everywhere).
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            })
            .collect();
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: SLOTS as u32,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: std::num::NonZeroU64::new(OVERLAY_UNIFORM),
            },
            count: None,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("pc_stroke_overlay"), entries: &entries });
        let dummy = coverage_texture(device, 1, 1, false);
        let dummy_view = dummy.create_view(&wgpu::TextureViewDescriptor::default());
        let none: [Option<Slot>; SLOTS] = Default::default();
        let idle_uniform =
            init_buffer(device, "pc_stroke_overlay_idle", &overlay_uniform(&none, &[]), wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST);
        let idle = overlay_bind_group(device, &bgl, [&dummy_view; SLOTS], &idle_uniform);
        let uniform = init_buffer(device, "pc_stroke_overlay", &overlay_uniform(&none, &[]), wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST);
        let _ = queue;
        let dab_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pc_stroke_dabs"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(DAB_UNIFORM),
                },
                count: None,
            }],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("pc_stroke_dabs"), source: wgpu::ShaderSource::Wgsl(DAB_WGSL.into()) });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pc_stroke_dabs"),
            bind_group_layouts: &[Some(&dab_bgl)],
            immediate_size: 0,
        });
        // Build-up as the CPU stroke buffer: c' = v + c·(1 − v).
        let build_up = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::OneMinusSrc, operation: wgpu::BlendOperation::Add };
        let attributes = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 },
        ];
        let dab_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pc_stroke_dabs"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (DAB_FLOATS * 4) as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
                compilation_options: Default::default(),
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: 1, mask: !0, alpha_to_coverage_enabled: false },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R16Float,
                    blend: Some(wgpu::BlendState { color: build_up, alpha: build_up }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let exact = exact.then(|| {
            let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("pc_stroke_dabs_exact"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: std::num::NonZeroU64::new(EXACT_UNIFORM),
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: true }, has_dynamic_offset: false, min_binding_size: None },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::ReadWrite,
                            format: wgpu::TextureFormat::R32Float,
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ],
            });
            let module =
                device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("pc_stroke_dabs_exact"), source: wgpu::ShaderSource::Wgsl(EXACT_DAB_WGSL.into()) });
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pc_stroke_dabs_exact"),
                bind_group_layouts: &[Some(&bgl)],
                immediate_size: 0,
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("pc_stroke_dabs_exact"),
                layout: Some(&layout),
                module: &module,
                entry_point: Some("cs"),
                compilation_options: Default::default(),
                cache: None,
            });
            (pipeline, bgl)
        });
        Self {
            exact,
            bgl,
            idle,
            dummy_view,
            _dummy: dummy,
            _idle_uniform: idle_uniform,
            uniform,
            bind_group: None,
            dab_bgl,
            dab_pipeline,
            slots: Default::default(),
            order: Vec::new(),
            next_id: 0,
            max_slots: SLOTS,
            max_side: MAX_SIDE,
        }
    }

    /// Bind group 3 for a view of document `doc`: the strokes showing on it, else the idle one.
    pub(super) fn bind_group(&self, doc: u64) -> &wgpu::BindGroup {
        let showing = self.order.iter().any(|&i| self.slots[i].as_ref().is_some_and(|s| s.visible && s.doc == doc));
        match &self.bind_group {
            Some(bg) if showing => bg,
            _ => &self.idle,
        }
    }

    /// Write the uniform for the slots as they are now.
    fn sync(&mut self, queue: &wgpu::Queue) {
        self.order.retain(|&i| self.slots[i].as_ref().is_some_and(|s| s.visible));
        queue.write_buffer(&self.uniform, 0, &overlay_uniform(&self.slots, &self.order));
    }

    /// Rebuild the bind group over the slots' textures.
    fn rebind(&mut self, device: &wgpu::Device) {
        let views: [&wgpu::TextureView; SLOTS] = std::array::from_fn(|i| self.slots[i].as_ref().map_or(&self.dummy_view, |s| &s.view));
        self.bind_group = Some(overlay_bind_group(device, &self.bgl, views, &self.uniform));
    }
}

/// Clear `view` to no coverage.
fn clear(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_stroke_clear") });
    drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("pc_stroke_clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    }));
    queue.submit([encoder.finish()]);
}

impl GpuCanvas {
    /// Start a Large Brush Mode stroke on document `doc` (`doc_size` px, viewed at `zoom` screen
    /// pixels per document pixel) in a free overlay slot: an empty coverage texture, painted
    /// `color` (the document's own encoding; sRGB-encoded here for a `linear` document, as its
    /// display texture stores it) at `opacity`. The slot, or `None` when every slot is taken (the
    /// caller lands a queued stroke first) or the GPU can't.
    /// `layer`: paint the stroke into that layer in its place in the stack (with its transparency
    /// locked or not), `color` in the document's values and `opacity` the brush's own; `None`:
    /// lay it over the canvas, `opacity` including the layer's. `blend`: the brush's blend mode
    /// and whether the document is 32-bit float; only a stroke painted into its layer can show a
    /// mode other than Normal.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_begin(
        &self,
        doc: u64,
        doc_size: [u32; 2],
        zoom: f32,
        color: [f32; 3],
        opacity: f32,
        linear: bool,
        layer: Option<(photocraft_doc::LayerId, bool)>,
        blend: (photocraft_color::BlendMode, bool),
    ) -> Option<usize> {
        if self.fault().is_some() || doc_size[0] == 0 || doc_size[1] == 0 {
            return None;
        }
        let (device, queue) = (&self.rs.device, &self.rs.queue);
        // Over the canvas the paint is in the display texture's encoding; in a layer, the
        // document's own values.
        let color = if linear && layer.is_none() { color.map(srgb_encode) } else { color.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 }) };
        let mut r = self.rs.renderer.write();
        let res = r.callback_resources.get_mut::<Resources>()?;
        let g = &mut res.stroke;
        let free = (0..g.max_slots.min(SLOTS)).find(|&i| g.slots[i].as_ref().is_none_or(|s| !s.visible))?;
        let scale = coverage_scale(doc_size[0], doc_size[1], device.limits().max_texture_dimension_2d.min(g.max_side), zoom);
        let size = [((doc_size[0] as f32 * scale).ceil() as u32).max(1), ((doc_size[1] as f32 * scale).ceil() as u32).max(1)];
        let reuse = g.slots[free].as_ref().is_some_and(|s| s.doc_size == doc_size && s.size == size);
        if !reuse {
            let texture = coverage_texture(device, size[0], size[1], g.exact.is_some());
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let dab_uniform = init_buffer(
                device,
                "pc_stroke_dabs",
                &f32_bytes(&[scale, size[0] as f32, size[1] as f32, 0.0]),
                wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            );
            let dab_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_stroke_dabs"),
                layout: &g.dab_bgl,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: dab_uniform.as_entire_binding() }],
            });
            g.slots[free] = Some(Slot {
                doc,
                doc_size,
                size,
                scale,
                color,
                opacity,
                linear,
                view,
                _texture: texture,
                dab_bind_group,
                _dab_uniform: dab_uniform,
                visible: false,
                hide: [0.0; 4],
                layer: None,
                blend: (photocraft_color::BlendMode::Normal, false),
                bounds: photocraft_geom::Rect::EMPTY,
                id: 0,
            });
            g.rebind(device);
        } else if g.bind_group.is_none() {
            g.rebind(device);
        }
        g.next_id += 1;
        let id = g.next_id;
        let s = g.slots[free].as_mut()?;
        (s.doc, s.color, s.opacity, s.linear, s.visible, s.hide) = (doc, color, opacity, linear, true, [0.0; 4]);
        (s.layer, s.blend, s.bounds, s.id) = (layer, blend, photocraft_geom::Rect::EMPTY, id);
        clear(device, queue, &s.view);
        g.order.retain(|&i| i != free);
        g.order.push(free);
        g.sync(queue);
        Some(free)
    }

    /// Draw `dabs` into stroke `slot`'s coverage.
    pub fn stroke_dabs(&self, slot: usize, dabs: &[photocraft_engine::brush_cmds::gpu_preview::GpuDab]) {
        if dabs.is_empty() || self.fault().is_some() {
            return;
        }
        let (device, queue) = (&self.rs.device, &self.rs.queue);
        // Where they reach: the compositor paints a layer's stroke there.
        let reach = dab_bounds(dabs);
        {
            let mut w = self.rs.renderer.write();
            if let Some(s) = w.callback_resources.get_mut::<Resources>().and_then(|res| res.stroke.slots.get_mut(slot)).and_then(Option::as_mut) {
                s.bounds = s.bounds.union(&reach);
            }
        }
        let r = self.rs.renderer.read();
        let Some(res) = r.callback_resources.get::<Resources>() else { return };
        let Some(s) = res.stroke.slots.get(slot).and_then(Option::as_ref).filter(|s| s.visible) else { return };
        let usable = |d: &&photocraft_engine::brush_cmds::gpu_preview::GpuDab| {
            d.alpha > 0.0 && d.ceil > 0.0 && d.reach.is_finite() && d.center.iter().all(|v| v.is_finite())
        };
        if let Some((pipeline, bgl)) = &res.stroke.exact {
            // The texels the dabs reach.
            let (x0, y0) = ((reach.x0 as f32 * s.scale).floor().max(0.0) as i32, (reach.y0 as f32 * s.scale).floor().max(0.0) as i32);
            let x1 = ((reach.x1 as f32 * s.scale).ceil() as i32).min(s.size[0] as i32);
            let y1 = ((reach.y1 as f32 * s.scale).ceil() as i32).min(s.size[1] as i32);
            let floats: Vec<f32> = dabs
                .iter()
                .filter(usable)
                .flat_map(|d| [d.center[0], d.center[1], d.angle, d.roundness, d.core, d.shoulder.max(1e-3), d.reach, d.alpha, d.ceil, 0.0, 0.0, 0.0])
                .collect();
            let n = floats.len() / EXACT_DAB_FLOATS;
            if n == 0 || x1 <= x0 || y1 <= y0 {
                return;
            }
            let mut u: Vec<u8> = Vec::with_capacity(EXACT_UNIFORM as usize);
            u.extend(f32_bytes(&[s.scale, s.size[0] as f32, s.size[1] as f32, 0.0]));
            u.extend([x0, y0, x1, y1].iter().flat_map(|v| v.to_ne_bytes()));
            u.extend([n as u32, 0, 0, 0].iter().flat_map(|v| v.to_ne_bytes()));
            let uniform = init_buffer(device, "pc_stroke_dabs_exact", &u, wgpu::BufferUsages::UNIFORM);
            let list = init_buffer(device, "pc_stroke_dabs_exact_list", &f32_bytes(&floats), wgpu::BufferUsages::STORAGE);
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_stroke_dabs_exact"),
                layout: bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: list.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&s.view) },
                ],
            });
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_stroke_dabs_exact") });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("pc_stroke_dabs_exact"), timestamp_writes: None });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &bg, &[]);
                pass.dispatch_workgroups(((x1 - x0) as u32).div_ceil(8), ((y1 - y0) as u32).div_ceil(8), 1);
            }
            queue.submit([encoder.finish()]);
            return;
        }
        // Plain build-up: the ceiling scales the dab instead (see the module docs).
        let floats: Vec<f32> = dabs
            .iter()
            .filter(usable)
            .flat_map(|d| [d.center[0], d.center[1], d.angle, d.roundness, d.core, d.shoulder.max(1e-3), d.reach, (d.alpha * d.ceil).clamp(0.0, 1.0)])
            .collect();
        let n = (floats.len() / DAB_FLOATS) as u32;
        if n == 0 {
            return;
        }
        let instances = init_buffer(device, "pc_stroke_dab_instances", &f32_bytes(&floats), wgpu::BufferUsages::VERTEX);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_stroke_dabs") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pc_stroke_dabs"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &s.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&res.stroke.dab_pipeline);
            pass.set_bind_group(0, &s.dab_bind_group, &[]);
            pass.set_vertex_buffer(0, instances.slice(..));
            // Dabs land in order, each building on the ones before (blending is ordered).
            pass.draw(0..6, 0..n);
        }
        queue.submit([encoder.finish()]);
    }

    /// Stop showing stroke `slot` (its write landed, or it was cancelled). The texture is kept
    /// for a later stroke on a document of the same size.
    pub fn stroke_end(&self, slot: usize) {
        let queue = &self.rs.queue;
        let mut r = self.rs.renderer.write();
        if let Some(res) = r.callback_resources.get_mut::<Resources>()
            && let Some(s) = res.stroke.slots.get_mut(slot).and_then(Option::as_mut)
        {
            s.visible = false;
            res.stroke.sync(queue);
        }
    }

    /// The canvas now draws stroke `slot`'s landed write inside `rect` (document pixels): the
    /// overlay stops laying the stroke over it there. A big stroke lands in bands of rows, so
    /// the overlay gives way band by band instead of all at once.
    pub fn stroke_hide(&self, slot: usize, rect: photocraft_geom::Rect) {
        let queue = &self.rs.queue;
        let mut r = self.rs.renderer.write();
        if let Some(res) = r.callback_resources.get_mut::<Resources>()
            && let Some(s) = res.stroke.slots.get_mut(slot).and_then(Option::as_mut)
        {
            s.hide = [rect.x0 as f32, rect.y0 as f32, rect.x1 as f32, rect.y1 as f32];
            res.stroke.sync(queue);
        }
    }

    /// Whether stroke `slot` is painted into its layer (not laid over the canvas).
    pub fn stroke_in_layer(&self, slot: usize) -> bool {
        let r = self.rs.renderer.read();
        r.callback_resources.get::<Resources>().and_then(|res| res.stroke.slots.get(slot)).and_then(Option::as_ref).is_some_and(|s| s.visible && s.layer.is_some())
    }

    /// Lay every stroke of `doc` painted into a layer over the canvas instead (the GPU
    /// compositor couldn't draw the document, so it can't paint them into their layers).
    pub fn stroke_to_overlay(&self, doc: u64) {
        let queue = &self.rs.queue;
        let mut r = self.rs.renderer.write();
        if let Some(res) = r.callback_resources.get_mut::<Resources>() {
            res.stroke.lay_over_canvas(doc, queue);
        }
    }

    /// Stop showing every stroke.
    pub fn stroke_end_all(&self) {
        let queue = &self.rs.queue;
        let mut r = self.rs.renderer.write();
        if let Some(res) = r.callback_resources.get_mut::<Resources>() {
            for s in res.stroke.slots.iter_mut().flatten() {
                s.visible = false;
            }
            res.stroke.sync(queue);
        }
    }

    /// Whether any stroke shows.
    pub fn stroke_showing(&self) -> bool {
        let r = self.rs.renderer.read();
        r.callback_resources.get::<Resources>().is_some_and(|res| !res.stroke.order.is_empty())
    }

    /// Whether a stroke could start now (a slot is free).
    pub fn stroke_slot_free(&self) -> bool {
        let r = self.rs.renderer.read();
        r.callback_resources
            .get::<Resources>()
            .is_some_and(|res| res.stroke.slots.iter().take(res.stroke.max_slots).any(|s| s.as_ref().is_none_or(|s| !s.visible)))
    }
}
