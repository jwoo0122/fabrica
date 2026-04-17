//! wgpu renderer for `sdui-core` scene graphs.
//!
//! Iteration 3: renders Frame quads via per-vertex colour pipeline and
//! Text nodes via glyphon (cosmic-text based wgpu text renderer).
//!
//! Render order within a single render pass (painter's algorithm):
//!   1. Frame quads drawn first
//!   2. Text overlay rendered via glyphon's TextRenderer
//!
//! API references:
//!   - wgpu RenderPipeline: <https://docs.rs/wgpu/29/wgpu/struct.RenderPipeline.html>
//!   - glyphon TextRenderer: <https://docs.rs/glyphon/0.11/glyphon/struct.TextRenderer.html>
//!   - cosmic-text Buffer: <https://docs.rs/cosmic-text/latest/cosmic_text/struct.Buffer.html>

use bytemuck::{Pod, Zeroable};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use sdui_core::{flatten_scene, FlatNodeKind, SceneNode};
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

// ── Vertex type ───────────────────────────────────────────────────────

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

// ── Renderer ──────────────────────────────────────────────────────────

/// wgpu renderer that draws Frame quads and Text via glyphon.
pub struct Renderer {
    render_pipeline: wgpu::RenderPipeline,
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

    /// Render the entire scene tree (Frame quads + Text).
    ///
    /// Flattens the tree, draws Frame quads via the vertex pipeline, then
    /// overlays Text via glyphon — all in a single render pass.
    pub fn render_scene(
        &mut self,
        scene: &SceneNode,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        surface_size: (u32, u32),
    ) {
        let flat = flatten_scene(scene);
        let (sw, sh) = (surface_size.0 as f32, surface_size.1 as f32);

        // ── Build Frame quad vertices ──
        let mut vertices: Vec<Vertex> = Vec::new();
        for node in &flat {
            if let FlatNodeKind::Frame { background_color } = &node.kind {
                let ndc_left = (node.abs_x / sw) * 2.0 - 1.0;
                let ndc_right = ((node.abs_x + node.width) / sw) * 2.0 - 1.0;
                let ndc_top = 1.0 - (node.abs_y / sh) * 2.0;
                let ndc_bottom = 1.0 - ((node.abs_y + node.height) / sh) * 2.0;
                let c = *background_color;

                vertices.extend_from_slice(&[
                    Vertex {
                        position: [ndc_left, ndc_top],
                        color: c,
                    },
                    Vertex {
                        position: [ndc_right, ndc_top],
                        color: c,
                    },
                    Vertex {
                        position: [ndc_left, ndc_bottom],
                        color: c,
                    },
                    Vertex {
                        position: [ndc_left, ndc_bottom],
                        color: c,
                    },
                    Vertex {
                        position: [ndc_right, ndc_top],
                        color: c,
                    },
                    Vertex {
                        position: [ndc_right, ndc_bottom],
                        color: c,
                    },
                ]);
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

        // ── Single render pass: Frame quads first, then Text overlay ──
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

            // Draw Frame quads
            if let Some(vb) = &vertex_buffer {
                pass.set_pipeline(&self.render_pipeline);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.draw(0..vertices.len() as u32, 0..1);
            }

            // Draw Text overlay via glyphon
            self.text_renderer
                .render(&self.text_atlas, &self.viewport, &mut pass)
                .expect("glyphon render");
        }
    }
}
