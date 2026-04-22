//! Iteration 6 `app-web`: reactive scene on a wasm + WebGPU canvas.
//!
//! Parity with `app-native` (hover / click / re-resolve / state
//! mutations), plus a mirror-DOM update loop that rebuilds the ARIA
//! tree on every re-resolve. Pointer events are captured on the canvas;
//! the mirror root carries `pointer-events: none` so it never eats a
//! click.
//!
//! References (checked 2026-04-21):
//!   - wgpu SurfaceTarget::Canvas: <https://docs.rs/wgpu/29/wgpu/enum.SurfaceTarget.html>
//!   - PointerEvent on canvas: <https://developer.mozilla.org/en-US/docs/Web/API/PointerEvent>
//!   - `pointer-events: none` spec: <https://developer.mozilla.org/en-US/docs/Web/CSS/pointer-events>

// Host builds (cargo check --workspace on aarch64-apple-darwin) only need the
// crate to compile; the entry point and wasm-specific code is gated.
#[cfg(target_arch = "wasm32")]
mod wasm_entry {
    use gloo_timers::future::TimeoutFuture;
    use js_sys::{ArrayBuffer, Uint8Array};
    use sdui_cel::{CelEngine, ExprCache};
    use sdui_core::{
        apply_mutations, build_eval_env, flatten_scene, hit_test,
        refresh_interaction_after_rebuild, resolve_scene, validate_flat, FlatNode,
        ImageSource, InteractionState, Scene, SceneNode,
    };
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{Request, RequestInit, RequestMode, Response};

    /// Bundled scene sources. Selected at runtime by URL path — the
    /// xtask web server routes `/examples/<name>/` to the same wasm
    /// bundle (xtask/src/main.rs:366), so the running wasm infers
    /// which scene to load from `location.pathname`.
    const SMOKE_SCENE: &str = include_str!("../../examples/smoke/scene.json");
    const COUNTER_SCENE: &str = include_str!("../../examples/counter/scene.json");

    fn select_scene_source() -> &'static str {
        let loc = web_sys::window().and_then(|w| w.location().pathname().ok());
        match loc {
            Some(p) if p.contains("counter") => COUNTER_SCENE,
            _ => SMOKE_SCENE,
        }
    }

    struct AppState {
        // Rendering.
        renderer: sdui_runtime_wgpu::Renderer,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,

        // Scene graph + state.
        scene_src: SceneNode,
        state: BTreeMap<String, serde_json::Value>,
        scene: SceneNode,
        flat: Vec<FlatNode>,

        // Interaction.
        interaction: InteractionState,
        last_cursor: Option<(f32, f32)>,

        // CEL.
        engine: CelEngine,
        cache: ExprCache,

        // DOM.
        mirror_root: web_sys::Element,
    }

    impl AppState {
        /// Rebuild `scene` / `flat` from the current interaction + state,
        /// then rebuild the mirror DOM. Does NOT schedule a redraw —
        /// callers do that after mutable borrow ends.
        fn rebuild(&mut self) {
            let env = build_eval_env(&self.interaction, &self.state);
            let resolved =
                resolve_scene(&self.scene_src, &self.engine, &env, &mut self.cache)
                    .unwrap_or_else(|| self.scene.clone());
            self.scene = resolved;
            self.flat = flatten_scene(&self.scene);
            if let Err(e) = validate_flat(&self.flat) {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "[sdui] scene validation: {e}"
                )));
            }
            refresh_interaction_after_rebuild(&mut self.interaction, &self.flat);
            if let Some((x, y)) = self.last_cursor {
                let new_hovered =
                    hit_test(&self.flat, x, y).and_then(|n| n.author_id.clone());
                if new_hovered != self.interaction.hovered {
                    self.interaction.hovered = new_hovered;
                }
            }
            self.rebuild_mirror();
        }

        /// Reconcile the mirror DOM subtree under `mirror_root` with
        /// the current `scene`.
        ///
        /// **Implementation strategy.** Id'd DOM elements persist across
        /// rebuilds: we drain the tree into a `HashMap<String, Element>`
        /// keyed by `data-sdui-id`, then mount the new tree, re-using
        /// any pooled element whose id (and tag) match. Non-id'd nodes
        /// are always recreated — anonymous elements have no stable
        /// identity for assistive tech to track, so their churn is
        /// acceptable.
        ///
        /// Codex adversarial review #3 (Iteration 6): the previous
        /// implementation wiped the full subtree on every pointer
        /// move, destroying AT focus, virtual-cursor position, and
        /// announcement continuity. Keeping id'd elements live
        /// preserves those across hover/click transitions.
        fn rebuild_mirror(&self) {
            use std::collections::HashMap;

            let window = match web_sys::window() {
                Some(w) => w,
                None => return,
            };
            let document = match window.document() {
                Some(d) => d,
                None => return,
            };

            let mut pool: HashMap<String, web_sys::Element> = HashMap::new();
            while let Some(child_node) = self.mirror_root.first_child() {
                let _ = self.mirror_root.remove_child(&child_node);
                if let Ok(el) = child_node.dyn_into::<web_sys::Element>() {
                    harvest_mirror_pool(&el, &mut pool);
                }
            }

            let _ = mount_mirror_node_with_pool(
                &document,
                &self.mirror_root,
                &self.scene,
                &mut pool,
            );
            // Any entries still in `pool` correspond to elements the
            // new scene no longer references. They drop here — JS GC
            // reclaims them once no other references remain.
        }

        /// Apply on_click mutations for the node with author_id == id.
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
        }
    }

    fn parse_scene() -> Result<Scene, JsValue> {
        let src = select_scene_source();
        serde_json::from_str(src)
            .map_err(|e| JsValue::from_str(&format!("scene.json parse error: {e}")))
    }

    #[wasm_bindgen(start)]
    pub fn start() -> Result<(), JsValue> {
        console_error_panic_hook::set_once();
        wasm_bindgen_futures::spawn_local(run());
        Ok(())
    }

    /// Depth-first drain of `el`'s subtree into `pool`. Every element
    /// with a `data-sdui-id` attribute is added to `pool`, stripped of
    /// its own children first so the pool map contains free-standing
    /// elements ready to be remounted.
    ///
    /// Anonymous (non-id'd) elements are not pooled — their contents are
    /// still recursed so any id'd descendants get pooled.
    fn harvest_mirror_pool(
        el: &web_sys::Element,
        pool: &mut std::collections::HashMap<String, web_sys::Element>,
    ) {
        // Drain children first, recursively.
        while let Some(child_node) = el.first_child() {
            let _ = el.remove_child(&child_node);
            if let Ok(child_el) = child_node.dyn_into::<web_sys::Element>() {
                harvest_mirror_pool(&child_el, pool);
            }
        }
        if let Some(id) = el.get_attribute("data-sdui-id") {
            pool.insert(id, el.clone());
        }
        // Non-id'd `el` drops here; JS GC reclaims once handles fall.
    }

    /// Return a pooled element for `id` whose tag matches `tag`, else a
    /// fresh `document.create_element(tag)`. Case-insensitive tag check
    /// (HTML tag names are upper-cased in the DOM).
    fn pool_or_create(
        pool: &mut std::collections::HashMap<String, web_sys::Element>,
        id: Option<&str>,
        document: &web_sys::Document,
        tag: &str,
    ) -> Result<web_sys::Element, JsValue> {
        if let Some(id) = id {
            if let Some(existing) = pool.remove(id) {
                if existing.tag_name().eq_ignore_ascii_case(tag) {
                    return Ok(existing);
                }
                // Tag changed for the same id — drop the reused element
                // and fall through to creating a fresh one.
            }
        }
        document.create_element(tag)
    }

    /// Set or clear the `data-sdui-id` attribute.
    fn set_sdui_id(el: &web_sys::Element, id: Option<&str>) -> Result<(), JsValue> {
        match id {
            Some(s) => el.set_attribute("data-sdui-id", s),
            None => {
                let _ = el.remove_attribute("data-sdui-id");
                Ok(())
            }
        }
    }

    /// Mount the scene into the mirror DOM, reusing elements from `pool`
    /// keyed by `data-sdui-id`. Non-id'd nodes are always fresh.
    fn mount_mirror_node_with_pool(
        document: &web_sys::Document,
        parent: &web_sys::Element,
        node: &SceneNode,
        pool: &mut std::collections::HashMap<String, web_sys::Element>,
    ) -> Result<(), JsValue> {
        match node {
            SceneNode::Frame(f) => {
                let el = pool_or_create(pool, f.id.as_deref(), document, "div")?;
                el.set_attribute(
                    "role",
                    if f.on_click.is_empty() { "group" } else { "button" },
                )?;
                el.set_attribute("aria-label", &f.label)?;
                set_sdui_id(&el, f.id.as_deref())?;
                parent.append_child(&el)?;
                for child in &f.children {
                    mount_mirror_node_with_pool(document, &el, child, pool)?;
                }
            }
            SceneNode::Text(t) => {
                let tag = if t.on_click.is_empty() { "span" } else { "div" };
                let el = pool_or_create(pool, t.id.as_deref(), document, tag)?;
                el.set_attribute(
                    "role",
                    if t.on_click.is_empty() {
                        "paragraph"
                    } else {
                        "button"
                    },
                )?;
                el.set_attribute("aria-label", &t.content)?;
                set_sdui_id(&el, t.id.as_deref())?;
                parent.append_child(&el)?;
            }
            SceneNode::Image(i) => {
                let alt = i.alt.as_deref().unwrap_or(&i.label);
                match &i.source {
                    ImageSource::Url { url } => {
                        let el = pool_or_create(pool, i.id.as_deref(), document, "img")?;
                        el.set_attribute("src", url)?;
                        el.set_attribute("alt", alt)?;
                        el.set_attribute("aria-label", alt)?;
                        el.set_attribute("width", &format!("{}", i.width as i32))?;
                        el.set_attribute("height", &format!("{}", i.height as i32))?;
                        set_sdui_id(&el, i.id.as_deref())?;
                        parent.append_child(&el)?;
                    }
                    ImageSource::Path { .. } => {
                        let el = pool_or_create(pool, i.id.as_deref(), document, "div")?;
                        el.set_attribute("role", "img")?;
                        el.set_attribute("aria-label", alt)?;
                        set_sdui_id(&el, i.id.as_deref())?;
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

    /// Recursively create mirror DOM elements. Clickable nodes
    /// (`on_click` non-empty) emit `<div role="button">` so assistive
    /// tech announces them correctly (Codex review finding #10); keyboard
    /// focus (tabindex) is scoped out for I6 and scheduled for I6.1.
    ///
    /// Used for the **first** mount only (the async `run()` fn). After
    /// that, [`AppState::rebuild_mirror`] reconciles via pooling.
    fn mount_mirror_node(
        document: &web_sys::Document,
        parent: &web_sys::Element,
        node: &SceneNode,
    ) -> Result<(), JsValue> {
        match node {
            SceneNode::Frame(f) => {
                let el = document.create_element("div")?;
                if f.on_click.is_empty() {
                    el.set_attribute("role", "group")?;
                } else {
                    el.set_attribute("role", "button")?;
                }
                el.set_attribute("aria-label", &f.label)?;
                if let Some(id) = &f.id {
                    el.set_attribute("data-sdui-id", id)?;
                }
                parent.append_child(&el)?;

                for child in &f.children {
                    mount_mirror_node(document, &el, child)?;
                }
            }
            SceneNode::Text(t) => {
                let el = document.create_element("span")?;
                if t.on_click.is_empty() {
                    el.set_attribute("role", "paragraph")?;
                } else {
                    el.set_attribute("role", "button")?;
                }
                // aria-label reflects the current (interpolated) content.
                el.set_attribute("aria-label", &t.content)?;
                if let Some(id) = &t.id {
                    el.set_attribute("data-sdui-id", id)?;
                }
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
                        if let Some(id) = &i.id {
                            el.set_attribute("data-sdui-id", id)?;
                        }
                        parent.append_child(&el)?;
                    }
                    ImageSource::Path { .. } => {
                        let el = document.create_element("div")?;
                        el.set_attribute("role", "img")?;
                        el.set_attribute("aria-label", alt)?;
                        if let Some(id) = &i.id {
                            el.set_attribute("data-sdui-id", id)?;
                        }
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

    async fn run() {
        // Parse scene + initial resolve.
        let scene_wrap: Scene = match parse_scene() {
            Ok(s) => s,
            Err(e) => {
                web_sys::console::error_1(&e);
                return;
            }
        };
        let engine = CelEngine::new();
        let mut cache = ExprCache::new();
        let state_map = scene_wrap.state.clone();
        let interaction = InteractionState::default();

        let env = build_eval_env(&interaction, &state_map);
        let scene = match resolve_scene(&scene_wrap.root, &engine, &env, &mut cache) {
            Some(s) => s,
            None => {
                web_sys::console::error_1(&JsValue::from_str("root scene resolved to nothing"));
                return;
            }
        };
        let flat = flatten_scene(&scene);
        if let Err(e) = validate_flat(&flat) {
            web_sys::console::error_1(&JsValue::from_str(&format!(
                "[sdui] scene validation: {e}"
            )));
        }

        // DOM: mirror root + canvas.
        let window = web_sys::window().expect("no window");
        let document = window.document().expect("no document");
        let body = document.body().expect("no body");

        let mirror_root = document
            .create_element("div")
            .expect("create mirror root");
        mirror_root
            .set_attribute("role", "application")
            .expect("aria");
        mirror_root.set_attribute("aria-label", "sdui").expect("aria");
        mirror_root.set_id("sdui-root");
        // Codex review finding #5: keep the mirror out of the pointer
        // path so the canvas reliably receives events.
        mirror_root
            .set_attribute(
                "style",
                "position: absolute; left: 0; top: 0; pointer-events: none;",
            )
            .expect("style");
        body.append_child(&mirror_root).expect("append mirror");
        mount_mirror_node(&document, &mirror_root, &scene).ok();

        let canvas = document
            .create_element("canvas")
            .expect("create canvas")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("not a canvas");
        canvas.set_id("sdui-canvas");
        canvas.set_width(800);
        canvas.set_height(600);
        body.append_child(&canvas).expect("append canvas");

        // wgpu init.
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

        let renderer = sdui_runtime_wgpu::Renderer::new(device, queue, format);

        let state = Rc::new(RefCell::new(AppState {
            renderer,
            surface,
            config,
            scene_src: scene_wrap.root,
            state: state_map,
            scene,
            flat,
            interaction,
            last_cursor: None,
            engine,
            cache,
            mirror_root,
        }));

        // Initial frame.
        render_once(&state);

        // Kick off async image loading for any Image nodes in the
        // initial resolved tree.
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

        // Install pointer listeners on the canvas.
        install_pointer_listeners(&canvas, state.clone());
    }

    fn install_pointer_listeners(
        canvas: &web_sys::HtmlCanvasElement,
        state: Rc<RefCell<AppState>>,
    ) {
        // pointermove → update hovered.
        {
            let state = state.clone();
            let cb = Closure::<dyn FnMut(web_sys::PointerEvent)>::wrap(Box::new(
                move |ev: web_sys::PointerEvent| {
                    let x = ev.offset_x() as f32;
                    let y = ev.offset_y() as f32;
                    let changed = {
                        let mut s = state.borrow_mut();
                        s.last_cursor = Some((x, y));
                        let new_hovered =
                            hit_test(&s.flat, x, y).and_then(|n| n.author_id.clone());
                        if new_hovered != s.interaction.hovered {
                            s.interaction.hovered = new_hovered;
                            s.rebuild();
                            true
                        } else {
                            false
                        }
                    };
                    if changed {
                        request_one_redraw(state.clone());
                    }
                },
            ));
            canvas
                .add_event_listener_with_callback("pointermove", cb.as_ref().unchecked_ref())
                .ok();
            cb.forget();
        }

        // pointerleave → clear hovered + pressed.
        {
            let state = state.clone();
            let cb = Closure::<dyn FnMut(web_sys::PointerEvent)>::wrap(Box::new(
                move |_ev: web_sys::PointerEvent| {
                    let changed = {
                        let mut s = state.borrow_mut();
                        s.last_cursor = None;
                        if s.interaction.hovered.is_some() || s.interaction.pressed.is_some() {
                            s.interaction.hovered = None;
                            s.interaction.pressed = None;
                            s.rebuild();
                            true
                        } else {
                            false
                        }
                    };
                    if changed {
                        request_one_redraw(state.clone());
                    }
                },
            ));
            canvas
                .add_event_listener_with_callback("pointerleave", cb.as_ref().unchecked_ref())
                .ok();
            cb.forget();
        }

        // pointerdown (left button) → update pressed.
        {
            let state = state.clone();
            let cb = Closure::<dyn FnMut(web_sys::PointerEvent)>::wrap(Box::new(
                move |ev: web_sys::PointerEvent| {
                    if ev.button() != 0 {
                        return;
                    }
                    let changed = {
                        let mut s = state.borrow_mut();
                        let new_pressed = s.interaction.hovered.clone();
                        if new_pressed != s.interaction.pressed {
                            s.interaction.pressed = new_pressed;
                            s.rebuild();
                            true
                        } else {
                            false
                        }
                    };
                    if changed {
                        request_one_redraw(state.clone());
                    }
                },
            ));
            canvas
                .add_event_listener_with_callback("pointerdown", cb.as_ref().unchecked_ref())
                .ok();
            cb.forget();
        }

        // pointerup (left button) → fire on_click if release over same id.
        {
            let state = state.clone();
            let cb = Closure::<dyn FnMut(web_sys::PointerEvent)>::wrap(Box::new(
                move |ev: web_sys::PointerEvent| {
                    if ev.button() != 0 {
                        return;
                    }
                    {
                        let mut s = state.borrow_mut();
                        let pressed = s.interaction.pressed.take();
                        if let Some(id) = pressed {
                            if s.interaction.hovered.as_deref() == Some(id.as_str()) {
                                s.run_on_click_for(&id);
                            }
                        }
                        s.rebuild();
                    }
                    request_one_redraw(state.clone());
                },
            ));
            canvas
                .add_event_listener_with_callback("pointerup", cb.as_ref().unchecked_ref())
                .ok();
            cb.forget();
        }
    }

    async fn load_image_with_retry(state: Rc<RefCell<AppState>>, source: ImageSource) {
        let url = match &source {
            ImageSource::Url { url } => url.clone(),
            ImageSource::Path { path } => {
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

    fn request_one_redraw(state: Rc<RefCell<AppState>>) {
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

    fn render_once(state: &Rc<RefCell<AppState>>) {
        use wgpu::CurrentSurfaceTexture;

        let mut s = state.borrow_mut();
        let AppState {
            renderer,
            surface,
            config,
            scene,
            ..
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
