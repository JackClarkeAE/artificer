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

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GpuViewportUniforms {
    pub view_proj: [[f32; 4]; 4],
    pub light_dir: [f32; 4],
    pub section_plane: [f32; 4],
    pub flags: [u32; 4],
}

pub struct BodyMeshGpuBuffer {
    pub vertex_buffer: &'static wgpu::Buffer,
    pub vertex_count: u32,
}

#[derive(Default)]
pub struct BodyMeshCache {
    buffers: HashMap<u64, BodyMeshGpuBuffer>,
}

impl BodyMeshCache {
    pub fn get_or_upload(
        &mut self,
        device: &wgpu::Device,
        key: u64,
        scene: &DebugScene,
        tint: Option<[f32; 4]>,
    ) -> Option<&BodyMeshGpuBuffer> {
        if let std::collections::hash_map::Entry::Vacant(e) = self.buffers.entry(key) {
            let mut vertices = Vec::with_capacity(scene.triangles.len() * 3);
            let default_color = tint.unwrap_or([0.78, 0.82, 0.88, 1.0]);

            for triangle in &scene.triangles {
                let normals = triangle.normals;
                for (index, &point) in triangle.vertices.iter().enumerate() {
                    let normal = normals[index];
                    vertices.push(GpuVertex {
                        position: [point.x as f32, point.y as f32, point.z as f32],
                        _pad0: 0.0,
                        normal: [normal.x as f32, normal.y as f32, normal.z as f32],
                        _pad1: 0.0,
                        color: default_color,
                    });
                }
            }

            if vertices.is_empty() {
                return None;
            }

            let vertex_buffer = Box::leak(Box::new(device.create_buffer_init(
                &wgpu::util::BufferInitDescriptor {
                    label: Some("artificer.viewport.body_mesh"),
                    contents: bytemuck::cast_slice(&vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                },
            )));

            e.insert(BodyMeshGpuBuffer {
                vertex_buffer,
                vertex_count: vertices.len() as u32,
            });
        }

        self.buffers.get(&key)
    }

    pub fn clear(&mut self) {
        self.buffers.clear();
    }
}

pub struct GpuViewportPipeline {
    shader: &'static wgpu::ShaderModule,
    pipeline_layout: &'static wgpu::PipelineLayout,
    pipelines: HashMap<wgpu::TextureFormat, &'static wgpu::RenderPipeline>,
    uniform_buffer: &'static wgpu::Buffer,
    bind_group: &'static wgpu::BindGroup,
    mesh_cache: BodyMeshCache,
    active_format: wgpu::TextureFormat,
}

impl GpuViewportPipeline {
    pub fn new(device: &wgpu::Device, initial_format: wgpu::TextureFormat) -> Self {
        let shader = Box::leak(Box::new(device.create_shader_module(
            wgpu::ShaderModuleDescriptor {
                label: Some("artificer.viewport.shader"),
                source: wgpu::ShaderSource::Wgsl(WGSL_SHADER.into()),
            },
        )));

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

        let uniform_buffer = Box::leak(Box::new(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("artificer.viewport.uniform_buffer"),
            size: std::mem::size_of::<GpuViewportUniforms>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })));

        let bind_group = Box::leak(Box::new(device.create_bind_group(
            &wgpu::BindGroupDescriptor {
                label: Some("artificer.viewport.bind_group"),
                layout: &bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                }],
            },
        )));

        let pipeline_layout = Box::leak(Box::new(device.create_pipeline_layout(
            &wgpu::PipelineLayoutDescriptor {
                label: Some("artificer.viewport.pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            },
        )));

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

    pub fn ensure_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> &'static wgpu::RenderPipeline {
        if let Some(&pipeline) = self.pipelines.get(&format) {
            return pipeline;
        }

        let pipeline = Box::leak(Box::new(device.create_render_pipeline(
            &wgpu::RenderPipelineDescriptor {
                label: Some("artificer.viewport.render_pipeline"),
                layout: Some(self.pipeline_layout),
                vertex: wgpu::VertexState {
                    module: self.shader,
                    entry_point: Some("vs_main"),
                    buffers: &[GpuVertex::LAYOUT],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: self.shader,
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
            },
        )));

        self.pipelines.insert(format, pipeline);
        pipeline
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

        queue.write_buffer(self.uniform_buffer, 0, bytemuck::cast_slice(&[uniforms]));
    }
}

pub struct ViewportGpuCallback {
    pub view: ViewState,
    pub aspect_ratio: f32,
    pub is_shaded: bool,
    pub target_format: wgpu::TextureFormat,
    pub bodies: Vec<(u64, DebugScene, Option<[f32; 4]>)>,
}

impl ViewportGpuCallback {
    #[must_use]
    pub fn new(
        view: ViewState,
        aspect_ratio: f32,
        is_shaded: bool,
        bodies: Vec<(u64, DebugScene, Option<[f32; 4]>)>,
    ) -> Self {
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

        for (key, scene, tint) in &self.bodies {
            pipeline
                .mesh_cache_mut()
                .get_or_upload(device, *key, scene, *tint);
        }

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
                .copied()
                .unwrap_or(pipeline.pipeline_for_fallback());

            for (key, _, _) in &self.bodies {
                if let Some(buffer) = pipeline.mesh_cache.buffers.get(key) {
                    render_pass.set_pipeline(active_pipe);
                    render_pass.set_bind_group(0, pipeline.bind_group, &[]);
                    render_pass.set_vertex_buffer(0, buffer.vertex_buffer.slice(..));
                    render_pass.draw(0..buffer.vertex_count, 0..1);
                }
            }
        }
    }
}

impl GpuViewportPipeline {
    fn pipeline_for_fallback(&self) -> &'static wgpu::RenderPipeline {
        self.pipelines
            .values()
            .next()
            .copied()
            .expect("GpuViewportPipeline must have at least one pipeline")
    }
}
