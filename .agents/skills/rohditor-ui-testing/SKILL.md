---
name: rohditor-ui-testing
description: See and drive the Rohditor UI headlessly to verify UI changes - render screenshots, click and drag widgets, run UI scripts with the scripted driver. Use when changing anything under apps/desktop/src, or when asked to look at, test, or iterate on the UI.
---

# Rohditor UI testing (closing the loop)

The desktop app (`apps/desktop`, package `rohditor-desktop`) is built on GPUI.

## Headless tests

Headless tests drive the real `Workspace` with GPUI's headless renderer and a wgpu adapter.
Code: `apps/desktop/src/ui_tests.rs` (the `Harness`) and `ui_tests/` (scenarios). Elements are
found by **test id** (`.test_id("section:Defringe")` in the UI code, recorded in test builds
only): `adjustment:<Parameter>`, `section:<Name>`, `button:<id>`, `viewport`, and so on.
`grep -rn 'test_id(' apps/desktop/src` lists them, and the driver writes the visible ones.

Environment variables (all tests are `#[ignore]`d):
- `ROHDITOR_RAW`: a RAW file. Tests copy it to a scratch folder as `A.ARW` and `B.ARW`, so
  sidecars never land next to the original. Without it, photo tests silently pass.
- `ROHDITOR_DCP`: a camera profile for that photo's camera (only the color-profile test).
- `ROHDITOR_SHOTS`: a directory; every `capture` writes a PNG there.
- `ROHDITOR_SCRIPT`: a script for the driver, below.

## The scripted driver: explore without compiling a test

Prefer this to writing a Rust test when trying something out or taking screenshots. Write a
script (`apps/desktop/src/ui_tests/driver.rs` documents every command) and run:

```sh
ROHDITOR_RAW=/path/photo.ARW ROHDITOR_SCRIPT=/tmp/s.txt ROHDITOR_SHOTS=/tmp/out \
  cargo test --release --locked -p rohditor-desktop run_the_script -- --ignored --nocapture
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

## Compiled scenarios

Add one to `ui_tests/` when behaviour should stay covered: build with `opened(name)`, act
with `click`, `drag_to`, `hover`, `scroll`, `keys`, assert on the session, `capture` a shot.
Run `cargo test --release --locked -p rohditor-desktop -- --ignored`, filtered by test name
when you can. Each test opens a full RAW: about 9 s in a release build against about 30 s in
debug (all 33 photo tests: roughly 20 min in debug). `[profile.test] opt-level = 3` is not
worth it: it rebuilds every dependency (about 9 min) and still runs slower than `--release`.
`color::profiles_are_chosen_and_imported` also needs `ROHDITOR_DCP`.

## Real-window screenshot (look only)

Launches the real app (real GPU and compositor), waits at least the delay (seconds, default 5)
and then until the preview has settled, saves one frame, and exits (status 1 when no PNG was
written):

```sh
XDG_CONFIG_HOME=$(mktemp -d) cargo run --release --locked -p rohditor-desktop -- <file.ARW|folder> \
  --screenshot /tmp/ui.png --screenshot-delay 2
```

The fresh `XDG_CONFIG_HOME` keeps the user's saved session and settings out of it; note that
`--demosaic` is saved like a settings change. `--diagnostics` opens the diagnostics tab,
`--processor cpu` forces the CPU. It cannot click anything; use the driver for interaction.

## When to take screenshots

After any change to what a panel, overlay or the viewport draws, and for drag interactions
(crop, masks) while the button is held. Look at the PNG; do not trust assertions alone.
Not covered, so check by hand: native file dialogs, window moving/resizing, fractional
scaling, the real compositor. Screenshots are not compared pixel by pixel.

## Always

Run `./scripts/check.sh` before handing off; the ignored UI tests are not part of it, so run
them yourself when you touched UI code.
