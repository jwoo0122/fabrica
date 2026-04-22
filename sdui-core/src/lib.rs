//! Scene-graph core: node types, hierarchy, coordinate resolution (pure, no rendering).
//!
//! Iteration 4 completes the closed primitive set with `Image` (leaf, textured).
//!
//! AccessKit API reference (Rect, Node::set_bounds):
//!   <https://docs.rs/accesskit/0.24/accesskit/struct.Rect.html>
//!   <https://docs.rs/accesskit/0.24/accesskit/struct.Node.html>

pub use accesskit::NodeId;
use accesskit::{Action, Node, Rect, Role};
use sdui_cel::{EvalEnv, ExprCache, ExpressionEngine};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ── Scene wrapper (state + root) ─────────────────────────────────────────

/// A Scene with author-declared state + a scene-graph root.
///
/// Iteration 6 adds reactive state; authors pre-declare every key they
/// reference from CEL expressions. Missing keys are not invented at
/// runtime (Codex review finding #1: `state.missing_key` raises a
/// `NoSuchKey` execution error in cel-interpreter 0.10 — dot-access is
/// strict).
///
/// **Back-compat.** The custom `Deserialize` below accepts the bare
/// `SceneNode` shape too, so existing `examples/smoke/scene.json`
/// (which is just a `Frame` at the top level) continues to parse as
/// `Scene { state: {}, root: <Frame> }`.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    /// Author-declared state — every key referenced by CEL must appear here.
    pub state: BTreeMap<String, serde_json::Value>,
    /// The scene-graph root.
    pub root: SceneNode,
}

impl Scene {
    /// Wrap a bare `SceneNode` with an empty state map.
    pub fn new(root: SceneNode) -> Self {
        Self {
            state: BTreeMap::new(),
            root,
        }
    }
}

impl Serialize for Scene {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Out<'a> {
            state: &'a BTreeMap<String, serde_json::Value>,
            root: &'a SceneNode,
        }
        Out {
            state: &self.state,
            root: &self.root,
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Scene {
    /// Disambiguates bare `SceneNode` vs wrapped `{state, root}` by
    /// presence of a `type` key at the top level.
    ///
    /// A bare `SceneNode` always carries `"type": "Frame"|"Text"|...` (the
    /// `#[serde(tag = "type")]` discriminant on the `SceneNode` enum). A
    /// wrapped `Scene` does not, so the heuristic is deterministic.
    ///
    /// Codex review finding #2: using a plain `#[serde(untagged)]` here
    /// silently swallows malformed wrapped scenes and falls through to
    /// `BareRoot`. This hand-rolled dispatch fails loud on the wrapped
    /// path via `deny_unknown_fields`.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(d)?;
        let is_bare = value
            .as_object()
            .is_some_and(|o| o.contains_key("type"));
        if is_bare {
            let root: SceneNode = serde_json::from_value(value).map_err(D::Error::custom)?;
            return Ok(Scene::new(root));
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Strict {
            #[serde(default)]
            state: BTreeMap<String, serde_json::Value>,
            root: SceneNode,
        }
        let strict: Strict = serde_json::from_value(value).map_err(D::Error::custom)?;
        Ok(Scene {
            state: strict.state,
            root: strict.root,
        })
    }
}

// ── Declarative click mutations ──────────────────────────────────────────

/// A single state mutation triggered by a node's `on_click`.
///
/// Semantics: evaluate `set` (a CEL expression) against the current
/// [`EvalEnv`], coerce the result to a `serde_json::Value`, and write it
/// to `state[path]`. `path` is implicitly rooted at `state` — authors
/// write `"count"`, not `"state.count"` — so authored expressions cannot
/// write to `ui.*` or any other reserved namespace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mutation {
    /// Dotted path into the scene state (e.g. `"count"`, `"cart.items"`).
    pub path: String,
    /// CEL source producing the new value. Evaluated with the *pre-mutation* env.
    pub set: String,
}

// ── Node types ───────────────────────────────────────────────────────────

/// Tagged union of all scene node types.
///
/// Uses `#[serde(tag = "type")]` for internally-tagged JSON representation:
/// `{"type": "Frame", ...}` or `{"type": "Text", ...}` or `{"type": "Image", ...}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum SceneNode {
    /// A coloured rectangle container with relative positioning.
    Frame(Frame),
    /// A text leaf node (no children).
    Text(Text),
    /// An image leaf node (no children) — bitmap sampled from `ImageSource`.
    Image(Image),
    /// Conditional operator — resolved to one of its branches at flatten time.
    Condition(Condition),
}

impl SceneNode {
    /// The human-readable label of this node.
    pub fn label(&self) -> &str {
        match self {
            SceneNode::Frame(f) => &f.label,
            SceneNode::Text(t) => &t.label,
            SceneNode::Image(i) => &i.label,
            SceneNode::Condition(_) => "",
        }
    }

    /// Child nodes of this node (empty slice for leaf nodes).
    ///
    /// `Condition` returns `&[]` because its branches are not children in
    /// the scene-graph sense — only one is selected at flatten time, and
    /// resolving requires an `ExpressionEngine` which this accessor lacks.
    pub fn children(&self) -> &[SceneNode] {
        match self {
            SceneNode::Frame(f) => &f.children,
            SceneNode::Text(_) => &[],
            SceneNode::Image(_) => &[],
            SceneNode::Condition(_) => &[],
        }
    }

    /// Build a basic AccessKit [`Node`] for this scene node.
    ///
    /// Note: this uses the node's own (relative) coordinates for bounds.
    /// For absolute-coordinate bounds, use [`FlatNode::to_accesskit_node`].
    pub fn to_accesskit_node(&self) -> Node {
        match self {
            SceneNode::Frame(f) => f.to_accesskit_node(),
            SceneNode::Text(t) => t.to_accesskit_node(),
            SceneNode::Image(i) => i.to_accesskit_node(),
            SceneNode::Condition(_) => {
                unreachable!("Condition must be resolved before AccessKit projection")
            }
        }
    }
}

/// Frame primitive — a coloured rectangle with position relative to its parent.
///
/// Coordinates `x` and `y` are offsets from the parent's top-left corner.
/// For root-level Frames, they are absolute (offset from surface origin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    /// Author-facing stable ID. When set, this Frame can be hit-tested
    /// and referenced by `ui.hovered`/`ui.pressed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Human-readable label — also used as the AccessKit label.
    pub label: String,

    /// RGBA background colour, each component in `[0.0, 1.0]`.
    pub background_color: [f32; 4],

    /// Horizontal offset in pixels (relative to parent).
    pub x: f32,
    /// Vertical offset in pixels (relative to parent).
    pub y: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,

    /// Child nodes. Coordinates are relative to this Frame's top-left corner.
    #[serde(default)]
    pub children: Vec<SceneNode>,

    /// Declarative state mutations to run on click (pointer or AT).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_click: Vec<Mutation>,
}

impl Frame {
    /// Create a leaf Frame (no children).
    pub fn new(
        label: impl Into<String>,
        background_color: [f32; 4],
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Self {
        Self {
            id: None,
            label: label.into(),
            background_color,
            x,
            y,
            width,
            height,
            children: Vec::new(),
            on_click: Vec::new(),
        }
    }

    /// Build an AccessKit [`Node`] using this Frame's own (relative) coordinates.
    pub fn to_accesskit_node(&self) -> Node {
        let mut node = Node::new(Role::GenericContainer);
        node.set_label(self.label.clone());
        let bounds = Rect::new(
            self.x as f64,
            self.y as f64,
            (self.x + self.width) as f64,
            (self.y + self.height) as f64,
        );
        node.set_bounds(bounds);
        node
    }
}

/// Text primitive — a leaf node that displays a text string.
///
/// Text has no children. Position is relative to its parent Frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Text {
    /// Author-facing stable ID. See `Frame::id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Human-readable label for accessibility.
    pub label: String,

    /// The text content to display.
    pub content: String,

    /// Font size in pixels.
    pub font_size: f32,

    /// RGBA text colour, each component in `[0.0, 1.0]`.
    pub color: [f32; 4],

    /// Horizontal offset in pixels (relative to parent).
    pub x: f32,
    /// Vertical offset in pixels (relative to parent).
    pub y: f32,
    /// Bounding box width in pixels (used for AccessKit and text clipping).
    pub width: f32,
    /// Bounding box height in pixels.
    pub height: f32,

    /// Declarative state mutations to run on click (pointer or AT).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_click: Vec<Mutation>,
}

impl Text {
    /// Build an AccessKit [`Node`] for this text node.
    ///
    /// Uses `Role::Label` with `content` as the label.
    pub fn to_accesskit_node(&self) -> Node {
        let mut node = Node::new(Role::Label);
        node.set_label(self.content.clone());
        let bounds = Rect::new(
            self.x as f64,
            self.y as f64,
            (self.x + self.width) as f64,
            (self.y + self.height) as f64,
        );
        node.set_bounds(bounds);
        node
    }
}

/// Source of an image's bitmap bytes.
///
/// External tag `kind` (snake_case) on the wire:
/// `{"kind": "url", "url": "..."}` or `{"kind": "path", "path": "..."}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    /// Remote image fetched over HTTP.
    Url { url: String },
    /// Local filesystem path (native only; web treats as placeholder).
    Path { path: String },
}

/// Image primitive — a leaf node that displays a bitmap.
///
/// Image has no children. Position is relative to its parent.
/// Fit mode is fixed to `stretch` in this iteration (UV = 0..1). An `ImageFit`
/// enum is a planned extension point.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Image {
    /// Author-facing stable ID. See `Frame::id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Human-readable label — also used as the AccessKit label fallback.
    pub label: String,

    /// Where to load the bitmap from.
    pub source: ImageSource,

    /// Optional alt text for accessibility. Falls back to `label` when absent.
    #[serde(default)]
    pub alt: Option<String>,

    /// Horizontal offset in pixels (relative to parent).
    pub x: f32,
    /// Vertical offset in pixels (relative to parent).
    pub y: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,

    /// Declarative state mutations to run on click (pointer or AT).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_click: Vec<Mutation>,
}

impl Image {
    /// Build an AccessKit [`Node`] for this image node using `Role::Image`.
    pub fn to_accesskit_node(&self) -> Node {
        let mut node = Node::new(Role::Image);
        let label = self.alt.clone().unwrap_or_else(|| self.label.clone());
        node.set_label(label);
        let bounds = Rect::new(
            self.x as f64,
            self.y as f64,
            (self.x + self.width) as f64,
            (self.y + self.height) as f64,
        );
        node.set_bounds(bounds);
        node
    }
}

/// Conditional operator — selects one of two branches at flatten time.
///
/// The `when` expression is evaluated by an [`ExpressionEngine`] during
/// [`flatten_scene`]. On `true` the `then` branch is emitted; on `false`
/// the `else_branch` is emitted if present, otherwise nothing is emitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Condition {
    /// CEL source for the predicate.
    pub when: String,
    /// Subtree selected when `when` evaluates to `true`.
    pub then: Box<SceneNode>,
    /// Subtree selected when `when` evaluates to `false`. Optional.
    #[serde(rename = "else", default)]
    pub else_branch: Option<Box<SceneNode>>,
}

// ── Condition resolution (pre-flatten pass) ──────────────────────────────

/// Resolve every [`Condition`] in `root` into its selected branch, returning a
/// condition-free clone of the tree. Leaves `Frame` / `Text` / `Image` untouched
/// aside from filtering children whose `Condition` resolved to nothing.
///
/// - `when` evaluates to `true` → keep (recursively resolved) `then` branch.
/// - `when` evaluates to `false` → keep (recursively resolved) `else` branch,
///   or `None` when `else` is absent.
/// - parse / eval / non-bool failures log a warning to stderr and are treated
///   as `false`, matching the graceful-degradation policy documented in
///   `.iteration-5-criteria.md` §T-5.
///
/// Returns `None` only when the root itself is a `Condition` whose selected
/// branch is absent. Callers typically `.expect(...)` because a resolved-to-
/// nothing root would render a blank scene.
///
/// This pass exists so [`flatten_scene`] stays engine-free — the renderer in
/// `sdui-runtime-wgpu` never learns about `Condition` or the expression engine.
///
/// `env` supplies the variables the `Condition.when` expression reads
/// (`ui.*`, `state.*`). `cache` amortizes `Program::compile` across
/// repeated resolutions — Iteration 6 re-resolves on every hover/click.
pub fn resolve_scene<E: ExpressionEngine>(
    root: &SceneNode,
    engine: &E,
    env: &EvalEnv,
    cache: &mut ExprCache,
) -> Option<SceneNode> {
    match root {
        SceneNode::Frame(f) => {
            let children = f
                .children
                .iter()
                .filter_map(|c| resolve_scene(c, engine, env, cache))
                .collect();
            Some(SceneNode::Frame(Frame {
                id: f.id.clone(),
                label: f.label.clone(),
                background_color: f.background_color,
                x: f.x,
                y: f.y,
                width: f.width,
                height: f.height,
                children,
                on_click: f.on_click.clone(),
            }))
        }
        SceneNode::Text(t) => Some(SceneNode::Text(Text {
            id: t.id.clone(),
            label: t.label.clone(),
            // Interpolate `${cel}` segments at resolve time so the
            // renderer still receives a flat String. Non-interp content
            // passes through unchanged.
            content: interpolate_text(&t.content, engine, env, cache),
            font_size: t.font_size,
            color: t.color,
            x: t.x,
            y: t.y,
            width: t.width,
            height: t.height,
            on_click: t.on_click.clone(),
        })),
        SceneNode::Image(_) => Some(root.clone()),
        SceneNode::Condition(c) => match resolve_condition_branch(c, engine, env, cache) {
            Some(branch) => resolve_scene(branch, engine, env, cache),
            None => None,
        },
    }
}

// ── ${...} interpolation for Text.content ────────────────────────────────

/// Substitute every `${expr}` in `src` with the CEL-evaluated value of
/// `expr`. The grammar is:
///
/// ```text
/// text    := ( literal | escape | interp )*
/// escape  := "$$"  → "$"
///          | "$${" → "${"
/// interp  := "${" cel_expr_until_first_rbrace "}"
/// literal := any codepoint; bare "$" not followed by "$" or "{" is literal
/// ```
///
/// Grammar decisions (locked in during planning §Phase 5.6a):
/// - Bare `$` (e.g. `"Cost: $5.99"`) is a literal — no escape needed.
/// - `$$` escapes to a literal `$`.
/// - `$${` escapes to a literal `${` (so authors can write `${` in text).
/// - Unclosed `${...` and empty `${}` log to stderr and emit nothing for
///   that interp, matching Iteration-5's graceful-degradation policy
///   (see `resolve_condition_branch`).
/// - Nested `${a${b}}` is NOT supported in Iteration 6 — the first `}`
///   closes the interp, so `${a${b}}` yields `<eval of "a${b">` followed
///   by literal `}`. Author error, logged only if compile fails.
pub fn interpolate_text<E: ExpressionEngine>(
    src: &str,
    engine: &E,
    env: &EvalEnv,
    cache: &mut ExprCache,
) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut chunk_start = 0;

    // Non-`$` bytes are copied as str slices to preserve UTF-8.
    // `$`, `{`, `}`, `"`, `'`, `\` are all ASCII (single-byte), so byte
    // scans for them never cut through a multi-byte codepoint boundary.
    let flush = |out: &mut String, start: usize, end: usize, src: &str| {
        if start < end {
            out.push_str(&src[start..end]);
        }
    };

    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        // Flush accumulated literal before handling the escape / interp.
        flush(&mut out, chunk_start, i, src);

        // $$ and $${: escape sequences.
        if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            if i + 2 < bytes.len() && bytes[i + 2] == b'{' {
                out.push_str("${");
                i += 3;
            } else {
                out.push('$');
                i += 2;
            }
            chunk_start = i;
            continue;
        }
        // ${expr}: interpolate.
        if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            let expr_start = i + 2;
            match find_interp_end(bytes, expr_start) {
                Some(end) => {
                    let expr_src = &src[expr_start..end];
                    if expr_src.is_empty() {
                        eprintln!("sdui: empty ${{}} in text content: {src:?}; skipping");
                        i = end + 1;
                        chunk_start = i;
                        continue;
                    }
                    let rendered = compile_and_render(expr_src, engine, env, cache);
                    out.push_str(&rendered);
                    i = end + 1;
                    chunk_start = i;
                    continue;
                }
                None => {
                    // Unclosed ${: preserve the whole remainder verbatim
                    // (including the `${`) so authors see the malformed
                    // text in place instead of silent truncation. Codex
                    // adversarial review #2 (Iteration 6).
                    eprintln!(
                        "sdui: unclosed ${{ in text content: {src:?}; emitting literal remainder"
                    );
                    out.push_str(&src[i..]);
                    return out;
                }
            }
        }
        // Bare `$<other>`: literal `$`, plus keep scanning from next byte.
        out.push('$');
        i += 1;
        chunk_start = i;
    }
    flush(&mut out, chunk_start, bytes.len(), src);
    out
}

/// Locate the `}` that closes the current `${…}` interpolation.
///
/// Walks `bytes` starting at `start` (the byte after `${`) with a small
/// state machine that tracks:
///
/// - **Brace depth**, starting at 1. `{` → +1, `}` → −1; when depth
///   hits 0 the `}` we just consumed is the closing delimiter.
/// - **String literals**. CEL supports single- and double-quoted
///   strings; inside a string the tokenizer must not count braces, or
///   expressions like `${"}"}` would close one byte too early. We
///   honor `\` as the single-byte escape so `'\''` and `"\""` stay
///   balanced.
///
/// Codex adversarial review #2 (Iteration 6): the previous byte scan
/// stopped at the first `}`, mangling map literals and strings that
/// legitimately contained `}`.
///
/// Returns the byte index of the closing `}`, or `None` if the scan
/// reaches end-of-input without balancing the outer `{`.
fn find_interp_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth: i32 = 1;
    let mut i = start;
    // None = outside a string literal; Some(quote_byte) = inside it.
    let mut in_string: Option<u8> = None;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = in_string {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == q {
                in_string = None;
            }
        } else {
            match c {
                b'"' | b'\'' => in_string = Some(c),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Compile + evaluate a single `${...}` body; errors log to stderr and
/// return an empty string (same policy as `resolve_condition_branch`).
fn compile_and_render<E: ExpressionEngine>(
    expr_src: &str,
    engine: &E,
    env: &EvalEnv,
    cache: &mut ExprCache,
) -> String {
    let compiled = match cache.compile_cached(engine, expr_src) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "sdui: interp parse failed: {e} (expr = {:?})",
                expr_src
            );
            return String::new();
        }
    };
    match engine.eval(compiled, env) {
        Ok(v) => eval_value_to_string(&v),
        Err(e) => {
            eprintln!(
                "sdui: interp eval failed: {e} (expr = {:?})",
                expr_src
            );
            String::new()
        }
    }
}

/// Render an [`sdui_cel::EvalValue`] as a short, human-visible string.
///
/// - Scalars format as their natural representation (`123`, `true`, etc.).
/// - Strings emit themselves (no quoting).
/// - Lists / Maps emit their JSON form for debugging; authors should
///   prefer to project individual fields.
fn eval_value_to_string(v: &sdui_cel::EvalValue) -> String {
    use sdui_cel::EvalValue;
    match v {
        EvalValue::Null => String::new(),
        EvalValue::Bool(b) => b.to_string(),
        EvalValue::Int(i) => i.to_string(),
        EvalValue::UInt(u) => u.to_string(),
        EvalValue::Float(f) => f.to_string(),
        EvalValue::String(s) => s.clone(),
        EvalValue::List(_) | EvalValue::Map(_) => eval_value_to_json(v).to_string(),
    }
}

/// Select the branch of a [`Condition`] implied by its `when` expression.
/// Parse / eval failures warn on stderr and fall through to the `else` branch.
fn resolve_condition_branch<'a, E: ExpressionEngine>(
    c: &'a Condition,
    engine: &E,
    env: &EvalEnv,
    cache: &mut ExprCache,
) -> Option<&'a SceneNode> {
    let compile_result = cache.compile_cached(engine, &c.when);
    let compiled = match compile_result {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "sdui: condition parse failed: {e} (expr = {:?}); defaulting to false",
                c.when
            );
            return c.else_branch.as_deref();
        }
    };
    match engine.eval_bool(compiled, env) {
        Ok(true) => Some(&c.then),
        Ok(false) => c.else_branch.as_deref(),
        Err(e) => {
            eprintln!(
                "sdui: condition eval failed: {e} (expr = {:?}); defaulting to false",
                c.when
            );
            c.else_branch.as_deref()
        }
    }
}

// ── Flattening & coordinate resolution ───────────────────────────────────

/// Distinguishes node types in a flattened scene tree.
pub enum FlatNodeKind {
    /// A coloured rectangle.
    Frame { background_color: [f32; 4] },
    /// A text string to render.
    Text {
        content: String,
        font_size: f32,
        color: [f32; 4],
    },
    /// A bitmap to sample and stretch.
    Image {
        source: ImageSource,
        alt: Option<String>,
    },
}

/// A scene node resolved to absolute coordinates with an assigned [`NodeId`].
///
/// Produced by [`flatten_scene`] in depth-first (painter's algorithm) order:
/// parent nodes appear before their children, so rendering in list order
/// draws children on top of parents.
pub struct FlatNode {
    /// AccessKit-facing node id.
    ///
    /// Iteration 6: derived from a **stable hash of the author-assigned
    /// `id: Option<String>`** when present, or an ephemeral counter
    /// otherwise. This replaces the Iteration-5 behavior (traversal-
    /// order counter) which renumbered every id under a changed
    /// `Condition` branch — Codex review finding #8.
    pub id: NodeId,
    /// Author-assigned string id (stable across resolve runs).
    ///
    /// `Some` iff the source node carried an `id`. Hit-testing targets
    /// only these nodes; `ui.hovered` / `ui.pressed` carry the same
    /// string for CEL comparison.
    pub author_id: Option<String>,
    /// Human-readable label.
    pub label: String,
    /// The kind of node (Frame or Text) with type-specific data.
    pub kind: FlatNodeKind,
    /// Absolute X position (accumulated parent offsets).
    pub abs_x: f32,
    /// Absolute Y position (accumulated parent offsets).
    pub abs_y: f32,
    /// Width in pixels.
    pub width: f32,
    /// Height in pixels.
    pub height: f32,
    /// IDs of direct children (for AccessKit `set_children`).
    pub child_ids: Vec<NodeId>,
    /// Declarative state mutations to run on click.
    pub on_click: Vec<Mutation>,
}

impl FlatNode {
    /// Returns `true` if this node should receive pointer / AT clicks.
    pub fn is_clickable(&self) -> bool {
        !self.on_click.is_empty()
    }

    /// Build an AccessKit [`Node`] with absolute-coordinate bounds and children.
    ///
    /// Nodes with a non-empty `on_click` carry `Action::Click` and — for
    /// Frames — promote from `GenericContainer` to `Role::Button` so
    /// assistive tech announces them correctly (Codex review finding #10).
    pub fn to_accesskit_node(&self) -> Node {
        let bounds = Rect::new(
            self.abs_x as f64,
            self.abs_y as f64,
            (self.abs_x + self.width) as f64,
            (self.abs_y + self.height) as f64,
        );
        match &self.kind {
            FlatNodeKind::Frame { .. } => {
                let role = if self.is_clickable() {
                    Role::Button
                } else {
                    Role::GenericContainer
                };
                let mut node = Node::new(role);
                node.set_label(self.label.clone());
                node.set_bounds(bounds);
                if !self.child_ids.is_empty() {
                    node.set_children(self.child_ids.clone());
                }
                if self.is_clickable() {
                    node.add_action(Action::Click);
                }
                node
            }
            FlatNodeKind::Text { content, .. } => {
                let role = if self.is_clickable() {
                    Role::Button
                } else {
                    Role::Label
                };
                let mut node = Node::new(role);
                node.set_label(content.clone());
                node.set_bounds(bounds);
                if self.is_clickable() {
                    node.add_action(Action::Click);
                }
                node
            }
            FlatNodeKind::Image { alt, .. } => {
                let mut node = Node::new(Role::Image);
                let label = alt.clone().unwrap_or_else(|| self.label.clone());
                node.set_label(label);
                node.set_bounds(bounds);
                if self.is_clickable() {
                    node.add_action(Action::Click);
                }
                node
            }
        }
    }
}

/// Stable root-node ID used by all platform adapters.
pub const ROOT_NODE_ID: NodeId = NodeId(0);

/// Reserved high bit: when set, the `NodeId` was minted from the
/// ephemeral counter (an anonymous node), not a hash.
///
/// Using the top bit to distinguish keeps the hash and counter spaces
/// disjoint — no collision is possible.
const EPHEMERAL_NODE_ID_BASE: u64 = 0x8000_0000_0000_0000;

/// FNV-1a 64-bit hash — deterministic (unlike `std::hash::DefaultHasher`,
/// which randomizes per process) and allocation-free.
///
/// The standard FNV-1a offset basis and prime:
///   offset = 0xcbf29ce484222325
///   prime  = 0x100000001b3
///
/// Reference: <http://www.isthe.com/chongo/tech/comp/fnv/>
#[inline]
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Derive a stable [`NodeId`] for an author-assigned string id.
///
/// - High bit cleared (`& !EPHEMERAL_NODE_ID_BASE`) so the named space
///   stays disjoint from ephemeral counters.
/// - Maps the forbidden values `0` (root) and the ephemeral base down to
///   harmless constants so no legitimate author id can collide with
///   reserved slots.
fn node_id_from_string(s: &str) -> NodeId {
    let raw = fnv1a_64(s.as_bytes()) & !EPHEMERAL_NODE_ID_BASE;
    let safe = if raw == 0 { 1 } else { raw };
    NodeId(safe)
}

/// Flatten a scene tree into a list of [`FlatNode`]s with absolute coordinates.
///
/// `NodeId` assignment:
///   - Author-assigned `id: Some(s)` → FNV-1a hash of `s` (high bit clear).
///   - Anonymous nodes → `EPHEMERAL_NODE_ID_BASE | counter` starting at 1.
///   - `NodeId(0)` is reserved for [`ROOT_NODE_ID`] (the AccessKit window).
///
/// The input is expected to be condition-free — callers must run
/// [`resolve_scene`] first. Encountering a `Condition` here is a bug and
/// triggers `unreachable!`, mirroring [`SceneNode::to_accesskit_node`].
pub fn flatten_scene(root: &SceneNode) -> Vec<FlatNode> {
    let mut result = Vec::new();
    let mut next_ephemeral = 1u64;
    flatten_recursive(root, 0.0, 0.0, &mut next_ephemeral, &mut result);
    result
}

/// Mint an id for a node: hash of the author string if set, else an
/// ephemeral counter tagged with the high bit.
fn mint_node_id(author_id: &Option<String>, next_ephemeral: &mut u64) -> NodeId {
    match author_id {
        Some(s) => node_id_from_string(s),
        None => {
            let counter = *next_ephemeral;
            *next_ephemeral += 1;
            NodeId(EPHEMERAL_NODE_ID_BASE | counter)
        }
    }
}

fn flatten_recursive(
    node: &SceneNode,
    parent_abs_x: f32,
    parent_abs_y: f32,
    next_ephemeral: &mut u64,
    out: &mut Vec<FlatNode>,
) -> Option<NodeId> {
    match node {
        SceneNode::Frame(f) => {
            let my_id = mint_node_id(&f.id, next_ephemeral);
            let abs_x = parent_abs_x + f.x;
            let abs_y = parent_abs_y + f.y;

            let my_index = out.len();
            out.push(FlatNode {
                id: my_id,
                author_id: f.id.clone(),
                label: f.label.clone(),
                kind: FlatNodeKind::Frame {
                    background_color: f.background_color,
                },
                abs_x,
                abs_y,
                width: f.width,
                height: f.height,
                child_ids: Vec::new(),
                on_click: f.on_click.clone(),
            });

            let child_ids: Vec<NodeId> = f
                .children
                .iter()
                .filter_map(|child| flatten_recursive(child, abs_x, abs_y, next_ephemeral, out))
                .collect();

            out[my_index].child_ids = child_ids;
            Some(my_id)
        }
        SceneNode::Text(t) => {
            let my_id = mint_node_id(&t.id, next_ephemeral);
            let abs_x = parent_abs_x + t.x;
            let abs_y = parent_abs_y + t.y;

            out.push(FlatNode {
                id: my_id,
                author_id: t.id.clone(),
                label: t.label.clone(),
                kind: FlatNodeKind::Text {
                    content: t.content.clone(),
                    font_size: t.font_size,
                    color: t.color,
                },
                abs_x,
                abs_y,
                width: t.width,
                height: t.height,
                child_ids: Vec::new(),
                on_click: t.on_click.clone(),
            });
            Some(my_id)
        }
        SceneNode::Image(i) => {
            let my_id = mint_node_id(&i.id, next_ephemeral);
            let abs_x = parent_abs_x + i.x;
            let abs_y = parent_abs_y + i.y;

            out.push(FlatNode {
                id: my_id,
                author_id: i.id.clone(),
                label: i.label.clone(),
                kind: FlatNodeKind::Image {
                    source: i.source.clone(),
                    alt: i.alt.clone(),
                },
                abs_x,
                abs_y,
                width: i.width,
                height: i.height,
                child_ids: Vec::new(),
                on_click: i.on_click.clone(),
            });
            Some(my_id)
        }
        SceneNode::Condition(_) => {
            unreachable!("Condition must be resolved via resolve_scene before flatten")
        }
    }
}

// ── Author-ID uniqueness validation ──────────────────────────────────────

/// Errors surfaced by [`validate_flat`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SceneValidation {
    /// The same `author_id` appeared on more than one flattened node.
    /// Duplicates collapse to the same `NodeId` (FNV-1a is deterministic)
    /// which breaks AccessKit's stable-ID invariant AND makes click
    /// dispatch ambiguous (`run_on_click_for` picks the first match in
    /// flat order — the author has no way to target the second).
    ///
    /// This is always an author bug: the scene JSON re-used an id string.
    /// Codex adversarial review finding #1 (Iteration 6).
    #[error(
        "duplicate author id {id:?} appears on {count} flattened nodes; \
         AT tree IDs alias and click dispatch is ambiguous"
    )]
    DuplicateAuthorId {
        /// The repeated id string.
        id: String,
        /// How many times it appeared (≥ 2).
        count: usize,
    },
}

/// Reject any flattened scene whose author-assigned `id`s are not unique.
///
/// Returns the *first* duplicate in flat (depth-first) order so errors
/// are reproducible. Intended for callers to log + warn the author;
/// they may choose to continue rendering (with the understanding that
/// AT behavior is undefined) or abort.
pub fn validate_flat(flat: &[FlatNode]) -> Result<(), SceneValidation> {
    use std::collections::HashMap;
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for n in flat {
        if let Some(id) = n.author_id.as_deref() {
            *counts.entry(id).or_insert(0) += 1;
        }
    }
    for n in flat {
        if let Some(id) = n.author_id.as_deref() {
            let c = counts[id];
            if c > 1 {
                return Err(SceneValidation::DuplicateAuthorId {
                    id: id.to_string(),
                    count: c,
                });
            }
        }
    }
    Ok(())
}

// ── Hit-test + state mutations ───────────────────────────────────────────

/// Reverse-DFS hit-test: return the topmost (last-drawn) interactive node
/// at pointer position `(x, y)`, or `None` if no interactive node is hit.
///
/// An *interactive* node is one with `author_id.is_some()`. Anonymous
/// layout nodes are skipped — hit-testing only targets what an author
/// explicitly named.
///
/// `flatten_scene` emits in painter's order (parent before children), so
/// reverse iteration naturally picks the child over its parent when both
/// overlap the pointer.
pub fn hit_test(flat: &[FlatNode], x: f32, y: f32) -> Option<&FlatNode> {
    flat.iter().rev().find(|n| {
        n.author_id.is_some()
            && x >= n.abs_x
            && x < n.abs_x + n.width
            && y >= n.abs_y
            && y < n.abs_y + n.height
    })
}

/// Apply a list of [`Mutation`]s to a scene `state` map.
///
/// For each mutation: evaluate `set` against the current `env`, coerce
/// the resulting [`sdui_cel::EvalValue`] into a `serde_json::Value`, and
/// write it at `path`. `path` is a dot-separated sequence of keys rooted
/// at `state` (e.g. `"count"` targets `state["count"]`,
/// `"cart.items"` targets `state["cart"]["items"]`, creating intermediate
/// objects if needed).
///
/// Parse / eval failures are logged to stderr and the mutation is
/// skipped — same graceful-degradation policy as `resolve_scene`. The
/// first hard error (if any) is returned for the caller's awareness,
/// but subsequent mutations still run.
pub fn apply_mutations<E: ExpressionEngine>(
    state: &mut BTreeMap<String, serde_json::Value>,
    mutations: &[Mutation],
    engine: &E,
    env: &EvalEnv,
    cache: &mut ExprCache,
) {
    for m in mutations {
        let compiled = match cache.compile_cached(engine, &m.set) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "sdui: mutation parse failed: {e} (path = {:?}, set = {:?})",
                    m.path, m.set
                );
                continue;
            }
        };
        let value = match engine.eval(compiled, env) {
            Ok(v) => v,
            Err(e) => {
                eprintln!(
                    "sdui: mutation eval failed: {e} (path = {:?}, set = {:?})",
                    m.path, m.set
                );
                continue;
            }
        };
        let json = eval_value_to_json(&value);
        write_path(state, &m.path, json);
    }
}

/// Coerce a `serde_json::Value` into an [`sdui_cel::EvalValue`] for
/// injection into the CEL environment.
///
/// Numbers: prefer `i64` → `u64` → `f64`. Objects become `Map`s with
/// lexicographic key order (`BTreeMap`). Unrepresentable values (e.g.
/// `f64::NAN`) collapse to `Null`.
pub fn json_to_eval_value(v: &serde_json::Value) -> sdui_cel::EvalValue {
    use sdui_cel::EvalValue;
    match v {
        serde_json::Value::Null => EvalValue::Null,
        serde_json::Value::Bool(b) => EvalValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                EvalValue::Int(i)
            } else if let Some(u) = n.as_u64() {
                EvalValue::UInt(u)
            } else if let Some(f) = n.as_f64() {
                if f.is_finite() {
                    EvalValue::Float(f)
                } else {
                    EvalValue::Null
                }
            } else {
                EvalValue::Null
            }
        }
        serde_json::Value::String(s) => EvalValue::String(s.clone()),
        serde_json::Value::Array(a) => EvalValue::List(a.iter().map(json_to_eval_value).collect()),
        serde_json::Value::Object(o) => {
            let mut m = BTreeMap::new();
            for (k, v) in o {
                m.insert(k.clone(), json_to_eval_value(v));
            }
            EvalValue::Map(m)
        }
    }
}

/// Snapshot of pointer-derived interaction state to feed into the CEL env.
///
/// Both fields carry an author-assigned id (see [`Frame::id`]) or `None`
/// when nothing is hovered / pressed. CEL `Condition.when` expressions
/// branch on these via `ui.hovered == "btn_id"`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InteractionState {
    /// ID of the node the pointer is currently over, if any.
    pub hovered: Option<String>,
    /// ID of the node where the left mouse button was pressed (still held).
    pub pressed: Option<String>,
}

/// Build an [`EvalEnv`] suitable for `resolve_scene` from the current
/// interaction state + scene state. Always binds `ui` and `state`.
pub fn build_eval_env(
    interaction: &InteractionState,
    state: &BTreeMap<String, serde_json::Value>,
) -> EvalEnv {
    use sdui_cel::EvalValue;
    let mut env = EvalEnv::new();

    let mut ui = BTreeMap::new();
    ui.insert(
        "hovered".to_string(),
        interaction
            .hovered
            .as_ref()
            .map(|s| EvalValue::String(s.clone()))
            .unwrap_or(EvalValue::Null),
    );
    ui.insert(
        "pressed".to_string(),
        interaction
            .pressed
            .as_ref()
            .map(|s| EvalValue::String(s.clone()))
            .unwrap_or(EvalValue::Null),
    );
    env.set("ui", EvalValue::Map(ui));

    let mut state_map = BTreeMap::new();
    for (k, v) in state {
        state_map.insert(k.clone(), json_to_eval_value(v));
    }
    env.set("state", EvalValue::Map(state_map));

    env
}

/// After a rebuild, clear any `hovered` / `pressed` whose node no longer
/// appears in `flat`. Codex review finding #11.
pub fn refresh_interaction_after_rebuild(
    interaction: &mut InteractionState,
    flat: &[FlatNode],
) {
    if let Some(h) = &interaction.hovered {
        if !flat
            .iter()
            .any(|n| n.author_id.as_deref() == Some(h.as_str()))
        {
            interaction.hovered = None;
        }
    }
    if let Some(p) = &interaction.pressed {
        if !flat
            .iter()
            .any(|n| n.author_id.as_deref() == Some(p.as_str()))
        {
            interaction.pressed = None;
        }
    }
}

/// Coerce an [`sdui_cel::EvalValue`] into a `serde_json::Value` for
/// round-trip storage in `Scene.state`.
fn eval_value_to_json(v: &sdui_cel::EvalValue) -> serde_json::Value {
    use sdui_cel::EvalValue;
    match v {
        EvalValue::Null => serde_json::Value::Null,
        EvalValue::Bool(b) => serde_json::Value::Bool(*b),
        EvalValue::Int(i) => serde_json::Value::Number((*i).into()),
        EvalValue::UInt(u) => serde_json::Value::Number((*u).into()),
        EvalValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        EvalValue::String(s) => serde_json::Value::String(s.clone()),
        EvalValue::List(items) => {
            serde_json::Value::Array(items.iter().map(eval_value_to_json).collect())
        }
        EvalValue::Map(m) => {
            let mut obj = serde_json::Map::with_capacity(m.len());
            for (k, v) in m {
                obj.insert(k.clone(), eval_value_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
    }
}

/// Write `value` into `state` at the dotted `path`, creating intermediate
/// objects as needed. A trailing `path` component that traverses a non-
/// object is replaced outright — last-writer-wins.
fn write_path(
    state: &mut BTreeMap<String, serde_json::Value>,
    path: &str,
    value: serde_json::Value,
) {
    let mut parts = path.split('.').collect::<Vec<_>>();
    if parts.is_empty() {
        return;
    }
    let last = parts.pop().unwrap();
    // Walk/insert intermediate objects.
    let mut cursor: &mut serde_json::Value = match state.get_mut(parts.first().copied().unwrap_or(last)) {
        Some(v) if !parts.is_empty() => v,
        _ => {
            // Top-level single-key path.
            if parts.is_empty() {
                state.insert(last.to_string(), value);
                return;
            }
            // Insert a fresh object for the head.
            let head = parts.first().unwrap().to_string();
            state.insert(head.clone(), serde_json::Value::Object(serde_json::Map::new()));
            state.get_mut(&head).unwrap()
        }
    };
    for part in parts.iter().skip(1) {
        if !cursor.is_object() {
            *cursor = serde_json::Value::Object(serde_json::Map::new());
        }
        let obj = cursor.as_object_mut().unwrap();
        obj.entry(part.to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        cursor = obj.get_mut(*part).unwrap();
    }
    if !cursor.is_object() {
        *cursor = serde_json::Value::Object(serde_json::Map::new());
    }
    cursor
        .as_object_mut()
        .unwrap()
        .insert(last.to_string(), value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_node_json_round_trip() {
        let scene = SceneNode::Frame(Frame::new(
            "smoke-frame",
            [0.2, 0.6, 1.0, 1.0],
            100.0,
            80.0,
            300.0,
            200.0,
        ));

        let json = serde_json::to_string_pretty(&scene).expect("serialize");
        let deserialized: SceneNode = serde_json::from_str(&json).expect("deserialize");
        let json2 = serde_json::to_string_pretty(&deserialized).expect("re-serialize");

        assert_eq!(scene, deserialized);
        assert_eq!(json, json2);

        match &deserialized {
            SceneNode::Frame(f) => {
                assert_eq!(f.label, "smoke-frame");
                assert_eq!(f.background_color, [0.2, 0.6, 1.0, 1.0]);
                assert_eq!(f.x, 100.0);
                assert!(f.children.is_empty());
            }
            _ => panic!("expected Frame"),
        }
    }

    #[test]
    fn nested_scene_json_round_trip() {
        let scene = SceneNode::Frame(Frame {
            id: None,
            label: "parent".into(),
            background_color: [0.2, 0.6, 1.0, 1.0],
            x: 100.0,
            y: 80.0,
            width: 300.0,
            height: 200.0,
            children: vec![SceneNode::Frame(Frame::new(
                "child",
                [1.0, 0.4, 0.1, 1.0],
                20.0,
                20.0,
                120.0,
                80.0,
            ))],
            on_click: Vec::new(),
        });

        let json = serde_json::to_string_pretty(&scene).expect("serialize");
        let deserialized: SceneNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(scene, deserialized);
    }

    #[test]
    fn text_node_json_round_trip() {
        let scene = SceneNode::Text(Text {
            id: None,
            label: "greeting".into(),
            content: "Hello, SDUI!".into(),
            font_size: 24.0,
            color: [1.0, 1.0, 1.0, 1.0],
            x: 10.0,
            y: 5.0,
            width: 200.0,
            height: 30.0,
            on_click: Vec::new(),
        });

        let json = serde_json::to_string_pretty(&scene).expect("serialize");
        assert!(json.contains("\"type\": \"Text\""));

        let deserialized: SceneNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(scene, deserialized);
        assert_eq!(deserialized.label(), "greeting");
        assert!(deserialized.children().is_empty());
    }

    #[test]
    fn accesskit_node_has_bounds() {
        let frame = Frame::new("test", [1.0, 0.0, 0.0, 1.0], 10.0, 20.0, 100.0, 50.0);
        let node = frame.to_accesskit_node();

        let bounds = node.bounds().expect("bounds should be set");
        assert_eq!(bounds.x0, 10.0);
        assert_eq!(bounds.y0, 20.0);
        assert_eq!(bounds.x1, 110.0);
        assert_eq!(bounds.y1, 70.0);
    }

    #[test]
    fn flatten_scene_resolves_absolute_positions() {
        let scene = SceneNode::Frame(Frame {
            id: None,
            label: "parent".into(),
            background_color: [0.2, 0.6, 1.0, 1.0],
            x: 100.0,
            y: 80.0,
            width: 300.0,
            height: 200.0,
            children: vec![SceneNode::Frame(Frame::new(
                "child",
                [1.0, 0.4, 0.1, 1.0],
                20.0,
                10.0,
                120.0,
                80.0,
            ))],
            on_click: Vec::new(),
        });

        let flat = flatten_scene(&scene);
        assert_eq!(flat.len(), 2);

        // Parent and child both anonymous → ephemeral IDs (top bit set).
        assert_ne!(flat[0].id, ROOT_NODE_ID);
        assert_ne!(flat[1].id, ROOT_NODE_ID);
        assert_ne!(flat[0].id, flat[1].id);
        assert_eq!(flat[0].abs_x, 100.0);
        assert_eq!(flat[0].abs_y, 80.0);
        assert_eq!(flat[0].child_ids, vec![flat[1].id]);

        assert_eq!(flat[1].abs_x, 120.0);
        assert_eq!(flat[1].abs_y, 90.0);
        assert!(flat[1].child_ids.is_empty());
    }

    #[test]
    fn flatten_scene_with_text_child() {
        let scene = SceneNode::Frame(Frame {
            id: None,
            label: "container".into(),
            background_color: [0.0; 4],
            x: 50.0,
            y: 30.0,
            width: 200.0,
            height: 100.0,
            children: vec![SceneNode::Text(Text {
                id: None,
                label: "title".into(),
                content: "Hello".into(),
                font_size: 24.0,
                color: [1.0, 1.0, 1.0, 1.0],
                x: 10.0,
                y: 10.0,
                width: 100.0,
                height: 30.0,
                on_click: Vec::new(),
            })],
            on_click: Vec::new(),
        });

        let flat = flatten_scene(&scene);
        assert_eq!(flat.len(), 2);

        // Frame parent — child_ids contains the child's (ephemeral) id.
        assert_eq!(flat[0].child_ids, vec![flat[1].id]);
        assert!(matches!(flat[0].kind, FlatNodeKind::Frame { .. }));

        // Text child — absolute position resolved
        assert_eq!(flat[1].abs_x, 60.0); // 50 + 10
        assert_eq!(flat[1].abs_y, 40.0); // 30 + 10
        assert!(flat[1].child_ids.is_empty());
        match &flat[1].kind {
            FlatNodeKind::Text {
                content,
                font_size,
                color,
            } => {
                assert_eq!(content, "Hello");
                assert_eq!(*font_size, 24.0);
                assert_eq!(*color, [1.0, 1.0, 1.0, 1.0]);
            }
            _ => panic!("expected Text"),
        }

        // AccessKit: Text uses StaticText role with content as label
        let ak = flat[1].to_accesskit_node();
        assert_eq!(ak.role(), Role::Label);
        assert_eq!(ak.label().unwrap().to_string(), "Hello");
    }

    #[test]
    fn flat_node_accesskit_has_absolute_bounds_and_children() {
        let scene = SceneNode::Frame(Frame {
            id: None,
            label: "root-frame".into(),
            background_color: [0.0; 4],
            x: 50.0,
            y: 30.0,
            width: 200.0,
            height: 100.0,
            children: vec![SceneNode::Frame(Frame::new(
                "nested", [1.0; 4], 10.0, 10.0, 80.0, 40.0,
            ))],
            on_click: Vec::new(),
        });

        let flat = flatten_scene(&scene);
        let parent_ak = flat[0].to_accesskit_node();
        let child_ak = flat[1].to_accesskit_node();

        let pb = parent_ak.bounds().unwrap();
        assert_eq!(pb.x0, 50.0);
        assert_eq!(pb.y0, 30.0);
        assert_eq!(pb.x1, 250.0);
        assert_eq!(pb.y1, 130.0);
        assert_eq!(parent_ak.children(), &[flat[1].id]);

        let cb = child_ak.bounds().unwrap();
        assert_eq!(cb.x0, 60.0);
        assert_eq!(cb.y0, 40.0);
        assert_eq!(cb.x1, 140.0);
        assert_eq!(cb.y1, 80.0);
    }

    #[test]
    fn image_node_json_round_trip() {
        let scene = SceneNode::Image(Image {
            id: None,
            label: "smoke-image".into(),
            source: ImageSource::Url {
                url: "http://127.0.0.1:8138/assets/smoke.png".into(),
            },
            alt: Some("Checker pattern test image".into()),
            x: 20.0,
            y: 180.0,
            width: 120.0,
            height: 120.0,
            on_click: Vec::new(),
        });

        let json = serde_json::to_string_pretty(&scene).expect("serialize");
        assert!(json.contains("\"type\": \"Image\""));
        assert!(json.contains("\"kind\": \"url\""));

        let deserialized: SceneNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(scene, deserialized);
        assert_eq!(deserialized.label(), "smoke-image");
        assert!(deserialized.children().is_empty());

        match deserialized {
            SceneNode::Image(i) => match i.source {
                ImageSource::Url { url } => {
                    assert_eq!(url, "http://127.0.0.1:8138/assets/smoke.png");
                }
                ImageSource::Path { .. } => panic!("expected Url source"),
            },
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn flatten_scene_with_image_child() {
        let scene = SceneNode::Frame(Frame {
            id: None,
            label: "container".into(),
            background_color: [0.0; 4],
            x: 50.0,
            y: 30.0,
            width: 200.0,
            height: 200.0,
            children: vec![SceneNode::Image(Image {
                id: None,
                label: "pic".into(),
                source: ImageSource::Path {
                    path: "/tmp/pic.png".into(),
                },
                alt: Some("A test picture".into()),
                x: 10.0,
                y: 20.0,
                width: 64.0,
                height: 64.0,
                on_click: Vec::new(),
            })],
            on_click: Vec::new(),
        });

        let flat = flatten_scene(&scene);
        assert_eq!(flat.len(), 2);

        // Frame parent — child_ids contains the child's (ephemeral) id.
        assert_eq!(flat[0].child_ids, vec![flat[1].id]);
        assert!(matches!(flat[0].kind, FlatNodeKind::Frame { .. }));

        // Image child — absolute position resolved.
        assert_eq!(flat[1].abs_x, 60.0); // 50 + 10
        assert_eq!(flat[1].abs_y, 50.0); // 30 + 20
        assert!(flat[1].child_ids.is_empty());
        match &flat[1].kind {
            FlatNodeKind::Image { source, alt } => {
                assert_eq!(
                    *source,
                    ImageSource::Path {
                        path: "/tmp/pic.png".into()
                    }
                );
                assert_eq!(alt.as_deref(), Some("A test picture"));
            }
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn flat_node_image_uses_role_image() {
        let scene = SceneNode::Image(Image {
            id: None,
            label: "fallback-label".into(),
            source: ImageSource::Url {
                url: "http://example/foo.png".into(),
            },
            alt: None,
            x: 0.0,
            y: 0.0,
            width: 50.0,
            height: 50.0,
            on_click: Vec::new(),
        });

        let flat = flatten_scene(&scene);
        assert_eq!(flat.len(), 1);

        let ak = flat[0].to_accesskit_node();
        assert_eq!(ak.role(), Role::Image);
        // alt is None, so label falls back to node.label.
        assert_eq!(ak.label().unwrap().to_string(), "fallback-label");

        let bounds = ak.bounds().unwrap();
        assert_eq!(bounds.x0, 0.0);
        assert_eq!(bounds.y0, 0.0);
        assert_eq!(bounds.x1, 50.0);
        assert_eq!(bounds.y1, 50.0);
    }

    fn frame_a() -> SceneNode {
        SceneNode::Frame(Frame::new("A", [1.0, 0.0, 0.0, 1.0], 0.0, 0.0, 10.0, 10.0))
    }

    fn frame_b() -> SceneNode {
        SceneNode::Frame(Frame::new("B", [0.0, 1.0, 0.0, 1.0], 0.0, 0.0, 20.0, 20.0))
    }

    /// Test helper: resolve against an empty CEL env + fresh cache.
    fn resolve(scene: &SceneNode) -> Option<SceneNode> {
        let engine = sdui_cel::CelEngine::new();
        let env = sdui_cel::EvalEnv::new();
        let mut cache = sdui_cel::ExprCache::new();
        resolve_scene(scene, &engine, &env, &mut cache)
    }

    #[test]
    fn condition_node_json_round_trip() {
        let scene = SceneNode::Condition(Condition {
            when: "true".into(),
            then: Box::new(frame_a()),
            else_branch: Some(Box::new(frame_b())),
        });

        let json = serde_json::to_string_pretty(&scene).expect("serialize");
        assert!(json.contains("\"type\": \"Condition\""));
        assert!(json.contains("\"then\""));
        assert!(json.contains("\"else\""));
        assert!(!json.contains("else_branch"));

        let deserialized: SceneNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(scene, deserialized);
    }

    #[test]
    fn condition_with_missing_else_deserializes() {
        let json = r#"{
            "type": "Condition",
            "when": "true",
            "then": {
                "type": "Frame",
                "label": "A",
                "background_color": [1.0, 0.0, 0.0, 1.0],
                "x": 0.0, "y": 0.0, "width": 10.0, "height": 10.0
            }
        }"#;
        let decoded: SceneNode = serde_json::from_str(json).expect("deserialize");
        match decoded {
            SceneNode::Condition(c) => {
                assert_eq!(c.when, "true");
                assert!(c.else_branch.is_none());
            }
            _ => panic!("expected Condition"),
        }
    }

    #[test]
    fn resolve_scene_true_selects_then() {
        let scene = SceneNode::Condition(Condition {
            when: "true".into(),
            then: Box::new(frame_a()),
            else_branch: Some(Box::new(frame_b())),
        });
        let resolved =
            resolve(&scene).expect("root resolved to a branch");
        let flat = flatten_scene(&resolved);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].label, "A");
        assert_ne!(flat[0].id, ROOT_NODE_ID);
    }

    #[test]
    fn resolve_scene_false_selects_else() {
        let scene = SceneNode::Condition(Condition {
            when: "false".into(),
            then: Box::new(frame_a()),
            else_branch: Some(Box::new(frame_b())),
        });
        let resolved =
            resolve(&scene).expect("root resolved to a branch");
        let flat = flatten_scene(&resolved);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].label, "B");
        assert_ne!(flat[0].id, ROOT_NODE_ID);
    }

    #[test]
    fn resolve_scene_false_with_missing_else_emits_none() {
        let scene = SceneNode::Condition(Condition {
            when: "false".into(),
            then: Box::new(frame_a()),
            else_branch: None,
        });
        assert!(resolve(&scene).is_none());
    }

    #[test]
    fn resolve_scene_inside_frame_filters_child() {
        let scene = SceneNode::Frame(Frame {
            id: None,
            label: "parent".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            children: vec![
                SceneNode::Condition(Condition {
                    when: "true".into(),
                    then: Box::new(frame_a()),
                    else_branch: None,
                }),
                SceneNode::Condition(Condition {
                    when: "false".into(),
                    then: Box::new(frame_b()),
                    else_branch: None,
                }),
            ],
            on_click: Vec::new(),
        });
        let resolved = resolve(&scene).expect("root resolved");
        let flat = flatten_scene(&resolved);
        assert_eq!(flat.len(), 2);
        assert_ne!(flat[0].id, ROOT_NODE_ID);
        assert_eq!(flat[0].child_ids, vec![flat[1].id]);
        assert_eq!(flat[1].label, "A");
        assert_ne!(flat[1].id, ROOT_NODE_ID);
    }

    #[test]
    fn resolve_scene_eval_failure_treated_as_false() {
        let scene = SceneNode::Condition(Condition {
            when: "1 + 1".into(),
            then: Box::new(frame_a()),
            else_branch: Some(Box::new(frame_b())),
        });
        let resolved = resolve(&scene).expect("root resolved");
        let flat = flatten_scene(&resolved);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].label, "B");
    }

    #[test]
    fn resolve_scene_parse_failure_treated_as_false_with_warning() {
        let scene = SceneNode::Condition(Condition {
            when: "1 +".into(),
            then: Box::new(frame_a()),
            else_branch: Some(Box::new(frame_b())),
        });
        let resolved = resolve(&scene).expect("root resolved");
        let flat = flatten_scene(&resolved);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat[0].label, "B");
    }

    // ── Step 2: Scene wrapper + Mutation + id/on_click serde ─────────────

    #[test]
    fn scene_wrapper_round_trip() {
        let scene = Scene {
            state: {
                let mut m = BTreeMap::new();
                m.insert("count".into(), serde_json::json!(0));
                m
            },
            root: frame_a(),
        };
        let json = serde_json::to_string_pretty(&scene).expect("serialize");
        assert!(json.contains("\"state\""));
        assert!(json.contains("\"root\""));
        let deserialized: Scene = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(scene, deserialized);
    }

    #[test]
    fn scene_wrapper_back_compat_bare_root() {
        // Existing examples/smoke/scene.json shape — a bare SceneNode.
        let bare_json = r#"{
            "type": "Frame",
            "label": "smoke-frame",
            "background_color": [0.2, 0.6, 1.0, 1.0],
            "x": 100.0, "y": 80.0, "width": 300.0, "height": 320.0
        }"#;
        let scene: Scene = serde_json::from_str(bare_json).expect("deserialize bare");
        assert!(scene.state.is_empty(), "bare root implies empty state");
        match &scene.root {
            SceneNode::Frame(f) => assert_eq!(f.label, "smoke-frame"),
            _ => panic!("expected Frame"),
        }
    }

    #[test]
    fn scene_wrapper_malformed_fails_loud() {
        // Wrapped-looking JSON (no top-level `type`) without a `root` key must
        // fail — not fall through to bare parsing. Codex finding #2.
        let bad = r#"{"state": {}}"#;
        let err = serde_json::from_str::<Scene>(bad).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("root") || msg.contains("missing field"),
            "error message should mention missing root: {msg}"
        );
    }

    #[test]
    fn scene_wrapper_rejects_unknown_fields_on_wrapped() {
        // Wrapped form (no top-level "type") with an unknown field must fail.
        let bad = r#"{
            "state": {},
            "root": {
                "type": "Frame",
                "label": "x",
                "background_color": [0,0,0,1],
                "x": 0, "y": 0, "width": 1, "height": 1
            },
            "extra_field": "should fail"
        }"#;
        let err = serde_json::from_str::<Scene>(bad).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("extra_field") || msg.contains("unknown field"),
            "expected unknown-field error, got: {msg}"
        );
    }

    #[test]
    fn mutation_serde_round_trip() {
        let m = Mutation {
            path: "count".into(),
            set: "state.count + 1".into(),
        };
        let json = serde_json::to_string(&m).expect("serialize");
        assert_eq!(json, r#"{"path":"count","set":"state.count + 1"}"#);
        let back: Mutation = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(m, back);
    }

    #[test]
    fn frame_with_id_and_on_click_round_trips() {
        let frame = SceneNode::Frame(Frame {
            id: Some("counter_btn".into()),
            label: "button".into(),
            background_color: [0.2, 0.45, 0.8, 1.0],
            x: 100.0,
            y: 200.0,
            width: 160.0,
            height: 64.0,
            children: vec![],
            on_click: vec![Mutation {
                path: "count".into(),
                set: "state.count + 1".into(),
            }],
        });
        let json = serde_json::to_string(&frame).expect("serialize");
        assert!(json.contains("\"id\":\"counter_btn\""));
        assert!(json.contains("\"on_click\""));
        assert!(json.contains("\"path\":\"count\""));
        let back: SceneNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(frame, back);
    }

    #[test]
    fn frame_without_id_or_on_click_keeps_serialized_json_clean() {
        // skip_serializing_if keeps the old smoke scene byte-compatible.
        let frame = SceneNode::Frame(Frame::new("x", [0.0; 4], 0.0, 0.0, 1.0, 1.0));
        let json = serde_json::to_string(&frame).expect("serialize");
        assert!(
            !json.contains("\"id\""),
            "empty id should not serialize: {json}"
        );
        assert!(
            !json.contains("\"on_click\""),
            "empty on_click should not serialize: {json}"
        );
    }

    #[test]
    fn existing_smoke_scene_still_parses_as_scene() {
        // The committed examples/smoke/scene.json is a bare Frame. Scene must
        // swallow it via the back-compat path.
        let smoke_json = include_str!("../../examples/smoke/scene.json");
        let scene: Scene = serde_json::from_str(smoke_json).expect("deserialize smoke");
        assert!(scene.state.is_empty());
        match &scene.root {
            SceneNode::Frame(f) => assert_eq!(f.label, "smoke-frame"),
            _ => panic!("expected Frame"),
        }
    }

    // ── Step 3: stable NodeId + hit_test + apply_mutations ────────────────

    #[test]
    fn stable_nodeid_hash_of_string_id_is_deterministic() {
        // Two flatten runs of the same scene produce the same NodeId for
        // id'd nodes.
        let scene = SceneNode::Frame(Frame {
            id: Some("counter_btn".into()),
            label: "btn".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            children: vec![],
            on_click: vec![],
        });
        let a = flatten_scene(&scene);
        let b = flatten_scene(&scene);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].id, b[0].id);
        assert_eq!(a[0].author_id.as_deref(), Some("counter_btn"));
    }

    #[test]
    fn stable_nodeid_different_ids_get_different_hashes() {
        let mk = |id: &str| -> SceneNode {
            SceneNode::Frame(Frame {
                id: Some(id.into()),
                label: "x".into(),
                background_color: [0.0; 4],
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                children: vec![],
                on_click: vec![],
            })
        };
        let a = flatten_scene(&mk("btn_a"));
        let b = flatten_scene(&mk("btn_b"));
        assert_ne!(a[0].id, b[0].id);
    }

    #[test]
    fn anonymous_nodes_get_ephemeral_nodeid_with_top_bit_set() {
        let scene = frame_a(); // anonymous (no id)
        let flat = flatten_scene(&scene);
        assert_eq!(flat.len(), 1);
        assert_ne!(flat[0].id, ROOT_NODE_ID);
        assert!(
            (flat[0].id.0 & EPHEMERAL_NODE_ID_BASE) != 0,
            "anonymous id {:#x} should have ephemeral high bit set",
            flat[0].id.0
        );
        assert!(flat[0].author_id.is_none());
    }

    #[test]
    fn named_and_anonymous_id_spaces_are_disjoint() {
        // Two unrelated scenes — named hash must never clash with an
        // ephemeral counter value.
        let named = SceneNode::Frame(Frame {
            id: Some("z".into()),
            label: "z".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
            children: vec![],
            on_click: vec![],
        });
        let named_flat = flatten_scene(&named);
        let anon_flat = flatten_scene(&frame_a());
        assert_eq!(named_flat[0].id.0 & EPHEMERAL_NODE_ID_BASE, 0);
        assert_ne!(anon_flat[0].id.0 & EPHEMERAL_NODE_ID_BASE, 0);
    }

    #[test]
    fn hit_test_topmost_wins() {
        // Two children at the same coords: the second (later in child list,
        // drawn on top) must win.
        let scene = SceneNode::Frame(Frame {
            id: None,
            label: "root".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            children: vec![
                SceneNode::Frame(Frame {
                    id: Some("a".into()),
                    label: "a".into(),
                    background_color: [0.0; 4],
                    x: 10.0,
                    y: 10.0,
                    width: 50.0,
                    height: 50.0,
                    children: vec![],
                    on_click: vec![],
                }),
                SceneNode::Frame(Frame {
                    id: Some("b".into()),
                    label: "b".into(),
                    background_color: [0.0; 4],
                    x: 20.0,
                    y: 20.0,
                    width: 50.0,
                    height: 50.0,
                    children: vec![],
                    on_click: vec![],
                }),
            ],
            on_click: vec![],
        });
        let flat = flatten_scene(&scene);
        // Point (30, 30) is inside both a (10..60, 10..60) and b (20..70, 20..70).
        let hit = hit_test(&flat, 30.0, 30.0).expect("hit");
        assert_eq!(hit.author_id.as_deref(), Some("b"));
    }

    #[test]
    fn hit_test_ignores_nodes_without_id() {
        // Only id'd nodes are interactive targets.
        let scene = SceneNode::Frame(Frame {
            id: None, // anonymous → never a target
            label: "root".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            children: vec![],
            on_click: vec![],
        });
        let flat = flatten_scene(&scene);
        assert_eq!(flat.len(), 1);
        assert!(hit_test(&flat, 50.0, 50.0).is_none());
    }

    #[test]
    fn hit_test_respects_absolute_bounds() {
        let scene = SceneNode::Frame(Frame {
            id: Some("btn".into()),
            label: "btn".into(),
            background_color: [0.0; 4],
            x: 100.0,
            y: 200.0,
            width: 160.0,
            height: 64.0,
            children: vec![],
            on_click: vec![],
        });
        let flat = flatten_scene(&scene);
        // Inside.
        assert!(hit_test(&flat, 180.0, 232.0).is_some());
        // Just outside (right edge is exclusive).
        assert!(hit_test(&flat, 260.0, 232.0).is_none());
        // Inside lower-right corner (bottom edge is exclusive).
        assert!(hit_test(&flat, 259.0, 263.0).is_some());
        assert!(hit_test(&flat, 100.0, 264.0).is_none());
    }

    #[test]
    fn apply_mutations_updates_state_map() {
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();

        let mut state = BTreeMap::new();
        state.insert("count".to_string(), serde_json::json!(0));

        // Build env from current state.
        let mut env = sdui_cel::EvalEnv::new();
        let mut state_map = BTreeMap::new();
        state_map.insert("count".to_string(), sdui_cel::EvalValue::Int(0));
        env.set("state", sdui_cel::EvalValue::Map(state_map));

        let muts = vec![Mutation {
            path: "count".into(),
            set: "state.count + 1".into(),
        }];
        apply_mutations(&mut state, &muts, &engine, &env, &mut cache);
        assert_eq!(state.get("count"), Some(&serde_json::json!(1)));
    }

    #[test]
    fn apply_mutations_skips_parse_failures_and_continues() {
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();

        let mut state = BTreeMap::new();
        state.insert("count".into(), serde_json::json!(5));
        let mut env = sdui_cel::EvalEnv::new();
        let mut state_map = BTreeMap::new();
        state_map.insert("count".into(), sdui_cel::EvalValue::Int(5));
        env.set("state", sdui_cel::EvalValue::Map(state_map));

        let muts = vec![
            Mutation {
                path: "count".into(),
                set: "1 +".into(), // parse failure — skipped
            },
            Mutation {
                path: "count".into(),
                set: "state.count + 10".into(),
            },
        ];
        apply_mutations(&mut state, &muts, &engine, &env, &mut cache);
        assert_eq!(state.get("count"), Some(&serde_json::json!(15)));
    }

    #[test]
    fn apply_mutations_writes_nested_path() {
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();
        let mut state: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        let env = sdui_cel::EvalEnv::new();
        let muts = vec![Mutation {
            path: "cart.items".into(),
            set: "3".into(),
        }];
        apply_mutations(&mut state, &muts, &engine, &env, &mut cache);
        assert_eq!(
            state.get("cart"),
            Some(&serde_json::json!({"items": 3}))
        );
    }

    #[test]
    fn resolve_with_hovered_switches_condition_branch() {
        // A Frame wrapping a Condition on `ui.hovered`. Two environments
        // should flip the branch.
        let scene = SceneNode::Condition(Condition {
            when: "ui.hovered == 'btn'".into(),
            then: Box::new(SceneNode::Frame(Frame {
                id: None,
                label: "hovered".into(),
                background_color: [0.3, 0.6, 0.9, 1.0],
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
                children: vec![],
                on_click: vec![],
            })),
            else_branch: Some(Box::new(SceneNode::Frame(Frame {
                id: None,
                label: "idle".into(),
                background_color: [0.2, 0.4, 0.7, 1.0],
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
                children: vec![],
                on_click: vec![],
            }))),
        });

        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();

        // Env #1 — nothing hovered.
        let mut env_idle = sdui_cel::EvalEnv::new();
        let mut ui = BTreeMap::new();
        ui.insert("hovered".into(), sdui_cel::EvalValue::Null);
        ui.insert("pressed".into(), sdui_cel::EvalValue::Null);
        env_idle.set("ui", sdui_cel::EvalValue::Map(ui));

        let resolved_idle =
            resolve_scene(&scene, &engine, &env_idle, &mut cache).expect("root");
        assert_eq!(resolved_idle.label(), "idle");

        // Env #2 — 'btn' hovered.
        let mut env_hover = sdui_cel::EvalEnv::new();
        let mut ui = BTreeMap::new();
        ui.insert("hovered".into(), sdui_cel::EvalValue::String("btn".into()));
        ui.insert("pressed".into(), sdui_cel::EvalValue::Null);
        env_hover.set("ui", sdui_cel::EvalValue::Map(ui));

        let resolved_hover =
            resolve_scene(&scene, &engine, &env_hover, &mut cache).expect("root");
        assert_eq!(resolved_hover.label(), "hovered");
    }

    #[test]
    fn fnv1a_64_is_deterministic() {
        // Sanity: the hash function must not depend on process state.
        assert_eq!(fnv1a_64(b"counter_btn"), fnv1a_64(b"counter_btn"));
        assert_ne!(fnv1a_64(b"counter_btn"), fnv1a_64(b"counter_btn_2"));
    }

    // ── Step 4: counter example end-to-end (logic only, no window) ──────

    /// Load the counter example JSON into a `Scene`.
    fn load_counter_scene() -> Scene {
        let json = include_str!("../../examples/counter/scene.json");
        serde_json::from_str(json).expect("parse counter scene")
    }

    #[test]
    fn counter_scene_parses_with_initial_count() {
        let scene = load_counter_scene();
        assert_eq!(scene.state.get("count"), Some(&serde_json::json!(0)));
        match &scene.root {
            SceneNode::Frame(f) => assert_eq!(f.label, "counter-root"),
            _ => panic!("expected Frame root"),
        }
    }

    #[test]
    fn counter_idle_resolves_to_button_idle_branch() {
        let scene = load_counter_scene();
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();

        let interaction = InteractionState::default();
        let env = build_eval_env(&interaction, &scene.state);
        let resolved =
            resolve_scene(&scene.root, &engine, &env, &mut cache).expect("resolve idle");
        let root = match resolved {
            SceneNode::Frame(f) => f,
            _ => panic!("root"),
        };
        let btn = match &root.children[0] {
            SceneNode::Frame(f) => f,
            _ => panic!("button"),
        };
        assert_eq!(btn.label, "button-idle");
        assert_eq!(btn.id.as_deref(), Some("counter_btn"));
    }

    #[test]
    fn counter_hovered_resolves_to_button_hovered_branch() {
        let scene = load_counter_scene();
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();

        let interaction = InteractionState {
            hovered: Some("counter_btn".into()),
            pressed: None,
        };
        let env = build_eval_env(&interaction, &scene.state);
        let resolved =
            resolve_scene(&scene.root, &engine, &env, &mut cache).expect("resolve hover");
        let root = match resolved {
            SceneNode::Frame(f) => f,
            _ => panic!("root"),
        };
        let btn = match &root.children[0] {
            SceneNode::Frame(f) => f,
            _ => panic!("button"),
        };
        assert_eq!(btn.label, "button-hovered");
        assert_eq!(btn.id.as_deref(), Some("counter_btn"));
        // Sanity: the hovered variant has the bluer highlight color.
        assert!(btn.background_color[0] > 0.25 && btn.background_color[2] >= 0.99);
    }

    #[test]
    fn counter_click_increments_state_count() {
        let scene = load_counter_scene();
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();

        let mut state = scene.state.clone();
        assert_eq!(state.get("count"), Some(&serde_json::json!(0)));

        // Simulate: hovered over button, then fire mutations.
        let interaction = InteractionState {
            hovered: Some("counter_btn".into()),
            pressed: Some("counter_btn".into()),
        };

        for click in 1..=3 {
            let env = build_eval_env(&interaction, &state);
            let resolved =
                resolve_scene(&scene.root, &engine, &env, &mut cache).expect("resolve");
            let flat = flatten_scene(&resolved);
            let btn = flat
                .iter()
                .find(|n| n.author_id.as_deref() == Some("counter_btn"))
                .expect("counter_btn flat");
            apply_mutations(&mut state, &btn.on_click, &engine, &env, &mut cache);
            assert_eq!(
                state.get("count"),
                Some(&serde_json::json!(click)),
                "after {click} clicks"
            );
        }
    }

    #[test]
    fn counter_hit_test_picks_button_at_center() {
        let scene = load_counter_scene();
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();

        let interaction = InteractionState::default();
        let env = build_eval_env(&interaction, &scene.state);
        let resolved = resolve_scene(&scene.root, &engine, &env, &mut cache).expect("resolve");
        let flat = flatten_scene(&resolved);

        // Button is at (100, 200)-(260, 264), center ≈ (180, 232).
        let hit = hit_test(&flat, 180.0, 232.0).expect("hit");
        assert_eq!(hit.author_id.as_deref(), Some("counter_btn"));

        // Point far from any interactive node → None.
        assert!(hit_test(&flat, 5.0, 5.0).is_none());

        // Over the counter display (300-600, 210-250).
        let hit = hit_test(&flat, 450.0, 230.0).expect("hit display");
        assert_eq!(hit.author_id.as_deref(), Some("counter_display"));
    }

    #[test]
    fn refresh_interaction_drops_stale_hovered() {
        // Simulate: hovered was set to some id that no longer exists in
        // the resolved flat (because a Condition flip removed the node).
        let scene = frame_a();
        let flat = flatten_scene(&scene);
        let mut interaction = InteractionState {
            hovered: Some("vanished".into()),
            pressed: Some("also_gone".into()),
        };
        refresh_interaction_after_rebuild(&mut interaction, &flat);
        assert!(interaction.hovered.is_none());
        assert!(interaction.pressed.is_none());
    }

    #[test]
    fn refresh_interaction_keeps_valid_hovered() {
        let scene = SceneNode::Frame(Frame {
            id: Some("kept".into()),
            label: "kept".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            children: vec![],
            on_click: vec![],
        });
        let flat = flatten_scene(&scene);
        let mut interaction = InteractionState {
            hovered: Some("kept".into()),
            pressed: None,
        };
        refresh_interaction_after_rebuild(&mut interaction, &flat);
        assert_eq!(interaction.hovered.as_deref(), Some("kept"));
    }

    // ── Step 5: ${} interpolation + AccessKit clickable roles ────────────

    fn interp_helper(src: &str, env: &EvalEnv) -> String {
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();
        interpolate_text(src, &engine, env, &mut cache)
    }

    fn env_with_count(count: i64) -> EvalEnv {
        let mut env = EvalEnv::new();
        let mut state = BTreeMap::new();
        state.insert("count".into(), sdui_cel::EvalValue::Int(count));
        env.set("state", sdui_cel::EvalValue::Map(state));
        env
    }

    #[test]
    fn interp_passes_through_plain_text() {
        let env = env_with_count(0);
        assert_eq!(interp_helper("Hello, world!", &env), "Hello, world!");
    }

    #[test]
    fn interp_bare_dollar_is_literal() {
        let env = env_with_count(0);
        assert_eq!(interp_helper("Cost: $5.99", &env), "Cost: $5.99");
        assert_eq!(interp_helper("trailing $", &env), "trailing $");
    }

    #[test]
    fn interp_escape_dollar() {
        let env = env_with_count(0);
        assert_eq!(interp_helper("$$", &env), "$");
        assert_eq!(interp_helper("price $$5", &env), "price $5");
    }

    #[test]
    fn interp_escape_braced_dollar() {
        let env = env_with_count(0);
        // `$${x}` → literal `${x}`, no substitution.
        assert_eq!(interp_helper("$${x}", &env), "${x}");
    }

    #[test]
    fn interp_single_interp() {
        let env = env_with_count(7);
        assert_eq!(interp_helper("Count: ${state.count}", &env), "Count: 7");
    }

    #[test]
    fn interp_multiple_interps() {
        let mut env = EvalEnv::new();
        env.set("x", sdui_cel::EvalValue::Int(1));
        env.set("y", sdui_cel::EvalValue::Int(2));
        assert_eq!(interp_helper("${x} and ${y}", &env), "1 and 2");
        assert_eq!(interp_helper("${x}${y}", &env), "12");
    }

    #[test]
    fn interp_unclosed_preserves_literal_remainder() {
        let env = env_with_count(0);
        // Post-fix for Codex HIGH #2: unclosed `${…` no longer silently
        // truncates — the literal `${unclosed` stays visible so authors
        // see their malformed content in place.
        assert_eq!(
            interp_helper("Count: ${unclosed", &env),
            "Count: ${unclosed"
        );
    }

    #[test]
    fn interp_parser_handles_double_quoted_brace_in_string() {
        let env = env_with_count(0);
        // `${"}"}` — the `}` inside the double-quoted string must not
        // close the interp.
        assert_eq!(interp_helper("${\"}\"}", &env), "}");
    }

    #[test]
    fn interp_parser_handles_single_quoted_brace_in_string() {
        let env = env_with_count(0);
        assert_eq!(interp_helper("${'}'}", &env), "}");
    }

    #[test]
    fn interp_parser_handles_nested_braces_in_expression() {
        // Map literal in CEL uses `{` `}`. Example: `{"k": 1}["k"]`
        // evaluates to `1`.
        let env = env_with_count(0);
        assert_eq!(interp_helper("${{'k': 1}['k']}", &env), "1");
    }

    #[test]
    fn interp_parser_handles_backslash_escaped_quote_in_string() {
        // `${'a\'b'}` — the string contains a literal `'` courtesy of `\`.
        // The expression evaluates to the string `a'b` so we render it.
        let env = env_with_count(0);
        assert_eq!(interp_helper("${'a\\'b'}", &env), "a'b");
    }

    #[test]
    fn interp_empty_braces_skipped() {
        let env = env_with_count(0);
        assert_eq!(interp_helper("before${}after", &env), "beforeafter");
    }

    #[test]
    fn interp_parse_failure_substitutes_empty() {
        let env = env_with_count(0);
        // `1 +` is a parse error — interpolate_text logs + emits "".
        assert_eq!(interp_helper("val=${1 +}", &env), "val=");
    }

    #[test]
    fn interp_eval_failure_substitutes_empty() {
        let env = env_with_count(0);
        // `undeclared` raises UndeclaredReference — emit "".
        assert_eq!(interp_helper("${undeclared}", &env), "");
    }

    #[test]
    fn interp_preserves_utf8_multibyte() {
        let mut env = EvalEnv::new();
        env.set("name", sdui_cel::EvalValue::String("세계".into()));
        assert_eq!(interp_helper("안녕, ${name}!", &env), "안녕, 세계!");
    }

    #[test]
    fn interp_balanced_nested_braces_are_forwarded_to_cel() {
        // Post-fix for Codex HIGH #2: braces inside the expression are
        // balanced, not swallowed. `${a${b}}` forwards the whole body
        // `a${b}` to the engine, which parse-fails ("${" isn't CEL) →
        // empty substitution, per Iteration-5 graceful degradation.
        let mut env = EvalEnv::new();
        env.set("a", sdui_cel::EvalValue::Int(1));
        assert_eq!(interp_helper("${a${b}}", &env), "");
    }

    #[test]
    fn resolve_scene_interpolates_text_content() {
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();
        let scene = SceneNode::Text(Text {
            id: Some("display".into()),
            label: "display".into(),
            content: "Count: ${state.count}".into(),
            font_size: 16.0,
            color: [1.0; 4],
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 20.0,
            on_click: vec![],
        });
        let mut env = EvalEnv::new();
        let mut state = BTreeMap::new();
        state.insert("count".into(), sdui_cel::EvalValue::Int(42));
        env.set("state", sdui_cel::EvalValue::Map(state));

        let resolved =
            resolve_scene(&scene, &engine, &env, &mut cache).expect("resolve");
        match resolved {
            SceneNode::Text(t) => assert_eq!(t.content, "Count: 42"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn counter_display_interpolates_current_count() {
        let scene = load_counter_scene();
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();
        let interaction = InteractionState::default();
        let env = build_eval_env(&interaction, &scene.state);

        let resolved =
            resolve_scene(&scene.root, &engine, &env, &mut cache).expect("resolve");
        let root = match resolved {
            SceneNode::Frame(f) => f,
            _ => panic!("root"),
        };
        // The Text node is the 2nd child (after the button Condition).
        match &root.children[1] {
            SceneNode::Text(t) => {
                assert_eq!(t.content, "Count: 0");
                assert_eq!(t.id.as_deref(), Some("counter_display"));
            }
            _ => panic!("expected Text for counter_display"),
        }
    }

    #[test]
    fn counter_full_click_cycle_increments_display_text() {
        let scene = load_counter_scene();
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();
        let mut state = scene.state.clone();

        for expected_count in 1..=3 {
            let interaction = InteractionState {
                hovered: Some("counter_btn".into()),
                pressed: Some("counter_btn".into()),
            };
            let env = build_eval_env(&interaction, &state);
            let resolved =
                resolve_scene(&scene.root, &engine, &env, &mut cache).expect("resolve");
            let flat = flatten_scene(&resolved);

            let btn = flat
                .iter()
                .find(|n| n.author_id.as_deref() == Some("counter_btn"))
                .unwrap();
            apply_mutations(&mut state, &btn.on_click, &engine, &env, &mut cache);

            // Re-resolve after the mutation and check the display text.
            let env_after = build_eval_env(&interaction, &state);
            let resolved_after =
                resolve_scene(&scene.root, &engine, &env_after, &mut cache).expect("resolve");
            let flat_after = flatten_scene(&resolved_after);
            let display = flat_after
                .iter()
                .find(|n| n.author_id.as_deref() == Some("counter_display"))
                .unwrap();
            match &display.kind {
                FlatNodeKind::Text { content, .. } => {
                    assert_eq!(content, &format!("Count: {expected_count}"));
                }
                _ => panic!("expected Text"),
            }
        }
    }

    #[test]
    fn clickable_frame_promotes_role_to_button_and_adds_action() {
        let scene = SceneNode::Frame(Frame {
            id: Some("btn".into()),
            label: "btn".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            children: vec![],
            on_click: vec![Mutation {
                path: "count".into(),
                set: "1".into(),
            }],
        });
        let flat = flatten_scene(&scene);
        let ak = flat[0].to_accesskit_node();
        assert_eq!(ak.role(), Role::Button);
        assert!(
            ak.supports_action(Action::Click),
            "clickable Frame must carry Action::Click"
        );
    }

    #[test]
    fn non_clickable_frame_stays_generic_container_without_click_action() {
        let scene = frame_a(); // no on_click
        let flat = flatten_scene(&scene);
        let ak = flat[0].to_accesskit_node();
        assert_eq!(ak.role(), Role::GenericContainer);
        assert!(!ak.supports_action(Action::Click));
    }

    // ── Post-I6 HIGH#1: duplicate-id validation ───────────────────────

    #[test]
    fn validate_flat_accepts_unique_ids() {
        let scene = SceneNode::Frame(Frame {
            id: Some("root".into()),
            label: "root".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            children: vec![SceneNode::Frame(Frame {
                id: Some("child".into()),
                label: "child".into(),
                background_color: [0.0; 4],
                x: 0.0,
                y: 0.0,
                width: 50.0,
                height: 50.0,
                children: vec![],
                on_click: vec![],
            })],
            on_click: vec![],
        });
        let flat = flatten_scene(&scene);
        assert!(validate_flat(&flat).is_ok());
    }

    #[test]
    fn validate_flat_accepts_all_anonymous_nodes() {
        let scene = SceneNode::Frame(Frame {
            id: None,
            label: "anon-parent".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            children: vec![frame_a(), frame_b()],
            on_click: vec![],
        });
        let flat = flatten_scene(&scene);
        assert!(validate_flat(&flat).is_ok());
    }

    #[test]
    fn validate_flat_rejects_duplicate_author_ids() {
        let dup_scene = SceneNode::Frame(Frame {
            id: Some("root".into()),
            label: "root".into(),
            background_color: [0.0; 4],
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
            children: vec![
                SceneNode::Frame(Frame {
                    id: Some("btn".into()),
                    label: "a".into(),
                    background_color: [0.0; 4],
                    x: 0.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                    children: vec![],
                    on_click: vec![],
                }),
                SceneNode::Frame(Frame {
                    id: Some("btn".into()),
                    label: "b".into(),
                    background_color: [0.0; 4],
                    x: 20.0,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                    children: vec![],
                    on_click: vec![],
                }),
            ],
            on_click: vec![],
        });
        let flat = flatten_scene(&dup_scene);
        match validate_flat(&flat) {
            Err(SceneValidation::DuplicateAuthorId { id, count }) => {
                assert_eq!(id, "btn");
                assert_eq!(count, 2);
            }
            other => panic!("expected DuplicateAuthorId, got {other:?}"),
        }
    }

    #[test]
    fn validate_flat_returns_duplicate_even_when_only_two() {
        let flat = vec![
            FlatNode {
                id: NodeId(1),
                author_id: Some("x".into()),
                label: "x1".into(),
                kind: FlatNodeKind::Frame {
                    background_color: [0.0; 4],
                },
                abs_x: 0.0,
                abs_y: 0.0,
                width: 1.0,
                height: 1.0,
                child_ids: vec![],
                on_click: vec![],
            },
            FlatNode {
                id: NodeId(2),
                author_id: Some("x".into()),
                label: "x2".into(),
                kind: FlatNodeKind::Frame {
                    background_color: [0.0; 4],
                },
                abs_x: 10.0,
                abs_y: 0.0,
                width: 1.0,
                height: 1.0,
                child_ids: vec![],
                on_click: vec![],
            },
        ];
        assert!(matches!(
            validate_flat(&flat),
            Err(SceneValidation::DuplicateAuthorId { .. })
        ));
    }

    #[test]
    fn validate_flat_counter_scene_is_unique() {
        // The bundled counter example must not regress into dup ids.
        let scene = load_counter_scene();
        let engine = sdui_cel::CelEngine::new();
        let mut cache = sdui_cel::ExprCache::new();
        let env = build_eval_env(&InteractionState::default(), &scene.state);
        let resolved =
            resolve_scene(&scene.root, &engine, &env, &mut cache).unwrap();
        let flat = flatten_scene(&resolved);
        assert!(validate_flat(&flat).is_ok());
    }
}
