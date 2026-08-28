# Rohditor

Rohditor is a Linux-first RAW photo editor being built for Sony `ILCE-6400`
files. Decoder validation (Phase 1) is complete; the next milestone is the CPU
reference image pipeline described in [`plan.md`](plan.md).

## Current capabilities

- A Rust 2024 workspace with separate core, RAW, GPU, CLI, and desktop packages.
- Headless `rohditor-cli inspect` and `extract-preview` commands backed by
  `rawler`.
- Content-based ARW detection, normalized metadata, bounded sensor dimensions,
  exact embedded-JPEG extraction, and typed corrupt-input errors.
- A CPU-only test path; building and testing does not require Vulkan or a window
  system.

Image development, export, the desktop UI, and GPU processing are not
implemented yet.

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
all opt-in decoder and CLI tests against it locally:

```console
cargo test --workspace --tests -- --ignored --nocapture
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

## Dependency and licensing note

`rawler` 0.7.2 is pinned because its API does not currently follow semantic
versioning. It is licensed under LGPL-2.1. Rohditor's own project license has
not been selected yet, so all workspace packages are currently marked
`publish = false`.
