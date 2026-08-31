# Rohditor

Rohditor is a Linux-first Rust RAW photo editor for Sony ILCE-6400 ARW files.
It provides a deterministic CPU development pipeline, optional GPU previews,
and a desktop editor plus headless CLI.

## Build and test

```console
cargo build --workspace
./scripts/check.sh
cargo bench -p rohditor-core --bench pipeline_stages
```

The private camera corpus is intentionally not checked in. Opt-in validation:

```console
cargo test --release --workspace --tests -- --ignored --nocapture
cargo test --release -p rohditor-gpu -- --ignored --nocapture
```

## Run

```console
cargo run --release -p rohditor-desktop
cargo run -p rohditor-cli -- inspect path/to/file.ARW
cargo run --release -p rohditor-cli -- develop input.ARW output.png
```

The desktop supports `--processor auto|gpu|cpu`, `--renderer auto|wgpu|glow`,
and an optional input path. The CLI also supports embedded-preview extraction,
quality-crop generation, and LibRaw verification.

See [project guidelines](docs/guidelines.md) and [implementation status](docs/status.md).

# Implementation status

Status: MVP implemented and validated on the current Sony ILCE-6400 corpus.

## Implemented

- [x] Rust workspace split into RAW, core, GPU, CLI, and desktop crates.
- [x] Content-based Sony ARW decoding, metadata, embedded JPEG extraction, and
      typed malformed-input errors.
- [x] CPU pipeline: crop, black/white normalization, MHC or bilinear Bayer
      demosaic, white balance, camera color conversion, global edits,
      orientation, and sRGB output.
- [x] Antialiased fixed-size previews, cancellable source-scale 1:1 inspection,
      caching, newest-wins scheduling, undo/redo, and background export.
- [x] Optional wgpu/WGSL GPU preview with CPU fallback and CPU/GPU parity tests.
- [x] CLI inspection, preview extraction, development/export, quality crops,
      and LibRaw verification.
- [x] Transactional JPEG/PNG export with safe EXIF, sRGB ICC metadata,
      overwrite protection, 8/16-bit PNG, and optional dithering.
- [x] Linux desktop UI, XDG file dialogs, diagnostics, `.desktop` entry, and
      SVG icon.
- [x] Unit, integration, synthetic-quality, and malformed-input tests.

## Missing or deliberately deferred

- [ ] Select and document the project license; distribution remains disabled.
- [ ] Catalog/library features: folders, ratings, tags, search, and batch work.
- [ ] Local edits: masks, brushes, gradients, crops, and perspective tools.
- [ ] Advanced image quality: tone curve/highlight recovery, camera profiles,
      sensor cleanup, denoising, sharpening, and lens correction.
- [ ] Monitor ICC management and wide-gamut output/display support.
- [ ] Full-resolution GPU export.
- [ ] Broader camera, RAW format, and platform support.

The private corpus and hardware-GPU tests are opt-in and are not required for
the normal check suite. The current target is not arbitrary hostile RAW input.
