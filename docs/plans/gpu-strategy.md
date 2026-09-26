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
| Base rendering and output gamut | Standard/Neutral base rendering and default hard sRGB clipping run on CPU and GPU. Opt-in Chroma Compress is wired through recipe, cache, preview and export. | The RX 9070 XT full-resolution 16-bit Chroma Compress test exceeded its 16-code maximum (18–24 observed). Diagnose the output mismatch, then complete visual review and qualify the Standard look with the chosen gamut policy. |
| Full-resolution export | Shared color/output kernels, headless execution, 8/16-bit quantization and bounded readback; CPU encoding and auto fallback. | RX 9070 XT parity and visual review for full exports; transfer-inclusive throughput, measured physical memory, concurrent-preview latency and failure recovery. |
| Capture sharpening | Bounded f32 GPU tiles with the CPU algorithm as reference; the old full-camera-RGB readback bridge was removed by resident spatial processing. A historical RX 9070 XT bridge test passed camera parity but averaged 1.560 s versus 784.7 ms for a matching CPU fixture. | Re-measure the *current resident path* with Capture On; check visual usefulness on real photos, memory, cancellation and recovery. The old bridge timings do not measure today's path. |
| Optics, reduction and Source 1:1 | Resident planar camera RGB; CPU Lensfun plan, GPU vignetting/remap/exact reduction; GPU fit and 1:1 textures; bounded export bands. Software Vulkan fixtures and a 24 MP Sony comparison passed their documented gates. | RX 9070 XT 24/48 MP optics and Capture Off/On parity, saved corner/worst-difference crops, actual memory, transfer-inclusive timing and editor interaction. Software Source 1:1 had an isolated 26-code maximum difference at high-contrast cubic samples (144 channel samples above three codes of 72 million); inspect hardware and real-image worst crops rather than relying on an average. |
| GPU sensor: normalization, Off/Clip, bilinear/MHC | Shared sensor contract and bounded u16 upload, f32 mosaic, resident camera planes; connected to fit, 1:1 and export with CPU recovery. Software fixtures cover crop/CFA, signed/HDR values, boundaries and lifecycle; RX 9070 XT MHC camera parity passed. | Two synthetic normalization/Off tests failed their 1e-6 absolute gate at an HDR value near 72 on the RX 9070 XT. Resolve the numerical contract; still measure broader corpus, memory, interaction and recovery. |
| GPU RCD | Shared 194/10/174 tile geometry; one 978,536-byte scratch tile and nine ordered passes; resident capture handoff; software stage/seam/corpus tests passed. Off/Clip RCD is already eligible for `auto`. | RX 9070 XT full-resolution camera-RGB and 8-bit export parity tests **failed**. Its speed advantage is not acceptance. Diagnose stage divergence and re-run corpus, visual, 24/48 MP memory, interaction and recovery gates before normal auto selection. |
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

### First RX 9070 XT measurements, 2026-09-26

Release CLI, two 6000×4000 Sony RAWs, Clip, 8-bit PNG, optics Off, two warmups
and five measured fresh-process runs per case. Median complete export time,
including decode, device creation, processing, readback, encoding and commit:

| Algorithm | Capture | CPU range | GPU range | Result |
| --- | --- | --- | --- | --- |
| MHC | Off | 0.86–0.91 s | 0.60–0.65 s | GPU about 1.4× faster |
| MHC | On | 1.64 s | 2.01–2.06 s | GPU about 20–25% slower |
| RCD | Off | 2.88–2.92 s | 1.01–1.06 s | GPU about 2.8× faster, parity failed |
| RCD | On | 3.60–3.64 s | 2.66–2.82 s | GPU about 1.3× faster, parity failed |

A further pass with precise process completion timing on `DSC00851.ARW` gave
MHC 0.84/0.58 s Off and 1.66/2.04 s On (CPU/GPU), and RCD 2.75/0.95 s Off
and 3.72/2.71 s On. Capture added 0.81/1.46 s to MHC and 0.97/1.76 s to
RCD (CPU/GPU). On the GPU, 96 capture tiles spent 1.42 s (MHC) or 1.69 s
(RCD) in compute/wait, with zero camera-RGB upload/readback; the capture
penalty is not the old transfer bridge. Full exports reported 1,124 MHC Capture
On submissions or 2,025 RCD Capture On submissions. These are export timings,
not resident editor slider-to-display measurements. The 24 MP RCD Capture On
case sampled about 698 MiB additional system-wide VRAM; no 48 MP case was
available. Full sample data are produced by `scripts/benchmark-gpu.py`.

With automatic Lensfun corrections on `DSC00851.ARW`, three further measured
MHC runs gave 2.28/1.16 s Capture Off and 3.02/2.48 s Capture On (CPU/GPU).
GPU optics gains therefore outweighed its slower capture stage in this complete
export. Capture itself still added 0.74 s on CPU versus 1.32 s on GPU.
The existing retained-source color timing test measured 0.332 ms median GPU
queue completion at 2560×1707; it excludes sensor work and screen presentation.

The ignored GPU suite selected the RX 9070 XT (RADV/Mesa 26.2.2): 39 passed,
7 failed. Failures include RCD camera/output parity, 16-bit Chroma Compress
export, and two strict synthetic normalization/Off HDR tolerances. MHC camera
parity and the spatial suite passed. Do not promote RCD or call the GPU path
fully qualified from these performance numbers.

## Strategy and implementation concerns

1. **Automatic selection is ahead of qualification.** `GpuSensorProcessor::supports`
   admits Off/Clip RCD and the desktop/export paths attempt it under automatic
   selection, although RX 9070 XT camera/output parity now fails. Repair and
   qualify representative workloads before treating that selection as accepted.
2. **Memory pressure can change the backend.** The 768 MiB reservation is shared
   by preview and export, but it estimates tracked resources rather than actual
   driver/host peaks. RCD Capture On can retain two full camera images, and 48 MP
   exceeds the budget by construction. Record peak reservations *and* physical
   memory under concurrent preview/export; consider bounded capture handoff or
   measured batching only where these results justify it. Keep the fallback
   visible and preserve image semantics.
3. **Capture is the measured latency target.** At 24 MP its 96 GPU tiles add
   roughly 1.3–1.8 s to export, almost entirely reported as compute/wait with
   no camera-RGB transfer. Use GPU timestamps or controlled batching to separate
   shader work from submission/synchronization cost; preserve the eight-iteration
   halo contract and parity before changing tile scheduling. RCD also waits per
   fixed tile, but its measured complete export is currently faster than CPU.
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
