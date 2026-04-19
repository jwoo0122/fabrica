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
use clap::Parser;
use notify_debouncer_full::{
    new_debouncer,
    notify::{RecommendedWatcher, RecursiveMode},
    DebounceEventResult, Debouncer, RecommendedCache,
};
use sdui_cel::CelEngine;
use sdui_core::{flatten_scene, resolve_scene, ImageSource, SceneNode, ROOT_NODE_ID};
use sdui_runtime_wgpu::Renderer;
use std::error::Error;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;
use wgpu::CurrentSurfaceTexture;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

const WINDOW_TITLE: &str = "sdui smoke";

#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value = "smoke")]
    example: String,
    #[arg(long)]
    scene_path: Option<PathBuf>,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("app-native is nested under workspace root")
        .to_path_buf()
}

fn resolve_scene_path(args: &Args) -> Result<PathBuf, Box<dyn Error>> {
    if let Some(path) = &args.scene_path {
        if !path.is_absolute() {
            return Err(format!("--scene-path must be absolute: {}", path.display()).into());
        }
        return Ok(path.clone());
    }

    Ok(workspace_root()
        .join("examples")
        .join(&args.example)
        .join("scene.json"))
}

fn load_scene(path: &Path) -> Result<SceneNode, Box<dyn Error>> {
    let data = std::fs::read_to_string(path)?;
    let scene = serde_json::from_str(&data)?;
    Ok(scene)
}

fn load_resolved_scene(path: &Path) -> Result<SceneNode, Box<dyn Error>> {
    let scene = load_scene(path)?;
    let engine = CelEngine::new();
    resolve_scene(&scene, &engine)
        .ok_or_else(|| format!("root scene resolved to nothing for {}", path.display()).into())
}

fn build_tree_update(scene: &SceneNode) -> TreeUpdate {
    let flat = flatten_scene(scene);

    let mut root = Node::new(Role::Window);
    root.set_label(WINDOW_TITLE);
    // Root's children are the top-level scene nodes (just the first one).
    let top_children: Vec<_> = flat.first().map(|n| vec![n.id]).unwrap_or_default();
    root.set_children(top_children);

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
            SceneNode::Condition(_) => {
                unreachable!("Condition must be resolved via resolve_scene before ax dump")
            }
        }
    }

    let doc = serde_json::json!({
        "pid": pid,
        "source": "app-native AccessKit dump",
        "root": {
            "role": "window",
            "label": WINDOW_TITLE,
            "children": [scene_to_json(scene, 0.0, 0.0)],
        }
    });
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()) {
        eprintln!("warning: failed to write ax-dump: {e}");
    }
}

/// Recursively collect every `ImageSource` referenced by this subtree.
/// The scene is expected to be condition-free (see `resolve_scene`).
fn collect_image_sources(node: &SceneNode, out: &mut Vec<ImageSource>) {
    match node {
        SceneNode::Frame(f) => {
            for child in &f.children {
                collect_image_sources(child, out);
            }
        }
        SceneNode::Text(_) => {}
        SceneNode::Image(i) => out.push(i.source.clone()),
        SceneNode::Condition(_) => {
            unreachable!("Condition must be resolved via resolve_scene before image collection")
        }
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

type SceneDebouncer = Debouncer<RecommendedWatcher, RecommendedCache>;

#[derive(Debug)]
enum UserEvent {
    AccessKit(AccessKitEvent),
    ReloadScene(PathBuf),
}

impl From<AccessKitEvent> for UserEvent {
    fn from(value: AccessKitEvent) -> Self {
        Self::AccessKit(value)
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
    proxy: EventLoopProxy<UserEvent>,
    window: Option<WindowState>,
    scene: SceneNode,
    _debouncer: SceneDebouncer,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>, scene: SceneNode, debouncer: SceneDebouncer) -> Self {
        Self {
            proxy,
            window: None,
            scene,
            _debouncer: debouncer,
        }
    }

    fn reload_scene(&mut self, path: &Path) {
        let scene = match load_resolved_scene(path) {
            Ok(scene) => scene,
            Err(e) => {
                eprintln!("reload: failed to load {}: {e}", path.display());
                return;
            }
        };
        self.scene = scene;

        let Some(state) = self.window.as_mut() else {
            return;
        };

        if let Some(gpu) = state.gpu.as_mut() {
            warm_image_cache(&self.scene, &mut gpu.renderer);
        }

        dump_ax_tree(&self.scene);
        let scene = &self.scene;
        state.adapter.update_if_active(|| build_tree_update(scene));
        state.window.request_redraw();
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

impl ApplicationHandler<UserEvent> for App {
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

    fn user_event(&mut self, _: &ActiveEventLoop, user_event: UserEvent) {
        match user_event {
            UserEvent::AccessKit(user_event) => {
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
            UserEvent::ReloadScene(path) => {
                self.reload_scene(&path);
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let scene_path = resolve_scene_path(&args)?;
    let scene = load_resolved_scene(&scene_path)?;

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let watched_path = scene_path.clone();
    let watcher_proxy = proxy.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(200),
        None,
        move |result: DebounceEventResult| match result {
            Ok(events) if !events.is_empty() => {
                let _ = watcher_proxy.send_event(UserEvent::ReloadScene(watched_path.clone()));
            }
            Ok(_) => {}
            Err(errors) => {
                for error in errors {
                    eprintln!("reload watch error: {error}");
                }
            }
        },
    )?;
    debouncer.watch(&scene_path, RecursiveMode::NonRecursive)?;

    let mut app = App::new(proxy, scene, debouncer);
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("fabrica-{name}-{nanos}.json"))
    }

    #[test]
    fn load_resolved_scene_succeeds_for_valid_scene() {
        let path = temp_file_path("valid-scene");
        fs::write(
            &path,
            r#"{
  "type": "Frame",
  "label": "root",
  "background_color": [0.0, 0.0, 0.0, 1.0],
  "x": 0.0,
  "y": 0.0,
  "width": 100.0,
  "height": 100.0,
  "children": [
    {
      "type": "Condition",
      "when": "true",
      "then": {
        "type": "Text",
        "label": "hello-label",
        "content": "hello",
        "font_size": 16.0,
        "color": [1.0, 1.0, 1.0, 1.0],
        "x": 10.0,
        "y": 10.0,
        "width": 80.0,
        "height": 20.0
      }
    }
  ]
}"#,
        )
        .unwrap();

        let scene = load_resolved_scene(&path).expect("valid scene should load");
        let text = serde_json::to_string(&scene).unwrap();
        assert!(text.contains("hello-label"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn load_resolved_scene_fails_for_parse_error() {
        let path = temp_file_path("parse-error");
        fs::write(&path, "{ broken").unwrap();

        let err = load_resolved_scene(&path).expect_err("parse error should fail");
        assert!(err.to_string().contains("key must be a string"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn load_resolved_scene_fails_when_root_resolves_to_nothing() {
        let path = temp_file_path("resolve-none");
        fs::write(
            &path,
            r#"{
  "type": "Condition",
  "when": "false",
  "then": {
    "type": "Text",
    "label": "never",
    "content": "never",
    "font_size": 16.0,
    "color": [1.0, 1.0, 1.0, 1.0],
    "x": 0.0,
    "y": 0.0,
    "width": 40.0,
    "height": 20.0
  }
}"#,
        )
        .unwrap();

        let err = load_resolved_scene(&path).expect_err("resolve-none should fail");
        assert!(err.to_string().contains("resolved to nothing"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn load_resolved_scene_fails_for_missing_file() {
        let path = temp_file_path("missing");
        let err = load_resolved_scene(&path).expect_err("missing file should fail");
        assert!(err.to_string().contains("No such file") || err.to_string().contains("os error"));
    }
}
