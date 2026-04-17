# assets/images

Test bitmaps consumed by `examples/*` scenes through the mock-server.

## Contents

- `smoke.png` — 64×64 RGBA checker pattern, 8 px cells, blue (`#1e40af`) × amber (`#f59e0b`).
  Used by `examples/smoke/scene.json` as the Image-node target.

## License

Self-generated, public domain. Reproducible byte-for-byte via `generate.py`:

```bash
python3 assets/images/generate.py
```

The generator uses only Python's `zlib` and `struct` modules — no Pillow, no
third-party dependency. PNG spec reference: <https://www.w3.org/TR/png/>.
