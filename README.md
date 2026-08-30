# Rohditor

Rohditor is a Linux-first RAW photo editor being built for Sony `ILCE-6400`
files. Decoder validation, the deterministic CPU reference pipeline, formal
export layer, desktop editor, and first GPU preview path (Phases 1 through 5)
are complete. The next milestone is preview caching, cancellation, and measured
performance work described in [`plan.md`](plan.md).

## Current capabilities

- A Rust 2024 workspace with separate core, RAW, GPU, CLI, and desktop packages.
- Headless `rohditor-cli inspect`, `extract-preview`, and `develop` commands.
- Content-based ARW detection, normalized metadata, bounded sensor dimensions,
  exact embedded-JPEG extraction, and typed corrupt-input errors.
- Typed sensor-mosaic, scene-linear Rec.2020, and display-sRGB buffers with
  black/white normalization, bilinear Bayer demosaic, camera color conversion,
  global adjustments, orientation, deterministic 8/16-bit quantization, and
  optional ordered dithering.
- Transactional 8-bit JPEG and 8/16-bit PNG export with an embedded sRGB ICC
  profile, selected safe EXIF, configurable JPEG quality, and explicit
  overwrite protection.
- An `eframe` desktop editor with a portal-backed open/save workflow, embedded
  loading preview, 2560-edge developed CPU or GPU preview, global adjustment
  controls, zoom/pan/fit, revision-safe background work, undo/redo, and
  background export.
- wgpu/Vulkan UI rendering with a compiled glow fallback. GPU previews share
  eframe's device and queue, retain the linear base on the GPU, and display an
  egui-native texture without CPU readback. CPU previews and all exports remain
  available without GPU processing.
- Headless core, RAW, and CLI test paths that do not initialize Vulkan or a
  window system.

The desktop worker and GPU-preview contracts are documented in
[`docs/desktop.md`](docs/desktop.md) and
[`docs/gpu-preview.md`](docs/gpu-preview.md). The Phase 2 color baseline is in
[`docs/cpu-pipeline.md`](docs/cpu-pipeline.md), and the export contract is in
[`docs/export.md`](docs/export.md).

## Prerequisites

- Rust 1.88 or newer, including `cargo`, `rustfmt`, and Clippy.
- Network access for the first build so Cargo can download dependencies.

The checked-in toolchain file selects stable Rust and the required components.
No native image-decoder dependency is required for the current implementation.

## Build and check

```console
cargo build --workspace
./scripts/check.sh
```

The private camera corpus is deliberately excluded from version control. To run
all opt-in decoder, CPU-pipeline, and CLI tests against it locally (release mode
keeps full-resolution development fast):

```console
cargo test --release --workspace --tests -- --ignored --nocapture
```

## Run the desktop editor

```console
cargo run --release -p rohditor-desktop
cargo run --release -p rohditor-desktop -- testdata/private/DSC00851.ARW
```

The first form opens files through the XDG portal-capable native dialog. The
second opens a file immediately. wgpu/Vulkan is preferred for drawing the UI;
use `--renderer glow` for the OpenGL fallback.

Interactive preview processing is independently selectable:

```console
rohditor-desktop --processor auto  # use a shared hardware wgpu device, otherwise CPU
rohditor-desktop --processor gpu   # require a compatible shared wgpu device
rohditor-desktop --processor cpu   # never create image-processing GPU resources
```

`--renderer glow` cannot provide a shared `wgpu` device, so `--processor auto`
falls back to CPU and `--processor gpu` reports an initialization error. Exports
remain full-resolution CPU renders in Phase 5.

## Inspect a RAW file

```console
cargo run -p rohditor-cli -- inspect testdata/private/DSC00851.ARW
cargo run -p rohditor-cli -- inspect --json testdata/private/DSC00851.ARW
```

Inspection decodes the sensor mosaic by default so it exercises the risky part
of the decoder. Pass `--metadata-only` for a faster metadata probe.

## Extract an embedded preview

```console
cargo run -p rohditor-cli -- extract-preview \
  testdata/private/DSC00851.ARW target/DSC00851-preview.jpg
```

For Sony ARW files, this copies the camera's original embedded JPEG without
re-encoding it. The destination must end in `.jpg` or `.jpeg`; an existing file
is preserved unless `--force` is passed. Preview output uses the same
transactional sibling-file commit as developed exports and refuses a destination
that resolves to the source RAW through either a hard or symbolic link.

## Develop and export a RAW on the CPU

```console
cargo run --release -p rohditor-cli -- develop \
  testdata/private/DSC00851.ARW target/DSC00851-developed.png

cargo run --release -p rohditor-cli -- develop \
  --jpeg-quality 92 \
  testdata/private/DSC00851.ARW target/DSC00851-developed.jpg

cargo run --release -p rohditor-cli -- develop \
  --png-bit-depth 16 --dither \
  testdata/private/DSC00851.ARW target/DSC00851-developed-16.png
```

`develop` selects JPEG or PNG from the destination extension and produces a
full-resolution, physically oriented sRGB image. JPEG quality defaults to 90;
PNG depth defaults to 8-bit. `--metadata none` omits source EXIF, while safe
metadata is preserved by default with orientation normalized to top-left after
pixel rotation. It prints decode, processing, encoding, and estimated
buffer-memory diagnostics. It is headless and never initializes `wgpu` or a
window system.

Explicit recipe arguments include `--exposure`, `--contrast`, `--saturation`,
relative `--white-balance RED,GREEN,BLUE`, `--crop`, `--demosaic`, and
`--orientation`. Run `rohditor-cli develop --help` for all export and recipe
options. Existing outputs are preserved unless `--force` is passed, failed
exports do not truncate an existing destination, and the command refuses to
replace a source RAW even through a hard link.

## Dependency and licensing note

`rawler` 0.7.2 is pinned because its API does not currently follow semantic
versioning. It is licensed under LGPL-2.1. Rohditor's own project license has
not been selected yet, so all workspace packages are currently marked
`publish = false`.
