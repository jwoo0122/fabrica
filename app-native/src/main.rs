//! Iteration 6 `app-native`: the scene is now *reactive*.
//!
//! Pointer events (`CursorMoved`, `CursorLeft`, `MouseInput`) feed an
//! [`InteractionState`] which, together with the scene's declared
//! `state`, populates the CEL [`EvalEnv`] that `resolve_scene` consults
//! to select `Condition` branches. Clicks fire the hovered node's
//! `on_click` mutations against `state`, then re-resolve.
//!
//! The renderer (`sdui-runtime-wgpu`) is unchanged — the dynamic tree
//! is produced entirely by the Iteration-5 `resolve_scene` pre-pass.
//!
//! References:
//!   - winit 0.30 `ApplicationHandler`: <https://docs.rs/winit/0.30/winit/application/trait.ApplicationHandler.html>
//!   - AccessKit `accesskit_winit` simple.rs: <https://github.com/AccessKit/accesskit/blob/main/platforms/winit/examples/simple.rs>

use accesskit::{Action, Node, Role, Tree, TreeId, TreeUpdate};
use accesskit_winit::{Adapter, Event as AccessKitEvent, WindowEvent as AccessKitWindowEvent};
use sdui_cel::{CelEngine, ExprCache};
use sdui_core::{
    apply_mutations, build_eval_env, flatten_scene, hit_test, refresh_interaction_after_rebuild,
    resolve_scene, validate_flat, FlatNode, ImageSource, InteractionState, Scene, SceneNode,
    ROOT_NODE_ID,
};
use sdui_runtime_wgpu::Renderer;
use std::collections::BTreeMap;
use std::error::Error;
use std::io::Read;
use std::path::PathBuf;
use wgpu::CurrentSurfaceTexture;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

const WINDOW_TITLE: &str = "sdui";

/// Parse `--example <name>` from argv; defaults to "smoke".
fn parse_example_arg(args: &[String]) -> String {
    let mut iter = args.iter().skip(1);
    while let Some(a) = iter.next() {
        if a == "--example" {
            if let Some(v) = iter.next() {
                return v.clone();
            }
        }
    }
    "smoke".to_string()
}

/// Load `examples/<example>/scene.json` as a [`Scene`], back-compat
/// accepting a bare `SceneNode` at the top level.
fn load_scene(example: &str) -> Scene {
    let rel = format!("examples/{example}/scene.json");
    let candidates = [
        PathBuf::from(&rel),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(&rel),
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

/// Dump the resolved scene as hierarchical JSON to
/// `target/ax-dump-<pid>.json` so `cargo xtask inspect-ax` can read it.
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
                    "id": f.id,
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
                    "id": t.id,
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
                    "id": i.id,
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

/// Warm the renderer's image cache for every `Image` in the currently
/// resolved scene. For Iteration 6 this runs once at startup (idle state);
/// scenes that vary their images per interaction would need per-resolve
/// warming, which is out of scope.
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

    /// Unresolved source tree — re-resolved on every state/interaction diff.
    scene_src: SceneNode,
    /// Author-declared state; mutations write here.
    state: BTreeMap<String, serde_json::Value>,
    /// Currently resolved tree (what's drawn + dumped).
    scene: SceneNode,
    /// Flattened absolute-coordinate list used for hit-testing.
    flat: Vec<FlatNode>,

    interaction: InteractionState,
    last_cursor: Option<(f32, f32)>,

    engine: CelEngine,
    cache: ExprCache,
}

impl App {
    fn new(proxy: EventLoopProxy<AccessKitEvent>, scene_wrap: Scene) -> Self {
        let engine = CelEngine::new();
        let mut cache = ExprCache::new();
        let state = scene_wrap.state.clone();
        let interaction = InteractionState::default();

        // Initial resolve against the idle env so the first frame is consistent.
        let env = build_eval_env(&interaction, &state);
        let scene = resolve_scene(&scene_wrap.root, &engine, &env, &mut cache)
            .expect("root scene must not resolve to nothing");
        let flat = flatten_scene(&scene);
        if let Err(e) = validate_flat(&flat) {
            eprintln!("[sdui] scene validation: {e}");
        }

        Self {
            proxy,
            window: None,
            scene_src: scene_wrap.root,
            state,
            scene,
            flat,
            interaction,
            last_cursor: None,
            engine,
            cache,
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

        warm_image_cache(&self.scene, &mut renderer);

        let ws = self.window.as_mut().unwrap();
        ws.gpu = Some(GpuState {
            surface,
            config,
            renderer,
        });
    }

    /// Rebuild `self.scene` + `self.flat` from the current interaction +
    /// state, update AccessKit, and request a redraw.
    ///
    /// Post-rebuild re-hit (Codex finding #11): after flatten, drop any
    /// `hovered`/`pressed` whose node disappeared, and then re-hit at
    /// the last known cursor position so `hovered` stays consistent
    /// with the new tree. A second rebuild is NOT triggered to avoid
    /// loops — the next pointer event corrects any remaining drift.
    fn rebuild_scene(&mut self) {
        let env = build_eval_env(&self.interaction, &self.state);
        let resolved = resolve_scene(&self.scene_src, &self.engine, &env, &mut self.cache)
            .unwrap_or_else(|| self.scene.clone());
        self.scene = resolved;
        self.flat = flatten_scene(&self.scene);
        if let Err(e) = validate_flat(&self.flat) {
            eprintln!("[sdui] scene validation: {e}");
        }

        // Drop stale references (node removed by a Condition flip).
        refresh_interaction_after_rebuild(&mut self.interaction, &self.flat);

        // Re-hit at last cursor so hover state matches the new flat list.
        if let Some((x, y)) = self.last_cursor {
            let new_hovered = hit_test(&self.flat, x, y).and_then(|n| n.author_id.clone());
            if new_hovered != self.interaction.hovered {
                self.interaction.hovered = new_hovered;
                // Second-pass env update without a full re-resolve — the
                // next render cycle will pick it up.
            }
        }

        dump_ax_tree(&self.scene);
        if let Some(ws) = self.window.as_mut() {
            let scene = self.scene.clone();
            ws.adapter.update_if_active(|| build_tree_update(&scene));
            ws.window.request_redraw();
        }
    }

    /// Resolve a node id to its `on_click` mutations (if any) and apply them.
    fn run_on_click_for(&mut self, id: &str) {
        let muts: Vec<_> = self
            .flat
            .iter()
            .find(|n| n.author_id.as_deref() == Some(id))
            .map(|n| n.on_click.clone())
            .unwrap_or_default();
        if muts.is_empty() {
            return;
        }
        let env = build_eval_env(&self.interaction, &self.state);
        apply_mutations(
            &mut self.state,
            &muts,
            &self.engine,
            &env,
            &mut self.cache,
        );
        eprintln!("[sdui] on_click {id}: state = {:?}", self.state);
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
            WindowEvent::CursorMoved { position, .. } => {
                let x = position.x as f32;
                let y = position.y as f32;
                self.last_cursor = Some((x, y));
                let new_hovered = hit_test(&self.flat, x, y).and_then(|n| n.author_id.clone());
                if new_hovered != self.interaction.hovered {
                    self.interaction.hovered = new_hovered;
                    self.rebuild_scene();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.last_cursor = None;
                if self.interaction.hovered.is_some() || self.interaction.pressed.is_some() {
                    self.interaction.hovered = None;
                    self.interaction.pressed = None;
                    self.rebuild_scene();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let new_pressed = self.interaction.hovered.clone();
                if new_pressed != self.interaction.pressed {
                    self.interaction.pressed = new_pressed;
                    self.rebuild_scene();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let pressed = self.interaction.pressed.take();
                let mut mutated = false;
                if let Some(id) = pressed {
                    // Only fire on release over the same node we pressed on.
                    if self.interaction.hovered.as_deref() == Some(id.as_str()) {
                        self.run_on_click_for(&id);
                        mutated = true;
                    }
                }
                // Either state or pressed changed — rebuild.
                if mutated || self.interaction.pressed.is_some() {
                    self.rebuild_scene();
                } else {
                    // pressed cleared but nothing fired — still rebuild so
                    // any ui.pressed-dependent Condition updates.
                    self.rebuild_scene();
                }
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
        match user_event.window_event {
            AccessKitWindowEvent::InitialTreeRequested => {
                let scene = self.scene.clone();
                if let Some(ws) = self.window.as_mut() {
                    ws.adapter.update_if_active(|| build_tree_update(&scene));
                }
            }
            AccessKitWindowEvent::ActionRequested(request) => {
                // Map AccessKit Action::Click → the same on_click pipeline
                // pointer events use. Unblocks assistive-tech click
                // (Codex review finding #10). Focus / keyboard
                // activation (Tab / Enter / Space) are out of scope for
                // Iteration 6 — see SPRINTS for I6.1.
                if request.action == Action::Click {
                    if let Some(id) = self
                        .flat
                        .iter()
                        .find(|n| n.id == request.target_node)
                        .and_then(|n| n.author_id.clone())
                    {
                        self.run_on_click_for(&id);
                        self.rebuild_scene();
                    }
                }
            }
            AccessKitWindowEvent::AccessibilityDeactivated => {}
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().collect();
    let example = parse_example_arg(&args);
    let scene_wrap = load_scene(&example);

    let event_loop = EventLoop::<AccessKitEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy, scene_wrap);
    event_loop.run_app(&mut app)?;
    Ok(())
}
