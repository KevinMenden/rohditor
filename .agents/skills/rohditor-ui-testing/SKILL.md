---
name: rohditor-ui-testing
description: See and drive the Rohditor UI headlessly to verify UI changes - render screenshots, click and drag widgets, run UI scripts. Covers the GPUI app (apps/gpui, scripted driver) and the egui app (apps/desktop, kittest snapshots). Use when changing anything under apps/gpui/src or apps/desktop/src/ui or app/, or when asked to look at, test, or iterate on the UI.
---

# Rohditor UI testing (closing the loop)

New UI work goes in `apps/gpui`; see the GPUI section first. The egui app (`apps/desktop`)
only gets fixes, and has two tools of its own.

## GPUI (`apps/gpui`)

Headless tests drive the real `Workspace` with GPUI's headless renderer and a wgpu adapter.
Code: `apps/gpui/src/ui_tests.rs` (the `Harness`) and `ui_tests/` (scenarios). Elements are
found by **test id** (`.test_id("section:Defringe")` in the UI code, recorded in test builds
only): `adjustment:<Parameter>`, `section:<Name>`, `button:<id>`, `viewport`, and so on.
`grep -rn 'test_id(' apps/gpui/src` lists them, and the driver writes the visible ones.

Environment variables (all tests are `#[ignore]`d):
- `ROHDITOR_RAW`: a RAW file. Tests copy it to a scratch folder as `A.ARW` and `B.ARW`, so
  sidecars never land next to the original. Without it, photo tests silently pass.
- `ROHDITOR_DCP`: a camera profile for that photo's camera (only the color-profile test).
- `ROHDITOR_SHOTS`: a directory; every `capture` writes a PNG there.
- `ROHDITOR_SCRIPT`: a script for the driver, below.

### The scripted driver: explore without compiling a test

Prefer this to writing a Rust test when trying something out or taking screenshots. Write a
script (`apps/gpui/src/ui_tests/driver.rs` documents every command) and run:

```sh
cd apps/gpui
ROHDITOR_RAW=/path/photo.ARW ROHDITOR_SCRIPT=/tmp/s.txt ROHDITOR_SHOTS=/tmp/out \
  cargo test --release --locked run_the_script -- --ignored --nocapture
```

```
window 1440 1800
open A.ARW
click section:Defringe
drag adjustment:CaptureAmount 100 0
hover section:Defringe
shot defringe
```

`shot NAME` waits for the preview, then writes `NAME.png` (Read it to look) and `NAME.txt`:
one line per visible selector with its bounds, `selector x= y= w= h=` in logical pixels.
Use that list to pick selectors and offsets for the next script. Commands: `open`, `click`,
`rightclick`, `hover`, `drag`, `press`/`move`/`release` (shots mid-drag), `scroll`, `keys`,
`window`, `shot`. A failing step (selector not shown) stops the script and writes
`error.png`/`error.txt`. Edit the script and re-run: only the first run compiles. Each run
loads and develops the photo again (about 10 s in a release build, 30 s in debug), so put
several shots in one script.

The list can include elements scrolled out of a panel but still laid out; check the PNG.

### Compiled scenarios

Add one to `ui_tests/` when behaviour should stay covered: build with `opened(name)`, act
with `click`, `drag_to`, `hover`, `scroll`, `keys`, assert on the session, `capture` a shot.
Run `cargo test --release --locked -- --ignored` in `apps/gpui`, filtered by test name when
you can. Each test opens a full RAW: about 9 s in a release build against about 30 s in
debug (all 33 photo tests: roughly 20 min in debug). `[profile.test] opt-level = 3` is not
worth it: it rebuilds every dependency (about 9 min) and still runs slower than `--release`.
`color::profiles_are_chosen_and_imported` also needs `ROHDITOR_DCP`.

### When to take screenshots

After any change to what a panel, overlay or the viewport draws, and for drag interactions
(crop, masks) while the button is held. Look at the PNG; do not trust assertions alone.
Not covered, so check by hand: native file dialogs, window moving/resizing, fractional
scaling, the real compositor. Screenshots are not compared pixel by pixel.

# egui app (`apps/desktop`)

Two tools. Prefer the first; use the second to confirm the final
look in the real window. Prefer the first; use the second to confirm the final
look in the real window.

## 1. Headless scenarios (`egui_kittest`) - primary loop

Runs the real `RohditorApp` with no window. Widgets are found by **accessible label**, and
`snapshot` renders the frame to a PNG you can open with the Read tool.

- Code: `apps/desktop/src/app/ui_tests/` (`mod.rs` = harness, `scenarios.rs` = scenarios).
- Snapshots: `apps/desktop/tests/snapshots/<name>.png` (committed; they are the baseline).
- The harness uses the CPU processor, a wgpu renderer, and a scratch config directory, so it
  never touches the user's settings or session.

Run (scenarios are `#[ignore]`d: they need a wgpu adapter):

```sh
# Compare against committed snapshots (fails and writes <name>.new.png / .diff.png on change)
cargo test -p rohditor-desktop ui_tests -- --ignored
# Accept new or intentionally changed visuals
UPDATE_SNAPSHOTS=1 cargo test -p rohditor-desktop ui_tests -- --ignored
```

Workflow for a UI change:
1. Change the UI code.
2. Add or extend a scenario in `scenarios.rs`: build with `app_harness(...)`, call
   `harness.run_steps(SETTLE_FRAMES)`, interact via
   `harness.get_by_label("Library").click()` (`Queryable` trait: `get_by_label`,
   `get_by_role`, `query_by_label`, ...), step again, then `harness.snapshot("name")`.
3. Run with `UPDATE_SNAPSHOTS=1`, then **Read the PNG** and check it looks right. Never
   accept a snapshot without looking at it.
4. Re-run without the env var to confirm it is stable, and commit the PNG with the change.

Gotchas:
- Never use `harness.run()`: the app requests repaints while workers run, so it panics with
  "exceeded max_steps". Use `run_steps(n)`.
- If a widget cannot be found by label, it may lack an accessible label (icon-only buttons);
  add one to the widget (`.on_hover_text` / `widget_info`) rather than clicking by position.
- Scenarios with an open photo need a RAW file, and none are checked in. Pass a path from an
  environment variable (skip the test when unset) and keep the file out of the repo
  (`testdata/private/` is git-ignored). Without a photo you can still cover the empty
  develop view, library view, dialogs and settings.
- The headless app has no shared wgpu device for image processing (CPU processor). GPU
  preview behaviour needs the real-window tool below.
- Dialogs from `rfd` (Open File/Folder) cannot be driven; set state through app paths
  (`initial_path`, the scratch config directory) instead.

## 2. Real-window screenshot (look only)

Launches the real app (real GPU and compositor), waits, saves one frame, and exits:

```sh
cargo run -p rohditor-desktop -- <file.ARW> --screenshot /tmp/ui.png --screenshot-delay 5
```

Use a fresh `XDG_CONFIG_HOME=$(mktemp -d)` to avoid the user's saved session. The delay
(seconds, default 5) lets the preview finish rendering. This cannot click anything; use
scenarios for interaction.

## Always

Run `./scripts/check.sh` (and `./scripts/check-gpui.sh` for `apps/gpui`) before handing off;
the ignored UI scenarios are not part of them, so run them yourself when you touched UI code.
