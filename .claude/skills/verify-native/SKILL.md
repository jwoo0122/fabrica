---
name: verify-native
description: Build, launch, and inspect the macOS native example app for this Rust+wgpu SDUI runtime. Dumps the AccessKit/NSAccessibility tree, runs AppleScript probes against it, diffs screenshots, and produces a verification report. Use after any runtime change to close the native-side feedback loop.
argument-hint: [example-name]
allowed-tools: Bash(cargo *) Bash(osascript *) Bash(screencapture *) Bash(kill *) Bash(curl *) Bash(mkdir *) Bash(cat *) Read Write Edit Glob Grep
---

# Verify native (macOS)

Closes the feedback loop for `app-native` after a runtime change. Produces a report in `target/verify-out/native-<timestamp>.md`.

Arguments: $ARGUMENTS — one example name, e.g. `/verify-native counter`. If empty, uses `smoke`.

## Why this approach

wgpu draws pixels onto an `NSView`; those pixels are opaque to UI automation. But our runtime binds **AccessKit** to every Frame/Text/Image node, and `accesskit_macos` publishes that tree through the Cocoa **NSAccessibility** protocol. That makes the app driveable by:

- `Accessibility Inspector.app` (Xcode) — interactive exploration
- AppleScript `System Events` — scripted navigation/clicks
- `cargo xtask inspect-ax` — programmatic JSON dump

Sources:
- AccessKit macOS adapter: <https://docs.rs/crate/accesskit_macos/latest>
- AppleScript UI scripting: <https://www.macosxautomation.com/applescript/uiscripting/>
- Accessibility Inspector: <https://developer.apple.com/documentation/accessibility/accessibility-inspector>
- egui+wgpu precedent for AccessKit integration: <https://github.com/AccessKit/accesskit>

## Procedure

Set example name:

```bash
EXAMPLE="${1:-smoke}"
STAMP=$(date +%Y%m%d-%H%M%S)
OUT="target/verify-out/native-$STAMP"
mkdir -p "$OUT"
```

### 1. Build and launch (background)

```bash
cargo xtask run-native --example "$EXAMPLE" --background --pidfile "$OUT/app.pid" 2>&1 | tee "$OUT/build.log" | tail -40
```

If `cargo xtask` itself does not exist (day-0 scaffolding incomplete): **stop and surface to the user.** Do not fall back to ad-hoc `cargo run`.

### 2. Wait for the window

```bash
osascript -e '
  repeat with i from 1 to 40
    try
      tell application "System Events"
        if exists (process "app-native") then
          if exists (window 1 of process "app-native") then return "ready"
        end if
      end tell
    end try
    delay 0.25
  end repeat
  return "timeout"
' | tee "$OUT/wait.log"
```

If `timeout`: kill the PID, write failure to report, stop.

### 3. Dump AccessKit tree (primary assertion surface)

```bash
cargo xtask inspect-ax --pid "$(cat "$OUT/app.pid")" > "$OUT/ax-tree.json" 2> "$OUT/ax.err"
```

If the tree is empty or the helper reports "no AccessKit root found": AccessKit integration is missing in the runtime. Record this in the report and stop — the runtime must be fixed before further verification is meaningful.

### 4. Run example-specific AppleScript assertions (optional)

If `examples/$EXAMPLE/verify.applescript` exists:

```bash
osascript "examples/$EXAMPLE/verify.applescript" > "$OUT/assertions.log" 2>&1
echo "exit=$?" >> "$OUT/assertions.log"
```

### 5. Screenshot + optional diff

Note: `System Events`'s `id of window 1` returns error -1728 on macOS 15 (Sequoia) for all apps. Use the **position+size → `screencapture -R`** workaround instead:

```bash
# Workaround for macOS 15: id of window returns -1728 for all apps.
# Use position+size to capture by region.
POS=$(osascript -e 'tell application "System Events" to tell process "app-native" to {position, size} of window 1' 2>/dev/null)
if [ -n "$POS" ]; then
  X=$(echo "$POS" | cut -d',' -f1 | tr -d ' ')
  Y=$(echo "$POS" | cut -d',' -f2 | tr -d ' ')
  W=$(echo "$POS" | cut -d',' -f3 | tr -d ' ')
  H=$(echo "$POS" | cut -d',' -f4 | tr -d ' ')
  screencapture -R"${X},${Y},${W},${H}" -x "$OUT/shot.png"
fi
```

If `examples/$EXAMPLE/golden/native.png` exists, use `Read` on both images and record a qualitative visual diff note in the report. Do not block on pixel-exact equality; treat goldens as guidance.

### 6. Teardown

```bash
kill "$(cat "$OUT/app.pid")" 2>/dev/null || true
```

### 7. Write the report

Create `$OUT/report.md` with these sections:

- **Example**: name
- **Build**: pass/fail, tail of build.log
- **Launch**: ready/timeout
- **AccessKit tree summary**: root node + child count + any node missing `label` or `role`
- **Assertions**: pass/fail/skipped (with exit code)
- **Screenshot**: path; if diff against golden attempted, summary
- **Console/stderr**: lines matching `WARN|ERROR|wgpu|panic`
- **Next step**: plain-English recommendation (what to try next, or "clean")

Print the report path to the user.

## Failure triage (do not silently continue)

| Symptom | Likely cause | Action |
|---|---|---|
| `cargo xtask` not found | Scaffolding incomplete | Stop; tell user to scaffold xtask |
| Build error | Code change broken | Surface error verbatim; stop |
| Window never appears | wgpu surface or winit failure | Surface stderr; stop |
| AX tree empty | AccessKit not wired in runtime | Record finding; stop |
| AppleScript returns nothing | Process lacks accessibility permission | Tell user to grant in System Settings → Privacy & Security → Accessibility |
| `id of window 1` error -1728 | macOS 15 (Sequoia) regression — affects all apps | Use position+size workaround (see step 5). NOT a code bug. |
| Screenshot blank/black | Window offscreen or wgpu frame never presented | Surface stderr; stop |

## What this skill does NOT do

- Does not install Xcode, grant accessibility permissions, or modify system settings.
- Does not retry on failure — diagnoses root cause.
- Does not create goldens; those are committed by the user.
