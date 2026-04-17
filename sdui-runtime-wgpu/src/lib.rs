//! wgpu renderer for `sdui-core` scene graphs.
//!
//! Iteration 4: renders Frame quads + Image textured quads + Text overlay,
//! all in a single render pass.
//!
//! Render order within the pass (painter's algorithm):
//!   1. Frame quads (per-vertex colour pipeline)
//!   2. Image quads (textured pipeline, one draw per image)
//!   3. Text overlay (glyphon TextRenderer)
//!
//! Missing-image handling: a cache miss renders a solid 50% gray rect via
//! the frame pipeline as a placeholder.
//!
//! API references:
//!   - wgpu RenderPipeline: <https://docs.rs/wgpu/29/wgpu/struct.RenderPipeline.html>
//!   - wgpu Texture sampling tutorial: <https://sotrh.github.io/learn-wgpu/beginner/tutorial5-textures/>
//!   - image crate: <https://docs.rs/image/0.25/image/>
//!   - glyphon TextRenderer: <https://docs.rs/glyphon/0.11/glyphon/struct.TextRenderer.html>

use bytemuck::{Pod, Zeroable};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use sdui_core::{flatten_scene, FlatNodeKind, ImageSource, SceneNode};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

/// Noto Sans Regular — bundled for cross-platform consistency (WASM lacks system fonts).
/// License: SIL Open Font License (see `assets/fonts/OFL.txt`).
const FONT_DATA: &[u8] = include_bytes!("../../assets/fonts/NotoSans-Regular.ttf");

// ── WGSL shader (inline) ──────────────────────────────────────────────

const SHADER_SRC: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

const IMAGE_SHADER_SRC: &str = r#"
struct ImageVertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct ImageVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: ImageVertexInput) -> ImageVertexOutput {
    var out: ImageVertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

@group(0) @binding(0) var t_image: texture_2d<f32>;
@group(0) @binding(1) var s_image: sampler;

@fragment
fn fs_main(in: ImageVertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_image, s_image, in.uv);
}
"#;

// ── Vertex types ──────────────────────────────────────────────────────

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 4],
}

impl Vertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct ImageVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

impl ImageVertex {
    const ATTRIBS: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2];

    fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<ImageVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &Self::ATTRIBS,
        }
    }
}

/// GPU-side handle for a decoded, uploaded image.
pub struct ImageGpuHandle {
    #[allow(dead_code)] // owned by the bind group; kept alive here for clarity
    pub texture: wgpu::Texture,
    #[allow(dead_code)]
    pub view: wgpu::TextureView,
    pub bind_group: wgpu::BindGroup,
    pub width: u32,
    pub height: u32,
}

/// 50% gray placeholder colour for images that haven't loaded yet.
const PLACEHOLDER_COLOR: [f32; 4] = [0.5, 0.5, 0.5, 1.0];

/// Cross-target warning logger. `eprintln!` works on both native and
/// `wasm32-unknown-unknown` (wasm-bindgen routes stderr through the console
/// when `console_error_panic_hook` is installed, which app-web does).
fn log_warn(msg: &str) {
    eprintln!("[sdui-runtime-wgpu] {msg}");
}

// ── Renderer ──────────────────────────────────────────────────────────

/// wgpu renderer that draws Frame quads, Image quads (textured), and Text via glyphon.
pub struct Renderer {
    render_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    image_bind_group_layout: wgpu::BindGroupLayout,
    image_sampler: wgpu::Sampler,
    /// Decoded + uploaded images keyed by their wire-level `ImageSource`.
    /// No eviction policy — MVP accepts unbounded growth.
    image_cache: HashMap<ImageSource, ImageGpuHandle>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    // glyphon text rendering state
    font_system: FontSystem,
    swash_cache: SwashCache,
    #[allow(dead_code)] // held alive for Viewport/TextAtlas internal references
    cache: Cache,
    viewport: Viewport,
    text_atlas: TextAtlas,
    text_renderer: TextRenderer,
}

impl Renderer {
    /// Create the renderer with quad pipeline + glyphon text pipeline.
    ///
    /// `surface_format` must match the surface the renderer will draw to.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        // ── Quad pipeline ──
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sdui frame shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SRC.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sdui pipeline layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sdui render pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Vertex::layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
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

        // ── Image pipeline (textured quad with sampler) ──
        let image_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sdui image shader"),
            source: wgpu::ShaderSource::Wgsl(IMAGE_SHADER_SRC.into()),
        });

        let image_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("sdui image bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let image_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("sdui image pipeline layout"),
                bind_group_layouts: &[Some(&image_bind_group_layout)],
                immediate_size: 0,
            });

        let image_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sdui image pipeline"),
            layout: Some(&image_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &image_shader,
                entry_point: Some("vs_main"),
                buffers: &[ImageVertex::layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &image_shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
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

        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("sdui image sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // ── glyphon text pipeline ──
        let mut font_system = FontSystem::new();
        font_system.db_mut().load_font_data(FONT_DATA.to_vec());

        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut text_atlas = TextAtlas::new(&device, &queue, &cache, surface_format);
        let text_renderer = TextRenderer::new(
            &mut text_atlas,
            &device,
            wgpu::MultisampleState::default(),
            None,
        );

        Self {
            render_pipeline,
            image_pipeline,
            image_bind_group_layout,
            image_sampler,
            image_cache: HashMap::new(),
            device,
            queue,
            font_system,
            swash_cache,
            cache,
            viewport,
            text_atlas,
            text_renderer,
        }
    }

    /// Reference to the wgpu device owned by this renderer.
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Reference to the wgpu queue owned by this renderer.
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Decode PNG bytes and upload them as an Rgba8UnormSrgb texture keyed by
    /// `source`. Existing entries for the same key are overwritten.
    ///
    /// Silently warns (stderr / web console) on decode failure and returns
    /// without inserting — the cache-miss placeholder keeps rendering.
    pub fn register_image(&mut self, source: &ImageSource, bytes: &[u8]) {
        let decoded = match image::load_from_memory(bytes) {
            Ok(img) => img.to_rgba8(),
            Err(e) => {
                log_warn(&format!("image decode failed ({e})"));
                return;
            }
        };
        let (width, height) = decoded.dimensions();
        let raw = decoded.into_raw();

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sdui image texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &raw,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sdui image bind group"),
            layout: &self.image_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.image_sampler),
                },
            ],
        });

        self.image_cache.insert(
            source.clone(),
            ImageGpuHandle {
                texture,
                view,
                bind_group,
                width,
                height,
            },
        );
    }

    /// Render the entire scene tree (Frame quads + Image quads + Text).
    ///
    /// Flattens the tree, then in one render pass draws Frame quads first,
    /// then each Image as a textured quad (or a gray placeholder if the
    /// bitmap isn't in `image_cache`), then the Text overlay via glyphon.
    pub fn render_scene(
        &mut self,
        scene: &SceneNode,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        surface_size: (u32, u32),
    ) {
        let flat = flatten_scene(scene);
        let (sw, sh) = (surface_size.0 as f32, surface_size.1 as f32);

        let push_quad = |vertices: &mut Vec<Vertex>,
                         abs_x: f32,
                         abs_y: f32,
                         width: f32,
                         height: f32,
                         color: [f32; 4]| {
            let ndc_left = (abs_x / sw) * 2.0 - 1.0;
            let ndc_right = ((abs_x + width) / sw) * 2.0 - 1.0;
            let ndc_top = 1.0 - (abs_y / sh) * 2.0;
            let ndc_bottom = 1.0 - ((abs_y + height) / sh) * 2.0;

            vertices.extend_from_slice(&[
                Vertex {
                    position: [ndc_left, ndc_top],
                    color,
                },
                Vertex {
                    position: [ndc_right, ndc_top],
                    color,
                },
                Vertex {
                    position: [ndc_left, ndc_bottom],
                    color,
                },
                Vertex {
                    position: [ndc_left, ndc_bottom],
                    color,
                },
                Vertex {
                    position: [ndc_right, ndc_top],
                    color,
                },
                Vertex {
                    position: [ndc_right, ndc_bottom],
                    color,
                },
            ]);
        };

        // ── Build Frame quad vertices, plus a placeholder quad for any Image
        // whose source is not in the cache yet.
        let mut vertices: Vec<Vertex> = Vec::new();
        for node in &flat {
            match &node.kind {
                FlatNodeKind::Frame { background_color } => {
                    push_quad(
                        &mut vertices,
                        node.abs_x,
                        node.abs_y,
                        node.width,
                        node.height,
                        *background_color,
                    );
                }
                FlatNodeKind::Image { source, .. } => {
                    if !self.image_cache.contains_key(source) {
                        push_quad(
                            &mut vertices,
                            node.abs_x,
                            node.abs_y,
                            node.width,
                            node.height,
                            PLACEHOLDER_COLOR,
                        );
                    }
                }
                FlatNodeKind::Text { .. } => {}
            }
        }

        // ── Build Image draw list (one vertex buffer per cached image). ──
        struct ImageDraw {
            vertex_buffer: wgpu::Buffer,
            vertex_count: u32,
            // Deferred lookup by key, resolved at draw time (lets us borrow
            // &self.image_cache during the pass without re-borrowing &mut).
            source_key: ImageSource,
        }
        let mut image_draws: Vec<ImageDraw> = Vec::new();
        for node in &flat {
            if let FlatNodeKind::Image { source, .. } = &node.kind {
                if !self.image_cache.contains_key(source) {
                    continue;
                }
                let ndc_left = (node.abs_x / sw) * 2.0 - 1.0;
                let ndc_right = ((node.abs_x + node.width) / sw) * 2.0 - 1.0;
                let ndc_top = 1.0 - (node.abs_y / sh) * 2.0;
                let ndc_bottom = 1.0 - ((node.abs_y + node.height) / sh) * 2.0;

                // UV origin (0,0) is the top-left of the texture — matches our
                // top-left scene-space convention. Fit is always stretch.
                let verts = [
                    ImageVertex {
                        position: [ndc_left, ndc_top],
                        uv: [0.0, 0.0],
                    },
                    ImageVertex {
                        position: [ndc_right, ndc_top],
                        uv: [1.0, 0.0],
                    },
                    ImageVertex {
                        position: [ndc_left, ndc_bottom],
                        uv: [0.0, 1.0],
                    },
                    ImageVertex {
                        position: [ndc_left, ndc_bottom],
                        uv: [0.0, 1.0],
                    },
                    ImageVertex {
                        position: [ndc_right, ndc_top],
                        uv: [1.0, 0.0],
                    },
                    ImageVertex {
                        position: [ndc_right, ndc_bottom],
                        uv: [1.0, 1.0],
                    },
                ];
                let vb = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("scene image vertex buffer"),
                        contents: bytemuck::cast_slice(&verts),
                        usage: wgpu::BufferUsages::VERTEX,
                    });
                image_draws.push(ImageDraw {
                    vertex_buffer: vb,
                    vertex_count: verts.len() as u32,
                    source_key: source.clone(),
                });
            }
        }

        // ── Build Text buffers and metadata ──
        let mut text_buffers: Vec<Buffer> = Vec::new();
        let mut text_metas: Vec<(f32, f32, f32, f32, [f32; 4])> = Vec::new();

        for node in &flat {
            if let FlatNodeKind::Text {
                content,
                font_size,
                color,
            } = &node.kind
            {
                let line_height = (*font_size * 1.2).ceil();
                let mut buffer =
                    Buffer::new(&mut self.font_system, Metrics::new(*font_size, line_height));
                buffer.set_size(&mut self.font_system, Some(node.width), Some(node.height));
                buffer.set_text(
                    &mut self.font_system,
                    content,
                    &Attrs::new().family(Family::Name("Noto Sans")),
                    Shaping::Advanced,
                    None,
                );
                buffer.shape_until_scroll(&mut self.font_system, false);
                text_buffers.push(buffer);
                text_metas.push((node.abs_x, node.abs_y, node.width, node.height, *color));
            }
        }

        let text_areas: Vec<TextArea> = text_buffers
            .iter()
            .zip(text_metas.iter())
            .map(|(buf, &(left, top, w, h, color))| TextArea {
                buffer: buf,
                left,
                top,
                scale: 1.0,
                bounds: TextBounds {
                    left: left as i32,
                    top: top as i32,
                    right: (left + w) as i32,
                    bottom: (top + h) as i32,
                },
                default_color: Color::rgba(
                    (color[0] * 255.0) as u8,
                    (color[1] * 255.0) as u8,
                    (color[2] * 255.0) as u8,
                    (color[3] * 255.0) as u8,
                ),
                custom_glyphs: &[],
            })
            .collect();

        // ── Prepare text for rendering ──
        self.viewport.update(
            &self.queue,
            Resolution {
                width: surface_size.0,
                height: surface_size.1,
            },
        );
        self.text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.text_atlas,
                &self.viewport,
                text_areas,
                &mut self.swash_cache,
            )
            .expect("glyphon prepare");

        // ── Vertex buffer for Frame quads ──
        let vertex_buffer = if !vertices.is_empty() {
            Some(
                self.device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("scene vertex buffer"),
                        contents: bytemuck::cast_slice(&vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
            )
        } else {
            None
        };

        // ── Single render pass: Frame quads → Image quads → Text overlay. ──
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sdui render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.15,
                            g: 0.15,
                            b: 0.15,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // 1. Frame quads (incl. placeholders for missing images).
            if let Some(vb) = &vertex_buffer {
                pass.set_pipeline(&self.render_pipeline);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.draw(0..vertices.len() as u32, 0..1);
            }

            // 2. Image quads — one draw per cached image.
            if !image_draws.is_empty() {
                pass.set_pipeline(&self.image_pipeline);
                for draw in &image_draws {
                    let handle = self
                        .image_cache
                        .get(&draw.source_key)
                        .expect("cached just above");
                    pass.set_bind_group(0, &handle.bind_group, &[]);
                    pass.set_vertex_buffer(0, draw.vertex_buffer.slice(..));
                    pass.draw(0..draw.vertex_count, 0..1);
                }
            }

            // 3. Text overlay via glyphon.
            self.text_renderer
                .render(&self.text_atlas, &self.viewport, &mut pass)
                .expect("glyphon render");
        }
    }
}
