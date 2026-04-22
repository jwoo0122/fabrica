# Fabrica

A Rust + wgpu **Server-Driven UI (SDUI) runtime** for macOS native and WebGPU-capable browsers. One Scene graph, one render pipeline, two deployment surfaces.

> ⚠️ **Status:** early / experimental. Runtime-first; node set intentionally minimal. No public API stability guarantees yet.

---

## What this is

A retained-mode UI runtime where the UI tree (*Scene*) is authored as JSON, ships as a wire format, and renders identically on macOS (native window via [winit](https://crates.io/crates/winit) + [wgpu](https://crates.io/crates/wgpu)) and in the browser (WASM → WebGPU `<canvas>`).

The motivating constraint: the same Scene graph must drive both the runtime (what end users see) and a future visual editor. That forces a clean separation between scene *content* and scene *wiring* (routers, state machines) — see the architecture section.

## Current capabilities

Implemented through **Iteration 5**. See [`SPRINTS.md`](./SPRINTS.md) for the completed-milestone index. The project follows a strict "ship the example every iteration" loop — each milestone below renders end-to-end on both platforms with an automated verification pass:

| Iteration | What it added                                                                                                                                               |
|-----------|-------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 0         | Workspace scaffold: 9 crates, `cargo xtask`, mock-server skeleton, verification skills.                                                                      |
| 1         | Single `Frame` (coloured rect) rendering. Scene loaded from `examples/smoke/scene.json`. AccessKit tree (native) + ARIA mirror DOM (web).                   |
| 2         | Nested `Frame` children with **relative positioning**. `flatten_scene()` resolves absolute coordinates with stable `NodeId`s. Per-vertex colour, single draw call. |
| 3         | `Text` leaf nodes rendered via [glyphon](https://crates.io/crates/glyphon) (cosmic-text + wgpu) with bundled Noto Sans. Single render pass — quads then text overlay. |
| 4         | `Image` leaf rendered via the [`image`](https://crates.io/crates/image) crate texture upload + wgpu sampled pipeline. mock-server serves the bundled test asset. |
| 5         | First operator: `Condition` with CEL-driven branch selection (literal-only env). `sdui-cel` realized via [`cel-interpreter`](https://crates.io/crates/cel-interpreter). `resolve_scene` pre-pass keeps the renderer byte-identical. |

### Scope today

- **Primitive node set (closed):** `Frame`, `Text`, `Image`
- **Operator nodes:** `Condition` (CEL predicate; literal-only evaluation environment in this iteration)
- **Platforms:** macOS native (Apple Silicon, Metal backend) + WebGPU (Safari 26+, Firefox 141+, Chromium)
- **No fallbacks:** no WebGL2, no software raster. WebGPU or nothing.
- **Accessibility is first-class:** every node carries `role` / `label` via [AccessKit](https://github.com/AccessKit/accesskit). The macOS NSAccessibility tree and the web ARIA mirror DOM are the primary verification surfaces.

### Not yet

`ForEach`, `Fetching`, `PresetRef`, Scene-level CEL variable bindings, preset library resolver, router / state machine, hot reload, editor. Each has a stub crate and a decision in `CLAUDE.md`; none are implemented.

---

## Architecture (brief)

**One Scene = one render pipeline.** Internal representation is a retained scene graph with stable node IDs, property-level patch bus, and transaction boundary.

**Closed node type set.**

- **Primitives:** `Frame` (box + layout), `Text` (leaf), `Image` (leaf)
- **Operators** (Turing-semi-complete): `ForEach`, `Condition`, `Fetching`
- **`PresetRef`:** named reference into a separately-loaded preset library JSON; expanded at resolve time with cycle detection
- **Semantic components** (Button, Tab, List, …) live in the preset library — **never** in the core node set

**Expression language: [CEL](https://cel.dev/)** via [`cel-interpreter`](https://crates.io/crates/cel-interpreter). Non-Turing-complete, linear-time, mutation-free, with bounded iteration macros. Exposed behind an `ExpressionEngine` trait in `sdui-cel` so JSON Logic or a custom DSL could replace it.

**Fetching:** REST only, no cache (MVP). Loading/error states render as `loading_fallback` / `error_fallback` child trees (React Suspense model).

See [`CLAUDE.md`](./CLAUDE.md) for the full architecture rationale and open decisions.

---

## Workspace layout

```
sdui-core/          Scene graph, nodes, flatten_scene, NodeIds (pure, no rendering)
sdui-wire/          JSON schema + serde for wire format  (stub)
sdui-cel/           ExpressionEngine trait + cel-interpreter wrapper  (stub)
sdui-runtime-wgpu/  Renderer: wgpu + glyphon text + per-vertex colour pipeline
sdui-presets/       Preset library resolver + default preset JSON  (stub)
app-native/         macOS binary (winit + accesskit_winit)
app-web/            wasm binary (WebGPU canvas + ARIA mirror DOM)
examples/
  mock-server/      Static JSON test server (for Fetching node)
  smoke/            Minimal scene exercising Frame + Text + Image
xtask/              cargo xtask subcommand runner
assets/fonts/       Bundled Noto Sans (SIL OFL)
```

---

## Quick start

**Prerequisites:** Rust stable, `wasm32-unknown-unknown` target, macOS 15+ for the native app, a WebGPU-capable browser for the web app.

```bash
rustup target add wasm32-unknown-unknown

# Build everything (native + wasm)
cargo xtask build-all

# Run the macOS native app with the smoke example
cargo xtask run-native --example smoke

# Serve the wasm build on localhost
cargo xtask run-web --example smoke --port 8137
# → open http://localhost:8137/examples/smoke/

# Inspect a running native app's accessibility tree
cargo xtask inspect-ax --pid $(pgrep -n app-native)
```

Standard cargo commands also work:

```bash
cargo test                      # unit tests (scene invariants, JSON round-trips, flatten correctness)
cargo clippy --all-targets
cargo fmt
cargo build -p app-web --target wasm32-unknown-unknown
```

---

## Verification harness

**One-shot completion is forbidden.** Every runtime change is validated against the example before moving on. Two project skills (`.claude/skills/`) drive this:

- **`/verify-native`** — dumps the AccessKit/NSAccessibility tree to JSON, runs AppleScript probes against it, captures a screenshot via `screencapture -R`.
- **`/verify-web`** — serves the wasm build, drives a browser session (via [agent-browser](https://github.com/anthropics/claude-code)) to query the ARIA mirror DOM, read console logs, and screenshot the canvas.

The key insight: wgpu renders into an opaque `<canvas>`, so pixel OCR is unreliable. AccessKit's web adapter mounts a **parallel `<div>` tree with proper ARIA roles and labels** that mirrors the scene graph. That mirror DOM is the primary assertion surface — not pixels.

Verification artifacts land in `target/verify-out/`.

---

## Contributing notes

- The working-directory conventions, principles ("evidence-grounded conclusions", "three-stage task pipeline"), and architectural decisions live in [`CLAUDE.md`](./CLAUDE.md). Read it before contributing.
- Don't introduce node types beyond the closed set. Semantic UI (buttons, tabs, …) is a *preset*, not a node.
- Don't skip the feedback loop. Even one-line runtime changes warrant a `/verify-*` run.
- License: **MIT OR Apache-2.0** (dual-licensed).

## References

- [accesskit](https://github.com/AccessKit/accesskit) — cross-platform accessibility tree
- [wgpu](https://wgpu.rs/) — Rust WebGPU implementation
- [winit](https://github.com/rust-windowing/winit) — cross-platform window creation
- [glyphon](https://github.com/grovesNL/glyphon) — cosmic-text-based wgpu text renderer
- [CEL spec](https://github.com/google/cel-spec) — expression language
- [Kubernetes Validating Admission Policy](https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/) — reference pattern for CEL use
