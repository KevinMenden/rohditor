# GPU processing strategy

Status: consolidated on 2026-09-26 from the GPU implementation plans and the
current working tree. All five migrations have at least partial code, but supported-hardware and
real-photo qualification is incomplete. "Implemented" below means a path is
connected and has stated test evidence; it does not mean every workload should
prefer that path. The CPU pipeline remains the reference and recovery path.

## Execution today

| Stage | CPU ownership | GPU ownership when selected |
| --- | --- | --- |
| Input and planning | RAW decode into immutable u16 samples; metadata, crop, CFA/level, WB and recipe validation; Lensfun matching and optics-plan resolution | None |
| Sensor development | Packs bounded RAW upload tiles. Performs normalization, selected highlight method and demosaic on CPU fallback | Normalizes into f32 mosaic; Off/Clip highlight handling; bilinear, MHC or RCD demosaic into resident camera RGB |
| Camera RGB | CPU reference and fallback for capture sharpening and optics | Optional bounded capture sharpening; vignetting, distortion and TCA with cubic remap |
| Preview | Orchestration, cache identity, cancellation and stale-result checks; small picker/histogram readbacks when needed | Exact area reduction for fit; full-resolution Source 1:1; shared color/rendering, gamut and display transform into native display textures |
| Export | JPEG/PNG encoding, metadata and transactional output write | Full-resolution spatial/color/output processing; bounded 8/16-bit integer bands read back for encoding |

The implemented resident path is:

```text
CPU decode -> bounded u16 upload -> GPU normalize -> GPU Off/Clip
  -> GPU bilinear/MHC/RCD -> GPU capture (if enabled) -> GPU optics
  -> GPU color/output
```

Fit preview adds exact area reduction; Source
1:1 renders full-resolution display bands; export reads back final integer
bands. Successful processing does not read a full mosaic or camera-RGB image
back to the CPU. The retained camera source is f32; the older RGBA16F source
lost near-neutral color differences and was replaced with RGBA32F. The working
color target can still be RGBA16F, while display conversion uses the f32 shader
result. Preview and headless export share processing contracts and kernels but
use separate devices and resident copies. GPU UI rendering alone does not imply
GPU image development.

Selection changes the boundary:

| Selection or condition | Preview / Source 1:1 | Export |
| --- | --- | --- |
| Off or Clip with bilinear, MHC or RCD and sufficient resources | GPU sensor, spatial and color | GPU sensor, spatial and color |
| Local Ratios or Opposed highlights, AMaZE demosaic, or sensor failure | CPU sensor, then GPU spatial/color when that path succeeds; report the sensor fallback | Current GPU call fails; desktop and CLI `auto` retry the complete CPU export |
| Explicit CPU choice or unavailable/failed GPU | Complete CPU pipeline | Complete CPU pipeline |
| CLI `--processor gpu` fails | Not applicable | Error before encoding a CPU replacement |

Desktop GPU preview failure under `auto` also selects the complete CPU path.
Cancellation and superseded work must not publish an old frame. CPU sensor
fallback can upload camera RGB once and still keep capture, optics and color
resident. The desktop status distinguishes full GPU, CPU sensor plus GPU
spatial/color, and complete CPU execution. A GPU export worker uses a separate
headless hardware Vulkan device; a software CPU adapter is refused there.

WB is not always a downstream-only edit: Clip ceilings depend on it. A settled
preview, Source 1:1 and export therefore need new sensor provenance. An HSL or
grading edit can reuse the resident sensor and spatial source. Cache keys must
include every pixel-producing setting, algorithm version and source identity.
RAW pixels remain immutable; GPU stages advance typed states only after complete
success. Source 1:1 and fit results are published only for the current ticket.

## What landed, and what is still open

| Migration | Implemented and evidenced | Open qualification or work |
| --- | --- | --- |
| HSL and three-way grading | Shared fused color pass; 33 controls retain the GPU source. RX 9070 XT shader/corpus parity was recorded on six Sony files, with at most one output code difference. | Measure actual slider-to-display and rapid-edit latency against a neutral baseline; review a representative portrait/skin-tone RAW. The reported sub-millisecond resident GPU timings exclude source preparation and presentation. |
| Base rendering and output gamut | Standard/Neutral base rendering and default hard sRGB clipping run on CPU and GPU. Opt-in Chroma Compress is wired through recipe, cache, preview and export. | Finish private-corpus visual review and hardware GPU parity for Chroma Compress; qualify the Standard look with the chosen gamut policy before changing defaults. |
| Full-resolution export | Shared color/output kernels, headless execution, 8/16-bit quantization and bounded readback; CPU encoding and auto fallback. | RX 9070 XT parity and visual review for full exports; transfer-inclusive throughput, measured physical memory, concurrent-preview latency and failure recovery. |
| Capture sharpening | Bounded f32 GPU tiles with the CPU algorithm as reference; the old full-camera-RGB readback bridge was removed by resident spatial processing. A historical RX 9070 XT bridge test passed camera parity but averaged 1.560 s versus 784.7 ms for a matching CPU fixture. | Re-measure the *current resident path* with Capture On; check visual usefulness on real photos, memory, cancellation and recovery. The old bridge timings do not measure today's path. |
| Optics, reduction and Source 1:1 | Resident planar camera RGB; CPU Lensfun plan, GPU vignetting/remap/exact reduction; GPU fit and 1:1 textures; bounded export bands. Software Vulkan fixtures and a 24 MP Sony comparison passed their documented gates. | RX 9070 XT 24/48 MP optics and Capture Off/On parity, saved corner/worst-difference crops, actual memory, transfer-inclusive timing and editor interaction. Software Source 1:1 had an isolated 26-code maximum difference at high-contrast cubic samples (144 channel samples above three codes of 72 million); inspect hardware and real-image worst crops rather than relying on an average. |
| GPU sensor: normalization, Off/Clip, bilinear/MHC | Shared sensor contract and bounded u16 upload, f32 mosaic, resident camera planes; connected to fit, 1:1 and export with CPU recovery. Software fixtures cover crop/CFA, signed/HDR values, boundaries and lifecycle. | RX 9070 XT corpus, physical-memory, latency and recovery gates against the CPU-sensor/GPU-spatial baseline. |
| GPU RCD | Shared 194/10/174 tile geometry; one 978,536-byte scratch tile and nine ordered passes; resident capture handoff; software stage/seam/corpus tests and one 6000×4000 Sony full-resolution camera-RGB comparison passed. Off/Clip RCD is already eligible for `auto`. | Broader real-photo review, RX 9070 XT 24/48 MP transfer-inclusive speed, physical memory, interaction and recovery. The software run used 902 submissions and 3.893 s for RCD after normalization/Clip; these are *llvmpipe* numbers, not discrete-GPU performance. Decide normal auto selection from hardware results. |
| Remaining sensor methods | CPU Local Ratios/Opposed and AMaZE remain selectable through fallback. | Port Local Ratios and Opposed with exact diagnostic/decision parity, then AMaZE with its CPU tile/border semantics, or explicitly revise the desired GPU coverage and keep the fallback visible. |

The RCD software evidence used `llvmpipe (device_type: Cpu)` without `/dev/dri`.
It proves shader execution and the tested numerical contracts, not RX 9070 XT
quality or speed. Its one reviewed scene omitted enough foliage, hair/fabric,
noise and lens-corner variety to close the visual gate. The recorded 24 MP RCD
logical demosaic peak was about 385 MB; this is a reservation estimate, not a
physical VRAM measurement. For 48 MP RCD with Capture On, the mosaic and two
resident RGB images alone require about 1.344 GB, above the 768 MiB default
reservation before padding and scratch, so that combination currently needs
CPU recovery or a separately qualified memory/streaming design.

## Strategy and implementation concerns

1. **Automatic selection is ahead of qualification.** `GpuSensorProcessor::supports`
   admits Off/Clip RCD and the desktop/export paths attempt it under automatic
   selection. There is no recorded RX 9070 XT end-to-end acceptance for RCD or
   the new sensor path. Qualify representative workloads and explicitly decide
   whether to keep that selection; successful software Vulkan tests alone are
   insufficient.
2. **Memory pressure can change the backend.** The 768 MiB reservation is shared
   by preview and export, but it estimates tracked resources rather than actual
   driver/host peaks. RCD Capture On can retain two full camera images, and 48 MP
   exceeds the budget by construction. Record peak reservations *and* physical
   memory under concurrent preview/export; consider bounded capture handoff or
   measured batching only where these results justify it. Keep the fallback
   visible and preserve image semantics.
3. **Too many RCD submissions may dominate latency.** The faithful fixed-tile
   port deliberately waits once per tile. Compare complete CPU-sensor/GPU-spatial
   and GPU-sensor paths on hardware before batching or fusing passes, then
   recheck stage parity and cancellation. Do not infer a win from shader time.
4. **Fallback has two granularities.** Preview can retain GPU spatial/color
   after CPU sensor recovery, whereas export currently retries the entire CPU
   pipeline. This is valid but should stay explicit in status, logs and timing;
   any future mixed export path needs its own provenance and memory tests.
5. **Existing image-quality issues are separate from backend parity.** The open
   issues report weak visible capture sharpening, suspect Local Ratios/Opposed,
   and sometimes low-quality RCD/AMaZE. CPU/GPU pixel parity would reproduce
   those results. Assess the CPU algorithm and representative RAWs separately
   before treating a GPU port as a quality improvement.

## Next work and acceptance

1. Record a supported-hardware baseline for the existing CPU-sensor/GPU-spatial
   path, including HSL/skin and export/capture/optics open items. Identify the
   adapter with `adapter.get_info()`; retain matched CPU/GPU fit, 1:1 and export
   crops. Cover corrected 24 MP and representative 48 MP RAWs, Capture Off/On,
   optics corners, fine texture, skin, noise and clipped highlights.
2. Qualify GPU Off/Clip with bilinear, MHC and RCD *separately* against both the
   CPU reference and the baseline. Record cold/warm first-open, repeated edits,
   Source 1:1, export, upload/readback bytes, submission count, p50/p95 preview
   latency during export, cancellation, device loss, recovery, reservation peaks
   and measured host/device memory. Resolve RCD auto selection from those results.
3. Port and qualify Local Ratios/Opposed, then AMaZE, only if preserving the CPU
   contracts and bounded resource use. Highlight ports need exact accept/fallback
   decisions and diagnostics; RCD/AMaZE need stage, border, partial-tile and
   measured-site parity. Keep unsupported selections on an observable CPU path.

For each changed implementation slice, run `./scripts/check.sh`, the ignored
workspace private suite, and the ignored GPU suite. Software adapters establish
deterministic correctness only. A feature is accepted on `auto` when matched
real-image output, hardware execution, transfer-inclusive latency, memory,
interaction and recovery have been reported for its supported workloads.

This document owns GPU execution strategy and status. Algorithm semantics for
capture sharpening and highlight reconstruction remain in
[capture sharpening](capture-sharpening.md) and
[clipping and reconstruction](clipping-and-reconstruction.md).
