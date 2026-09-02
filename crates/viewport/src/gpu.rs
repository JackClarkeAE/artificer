//! GPU rendering pipeline and buffer cache for viewport shaded triangles using `wgpu`.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use egui_wgpu::wgpu;
use egui_wgpu::wgpu::util::DeviceExt;

use artificer_kernel::DebugScene;
use artificer_ui_core::presentation::ViewState;

const WGSL_SHADER: &str = r#"
struct Uniforms {
    view_proj: mat4x4<f32>,
    light_dir: vec4<f32>,
    section_plane: vec4<f32>,
    flags: vec4<u32>,
};

@group(0) @binding(0)
var<uniform> uniforms: Uniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) lighting: f32,
};

@vertex
fn vs_main(model: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.world_position = model.position;
    out.clip_position = uniforms.view_proj * vec4<f32>(model.position, 1.0);
    out.color = model.color;

    if (uniforms.flags.y != 0u) {
        let key_dir = normalize(uniforms.light_dir.xyz);
        let n_dot_key = max(dot(model.normal, key_dir), 0.0);
        let key = 0.62 * n_dot_key;

        let fill_dir = normalize(vec3<f32>(-key_dir.x, -key_dir.y * 0.5, key_dir.z));
        let n_dot_fill = max(dot(model.normal, fill_dir), 0.0);
        let fill = 0.20 * n_dot_fill;

        let hemi = 0.5 * (1.0 + model.normal.z);
        let ambient = 0.18 * hemi;

        let rim = 0.08;
        out.lighting = clamp(key + fill + ambient + rim, 0.12, 1.0);
    } else {
        out.lighting = 1.0;
    }

    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (uniforms.flags.x != 0u) {
        let dist = dot(in.world_position, uniforms.section_plane.xyz) + uniforms.section_plane.w;
        if (dist < 0.0) {
            discard;
        }
    }

    let rgb = in.color.rgb * in.lighting;
    return vec4<f32>(rgb, in.color.a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GpuVertex {
    pub position: [f32; 3],
    pub _pad0: f32,
    pub normal: [f32; 3],
    pub _pad1: f32,
    pub color: [f32; 4],
}

impl GpuVertex {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Self>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                offset: 0,
                shader_location: 0,
                format: wgpu::VertexFormat::Float32x3,
            },
            wgpu::VertexAttribute {
                offset: 16,
                shader_location: 1,
                format: wgpu::VertexFormat::Float32x3,
            },
            wgpu::VertexAttribute {
                offset: 32,
                shader_location: 2,
                format: wgpu::VertexFormat::Float32x4,
            },
        ],
    };
}

/// The neutral steel an unassigned body takes, matching the CPU path's own
/// unassigned shade.
pub const NEUTRAL_BODY_COLOR: [f32; 4] = [0.78, 0.82, 0.88, 1.0];

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GpuViewportUniforms {
    pub view_proj: [[f32; 4]; 4],
    pub light_dir: [f32; 4],
    pub section_plane: [f32; 4],
    pub flags: [u32; 4],
}

/// One body's uploaded facets, and the revision of the body they stand for.
pub struct BodyMeshGpuBuffer {
    pub vertex_buffer: wgpu::Buffer,
    pub vertex_count: u32,
    /// What the buffer holds. A body whose revision has moved on is
    /// re-uploaded over this entry rather than drawn stale.
    revision: u64,
}

/// One vertex buffer per body, replaced when that body changes and dropped
/// when it leaves the frame.
///
/// Both halves matter. Keying on the body alone drew the geometry a body
/// used to have after every edit; keeping the entries forever leaked a
/// buffer per body per colour, which a per-frame heat map turns into a
/// buffer per frame.
#[derive(Default)]
pub struct BodyMeshCache {
    buffers: HashMap<u64, BodyMeshGpuBuffer>,
}

/// One body as the callback carries it to the GPU.
#[derive(Clone)]
pub struct GpuBody {
    pub key: u64,
    /// Changes whenever the body's facets or colours do.
    pub revision: u64,
    pub scene: DebugScene,
    pub tint: Option<[f32; 4]>,
    /// One colour per facet corner in scene order, taking the place of the
    /// tint wherever it reaches. This is the analysis heat map.
    pub colors: Option<Vec<[f32; 4]>>,
}

/// One body's facets as the vertex buffer holds them.
///
/// The per-corner colours take precedence where they reach, the tint stands
/// in where they do not, and an unassigned body falls back to the same
/// neutral steel the software renderer paints it.
#[must_use]
pub fn body_vertices(body: &GpuBody) -> Vec<GpuVertex> {
    let default_color = body.tint.unwrap_or(NEUTRAL_BODY_COLOR);
    let mut vertices = Vec::with_capacity(body.scene.triangles.len() * 3);
    for (facet, triangle) in body.scene.triangles.iter().enumerate() {
        let normals = triangle.normals;
        for (corner, &point) in triangle.vertices.iter().enumerate() {
            let normal = normals[corner];
            let color = body
                .colors
                .as_ref()
                .and_then(|colors| colors.get(facet * 3 + corner))
                .copied()
                .unwrap_or(default_color);
            vertices.push(GpuVertex {
                position: [point.x as f32, point.y as f32, point.z as f32],
                _pad0: 0.0,
                normal: [normal.x as f32, normal.y as f32, normal.z as f32],
                _pad1: 0.0,
                color,
            });
        }
    }
    vertices
}

impl BodyMeshCache {
    pub fn get_or_upload(
        &mut self,
        device: &wgpu::Device,
        body: &GpuBody,
    ) -> Option<&BodyMeshGpuBuffer> {
        let stale = self
            .buffers
            .get(&body.key)
            .is_none_or(|held| held.revision != body.revision);
        if stale {
            let vertices = body_vertices(body);
            if vertices.is_empty() {
                self.buffers.remove(&body.key);
                return None;
            }

            let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("artificer.viewport.body_mesh"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });

            self.buffers.insert(
                body.key,
                BodyMeshGpuBuffer {
                    vertex_buffer,
                    vertex_count: vertices.len() as u32,
                    revision: body.revision,
                },
            );
        }

        self.buffers.get(&body.key)
    }

    /// Drops every body the frame no longer draws.
    pub fn retain_live(&mut self, live: &[GpuBody]) {
        self.buffers
            .retain(|key, _| live.iter().any(|body| body.key == *key));
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }

    pub fn clear(&mut self) {
        self.buffers.clear();
    }
}

pub struct GpuViewportPipeline {
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    mesh_cache: BodyMeshCache,
    active_format: wgpu::TextureFormat,
}

impl GpuViewportPipeline {
    pub fn new(device: &wgpu::Device, initial_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("artificer.viewport.shader"),
            source: wgpu::ShaderSource::Wgsl(WGSL_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("artificer.viewport.bind_group_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("artificer.viewport.uniform_buffer"),
            size: std::mem::size_of::<GpuViewportUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("artificer.viewport.bind_group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("artificer.viewport.pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let mut slf = Self {
            shader,
            pipeline_layout,
            pipelines: HashMap::new(),
            uniform_buffer,
            bind_group,
            mesh_cache: BodyMeshCache::default(),
            active_format: initial_format,
        };

        for format in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ] {
            slf.ensure_pipeline(device, format);
        }

        slf
    }

    pub fn ensure_pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.pipelines.contains_key(&format) {
            return;
        }

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("artificer.viewport.render_pipeline"),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs_main"),
                buffers: &[GpuVertex::LAYOUT],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: 1,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            multiview_mask: None,
            cache: None,
        });

        self.pipelines.insert(format, pipeline);
    }

    pub fn mesh_cache_mut(&mut self) -> &mut BodyMeshCache {
        &mut self.mesh_cache
    }

    pub fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        view: ViewState,
        aspect_ratio: f32,
        is_shaded: bool,
    ) {
        let mvp = view.view_projection_matrix(aspect_ratio);
        let section = view.section_cut_plane.unwrap_or_default();

        let uniforms = GpuViewportUniforms {
            view_proj: mvp,
            light_dir: [-0.35, 0.82, 0.45, 0.0],
            section_plane: [
                section.normal.x as f32,
                section.normal.y as f32,
                section.normal.z as f32,
                section.offset as f32,
            ],
            flags: [
                if section.active && view.section_cut_plane.is_some() {
                    1
                } else {
                    0
                },
                if is_shaded { 1 } else { 0 },
                0,
                0,
            ],
        };

        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));
    }
}

pub struct ViewportGpuCallback {
    pub view: ViewState,
    pub aspect_ratio: f32,
    pub is_shaded: bool,
    pub target_format: wgpu::TextureFormat,
    pub bodies: Vec<GpuBody>,
}

impl ViewportGpuCallback {
    #[must_use]
    pub fn new(view: ViewState, aspect_ratio: f32, is_shaded: bool, bodies: Vec<GpuBody>) -> Self {
        Self {
            view,
            aspect_ratio,
            is_shaded,
            target_format: wgpu::TextureFormat::Rgba8Unorm,
            bodies,
        }
    }
}

impl egui_wgpu::CallbackTrait for ViewportGpuCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let pipeline = callback_resources
            .entry::<GpuViewportPipeline>()
            .or_insert_with(|| GpuViewportPipeline::new(device, self.target_format));

        pipeline.active_format = self.target_format;
        pipeline.ensure_pipeline(device, self.target_format);
        pipeline.prepare(queue, self.view, self.aspect_ratio, self.is_shaded);

        for body in &self.bodies {
            pipeline.mesh_cache_mut().get_or_upload(device, body);
        }
        // A body hidden, deleted or moved to another document keeps no
        // buffer: the cache holds exactly what the frame draws.
        pipeline.mesh_cache_mut().retain_live(&self.bodies);

        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(pipeline) = callback_resources.get::<GpuViewportPipeline>() {
            let active_pipe = pipeline
                .pipelines
                .get(&pipeline.active_format)
                .or_else(|| pipeline.pipelines.get(&wgpu::TextureFormat::Rgba8Unorm))
                .unwrap_or_else(|| pipeline.pipeline_for_fallback());

            for body in &self.bodies {
                if let Some(buffer) = pipeline.mesh_cache.buffers.get(&body.key) {
                    render_pass.set_pipeline(active_pipe);
                    render_pass.set_bind_group(0, &pipeline.bind_group, &[]);
                    render_pass.set_vertex_buffer(0, buffer.vertex_buffer.slice(..));
                    render_pass.draw(0..buffer.vertex_count, 0..1);
                }
            }
        }
    }
}

impl GpuViewportPipeline {
    fn pipeline_for_fallback(&self) -> &wgpu::RenderPipeline {
        self.pipelines
            .values()
            .next()
            .expect("GpuViewportPipeline must have at least one pipeline")
    }
}
