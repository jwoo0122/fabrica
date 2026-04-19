//! Iteration 4 `app-web`: renders a scene tree of nested Frames + Text +
//! Image nodes from `scene.json` via wgpu into an HTML canvas, and publishes
//! a hierarchical AccessKit-mirror DOM tree.
//!
//! Image loading is asynchronous — `spawn_local` fetches each URL, retries
//! every second until success, then calls `register_image` and requests one
//! new frame via `requestAnimationFrame`. No per-frame render loop.
//!
//! References checked 2026-04-17:
//!   - wgpu canvas target: <https://docs.rs/wgpu/latest/wasm32-unknown-unknown/wgpu/enum.SurfaceTarget.html>
//!   - web-sys fetch pattern: <https://rustwasm.github.io/docs/wasm-bindgen/examples/fetch.html>
//!   - gloo-timers TimeoutFuture: <https://docs.rs/gloo-timers/0.3/gloo_timers/future/struct.TimeoutFuture.html>

// Host builds (cargo check --workspace on aarch64-apple-darwin) only need the
// crate to compile; the entry point and wasm-specific code is gated.
#[cfg(target_arch = "wasm32")]
mod wasm_entry {
    use gloo_timers::future::TimeoutFuture;
    use js_sys::{ArrayBuffer, Uint8Array};
    use sdui_cel::CelEngine;
    use sdui_core::{resolve_scene, ImageSource, SceneNode};
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{Request, RequestInit, RequestMode, Response};

    /// Shared render state — owned by the start function, borrowed by the
    /// render loop closures and by each spawned image loader.
    struct RenderState {
        renderer: sdui_runtime_wgpu::Renderer,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
        scene: SceneNode,
    }

    /// Fetch `./scene.json`, parse it as a `SceneNode`, resolve every
    /// `Condition` against a fresh `CelEngine`, and return the condition-free
    /// tree. Any failure is surfaced as `JsValue` so startup can reject cleanly.
    async fn load_resolved_scene() -> Result<SceneNode, JsValue> {
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
        let resp_value = JsFuture::from(window.fetch_with_str("./scene.json")).await?;
        let resp: Response = resp_value.dyn_into()?;
        if !resp.ok() {
            return Err(JsValue::from_str(&format!(
                "scene.json fetch failed: HTTP {}",
                resp.status()
            )));
        }
        let text = JsFuture::from(resp.text()?).await?;
        let json = text
            .as_string()
            .ok_or_else(|| JsValue::from_str("scene.json response was not a string"))?;
        let raw: SceneNode = serde_json::from_str(&json)
            .map_err(|e| JsValue::from_str(&format!("scene.json parse error: {e}")))?;
        let engine = CelEngine::new();
        resolve_scene(&raw, &engine)
            .ok_or_else(|| JsValue::from_str("root scene resolved to nothing"))
    }

    #[wasm_bindgen(start)]
    pub fn start() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        wasm_bindgen_futures::spawn_local(async {
            if let Err(e) = run().await {
                web_sys::console::error_1(&JsValue::from_str(&format!("sdui boot failure: {e:?}")));
            }
        });
        Ok(())
    }

    fn stage_mount_parent(document: &web_sys::Document) -> Result<web_sys::Element, JsValue> {
        if let Some(host) = document.get_element_by_id("sdui-canvas-host") {
            return Ok(host);
        }
        let body = document
            .body()
            .ok_or_else(|| JsValue::from_str("no body"))?;
        Ok(body.unchecked_into::<web_sys::Element>())
    }

    /// Build a hierarchical mirror DOM tree reflecting the scene graph.
    fn mount_mirror_root(
        document: &web_sys::Document,
        scene: &SceneNode,
    ) -> Result<web_sys::Element, JsValue> {
        let parent = stage_mount_parent(document)?;

        if let Some(existing) = document.get_element_by_id("sdui-root") {
            existing.remove();
        }

        let root = document.create_element("div")?;
        root.set_attribute("role", "application")?;
        root.set_attribute("aria-label", "sdui runtime")?;
        root.set_id("sdui-root");
        parent.append_child(&root)?;

        mount_mirror_node(document, &root, scene)?;

        Ok(root)
    }

    /// Recursively create mirror DOM elements for a scene node and its children.
    /// The scene is expected to be condition-free (see `resolve_scene`).
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
            SceneNode::Image(i) => {
                let alt = i.alt.as_deref().unwrap_or(&i.label);
                match &i.source {
                    ImageSource::Url { url } => {
                        let el = document.create_element("img")?;
                        el.set_attribute("src", url)?;
                        el.set_attribute("alt", alt)?;
                        el.set_attribute("aria-label", alt)?;
                        el.set_attribute("width", &format!("{}", i.width as i32))?;
                        el.set_attribute("height", &format!("{}", i.height as i32))?;
                        parent.append_child(&el)?;
                    }
                    ImageSource::Path { .. } => {
                        // Browser cannot read filesystem paths — fall back to
                        // a role=img placeholder with aria-label only.
                        let el = document.create_element("div")?;
                        el.set_attribute("role", "img")?;
                        el.set_attribute("aria-label", alt)?;
                        parent.append_child(&el)?;
                    }
                }
            }
            SceneNode::Condition(_) => {
                unreachable!("Condition must be resolved via resolve_scene before DOM mount")
            }
        }
        Ok(())
    }

    /// Recursively collect every `ImageSource` referenced by a subtree.
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

    async fn run() -> Result<(), JsValue> {
        let scene = load_resolved_scene().await?;

        let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
        let document = window
            .document()
            .ok_or_else(|| JsValue::from_str("no document"))?;
        let mount_parent = stage_mount_parent(&document)?;

        mount_mirror_root(&document, &scene)?;

        // Create canvas element.
        let canvas = document
            .create_element("canvas")?
            .dyn_into::<web_sys::HtmlCanvasElement>()?;
        canvas.set_id("sdui-canvas");
        canvas.set_width(800);
        canvas.set_height(600);
        mount_parent.append_child(&canvas)?;

        // wgpu init — WebGPU only, no WebGL2 fallback (Q3 decision).
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| JsValue::from_str(&format!("create_surface from canvas failed: {e:?}")))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| JsValue::from_str(&format!("No WebGPU adapter: {e:?}")))?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("sdui web device"),
                ..Default::default()
            })
            .await
            .map_err(|e| JsValue::from_str(&format!("request_device failed: {e:?}")))?;

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

        let renderer = sdui_runtime_wgpu::Renderer::new(device, queue, format);

        let state = Rc::new(RefCell::new(RenderState {
            renderer,
            surface,
            config,
            scene,
        }));

        // Initial frame — Images show placeholders until fetches land.
        render_once(&state);

        // Spawn an async loader per Image. Each loop retries every second on
        // failure; on success, it calls register_image and re-renders once.
        let mut sources = Vec::new();
        {
            let s = state.borrow();
            collect_image_sources(&s.scene, &mut sources);
        }
        for source in sources {
            let state = state.clone();
            wasm_bindgen_futures::spawn_local(async move {
                load_image_with_retry(state, source).await;
            });
        }

        Ok(())
    }

    /// Fetch-and-register one image source, retrying every 1 s on error. On
    /// success, decode on the wgpu side and schedule one animation frame.
    async fn load_image_with_retry(state: Rc<RefCell<RenderState>>, source: ImageSource) {
        let url = match &source {
            ImageSource::Url { url } => url.clone(),
            ImageSource::Path { path } => {
                // Browsers can't fetch(path); mirror DOM already falls back,
                // and the renderer keeps the placeholder.
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "[app-web] Path source '{path}' not loadable in browser; leaving placeholder"
                )));
                return;
            }
        };

        loop {
            match fetch_bytes(&url).await {
                Ok(bytes) => {
                    state.borrow_mut().renderer.register_image(&source, &bytes);
                    request_one_redraw(state.clone());
                    break;
                }
                Err(e) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[app-web] image fetch {url} failed ({e:?}); retrying in 1s"
                    )));
                    TimeoutFuture::new(1000).await;
                }
            }
        }
    }

    /// `fetch(url)` → `Response.arrayBuffer()` → `Vec<u8>`.
    async fn fetch_bytes(url: &str) -> Result<Vec<u8>, JsValue> {
        let opts = RequestInit::new();
        opts.set_method("GET");
        opts.set_mode(RequestMode::Cors);

        let request = Request::new_with_str_and_init(url, &opts)?;
        let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
        let resp_value = JsFuture::from(window.fetch_with_request(&request)).await?;
        let resp: Response = resp_value.dyn_into()?;
        if !resp.ok() {
            return Err(JsValue::from_str(&format!(
                "HTTP {} for {url}",
                resp.status()
            )));
        }
        let ab = JsFuture::from(resp.array_buffer()?).await?;
        let ab: ArrayBuffer = ab.dyn_into()?;
        let u8s = Uint8Array::new(&ab);
        let mut buf = vec![0u8; u8s.length() as usize];
        u8s.copy_to(&mut buf);
        Ok(buf)
    }

    /// Schedule exactly one `requestAnimationFrame` that re-renders from
    /// `state`. No persistent frame loop — calls are event-driven.
    fn request_one_redraw(state: Rc<RefCell<RenderState>>) {
        let window = match web_sys::window() {
            Some(w) => w,
            None => return,
        };
        let closure = Closure::once_into_js(move || {
            render_once(&state);
        });
        if let Err(e) =
            window.request_animation_frame(closure.as_ref().unchecked_ref::<js_sys::Function>())
        {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "[app-web] requestAnimationFrame failed: {e:?}"
            )));
        }
    }

    /// Render the scene once using the current state.
    fn render_once(state: &Rc<RefCell<RenderState>>) {
        use wgpu::CurrentSurfaceTexture;

        let mut s = state.borrow_mut();
        let RenderState {
            renderer,
            surface,
            config,
            scene,
        } = &mut *s;

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
