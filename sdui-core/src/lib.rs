//! Scene-graph core: node types, hierarchy, coordinate resolution (pure, no rendering).
//!
//! Iteration 4 completes the closed primitive set with `Image` (leaf, textured).
//!
//! AccessKit API reference (Rect, Node::set_bounds):
//!   <https://docs.rs/accesskit/0.24/accesskit/struct.Rect.html>
//!   <https://docs.rs/accesskit/0.24/accesskit/struct.Node.html>

pub use accesskit::NodeId;
use accesskit::{Node, Rect, Role};
use serde::{Deserialize, Serialize};

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
}

impl SceneNode {
    /// The human-readable label of this node.
    pub fn label(&self) -> &str {
        match self {
            SceneNode::Frame(f) => &f.label,
            SceneNode::Text(t) => &t.label,
            SceneNode::Image(i) => &i.label,
        }
    }

    /// Child nodes of this node (empty slice for leaf nodes).
    pub fn children(&self) -> &[SceneNode] {
        match self {
            SceneNode::Frame(f) => &f.children,
            SceneNode::Text(_) => &[],
            SceneNode::Image(_) => &[],
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
        }
    }
}

/// Frame primitive — a coloured rectangle with position relative to its parent.
///
/// Coordinates `x` and `y` are offsets from the parent's top-left corner.
/// For root-level Frames, they are absolute (offset from surface origin).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Frame {
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
            label: label.into(),
            background_color,
            x,
            y,
            width,
            height,
            children: Vec::new(),
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
    /// Unique ID assigned during depth-first traversal.
    pub id: NodeId,
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
}

impl FlatNode {
    /// Build an AccessKit [`Node`] with absolute-coordinate bounds and children.
    pub fn to_accesskit_node(&self) -> Node {
        let role = match &self.kind {
            FlatNodeKind::Frame { .. } => Role::GenericContainer,
            FlatNodeKind::Text { content, .. } => {
                let mut node = Node::new(Role::Label);
                node.set_label(content.clone());
                let bounds = Rect::new(
                    self.abs_x as f64,
                    self.abs_y as f64,
                    (self.abs_x + self.width) as f64,
                    (self.abs_y + self.height) as f64,
                );
                node.set_bounds(bounds);
                return node;
            }
            FlatNodeKind::Image { alt, .. } => {
                let mut node = Node::new(Role::Image);
                let label = alt.clone().unwrap_or_else(|| self.label.clone());
                node.set_label(label);
                let bounds = Rect::new(
                    self.abs_x as f64,
                    self.abs_y as f64,
                    (self.abs_x + self.width) as f64,
                    (self.abs_y + self.height) as f64,
                );
                node.set_bounds(bounds);
                return node;
            }
        };
        let mut node = Node::new(role);
        node.set_label(self.label.clone());
        let bounds = Rect::new(
            self.abs_x as f64,
            self.abs_y as f64,
            (self.abs_x + self.width) as f64,
            (self.abs_y + self.height) as f64,
        );
        node.set_bounds(bounds);
        if !self.child_ids.is_empty() {
            node.set_children(self.child_ids.clone());
        }
        node
    }
}

/// Stable root-node ID used by all platform adapters.
pub const ROOT_NODE_ID: NodeId = NodeId(0);

/// Flatten a scene tree into a list of [`FlatNode`]s with absolute coordinates.
///
/// Node IDs are assigned depth-first starting from `NodeId(1)`.
/// `NodeId(0)` is reserved for [`ROOT_NODE_ID`] (the AccessKit tree root,
/// not part of the scene graph).
pub fn flatten_scene(root: &SceneNode) -> Vec<FlatNode> {
    let mut result = Vec::new();
    let mut next_id = 1u64;
    flatten_recursive(root, 0.0, 0.0, &mut next_id, &mut result);
    result
}

fn flatten_recursive(
    node: &SceneNode,
    parent_abs_x: f32,
    parent_abs_y: f32,
    next_id: &mut u64,
    out: &mut Vec<FlatNode>,
) -> NodeId {
    let my_id = NodeId(*next_id);
    *next_id += 1;

    match node {
        SceneNode::Frame(f) => {
            let abs_x = parent_abs_x + f.x;
            let abs_y = parent_abs_y + f.y;

            let my_index = out.len();
            out.push(FlatNode {
                id: my_id,
                label: f.label.clone(),
                kind: FlatNodeKind::Frame {
                    background_color: f.background_color,
                },
                abs_x,
                abs_y,
                width: f.width,
                height: f.height,
                child_ids: Vec::new(),
            });

            let child_ids: Vec<NodeId> = f
                .children
                .iter()
                .map(|child| flatten_recursive(child, abs_x, abs_y, next_id, out))
                .collect();

            out[my_index].child_ids = child_ids;
        }
        SceneNode::Text(t) => {
            let abs_x = parent_abs_x + t.x;
            let abs_y = parent_abs_y + t.y;

            out.push(FlatNode {
                id: my_id,
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
            });
        }
        SceneNode::Image(i) => {
            let abs_x = parent_abs_x + i.x;
            let abs_y = parent_abs_y + i.y;

            out.push(FlatNode {
                id: my_id,
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
            });
        }
    }

    my_id
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
        });

        let json = serde_json::to_string_pretty(&scene).expect("serialize");
        let deserialized: SceneNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(scene, deserialized);
    }

    #[test]
    fn text_node_json_round_trip() {
        let scene = SceneNode::Text(Text {
            label: "greeting".into(),
            content: "Hello, SDUI!".into(),
            font_size: 24.0,
            color: [1.0, 1.0, 1.0, 1.0],
            x: 10.0,
            y: 5.0,
            width: 200.0,
            height: 30.0,
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
        });

        let flat = flatten_scene(&scene);
        assert_eq!(flat.len(), 2);

        assert_eq!(flat[0].id, NodeId(1));
        assert_eq!(flat[0].abs_x, 100.0);
        assert_eq!(flat[0].abs_y, 80.0);
        assert_eq!(flat[0].child_ids, vec![NodeId(2)]);

        assert_eq!(flat[1].id, NodeId(2));
        assert_eq!(flat[1].abs_x, 120.0);
        assert_eq!(flat[1].abs_y, 90.0);
        assert!(flat[1].child_ids.is_empty());
    }

    #[test]
    fn flatten_scene_with_text_child() {
        let scene = SceneNode::Frame(Frame {
            label: "container".into(),
            background_color: [0.0; 4],
            x: 50.0,
            y: 30.0,
            width: 200.0,
            height: 100.0,
            children: vec![SceneNode::Text(Text {
                label: "title".into(),
                content: "Hello".into(),
                font_size: 24.0,
                color: [1.0, 1.0, 1.0, 1.0],
                x: 10.0,
                y: 10.0,
                width: 100.0,
                height: 30.0,
            })],
        });

        let flat = flatten_scene(&scene);
        assert_eq!(flat.len(), 2);

        // Frame parent
        assert_eq!(flat[0].child_ids, vec![NodeId(2)]);
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
            label: "root-frame".into(),
            background_color: [0.0; 4],
            x: 50.0,
            y: 30.0,
            width: 200.0,
            height: 100.0,
            children: vec![SceneNode::Frame(Frame::new(
                "nested", [1.0; 4], 10.0, 10.0, 80.0, 40.0,
            ))],
        });

        let flat = flatten_scene(&scene);
        let parent_ak = flat[0].to_accesskit_node();
        let child_ak = flat[1].to_accesskit_node();

        let pb = parent_ak.bounds().unwrap();
        assert_eq!(pb.x0, 50.0);
        assert_eq!(pb.y0, 30.0);
        assert_eq!(pb.x1, 250.0);
        assert_eq!(pb.y1, 130.0);
        assert_eq!(parent_ak.children(), &[NodeId(2)]);

        let cb = child_ak.bounds().unwrap();
        assert_eq!(cb.x0, 60.0);
        assert_eq!(cb.y0, 40.0);
        assert_eq!(cb.x1, 140.0);
        assert_eq!(cb.y1, 80.0);
    }

    #[test]
    fn image_node_json_round_trip() {
        let scene = SceneNode::Image(Image {
            label: "smoke-image".into(),
            source: ImageSource::Url {
                url: "http://127.0.0.1:8138/assets/smoke.png".into(),
            },
            alt: Some("Checker pattern test image".into()),
            x: 20.0,
            y: 180.0,
            width: 120.0,
            height: 120.0,
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
            label: "container".into(),
            background_color: [0.0; 4],
            x: 50.0,
            y: 30.0,
            width: 200.0,
            height: 200.0,
            children: vec![SceneNode::Image(Image {
                label: "pic".into(),
                source: ImageSource::Path {
                    path: "/tmp/pic.png".into(),
                },
                alt: Some("A test picture".into()),
                x: 10.0,
                y: 20.0,
                width: 64.0,
                height: 64.0,
            })],
        });

        let flat = flatten_scene(&scene);
        assert_eq!(flat.len(), 2);

        // Frame parent
        assert_eq!(flat[0].child_ids, vec![NodeId(2)]);
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
            label: "fallback-label".into(),
            source: ImageSource::Url {
                url: "http://example/foo.png".into(),
            },
            alt: None,
            x: 0.0,
            y: 0.0,
            width: 50.0,
            height: 50.0,
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
}
