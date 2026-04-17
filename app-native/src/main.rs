//! Iteration 2 `app-native`: renders a scene tree of nested Frames from
//! `scene.json` via wgpu, and publishes a hierarchical AccessKit tree.
//!
//! Based on the Iteration 1 scaffold, extended with:
//!   - `SceneNode` enum deserialization (replaces bare `Frame`)
//!   - Recursive AccessKit tree from `flatten_scene`
//!   - Hierarchical AX dump for `xtask inspect-ax`
//!   - `render_scene` replaces single-frame `render`
//!
//! Structure follows the upstream `accesskit_winit` simple.rs example:
//!   <https://github.com/AccessKit/accesskit/blob/main/platforms/winit/examples/simple.rs>

use accesskit::{Node, Role, Tree, TreeId, TreeUpdate};
use accesskit_winit::{Adapter, Event as AccessKitEvent, WindowEvent as AccessKitWindowEvent};
use sdui_core::{flatten_scene, ImageSource, SceneNode, ROOT_NODE_ID};
use sdui_runtime_wgpu::Renderer;
use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use wgpu::CurrentSurfaceTexture;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

const WINDOW_TITLE: &str = "sdui smoke";

/// Load `examples/smoke/scene.json` relative to the workspace root.
fn load_scene() -> SceneNode {
    let candidates = [
        PathBuf::from("examples/smoke/scene.json"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("examples/smoke/scene.json"),
    ];
    for path in &candidates {
        if let Ok(data) = std::fs::read_to_string(path) {
            return serde_json::from_str(&data)
                .unwrap_or_else(|e| panic!("invalid scene.json at {}: {e}", path.display()));
        }
    }
    panic!(
        "scene.json not found; tried: {:?}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
    );
}

fn build_tree_update(scene: &SceneNode) -> TreeUpdate {
    let flat = flatten_scene(scene);

    let mut root = Node::new(Role::Window);
    root.set_label(WINDOW_TITLE);
    // Root's children are the top-level scene nodes (just the first one).
    root.set_children(vec![flat[0].id]);

    let mut nodes: Vec<(accesskit::NodeId, Node)> = vec![(ROOT_NODE_ID, root)];
    for flat_node in &flat {
        nodes.push((flat_node.id, flat_node.to_accesskit_node()));
    }

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(ROOT_NODE_ID)),
        tree_id: TreeId::ROOT,
        focus: ROOT_NODE_ID,
    }
}

/// Dump the AccessKit tree as hierarchical JSON to `target/ax-dump-<pid>.json`.
fn dump_ax_tree(scene: &SceneNode) {
    let pid = std::process::id();
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target");
    std::fs::create_dir_all(&target_dir).ok();
    let path = target_dir.join(format!("ax-dump-{pid}.json"));

    fn scene_to_json(node: &SceneNode, parent_x: f32, parent_y: f32) -> serde_json::Value {
        match node {
            SceneNode::Frame(f) => {
                let abs_x = parent_x + f.x;
                let abs_y = parent_y + f.y;
                let children: Vec<serde_json::Value> = f
                    .children
                    .iter()
                    .map(|c| scene_to_json(c, abs_x, abs_y))
                    .collect();

                serde_json::json!({
                    "role": "generic_container",
                    "label": f.label,
                    "bounds": {
                        "x0": abs_x,
                        "y0": abs_y,
                        "x1": abs_x + f.width,
                        "y1": abs_y + f.height,
                    },
                    "children": children,
                })
            }
            SceneNode::Text(t) => {
                let abs_x = parent_x + t.x;
                let abs_y = parent_y + t.y;

                serde_json::json!({
                    "role": "static_text",
                    "label": t.content,
                    "bounds": {
                        "x0": abs_x,
                        "y0": abs_y,
                        "x1": abs_x + t.width,
                        "y1": abs_y + t.height,
                    },
                })
            }
            SceneNode::Image(i) => {
                let abs_x = parent_x + i.x;
                let abs_y = parent_y + i.y;

                serde_json::json!({
                    "role": "image",
                    "label": i.alt.as_deref().unwrap_or(&i.label),
                    "bounds": {
                        "x0": abs_x,
                        "y0": abs_y,
                        "x1": abs_x + i.width,
                        "y1": abs_y + i.height,
                    },
                })
            }
        }
    }

    let doc = serde_json::json!({
        "pid": pid,
        "source": "app-native AccessKit dump",
        "root": {
            "role": "window",
            "label": WINDOW_TITLE,
            "children": [scene_to_json(scene, 0.0, 0.0)]
        }
    });
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()) {
        eprintln!("warning: failed to write ax-dump: {e}");
    }
}

/// Recursively collect every `ImageSource` referenced by this subtree.
fn collect_image_sources(node: &SceneNode, out: &mut Vec<ImageSource>) {
    match node {
        SceneNode::Frame(f) => {
            for child in &f.children {
                collect_image_sources(child, out);
            }
        }
        SceneNode::Text(_) => {}
        SceneNode::Image(i) => out.push(i.source.clone()),
    }
}

/// Fetch the bytes for an `ImageSource` (blocking). `Url` uses `ureq`, `Path`
/// reads the filesystem. Errors are propagated so the caller can warn and skip.
fn load_image_bytes(source: &ImageSource) -> Result<Vec<u8>, Box<dyn Error>> {
    match source {
        ImageSource::Url { url } => {
            let resp = ureq::get(url).call()?;
            let mut reader = resp.into_body().into_reader();
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf)?;
            Ok(buf)
        }
        ImageSource::Path { path } => {
            let buf = std::fs::read(path)?;
            Ok(buf)
        }
    }
}

/// Warm up `renderer`'s image cache for every `Image` node in `scene`.
///
/// Failures (network/FS/decode) log to stderr and skip — the placeholder
/// keeps rendering.
fn warm_image_cache(scene: &SceneNode, renderer: &mut Renderer) {
    let mut sources = Vec::new();
    collect_image_sources(scene, &mut sources);

    for source in sources {
        match load_image_bytes(&source) {
            Ok(bytes) => renderer.register_image(&source, &bytes),
            Err(e) => eprintln!("image load failed for {source:?}: {e}"),
        }
    }
}

struct GpuState {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
}

struct WindowState {
    window: Window,
    adapter: Adapter,
    gpu: Option<GpuState>,
}

struct App {
    proxy: EventLoopProxy<AccessKitEvent>,
    window: Option<WindowState>,
    scene: SceneNode,
}

impl App {
    fn new(proxy: EventLoopProxy<AccessKitEvent>, scene: SceneNode) -> Self {
        Self {
            proxy,
            window: None,
            scene,
        }
    }

    fn init_gpu(&mut self) {
        let state = self.window.as_ref().expect("window must exist");
        let window = &state.window;

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance.create_surface(window).expect("create_surface");
        let surface: wgpu::Surface<'static> = unsafe { std::mem::transmute(surface) };

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no suitable GPU adapter found");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("sdui device"),
            ..Default::default()
        }))
        .expect("request_device");

        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let mut renderer = Renderer::new(device, queue, format);

        // Synchronously load every Image referenced by the scene so the first
        // frame doesn't flash a placeholder. Failures keep the placeholder.
        warm_image_cache(&self.scene, &mut renderer);

        let ws = self.window.as_mut().unwrap();
        ws.gpu = Some(GpuState {
            surface,
            config,
            renderer,
        });
    }
}

impl ApplicationHandler<AccessKitEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(WINDOW_TITLE)
            .with_visible(false);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        let adapter = Adapter::with_event_loop_proxy(event_loop, &window, self.proxy.clone());
        self.window = Some(WindowState {
            window,
            adapter,
            gpu: None,
        });

        self.init_gpu();
        dump_ax_tree(&self.scene);

        // Make the window visible only AFTER the surface is configured and the
        // image cache is warmed — otherwise macOS may commit it while wgpu is
        // still initializing, the first frames land in the Occluded state, and
        // nothing subsequently kicks a redraw.
        let ws = self.window.as_ref().expect("window must exist");
        ws.window.set_visible(true);
        ws.window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(state) = self.window.as_mut() else {
            return;
        };
        state.adapter.process_event(&state.window, &event);

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if let Some(gpu) = state.gpu.as_mut() {
                    gpu.config.width = new_size.width.max(1);
                    gpu.config.height = new_size.height.max(1);
                    gpu.surface.configure(gpu.renderer.device(), &gpu.config);
                    state.window.request_redraw();
                }
            }
            WindowEvent::Occluded(false) => {
                state.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if let Some(gpu) = state.gpu.as_mut() {
                    let output = match gpu.surface.get_current_texture() {
                        CurrentSurfaceTexture::Success(tex)
                        | CurrentSurfaceTexture::Suboptimal(tex) => tex,
                        CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                            gpu.surface.configure(gpu.renderer.device(), &gpu.config);
                            state.window.request_redraw();
                            return;
                        }
                        other => {
                            eprintln!("surface texture error: {other:?}");
                            state.window.request_redraw();
                            return;
                        }
                    };
                    let view = output
                        .texture
                        .create_view(&wgpu::TextureViewDescriptor::default());
                    let mut encoder = gpu.renderer.device().create_command_encoder(
                        &wgpu::CommandEncoderDescriptor {
                            label: Some("sdui encoder"),
                        },
                    );
                    gpu.renderer.render_scene(
                        &self.scene,
                        &mut encoder,
                        &view,
                        (gpu.config.width, gpu.config.height),
                    );
                    gpu.renderer
                        .queue()
                        .submit(std::iter::once(encoder.finish()));
                    output.present();
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, user_event: AccessKitEvent) {
        let Some(state) = self.window.as_mut() else {
            return;
        };
        let scene = &self.scene;
        match user_event.window_event {
            AccessKitWindowEvent::InitialTreeRequested => {
                state.adapter.update_if_active(|| build_tree_update(scene));
            }
            AccessKitWindowEvent::ActionRequested(_) => {}
            AccessKitWindowEvent::AccessibilityDeactivated => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let _ = std::env::args().collect::<Vec<_>>();

    let scene = load_scene();

    let event_loop = EventLoop::<AccessKitEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy, scene);
    event_loop.run_app(&mut app)?;
    Ok(())
}
