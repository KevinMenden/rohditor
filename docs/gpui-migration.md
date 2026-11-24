# Moving the desktop app to GPUI

Goal: `apps/gpui` replaces the egui app (`apps/desktop`) as the main Rohditor desktop app, on
Linux. It covers what the egui app does today, but is not a copy: the look takes inspiration from
Zed, and weak spots are redesigned. egui stays usable until the last stage. Background: #203.

Work through the steps in order, ticking them off here. One pull request per step where it fits.

## Decisions

Assumed from the recommendations unless marked open; change them here if they turn out wrong.

- [x] Linux only for now. Zero-copy preview works only on GPUI's Linux (wgpu) backend.
- [x] egui features are frozen; it only gets fixes.
- [x] `apps/gpui` merges to main early, as an experimental app.
- [x] The redesigns below.
- [ ] **Open:** is X11 required, or is Wayland enough? Needed by step 7.1.
- [x] The Zed revision stays pinned until stage 7: newer ones need an unreleased wgpu.

## Where we stand

| Area | GPUI today | Missing |
|---|---|---|
| Open, folder list, autosave, undo | Done | Save status and errors, recovery checks |
| Basic sliders | Typed, by keyboard, grouped and searchable, with section reset and changed markers | |
| Light | Tone, parametric curve, presence, highlight reconstruction | |
| Color | White balance, mixer, grading wheels, profiles, pickers | |
| Other adjustments | None | Defringe, moiré, optics, full detail controls |
| Viewport | Fit, zoom, pan, 1:1, clipping overlay | Tool overlays |
| Tools | None | Crop, masks, white balance and colour pickers |
| Presets, info | None | All |
| Library | File list | Grid, sorting, filmstrip |
| Export | JPEG only | PNG, size, sharpening, metadata, overwrite |
| Settings, diagnostics | None | All, including neural demosaic |

## Redesigns

- **Panel:** sections grouped into Light, Color, Detail and Optics, with search and an
  "edited only" filter.
- **White balance:** one preset dropdown plus temperature and tint. Old recipes with manual
  multipliers keep rendering, but there is no UI for them.
- **Color grading:** three colour wheels over the recipe values of today's nine RGB sliders.
- **Tone curve:** stays parametric for now, laid out so that #175, #176 and #177 fit in.
- **Library:** a tab beside Develop and a toggleable filmstrip. No single-file panel.
- **Masks:** a compact list, with shape controls on the canvas.
- **Commands:** everything is in the command palette; tooltips show keys.
- **Diagnostics:** a plain panel with the same report export.

## Rules for the work

- Logic the egui app holds today moves down when GPUI needs it, with its tests: recipe math to the
  lower crates, interactive behaviour to `rohditor-engine`. Never copy it into `apps/gpui`.
- egui is adapted only as far as such a move requires.
- Changes to the vendored `gpui_wgpu` are marked `ROHDITOR` and kept small.
- Nothing may hold GPUI's wgpu instance or device strongly past GPUI's shutdown.

## Stage 0: Foundation

- [x] 0.1 Land the wgpu 29 / egui 0.35 update on main (`chore/wgpu-29`).
- [x] 0.2 Clean up the spike and merge `apps/gpui` as an experimental app, with its own CI job.
- [x] 0.3 Split `Session`, so that tools and panels do not all land in it.
- [x] 0.4 Find elements by selector in UI tests; cache the panels that did not change.

Done when CI builds `apps/gpui` and no test clicks raw pixel positions.

## Stage 1: Widget kit

- [x] 1.1 Text input, dropdown, popover, tooltip, modal, context menu, toggle, toasts.
- [x] 1.2 Final slider: typing, arrow keys, reset, coloured tracks.
- [x] 1.3 Focus order and accessible labels (AccessKit).

Done when a full slider edit works by keyboard alone.

## Stage 2: Viewport

- [x] 2.1 Source-resolution 1:1.
- [x] 2.2 An overlay layer with screen-to-image mapping.
- [x] 2.3 Clipping overlay.
- [x] 2.4 Measure large CPU frames (crop, CPU-fallback 1:1; about 24 MP). Upload them straight to
  a texture instead of GPUI's atlas if that is slow.

Done when 1:1 and clipping match egui on GPU and CPU.

## Stage 3: All adjustments

- [x] 3.1 Move the slider-to-recipe mapping down; section reset and changed markers.
- [x] 3.2 Light, tone curve, highlight reconstruction.
- [x] 3.3a Color: white balance presets, mixer, wheels, profiles, DCP import.
- [x] 3.3b White balance and mixer pickers.
- [x] 3.4 Detail: noise reduction with Auto, sharpening, moiré, defringe.
- [x] 3.5 Optics with lens search.
- [ ] 3.6 Info and presets.

Done when every global recipe control is editable, apart from the omissions above.

## Stage 4: Crop

- [ ] 4.1 Tool switching and lifecycle.
- [ ] 4.2 Overlay with handles.
- [ ] 4.3 Aspect, lock, orientation, commit, cancel.

Done when each crop is one undo step.

## Stage 5: Masks (highest risk)

Reuses tool switching and overlays from stage 4.

- [ ] 5.1 Move pointer, action and tint logic into the engine, with its tests.
- [ ] 5.2 Mask list, components, local sliders.
- [ ] 5.3 Linear and radial gradients.
- [ ] 5.4 Brush.
- [ ] 5.5 Luminance and color ranges, tint overlay.

Done when the moved tests pass and painting feels smooth on real RAWs.

## Stage 6: Library, export, settings

- [ ] 6.1 Library grid, sorting, filmstrip.
- [ ] 6.2 Full export dialog.
- [ ] 6.3 Settings, neural model, diagnostics.
- [ ] 6.4 Startup options and `--screenshot`.

Done when nothing needs egui any more.

## Stage 7: Switch over

- [ ] 7.1 X11 if required, device-loss recovery, hand tests of file dialogs and window resizing.
- [ ] 7.2 Measure drag latency against egui.
- [ ] 7.3 About two weeks of daily use.
- [ ] 7.4 Remove egui, rename to `apps/desktop`, merge the Cargo workspaces. GPUI needs Rust 1.95,
  so the minimum rises from 1.92. Decide on the Zed revision.
- [ ] 7.5 Update the install script, AGENTS.md and the UI-testing skill.

## Effect on open issues

- #187: drop the desktop half; the CLI half stands.
- #167: the engine half stands; its UI moves to GPUI.
- #164, #168, #173, #176, #194: their UI moves to GPUI.
- #195 (work queue): needs the above, or it keeps sending UI work to egui.
