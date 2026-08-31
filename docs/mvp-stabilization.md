# Phase 8 MVP stabilization

Phase 8 records the release gates for the first MVP. The target is the Sony
`ILCE-6400` private corpus on the reference Linux workstation, not arbitrary
untrusted camera files.

## Release commands

```console
./scripts/check.sh
cargo test --release --locked --workspace --tests -- --ignored --nocapture
cargo test --release --locked -p rohditor-gpu -- --ignored --nocapture
cargo build --release --locked -p rohditor-desktop -p rohditor-cli
```

The GPU suite takes a process-local lock around its opt-in tests. Concurrent
Vulkan device creation caused a RADV SIGSEGV on the reference RX 9070 XT; the
lock retains full test-runner parallelism for unrelated tests while serializing
only the hardware-device tests.

## Backend and corpus matrix

Recorded on 2026-08-31 on Plasma/KWin, RX 9070 XT with RADV/Vulkan:

| Path | Input/scope | Result |
| --- | --- | --- |
| Release CLI CPU | All six private ARWs | Decoded successfully: four 14-bit and two 12-bit Sony-compressed files; the rotated file retained its portrait result. |
| Release desktop CPU | All six private ARWs, glow/X11 | Each installed a developed CPU preview; five were 2560×1707 and the rotated file was 1707×2560. |
| Release desktop CPU | `DSC00851.ARW`, wgpu/Wayland | Developed CPU preview completed in 96.4 ms after decode. |
| Release desktop GPU | `DSC00851.ARW`, wgpu/Wayland | Used `AMD Radeon RX 9070 XT (RADV GFX1201)` through Vulkan and displayed a 2560×1707 direct GPU preview. |
| GPU parity | Synthetic orientations and `DSC00851.ARW` | Passed on the host GPU; CPU/GPU output tolerance remains at most two 8-bit sRGB codes per channel. |
| Automatic fallback | `DSC00851.ARW`, glow/Auto | Chose CPU and completed the developed preview. |
| Required unavailable GPU | glow/GPU | Opens an actionable in-window error and never schedules a CPU preview. |
| Corrupt input | Two redistributable ARW-named fixtures | Every decoder operation returns a typed error without an uncaught panic. |

The normal wgpu renderer is native Wayland. On Linux, the legacy glow fallback
uses XWayland when `DISPLAY` is available: eframe 0.33 glow did not reliably
wake for background worker repaint requests on the reference Wayland session.
That narrow workaround keeps the CPU fallback responsive without changing the
default GPU/Wayland path.

`rfd` is configured for XDG portals. The reference session had
`org.freedesktop.portal.Desktop` and KDE portal services active. A real user
should still perform one manual open/save selection after installation, because
portal appearance and permissions are desktop-user state rather than a
repeatable headless assertion.

## Diagnostics and malformed inputs

Use **Diagnostics → Save report…** (or start with `--diagnostics`) to save a
pretty JSON support report. It contains the Rohditor version, OS/architecture,
renderer and processor, adapter details, queue/cache state, stage timings,
memory estimates, and visible errors/fallback notes. It deliberately omits
source paths, RAW metadata, and all image pixels. Reports never overwrite an
existing path.

`testdata/synthetic/corrupt/` contains tiny text and truncated-TIFF inputs that
are safe to redistribute. Core and RAW allocation calculations use checked
dimension/stride/byte arithmetic, with decoder limits applied before full RAW
allocation. The review found no unchecked allocation conversion on the
file-provided image-dimension path.

## Release measurements

The 2026-08-31 release CLI export of `DSC00851.ARW` to a quality-90 JPEG was:

| Measurement | Result |
| --- | ---: |
| Developed output | 6000×4000 RGB8 JPEG, 3,058,035 bytes |
| CPU pipeline | 398.9 ms |
| JPEG encode and transactional commit | 660.5 ms |
| End-to-end elapsed time | 1.20 s |
| Peak process RSS | 474,916 KiB |
| Estimated CPU image-buffer peak | 413 MiB |
| Desktop release binary | 28 MiB |
| CLI release binary | 12 MiB |

The GPU cached-edit release measurement remained 0.163 ms median and 0.585 ms
maximum queue-completion time, with 83.3 MiB of resident source/working/display
textures. These are reference-machine measurements, not a cross-machine
performance guarantee.

## Installable desktop application

The stable application ID is `io.github.kevin.rohditor`; it matches the
Freedesktop desktop-file basename and icon name. This gives KWin/Wayland a
stable identity for grouping and icon lookup rather than eframe's fallback
identity.

```console
cargo build --release --locked -p rohditor-desktop
./scripts/install-desktop.sh
```

The installer copies the release binary, `.desktop` entry, and scalable SVG
icon under `~/.local` by default. Pass a different prefix as its first argument
for a package staging directory. Ensure `<prefix>/bin` is on `PATH` before
launching from the desktop menu.

## Known MVP limits

- Bilinear demosaic is a correctness reference and can show color artifacts at
  saturated edges.
- The D65 camera matrix is selected without dual-illuminant interpolation.
- The neutral rendering has no Sony Creative Look, camera tone curve, highlight
  roll-off, lens correction, noise reduction, sharpening, or monitor ICC
  conversion.
- Outputs are sRGB; wide-gamut editing/display soft proofing is post-MVP work.
- Full-resolution export remains CPU-only and can use substantial RAM.
