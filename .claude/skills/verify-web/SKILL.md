---
name: verify-web
description: Build the wasm example of this Rust+wgpu SDUI runtime, serve it locally, then drive headless Chrome via the agent-browser skill to inspect the AccessKit mirror-DOM, capture screenshots, and read console errors. Use after any runtime change to close the web-side feedback loop.
argument-hint: [example-name]
allowed-tools: Bash(cargo *) Bash(curl *) Bash(kill *) Bash(mkdir *) Bash(cat *) Read Write Edit Glob Grep
---

# Verify web (browser)

Closes the feedback loop for `app-web` after a runtime change. Produces a report in `target/verify-out/web-<timestamp>.md`.

Arguments: $ARGUMENTS — one example name, e.g. `/verify-web counter`. If empty, uses `smoke`.

## Why mirror-DOM inspection, not pixel OCR

The runtime renders with wgpu into a `<canvas>`. Canvas pixels are **opaque to DOM queries** — `document.querySelector` cannot see a "Submit" button drawn on a canvas.

AccessKit's web adapter mounts a **parallel `<div>` tree** into the DOM, with one element per scene-graph node, carrying ARIA `role` + `aria-label` mirroring the node's `role`/`label` attributes. This mirror tree is:

- Stable (ULID-based node ids)
- Semantic (ARIA-compliant)
- Queryable by `agent-browser` using standard selectors

That's the assertion surface. Never assert against canvas pixels except as a last-resort visual smoke test.

References:
- AccessKit: <https://github.com/AccessKit/accesskit>
- WebGPU implementation status: <https://github.com/gpuweb/gpuweb/wiki/Implementation-Status>
- bundled `agent-browser` skill — invoke via `/agent-browser` or from inside this skill

## Procedure

```bash
EXAMPLE="${1:-smoke}"
PORT=8137
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="target/verify-out/web-$STAMP"
mkdir -p "$OUT"
```

### 0. Mock-server preflight (MANDATORY — see CLAUDE.md §"Preflight")

`xtask run-web` co-boots `mock-server` automatically, but a silent 404 from that server has masked real failures before (stale binary compiled against renamed workspace path). Before trusting anything downstream, confirm the asset route:

```bash
cargo build -p mock-server 2>&1 | tail -5
/Users/jinwoo/repos/fabrica/target/debug/mock-server 8138 &
MOCK_PROBE_PID=$!
sleep 1
curl -sf http://localhost:8138/health || { echo "PREFLIGHT FAIL: /health"; kill $MOCK_PROBE_PID; exit 1; }
curl -sfo "$OUT/smoke-probe.png" -w 'probe: %{http_code} %{content_type}\n' http://localhost:8138/assets/smoke.png | tee -a "$OUT/preflight.log"
kill $MOCK_PROBE_PID 2>/dev/null; wait $MOCK_PROBE_PID 2>/dev/null || true
grep -q '200 image/png' "$OUT/preflight.log" || {
  echo "PREFLIGHT FAIL: /assets/smoke.png did not return 200 image/png."
  echo "Fix: cargo clean -p mock-server && cargo build -p mock-server"
  exit 1
}
```

If preflight fails, **stop**. Do not continue to step 1.

### 1. Build and serve (background)

```bash
cargo xtask run-web --example "$EXAMPLE" --background --port "$PORT" --pidfile "$OUT/server.pid" 2>&1 | tee "$OUT/build.log" | tail -40
```

If `cargo xtask` doesn't exist: **stop and surface to user.** No ad-hoc fallback.

### 2. Wait for server (max 60s — wasm compile is slow)

```bash
for i in $(seq 1 120); do
  if curl -sf "http://localhost:$PORT/health" > /dev/null; then
    echo "ready" > "$OUT/wait.log"
    break
  fi
  sleep 0.5
done
grep -q ready "$OUT/wait.log" || echo "timeout" > "$OUT/wait.log"
```

If `timeout`: record and stop.

### 3. Drive with agent-browser

Use the bundled `agent-browser` skill (via `mcp__claude-in-chrome__*` tools once they are loaded; load them through `ToolSearch` with `select:mcp__claude-in-chrome__<tool_name>` as needed).

Steps:

1. **Tab context** — `mcp__claude-in-chrome__tabs_context_mcp` (never reuse tabs from prior sessions)
2. **Navigate** — create a new tab at `http://localhost:$PORT/examples/$EXAMPLE`
3. **Wait for mirror root** — poll for `div[role="application"]` to exist, up to 30s. This is AccessKit's root element.
4. **Dump AX mirror tree**:

   ```js
   (() => {
     const root = document.querySelector('div[role="application"]');
     if (!root) return null;
     const walk = (el) => ({
       role: el.getAttribute('role'),
       label: el.getAttribute('aria-label'),
       id: el.id,
       children: [...el.children].map(walk),
     });
     return walk(root);
   })()
   ```

   Save to `$OUT/ax-tree.json`. If root absent → AccessKit web adapter missing; record and stop.
5. **Screenshot** — full page → `$OUT/shot.png`
6. **Console messages** — filtered to `ERROR|WARN|wgpu|panic` → `$OUT/console.log`. Specifically look for WebGPU adapter-acquisition failures; Safari 26+/FF 141+/Chromium are required.
7. **Example-specific steps** — if `examples/$EXAMPLE/verify-web.json` exists (a declarative step list), execute each step and log outcomes.

### 4. Teardown

```bash
kill "$(cat "$OUT/server.pid")" 2>/dev/null || true
```

Also close the browser tab you opened (don't leak tabs).

### 5. Write the report

Create `$OUT/report.md` mirroring the native-report structure:

- **Example**: name
- **Build**: pass/fail, tail of build.log
- **Serve**: ready/timeout, port
- **WebGPU**: adapter acquired? (from console)
- **AX mirror tree summary**: root + child count, any missing role/label
- **Assertions**: from verify-web.json, if present
- **Screenshot**: path
- **Console**: filtered errors/warnings
- **Next step**: plain-English recommendation

Print the report path to the user.

## Failure triage (do not silently continue)

| Symptom | Likely cause | Action |
|---|---|---|
| `cargo xtask` not found | Scaffolding incomplete | Stop |
| wasm build fails | Code or trunk config | Surface verbatim; stop |
| `/health` times out | Server crashed during startup | `cat $OUT/build.log`; stop |
| `div[role=application]` absent | AccessKit web adapter not wired | Record; stop |
| Console: "No WebGPU adapter" | Browser lacks support or hardware unavailable | Confirm browser/version matches targets; stop |
| Canvas blank but DOM present | wgpu surface mounted but no draw | Likely renderer bug; surface console errors |
| agent-browser returns stale tabs | Session reuse | Re-query `tabs_context_mcp`, open new tab |

## What this skill does NOT do

- Does not install browsers or set up trunk/wasm-pack. Those are xtask's job.
- Does not retry silently — always diagnoses before retrying.
- Does not touch goldens; humans commit those.
- Does not drive canvas pixels for semantic assertions. Mirror-DOM only.
