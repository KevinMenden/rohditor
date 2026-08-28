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

The current workspace checks do not initialize Vulkan. GPU capability checks
belong to Phase 5.

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
| `DSC03270.ARW` | 14-bit | 200 | Portrait orientation |
| `DSC03687.ARW` | 12-bit | 1000 | Bright subject against dark foliage |
| `DSC03821.ARW` | 14-bit | 200 | Warm backlight and highlight circles |

The corpus lives in `testdata/private/` and is ignored by Git. Matching
in-camera JPEGs remain optional references.

## Still open

- Whether Wayland, X11, or both need first-class testing.
- The eventual Rohditor project license.
- Live `wgpu` format/limit validation, deferred until the GPU phase.

