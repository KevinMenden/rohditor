# Reference development environment

Recorded on 2026-08-28 for the first decoder spike. These facts describe the
reference workstation, not minimum system requirements.

## Host

- Distribution: openSUSE Tumbleweed 20260822
- Kernel: Linux 7.1.8-1-default, x86-64
- CPU: AMD Ryzen 5 2600X, 6 cores / 12 logical CPUs
- Memory: 62 GiB
- Rust: rustc 1.95.0, cargo 1.95.0
- GPU: PowerColor AMD Radeon RX 9070 XT 16 GB (`1002:7550`)
- Kernel driver: `amdgpu`
- Vulkan adapter: AMD Radeon RX 9070 XT (RADV GFX1201)
- Vulkan API: 1.4.354
- Vulkan driver: Mesa RADV 26.2.1
- Software fallback also visible: llvmpipe, Mesa 26.2.1

The normal workspace checks do not initialize Vulkan. Opt-in GPU parity and
performance checks exercise the host Vulkan adapter.

## Phase 2 CPU baseline

On this workstation, a release build developed `DSC00851.ARW` at the 6000 x
4000 recommended crop in 1.17 seconds for the CPU image stages and 2.01 seconds
end to end including decode and lossless PNG output. `/usr/bin/time -v` reported
a maximum resident set size of 475,368 KiB. Rohditor's deterministic live-buffer
estimate was 413 MiB; the difference includes decoder, encoder, allocator,
thread, executable, and operating-system overhead. Warm release renders of all
six private samples took 0.30 to 0.35 seconds per CPU pipeline invocation before
PNG encoding. See [`cpu-pipeline.md`](cpu-pipeline.md) for stage-level details
and the exact baseline behavior.

## Phase 3 export validation

Measured on 2026-08-29 with the same release build and reference workstation:

- A neutral 6000 x 4000 16-bit PNG completed in about 1.3 seconds and produced
  a 125,194,332-byte file. The reported core/decoded-buffer estimate was 459
  MiB; the PNG encoder's endian-conversion allocation is additional.
- A physically rotated 4000 x 6000 JPEG at quality 90 completed in about 1.3
  seconds and produced a 3,343,286-byte file.
- For the same 6000 x 4000 landscape, JPEG quality 20 produced 863,790 bytes
  while quality 95 produced 5,260,150 bytes. Integration tests also confirm
  lower decoded error at quality 95 relative to the lossless PNG reference.
- `feh --loadable`, ImageMagick `identify`, FFmpeg `ffprobe`, the Rust `image`
  decoder, and `file` all accepted the representative JPEG and PNG. ImageMagick
  reported sRGB, top-left orientation, and both ICC and EXIF profiles. FFmpeg
  reported `rgb48be` for the 16-bit PNG, and `file` independently identified it
  as 16-bit-per-color RGB.

See [`export.md`](export.md) for the format, metadata, and destination-write
contract.

## Phase 4 desktop validation

Measured on 2026-08-29 with a debug desktop build on the Plasma/KWin Wayland
session:

- The glow fallback opened `DSC00851.ARW`, installed the embedded placeholder,
  and replaced it with a 2560×1707 CPU-developed texture in 991 ms.
- The default wgpu path selected Vulkan and
  `AMD Radeon RX 9070 XT (RADV GFX1201)`, then displayed the same CPU preview in
  1,087 ms. llvmpipe was visible as a second adapter but was not selected.
- Both windows reported the UI renderer separately from `Processor: CPU` and
  displayed the expected adjustment, history, viewport, progress, and export
  controls.
- Across two release private-suite runs, all six 2560-edge previews completed
  in 61 to 77 ms each after decode. The end-to-end desktop-worker test (open,
  preview, and full-resolution JPEG snapshot export) completed in about 1.3
  seconds on a warm filesystem cache.
- The desktop dependency graph contains one `wgpu` version shared by eframe,
  egui-wgpu, and Rohditor's explicit Vulkan feature selection. The glow backend
  remains compiled for fallback use.

The timings above measure the CPU preview stages after the full sensor frame was
decoded; embedded-placeholder latency and decode timing are reported as
separate UI states. See [`desktop.md`](desktop.md) for the worker and revision
contract.

## Phase 6 preview performance

Measured on 2026-08-30 with release builds on the same workstation. The
2560×1707 `DSC00851.ARW` CPU cache test measured 67.19 ms for its first developed
preview and 32.12 ms median / 35.17 ms maximum across 24 cached downstream
edits. The bounded CPU cache estimate stayed at 175.6 MiB. Forty cached GPU
edits on the RX 9070 XT measured 0.159 ms median / 0.406 ms maximum conservative
queue-completion latency, with 0.044 ms final encode/submit time and an 83.3 MiB
resident texture estimate. See [`preview-performance.md`](preview-performance.md)
for stage results, commands, measurement caveats, and the still-in-budget
contention results from the complete ignored workspace suite.

## Target camera corpus

- Camera: Sony alpha 6400
- EXIF model: `ILCE-6400`
- Camera firmware: 2.00
- Raw dimensions reported by the file: 6048 x 4024
- RAW encoding: Sony compressed ARW
- Precision represented in the corpus: 12-bit and 14-bit

| Private sample | Precision | ISO | Purpose noted during intake |
| --- | ---: | ---: | --- |
| `DSC00851.ARW` | 14-bit | 100 | Daylight detail and shallow depth of field |
| `DSC01166.ARW` | 12-bit | 2000 | Low-light and shadow detail |
| `DSC02382.ARW` | 14-bit | 100 | Bright highlight and cool white balance |
| `DSC03270.ARW` | 14-bit | 250 | Portrait orientation |
| `DSC03687.ARW` | 12-bit | 1000 | Bright subject against dark foliage |
| `DSC03821.ARW` | 14-bit | 200 | Warm backlight and highlight circles |

The corpus lives in `testdata/private/` and is ignored by Git. Matching
in-camera JPEGs remain optional references.

## Still open

- X11 behavior still needs a live validation pass; Wayland is validated.
- The eventual Rohditor project license.
