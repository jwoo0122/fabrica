# examples/smoke

The minimal scene driving the runtime feedback loop. **Not a Cargo crate** — an example *name* passed as `--example smoke` to the `app-native` and `app-web` binaries, which load `scene.json` from this directory.

## Run

```bash
cargo xtask run-native --example smoke
cargo xtask run-web    --example smoke --port 8137
```

## What it exercises

- Nested `Frame` with relative positioning (blue parent → orange child)
- `Text` leaf rendered via glyphon on a solid background

`scene.json` is the SSOT; the same JSON drives both platforms.
