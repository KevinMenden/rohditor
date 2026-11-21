---
name: rohditor-ui-testing
description: See and drive the Rohditor desktop UI headlessly to verify UI changes - render screenshots, click widgets by label, and diff snapshots. Use when changing anything under apps/desktop/src/ui or app/, or when asked to look at, test, or iterate on the UI.
---

# Rohditor UI testing (closing the loop)

Two tools, both in `apps/desktop`. Prefer the first; use the second to confirm the final
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

Run `./scripts/check.sh` before handing off; the ignored UI scenarios are not part of it,
so run them yourself when you touched UI code.
