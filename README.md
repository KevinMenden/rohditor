# Rohditor

Rohditor is a Linux-first RAW photo editor being built for Sony `ILCE-6400`
files. Decoder validation, the deterministic CPU reference pipeline, and the
formal export layer (Phases 1 through 3) are complete. The next milestone is the
minimal desktop application described in [`plan.md`](plan.md).

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
- A CPU-only test path; building and testing does not require Vulkan or a window
  system.

The desktop UI and GPU processing are not implemented yet. The Phase 2 color
baseline is documented in [`docs/cpu-pipeline.md`](docs/cpu-pipeline.md), and
the export contract is documented in [`docs/export.md`](docs/export.md).

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
is preserved unless `--force` is passed.

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
