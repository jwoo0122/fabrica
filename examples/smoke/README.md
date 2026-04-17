# examples/smoke

The minimal scene driving the runtime feedback loop. **Not a Cargo crate** — an example *name* passed as `--example smoke` to the `app-native` and `app-web` binaries, which load `scene.json` from this directory.

## Run

```bash
cargo xtask run-native --example smoke
cargo xtask run-web    --example smoke --port 8137
```

> Requires mock-server on port 8138; `cargo xtask run-native` / `run-web` launches it automatically.

## What it exercises

- Nested `Frame` with relative positioning (blue parent → orange child)
- `Text` leaf rendered via glyphon on a solid background
- `Image` leaf rendered as a textured wgpu quad; PNG fetched from `http://127.0.0.1:8138/assets/smoke.png`

`scene.json` is the SSOT; the same JSON drives both platforms.
