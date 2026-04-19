# examples/condition-panel

A live-reload exercise scene for **Iteration 6**. This example is intentionally built around `Condition` so both the web textarea panel and the native file-watcher loop can flip branches without rebuilding Rust or rebundling wasm.

## Run

```bash
cargo xtask run-native --example condition-panel
cargo xtask run-web --example condition-panel --port 8137
```

For native file-watcher testing against a disposable copy:

```bash
cp examples/condition-panel/scene.json /tmp/condition-panel-reload.json
cargo xtask run-native --scene-path /tmp/condition-panel-reload.json
```

> Requires mock-server on port 8138; `cargo xtask run-native` / `run-web` launches it automatically.

## What it exercises

- A top-level `Frame` with explanatory `Text`
- One `Condition` that starts with `"when": "true"` and renders an `Image`
- One `Condition` that starts with `"when": "false"` and renders a fallback `Text`
- The shared `resolve_scene(...)` pre-pass used by both native and web

`scene.json` remains the SSOT; both deployment surfaces load and resolve the same JSON.

## Manual test recipes

### 1. Toggle the image branch

In web, edit the textarea and change the first condition from:

```json
"when": "true"
```

to:

```json
"when": "false"
```

Then click **Apply**.

Expected result:
- the image disappears
- the placeholder text `placeholder: image disabled` appears
- no rebuild happens between edit and result

For native, perform the same edit on `/tmp/condition-panel-reload.json` while the app is running with `--scene-path /tmp/condition-panel-reload.json`.

### 2. Nested `Condition` experiment

This baseline scene uses two sibling `Condition` nodes. To exercise nesting manually, wrap the second condition inside the `else` branch of the first condition in the textarea or temp file copy.

Expected result:
- the scene still resolves through the same `resolve_scene(...)` path
- only the selected branch remains in the rendered / AX tree
- no renderer-specific logic is needed for nested conditions

### 3. Malformed JSON recovery

Replace the textarea or watched temp file contents with:

```json
{ broken
```

Expected result:
- web: `#scene-error` becomes visible and includes `parse error`
- native: stderr prints a reload warning
- both surfaces keep the **previous valid scene** instead of blanking out
