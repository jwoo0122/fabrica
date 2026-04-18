# Sprint / Iteration Status

This file is the quick index for the repo's **completed** milestones.

Use the docs in this order:

1. [`SPRINTS.md`](./SPRINTS.md) — completed iteration index
2. [`README.md`](./README.md) — public project overview and current capability snapshot
3. [`CLAUDE.md`](./CLAUDE.md) — standing rules, architecture, and verification discipline
4. Local worktree only: `.iteration-0-criteria.md` → `.iteration-5-criteria.md` — archived done-definitions for each shipped milestone (currently gitignored; useful during active planning, not the public repo surface)

## Current baseline on trunk

Completed through **Iteration 5**.

- Runtime primitives shipped: `Frame`, `Text`, `Image`
- Operator nodes shipped: `Condition` (CEL-driven branch selection, literal-only environment)
- Expression engine: `ExpressionEngine` trait + `cel-interpreter`-backed `CelEngine` in `sdui-cel`
- Deployment surfaces shipped: macOS native + WebGPU web
- Verification surfaces shipped: native AccessKit tree + web ARIA mirror DOM, plus mandatory mock-server preflight
- Example driving the feedback loop: `examples/smoke/scene.json`

## Completed iterations

| Iteration | Status | Delivered outcome | Primary repo evidence |
|---|---|---|---|
| 0 | Complete | Workspace scaffold, `cargo xtask`, native/web shells, verification harness skeleton | `Cargo.toml`, `xtask/src/main.rs`, `app-native/`, `app-web/`, `examples/mock-server/` |
| 1 | Complete | Single `Frame` rendering from `examples/smoke/scene.json` with native AccessKit + web mirror DOM | `sdui-core/src/lib.rs`, `sdui-runtime-wgpu/src/lib.rs`, `examples/smoke/scene.json` |
| 2 | Complete | Nested `Frame` children, relative positioning, flattened absolute coordinates, stable `NodeId`s | `sdui-core/src/lib.rs`, `app-native/src/main.rs`, `app-web/src/lib.rs` |
| 3 | Complete | `Text` leaf rendering via glyphon with bundled Noto Sans | `sdui-runtime-wgpu/src/lib.rs`, `assets/fonts/NotoSans-Regular.ttf` |
| 4 | Complete | `Image` leaf rendering via PNG decode + wgpu textured pipeline, mock-server asset serving, async web image load | `sdui-core/src/lib.rs`, `sdui-runtime-wgpu/src/lib.rs`, `app-native/src/main.rs`, `app-web/src/lib.rs`, `examples/mock-server/src/main.rs`, `assets/images/smoke.png` |
| 5 | Complete | `Condition` operator with literal-only CEL evaluation; `sdui-cel` realized (`ExpressionEngine` + `CelEngine`); `resolve_scene` pre-pass keeps the renderer byte-identical; mock-server preflight added to verification pipeline | `sdui-cel/src/lib.rs`, `sdui-core/src/lib.rs`, `app-native/src/main.rs`, `app-web/src/lib.rs`, `examples/smoke/scene.json`, `examples/mock-server/src/main.rs`, `.claude/skills/verify-*/SKILL.md` |

## Notes on the criteria files

The local `.iteration-*.md` files are still useful when checking what "done" meant for each iteration, but they are currently **gitignored planning artifacts** rather than the public repo surface. The commit-facing current-state summary should live in this file and in [`README.md`](./README.md).
