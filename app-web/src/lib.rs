//! Iteration 2 `app-web`: renders a scene tree of nested Frames from
//! `scene.json` via wgpu into an HTML canvas, and publishes a hierarchical
//! AccessKit-mirror DOM tree.
//!
//! wgpu wasm surface creation uses `SurfaceTarget::Canvas(HtmlCanvasElement)`.
//! Reference: <https://docs.rs/wgpu/latest/wasm32-unknown-unknown/wgpu/enum.SurfaceTarget.html>

// Host builds (cargo check --workspace on aarch64-apple-darwin) only need the
// crate to compile; the entry point and wasm-specific code is gated.
#[cfg(target_arch = "wasm32")]
mod wasm_entry {
    use sdui_core::SceneNode;
    use wasm_bindgen::prelude::*;

    /// scene.json is embedded at build time so no async fetch is needed.
    const SCENE_JSON: &str = include_str!("../../examples/smoke/scene.json");

    #[wasm_bindgen(start)]
    pub fn start() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        mount_mirror_root()?;
        wasm_bindgen_futures::spawn_local(run());
        Ok(())
    }

    /// Build a hierarchical mirror DOM tree reflecting the scene graph.
    fn mount_mirror_root() -> Result<(), JsValue> {
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
        let document = window
            .document()
            .ok_or_else(|| JsValue::from_str("no document"))?;
        let body = document
            .body()
            .ok_or_else(|| JsValue::from_str("no body"))?;

        // Idempotent: skip if already mounted.
        if document
            .query_selector("div[role=\"application\"]")?
            .is_some()
        {
            return Ok(());
        }

        let root = document.create_element("div")?;
        root.set_attribute("role", "application")?;
        root.set_attribute("aria-label", "sdui smoke")?;
        root.set_id("sdui-root");
        body.append_child(&root)?;

        let scene: SceneNode = serde_json::from_str(SCENE_JSON)
            .map_err(|e| JsValue::from_str(&format!("scene.json parse error: {e}")))?;

        // Recursively build mirror DOM from scene tree.
        mount_mirror_node(&document, &root, &scene)?;

        Ok(())
    }

    /// Recursively create mirror DOM elements for a scene node and its children.
    fn mount_mirror_node(
        document: &web_sys::Document,
        parent: &web_sys::Element,
        node: &SceneNode,
    ) -> Result<(), JsValue> {
        match node {
            SceneNode::Frame(f) => {
                let el = document.create_element("div")?;
                el.set_attribute("role", "group")?;
                el.set_attribute("aria-label", &f.label)?;
                parent.append_child(&el)?;

                for child in &f.children {
                    mount_mirror_node(document, &el, child)?;
                }
            }
            SceneNode::Text(t) => {
                let el = document.create_element("span")?;
                el.set_attribute("role", "paragraph")?;
                el.set_attribute("aria-label", &t.content)?;
                parent.append_child(&el)?;
            }
        }
        Ok(())
    }

    async fn run() {
        let scene: SceneNode = serde_json::from_str(SCENE_JSON).expect("scene.json parse");

        let window = web_sys::window().expect("no window");
        let document = window.document().expect("no document");
        let body = document.body().expect("no body");

        // Create canvas element.
        let canvas = document
            .create_element("canvas")
            .expect("create canvas")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("not a canvas");
        canvas.set_id("sdui-canvas");
        canvas.set_width(800);
        canvas.set_height(600);
        body.append_child(&canvas).expect("append canvas");

        // wgpu init — WebGPU only, no WebGL2 fallback (Q3 decision).
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .expect("create_surface from canvas");

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("No WebGPU adapter");

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("sdui web device"),
                ..Default::default()
            })
            .await
            .expect("request_device");

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
            width: canvas.width().max(1),
            height: canvas.height().max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let mut renderer = sdui_runtime_wgpu::Renderer::new(device, queue, format);

        render_frame(&mut renderer, &surface, &config, &scene);
    }

    fn render_frame(
        renderer: &mut sdui_runtime_wgpu::Renderer,
        surface: &wgpu::Surface<'_>,
        config: &wgpu::SurfaceConfiguration,
        scene: &SceneNode,
    ) {
        use wgpu::CurrentSurfaceTexture;

        let output = match surface.get_current_texture() {
            CurrentSurfaceTexture::Success(tex) | CurrentSurfaceTexture::Suboptimal(tex) => tex,
            other => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "surface texture error: {other:?}"
                )));
                return;
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            renderer
                .device()
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("sdui web encoder"),
                });
        renderer.render_scene(scene, &mut encoder, &view, (config.width, config.height));
        renderer.queue().submit(std::iter::once(encoder.finish()));
        output.present();
    }
}
