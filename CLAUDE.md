# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

See [`README.md`](./README.md) for the public project overview, quick start, and workspace layout. This file captures the **standing rules** and **architectural decisions** that constrain how work proceeds.

## Repository status

Through **Iteration 6**: primitives `{Frame, Text, Image}` plus the first operator `Condition` (CEL-driven branch selection, literal-only evaluation environment). `sdui-cel` is realized as an `ExpressionEngine` trait backed by `cel-interpreter`. A `resolve_scene` pre-pass substitutes `Condition` nodes at scene-load time, keeping `sdui-runtime-wgpu` byte-identical to its pre-Iteration-5 form. AccessKit tree on native, ARIA mirror DOM on web. Live-reload infrastructure now lives in `app-native`, `app-web`, and `xtask` — not in `sdui-runtime-wgpu` or `sdui-core`. See [`README.md`](./README.md) and [`SPRINTS.md`](./SPRINTS.md) for the completed iteration log.

## Non-negotiable principles

These override default behavior. They apply to every task in this repo.

### 1. Evidence-grounded conclusions

- Training data is a hint. Final conclusions require **external evidence** (codebase, docs, or web research).
- When evidence is insufficient despite exhaustive search, present **at least 3 plausible options with supporting evidence**. Do not pick one silently.
- When no evidence can be found for a claim, **discard it**. Do not invent.
- Always cite sources inline (URL or file path).

### 2. Three-stage task pipeline

Every non-trivial task splits into:

1. **Requirements criteria** — what "done" means.
2. **Execution** — the actual work.
3. **Verification** — checking against criteria, typically invoking the skills below.

These stages must be distinct in your reasoning. For small changes you may do all three yourself, but do not collapse them.

### 3. Tight feedback loop — ship the example every iteration

**One-shot completion is forbidden.** Start from the simplest requirement that renders anything, prove it works, then grow incrementally toward the vision.

Every iteration:

1. Modify runtime code.
2. Rebuild the affected example (native + web).
3. **Launch and inspect** the result via `/verify-native` and `/verify-web`.
4. Feed findings into the next iteration.

Never batch multiple runtime changes between verifications.

## Architecture summary (authoritative)

**MVP scope** — runtime only. macOS native + WebGPU-supported browsers (Safari 26+, FF 141+, Chromium). No WebGL2 fallback. Example apps drive every iteration.

**Scene SSOT** — one Scene = one render pipeline, shared between runtime and (future) editor. Internal: retained scene graph + stable node IDs + property-level patch bus + transaction boundary. Scene-to-Scene wiring is separate: runtime uses Router/StateMachine, editor uses FigJam-style node-edge graph.

**Closed node type set**

- Primitives: `Frame` (box + layout) | `Text` (leaf) | `Image` (leaf)
- Operators (Turing-semi-complete): `ForEach` | `Condition` | `Fetching`
- `PresetRef` — named reference into a separately-loaded preset library JSON; runtime expands at resolve time with cycle detection
- Semantic components (Button, Tab, …) live in the preset library, **never** in the core node set

**Expression language: CEL (Common Expression Language)**

Operators (`ForEach` iteration binding, `Condition` predicate, `Fetching` URL/headers templating) use CEL via the `cel-interpreter` Rust crate. Non-Turing-complete, linear-time, mutation-free, with bounded iteration macros (`all/exists/filter/map`).

Rationale: industry-proven for exactly this use case (Kubernetes VAP, GCP IAM, Envoy). WASM bundle-size impact must be monitored; unused CEL features stripped. Pivot path kept open: expose as `ExpressionEngine` trait in `sdui-cel` so JSON Logic or a custom DSL could replace it without touching runtime core.

CEL references:
- Spec: <https://github.com/google/cel-spec>
- Site: <https://cel.dev/>
- Rust crate: <https://crates.io/crates/cel-interpreter>
- Docs: <https://docs.rs/cel-interpreter/latest/cel_interpreter/>
- Kubernetes VAP (proven pattern, read to understand ergonomics & limits): <https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/>
- Criticism / gotchas (learn the failure modes): <https://www.chainguard.dev/unchained/are-kubernetes-validating-admission-policies-the-end-of-admission-controllers>

**Fetching node** — REST only, no cache (MVP). Loading/error represented as `loading_fallback` / `error_fallback` child trees (React Suspense model). Example apps point at a bundled `examples/mock-server/` (no auth, static JSON).

**Accessibility is first-class** — every node carries `role` / `label` / `a11y_state` via AccessKit. This is load-bearing for the verification harness: the macOS and web adapters expose the same semantic tree to Accessibility Inspector, AppleScript, and `agent-browser`. Precedent: egui + wgpu (<https://github.com/AccessKit/accesskit>).

## Commands

All orchestration goes through `cargo xtask`. Direct cargo commands work but skip conventions.

- `cargo xtask build-all` — build native + wasm in one go
- `cargo xtask run-native [--example <name>] [--scene-path <abs>]` — build + launch `app-native`
- `cargo xtask run-web [--example <name>] [--port <n>]` — wasm build + localhost serve
- `cargo xtask inspect-ax --pid <pid>` — dump AccessKit tree of a running native app as JSON
- `cargo test` — unit + integration (scene invariants, CEL edge cases, wire round-trips)
- `cargo clippy --all-targets`
- `cargo fmt`

## Verification harness

Two bundled project skills close the feedback loop. Invoke via `/verify-native` and `/verify-web`. Artifacts land in `target/verify-out/`.

### Preflight — mock-server health (MANDATORY before every verification run)

`examples/mock-server` serves bundled assets (e.g., `/assets/smoke.png`) that the example scenes fetch. A silent 404 from this server has masked real failures in the past (a stale binary compiled against a renamed workspace path continued to "succeed" at boot but served 404s). The verifier must confirm the server is up **and serving the expected asset** before trusting any downstream visual or AX assertion.

Required preflight at the start of **every** `/verify-native` or `/verify-web` run, before the app is launched:

1. Rebuild from source (cheap when up-to-date, guards against stale-binary bugs):
   ```
   cargo build -p mock-server
   ```
2. Launch mock-server (xtask does this automatically for `run-native`/`run-web`).
3. Probe health **and** the asset route:
   ```
   curl -sf http://localhost:8138/health
   curl -sfo /dev/null -w '%{http_code} %{content_type}\n' http://localhost:8138/assets/smoke.png
   ```
   Expect `ok` and `200 image/png`. If the PNG probe returns anything else, **stop** — do not run the rest of the verification. Resolve by:
   ```
   cargo clean -p mock-server && cargo build -p mock-server
   ```
   then retry. (mock-server now exits loudly on startup if its asset directory is missing — see `examples/mock-server/src/main.rs::assets_dir`.)
4. Record both probe outputs in the verification report.

Rule: a verification run that did not complete the asset probe above is **not** a verification run. Re-do it.

### Native (macOS) — `/verify-native`

Three layered techniques; use in order:

1. **AccessKit tree dump** — `cargo xtask inspect-ax` serializes the running app's NSAccessibility tree to JSON. Primary assertion surface.
2. **AppleScript `System Events`** — navigates and clicks against the accessibility tree. Apple's documented path: <https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/AutomatetheUserInterface.html>
3. **Visual diff** — `screencapture -R"X,Y,W,H"` (position+size from `System Events`) vs. `examples/<name>/golden/native.png`. Note: `id of window 1` fails on macOS 15 (error -1728, platform regression); use position+size workaround.

Fallback (no AccessKit yet, or shader debug scene): `cliclick` / image-recognition. Do not let this become primary.

Also available for manual inspection: **Accessibility Inspector.app** (bundled with Xcode). Reference: <https://developer.apple.com/documentation/accessibility/accessibility-inspector>.

### Web (browser) — `/verify-web`

Drives the wasm build via the bundled `agent-browser` skill. The key insight: wgpu renders into `<canvas>` which is opaque to DOM queries, but AccessKit's web adapter mounts a parallel `<div>` tree with proper ARIA roles/labels mirroring the scene graph. That mirror tree is stable and queryable — use it, not pixel OCR.

Procedure:
1. `cargo xtask run-web --background`
2. `agent-browser` navigates to `http://localhost:<port>/examples/<name>`
3. Waits for AccessKit-mirror DOM root (`div[role="application"]`)
4. Queries nodes by ARIA role/label; dumps tree; takes screenshot; reads console
5. Compares against `examples/<name>/golden-web/`

### Skill discipline

After **any** runtime change, run at least one of `/verify-native` or `/verify-web` on the minimal example exercising the change before moving on. If the verification skill fails (build error, missing helper, adapter missing), **stop and diagnose root cause** — do not disable the skill or skip it.

## Open decisions (deferred to future turns)

- Frame's minimal attribute schema (what ships as native properties vs. presets)
- CEL type binding layer (how Scene state + server data maps to CEL environment)
- Preset library delivery (bundled JSON vs. lazy fetch vs. hot-reload)
- Scene `inputs`/`outputs` contract specifics (schema reserved now, implementation later)
- Event/action wire format (server-ID + payload → new tree)

Don't freeze these without explicit user sign-off.

## What not to do

- Don't implement editor features (multi-Scene canvas, selection, property panels) in MVP.
- Don't add WebGL2 or other renderer fallbacks.
- Don't introduce node types beyond `{Frame, Text, Image, ForEach, Condition, Fetching, PresetRef}`. Semantic UI = presets.
- Don't skip the feedback loop. Even one-line changes warrant a `/verify-*` run.
- Don't cite training-data claims without external confirmation.
- Don't collapse the three-stage pipeline without noticing.
