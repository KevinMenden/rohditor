# Rohditor restructuring review

Date: 2026-09-01

This review reflects the working tree inspected during the audit, including the existing local editing-control changes. It is a maintainability plan, not a proposal to change product scope or processing behavior.

## Summary

The existing top-level boundaries are sound:

- `rohditor-raw` decodes RAW files and owns decoder-facing metadata.
- `rohditor-core` owns the deterministic CPU reference pipeline.
- `rohditor-gpu` owns optional interactive GPU processing.
- `apps/cli` and `apps/desktop` own presentation and application orchestration.

The main problem is not that the workspace has too few crates. It is that `rohditor-core` now contains several independently coherent domains, while a handful of files in `core`, `gpu`, and the applications have accumulated multiple responsibilities.

Demosaicing is a good separate-crate candidate. It is already written as a self-contained algorithm module and has substantial dedicated correctness and quality tests. It should not, however, be extracted by making a new crate depend on `rohditor-core`; that would reverse the intended dependency direction. A small foundational image-types crate should be established first, or the demosaic API should otherwise be made independent of core-owned types.

The recommended direction is:

1. Split the largest files into responsibility-based modules without changing crate boundaries.
2. Extract a small `rohditor-image` foundation for typed image states and layout validation.
3. Extract `rohditor-edit`, then `rohditor-demosaic`.
4. Consider `rohditor-color` and `rohditor-export` after the earlier boundaries have settled.
5. Keep desktop scheduling, UI widgets, RAW adapter details, resampling, and histogram analysis as modules rather than making every subsystem a crate.

## What should justify a crate

A crate should provide at least one concrete benefit that a module cannot:

- It establishes an important one-way dependency boundary.
- It is used directly by more than one existing crate.
- It is an independently testable algorithm or domain model with a small public API.
- It removes heavyweight or unrelated dependencies from another crate.
- It is likely to evolve independently and has a clear owner and vocabulary.

A file being long is not, on its own, enough reason to create a crate. Long application orchestration and UI files should usually be split into modules because they are still used by only one application. Tiny crates for cancellation, histograms, tone curves, resampling, individual adjustments, or filesystem helpers would add manifests and public APIs without producing a meaningful boundary.

## Current pressure points

Approximate line counts include inline tests, but still show where responsibilities have accumulated.

| Area | Approximate size | Current responsibilities | Recommended action |
| --- | ---: | --- | --- |
| `crates/core/src/cpu.rs` | 1,975 lines | RAW normalization, white balance, camera conversion, all adjustments, HSL utilities, output conversion, dithering, validation, and many tests | Split into processing-stage modules; move edit semantics out later |
| `crates/core/src/pipeline.rs` | 1,023 lines | Public pipeline API, preview-base lifecycle, export rendering, memory estimates, timings, and stage orchestration | Keep the orchestration in core, but split types, preview preparation, rendering, and memory accounting |
| `crates/core/src/export.rs` plus `output.rs` | 1,087 lines | Export settings, quantized-image wrapper, codecs, ICC generation, EXIF generation, and transactional writes | First split internally; later extract an outer `rohditor-export` crate |
| `crates/gpu/src/preview.rs` | 1,861 lines | Upload packing, retained GPU resources, processor setup, dispatch, parameters, readback, and parity tests | Keep one GPU crate; split this file into GPU-specific modules |
| `apps/desktop/src/app.rs` | 2,409 lines | Document state, lifecycle, job handling, GPU lifecycle, export flow, picker logic, UI model construction, and view composition | Continue the existing `app/events.rs` and `app/gpu.rs` split; move document and controller responsibilities out |
| `apps/desktop/src/coordinator.rs` | 2,280 lines | Worker protocol, scheduling, decode/open, cache coordination, preview development, sampling, export, placeholder conversion, and tests | Split into desktop-only coordinator modules, not crates |
| `apps/desktop/src/ui/adjustment_panel.rs` | 1,155 lines | All adjustment groups, histogram, tone curve, export controls, and messages | Split by panel section while retaining one UI module boundary |
| `apps/cli/src/main.rs` | 1,689 lines | Clap model, inspect, preview extraction, develop, quality crops, LibRaw verification, PGM parsing, formatting, and tests | Split into `args`, `commands`, and `output`; consider a devtools binary later |
| `crates/raw/src/rawler_adapter.rs` | 699 lines | Session lifecycle, metadata mapping, mosaic decoding, embedded preview handling, and rawler-specific validation | Split internally by adapter responsibility; do not create a rawler crate yet |

The current dependency graph is simple and healthy: `raw` is below `core`, `gpu` depends on `core` and `raw`, and both applications depend on the libraries they need. New crates should preserve that acyclic direction.

## Recommended crate extractions

### 1. `rohditor-image`: foundational typed image states

Recommendation: extract first as an enabling boundary.

Move the generic, Rohditor-owned image vocabulary from `core`:

- `BayerPattern` and `CfaColor`
- `MosaicImage<T>`
- `LinearRgbImage<T>` and `LinearRgbSpace`
- `DisplayRgbImage<T>` and `DisplayTransfer`
- `ImageRegion` and `Halo`
- checked image-layout construction and allocation helpers
- the generic orientation value and coordinate mapping, if the RAW and edit layers can both use it

This crate should own an `ImageError` for invalid layouts, allocation failures, and wrong typed states. `PipelineError` can wrap or convert it. Constructors such as `MosaicImage::new` should not return a core-pipeline error from a lower-level crate.

Keep this crate deliberately narrow. It should not depend on `rawler`, `image` codecs, `wgpu`, `eframe`, the desktop application, or the pipeline. Ideally it needs only the standard library and possibly `thiserror`.

Why this helps:

- Demosaic and color code can depend on typed images without depending on the whole CPU pipeline.
- CPU and GPU boundary objects share precise image-state vocabulary.
- Typed-state and checked-layout invariants get a clear lowest-level owner.
- It prevents a future `rohditor-demosaic -> rohditor-core` dependency inversion.

Do not turn this into a general-purpose image framework. It should contain only states already required by Rohditor.

### 2. `rohditor-edit`: recipe and adjustment semantics

Recommendation: high-value extraction immediately after the foundational types are settled.

Move:

- `EditRecipe` and its schema migration
- adjustment groups and `WhiteBalance`
- parameter ranges and validation
- `LightToneLut`
- pure tone-curve and HSL-band evaluation helpers
- other small deterministic adjustment semantics that CPU and GPU preparation must share

The current edit model is used by core, GPU, CLI, and desktop. It is therefore a real shared domain boundary rather than a cosmetic split. It also lets recipe serialization and migration evolve without making the image-processing core the owner of persistence concerns.

The crate should define an `EditError`, which core can convert to `PipelineError`. It should depend on `serde` and the foundational orientation type, but not on `rohditor-core`, `wgpu`, `eframe`, or CLI types.

Keep whole-image iteration and Rayon scheduling in core initially. `rohditor-edit` should own what an adjustment means; core should own applying the deterministic CPU stages to an image. Shared lookup tables are especially valuable because they reduce CPU/WGSL semantic drift. Do not create one crate per adjustment group.

During migration, `rohditor-core` can temporarily re-export edit types so applications do not need one large import rewrite in the same change.

### 3. `rohditor-demosaic`: Bayer reconstruction algorithms

Recommendation: yes, extract it, after its input/output boundary no longer belongs to core.

Move:

- `crates/core/src/demosaic/`
- `DemosaicAlgorithm`
- MHC halo information
- the focused demosaic correctness and synthetic-quality tests
- a demosaic-specific benchmark target

The public contract should be approximately: a validated normalized `MosaicImage<f32>` plus an algorithm and cancellation check produces a camera-native `LinearRgbImage<f32>` or `DemosaicError`.

Important boundary decisions:

- The crate must depend on `rohditor-image`, never on `rohditor-core` or `rohditor-raw`.
- It should not know about `RawFrame`, crop policy, recipes, preview caches, export, or UI state.
- Prefer making demosaic itself white-balance neutral. Reconstruct camera-native channels, then let the core/color stage apply gains immediately afterward. This gives demosaic one responsibility and preserves preview reconstruction reuse. A compatibility wrapper can preserve behavior during the move.
- Cancellation should be represented by a small borrowed trait/callback or another lower-level contract. Do not make the algorithm crate reach up to `core::CancellationToken`.
- `DemosaicError` should describe dimensions, non-finite data, allocation, and cancellation; core maps it to its pipeline-level error.

This crate is large enough to be worthwhile even though the production implementation is compact: it has multiple algorithms, a stable selector, border/halo rules, Rayon execution, and extensive algorithm-specific tests. New algorithms can be added without expanding the core orchestrator.

### 4. `rohditor-color`: color math and transforms

Recommendation: good candidate, but extract after image and edit boundaries.

Move:

- `Matrix3`
- standard color-space matrices
- chromatic adaptation
- linear sRGB transfer helpers
- Rec.2020-to-display conversion
- camera color-transform calculation expressed through a color-owned calibration input

The current color module is coherent, and GPU code already consumes its matrices and constants. A shared crate would make CPU/GPU color contracts more explicit.

Avoid accepting `RawFileInfo` directly in the low-level color API. Core should translate decoder metadata into a small `CameraCalibration` value containing only the matrices and illuminants the color crate needs. That prevents `rohditor-color` from depending on the complete RAW decoder model and makes the math easy to test with small fixtures.

White-balance selection belongs to edit semantics; resolving a selection through camera calibration belongs either in color or in a thin core adapter. Whichever location is chosen, keep one implementation that CPU and GPU parameter preparation both call.

Do not split matrix math, transfer functions, and chromatic adaptation into separate crates. They form one small color domain.

### 5. `rohditor-export`: encoding and transactional output

Recommendation: worthwhile outer-layer extraction, but not the first move.

Move:

- JPEG and PNG settings and encoders
- ICC profile construction
- safe EXIF construction
- export validation and report types
- transactional file creation and overwrite protection
- existing export integration tests

This is a strong semantic boundary because it owns side effects and codec dependencies, while core is supposed to own deterministic image processing. Extracting it would remove the `image` codec and `bytemuck` dependencies from core's normal processing implementation.

There is currently a cycle risk: `CpuPipeline::render_export` uses `ExportImage`, `OutputBitDepth`, and `DitherMode`, which are declared in `export.rs`. Before extracting the crate:

1. Rename the processing result to a core/image concept such as `QuantizedDisplayImage`.
2. Keep output sample depth and deterministic dithering as render options owned below the encoder layer.
3. Let `rohditor-export` map `ExportSettings` to those render options and encode the returned display image.

The final dependency must be `rohditor-export -> rohditor-core` and `rohditor-raw`, never `rohditor-core -> rohditor-export`. The application can continue to perform rendering and encoding as two explicit cancellable/progress-reporting steps.

## Areas that should remain modules

### RAW decoding

Keep one `rohditor-raw` crate. `RawlerDecoder` is an adapter behind the existing `RawDecoder` and `RawSession` traits, and there is currently no second decoder implementation that would justify another crate. Split `rawler_adapter.rs` into internal modules such as session, metadata mapping, embedded preview, and mosaic conversion. Preserve the rule that rawler types never escape the crate.

### Resampling, histogram, orientation mapping, and cancellation

These are small, coherent building blocks but not independent products. Keep them as modules in the lowest appropriate crate. Orientation mapping likely belongs with image types; histogram analysis and resampling remain processing modules unless a second real consumer appears. Cancellation should remain a pipeline/application mechanism except for the minimal interface algorithms accept.

### GPU preview

Keep one `rohditor-gpu` crate. Uploads, resources, dispatch, readback, and shader parameters all share `wgpu` concepts and lifecycle. Splitting `preview.rs` into `upload.rs`, `resources.rs`, `processor.rs`, `parameters.rs`, and `readback.rs` will improve navigation without creating public crate boundaries. Keep CPU/GPU parity tests close to this crate.

### Desktop application

Desktop document state, scheduling, egui models, and controller actions have only one consumer and should remain inside `apps/desktop`. Splitting them into modules will give more benefit than publishing application-internal crates.

### CLI commands and developer tools

Split `main.rs` into modules. `quality-crops` and `verify-libraw` are development/quality tools rather than normal development-pipeline commands; if they keep growing, move them into a separate `apps/devtools` binary package. That would be a product/binary separation, not a reusable library extraction.

## Non-crate organization improvements

### Split `cpu.rs` by pipeline stage

A useful module layout is:

```text
core/src/cpu/
  mod.rs            public CPU-stage facade
  normalize.rs      crop, levels, CFA phase, RAW validation
  white_balance.rs  gain resolution and temperature/tint calibration
  adjustments.rs    whole-image stage orchestration
  tone_curve.rs     CPU tone-curve application if not moved to edit
  hsl.rs            HSL mixer and color grading helpers
  display.rs        orientation, transfer encoding, and quantization
  tests/            focused private-module tests
```

This can be behavior-preserving. Keep the small public facade and make helpers private or `pub(crate)`. Moving the large inline test block into focused test modules will also make production responsibilities easier to see.

### Make `pipeline.rs` an orchestrator rather than a second algorithm file

Keep `CpuPipeline` as the high-level entry point. Move public option/result types to `pipeline/types.rs`, memory calculations to `pipeline/memory.rs`, reconstruction/base preparation to `pipeline/prepare.rs`, and preview/export rendering orchestration to focused modules. The high-level sequence should remain obvious in `pipeline/mod.rs`.

Avoid a proliferation of similar public methods as cache paths are added. Consider request objects for preview preparation and render execution, while keeping the current convenience methods as thin wrappers. Cacheable products such as `ReconstructedPreview` and `DemosaicedBase` should document exactly which recipe fields affect their identity.

### Reduce `RohditorApp` responsibilities

Continue the current extraction of event and GPU code. In particular:

- Move the `Document` struct out of `app.rs` into a `document/` module with edit history and preview state.
- Rename or reorganize the current `document.rs`, which owns `EditSession` and `PreviewTicket` but not the document itself.
- Move picker sampling and auto-tone actions into testable controller/action modules.
- Keep egui view functions responsible for building models and returning intents, not performing worker or filesystem operations.
- Aim for `eframe::App::update` to compose panels, consume intents/events, and delegate state transitions.

### Split the desktop coordinator around the worker protocol

A useful layout is:

```text
coordinator/
  mod.rs        RenderCoordinator facade
  protocol.rs   request, event, job, and progress types
  scheduler.rs  newest-wins preview mailbox and cancellation
  worker.rs     worker loop and panic/error boundary
  open.rs       session creation and embedded placeholder
  preview.rs    reconstruction/base/render/cache flow
  sampling.rs   white-balance and other image samples
  export.rs     render/export job orchestration
```

The worker should return plain Rohditor image/pixel data. Convert it to `egui::ColorImage` at the UI boundary instead of storing an egui type in `WorkerImage`. That makes worker protocol tests independent of rendering widgets and keeps application processing reusable inside the desktop package.

### Split large UI and GPU files without adding crates

- Divide `adjustment_panel.rs` into light, color, histogram, export, and message sections behind one panel facade.
- Divide `gpu/src/preview.rs` by resource lifecycle and operation, while keeping all wgpu ownership inside `rohditor-gpu`.
- Divide `rawler_adapter.rs` by rawler-facing responsibility.
- Put large private test modules in adjacent `tests.rs` files or focused test submodules where private access is required.

### Split the CLI by command

Keep `main.rs` limited to argument parsing, tracing setup, and dispatch. Suggested modules are `args.rs`, `commands/inspect.rs`, `commands/extract_preview.rs`, `commands/develop.rs`, `commands/quality_crops.rs`, `commands/verify_libraw.rs`, and `output.rs`. PGM parsing belongs beside LibRaw verification rather than in the application root.

### Use explicit public facades and dependency checks

- Give each library crate a small `lib.rs` that exposes intentional domain modules or a curated facade.
- Default to private modules and `pub(crate)` helpers; avoid making functions public only to work around an inconvenient file layout.
- Use temporary re-exports during migrations, then remove them in a separate cleanup change.
- Add a cheap architecture check that the CLI dependency tree contains neither `rohditor-gpu` nor desktop/eframe dependencies and that foundational crates do not depend upward on core or applications.
- Keep algorithm benchmarks with the crate that owns the algorithm. Keep end-to-end pipeline benchmarks in core and UI responsiveness measurements outside algorithm timings.

## Proposed dependency direction

The exact crate count can stop after `image`, `edit`, and `demosaic` if later extractions do not prove useful. In the diagram below, `A -> B` means “A depends on B”:

```text
apps/cli --------> rohditor-export, rohditor-core, rohditor-raw, rohditor-edit
apps/desktop ----> rohditor-export, rohditor-gpu, rohditor-core, rohditor-raw, rohditor-edit
rohditor-export -> rohditor-core, rohditor-raw, rohditor-image
rohditor-gpu ---> rohditor-core and the shared edit/color/image contracts it consumes
rohditor-core --> rohditor-raw, rohditor-edit, rohditor-demosaic, rohditor-color, rohditor-image
rohditor-raw ---> rohditor-image (only if shared orientation/image primitives are used)
rohditor-edit --> rohditor-image
rohditor-demosaic -> rohditor-image
rohditor-color --> rohditor-image
```

Not every listed dependency is mandatory. No lower-level crate should depend on core, GPU, CLI, or desktop. Export and GPU are outer services around the CPU reference, not dependencies of the deterministic core.

## Safe implementation sequence

Each step should be a behavior-preserving change with a focused diff.

1. **Module-only split:** split `cpu.rs`, `pipeline.rs`, desktop app/coordinator, GPU preview, rawler adapter, adjustment panel, and CLI. Keep public APIs unchanged.
2. **Image foundation:** add `rohditor-image`, move typed image states and layout errors, and have core re-export the types temporarily.
3. **Edit domain:** add `rohditor-edit`, move recipe/schema/ranges/shared evaluators, convert edit errors in core, and retain temporary core re-exports.
4. **Demosaic algorithms:** add `rohditor-demosaic`, move algorithms/tests/benchmarks, remove recipe/RAW dependencies from its API, and map its errors in core.
5. **Color domain:** add `rohditor-color` with a narrow calibration input and move shared CPU/GPU color math.
6. **Export boundary:** first separate render-output types from codec settings, then add `rohditor-export` and move filesystem/codec/metadata tests.
7. **Facade cleanup:** update direct consumers, remove temporary core re-exports, and add dependency-direction checks.

For every extraction:

- Run `./scripts/check.sh`.
- Run the ignored private release suite when normalization, demosaic, color, full-resolution rendering, or export behavior is touched.
- Run the ignored GPU suite when shared recipe/color semantics or GPU boundary types change.
- Keep small asymmetric typed-image tests and algorithm benchmarks with their owning crate.
- Compare output fixtures or hashes before and after the move; a restructuring change should not silently alter image output.

## Recommended stopping point

The best near-term target is the seven library crates `image`, `raw`, `edit`, `demosaic`, `color`, `core`, and `gpu`. `export` would be an eighth crate and should be added only when the render/encode cycle has first been removed and the separate boundary still proves valuable. This is not a quota. If `color` or `export` still has only one practical consumer after the earlier refactors, leaving it as a well-structured module is preferable.

The most valuable first work item is the module-only split, followed by `rohditor-image` and `rohditor-edit`. The demosaic extraction then becomes small, clean, and genuinely self-contained instead of being a new package wrapped around core-owned types.
