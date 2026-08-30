# Phase 6 preview caching and performance

Measurements were recorded on 2026-08-30 with release builds on the reference
workstation in [`development-environment.md`](development-environment.md): Ryzen
5 2600X, 62 GiB RAM, and RX 9070 XT using RADV/Vulkan. The representative input
was the 6048×4024 14-bit `DSC00851.ARW`; interactive output was 2560×1707.

## Scheduling, cancellation, and memory contract

The desktop accepts any number of slider events but retains at most one pending
preview plus one active preview. Replacing the pending value increments a
coalescing counter rather than allocating another channel message. A newer
revision cancels the active token and is the next revision selected. Tests send
1,000 revisions without draining the worker and observe one wake message, one
pending value, and revision 999 selected.

CPU preview caching is a cascading, one-document structure with explicit keys
for decoded RAW, normalized mosaic, demosaiced base, and adjusted output. One
scene-linear adjustment workspace is retained and overwritten. The private
cache test performs 24 distinct edits, verifies that normalization, demosaic,
and color conversion all report zero time on every hit, and verifies that the
logical retained-buffer total remains exactly constant after the first render.
This deterministic total is not process RSS: it includes image buffers retained
by the cache and may count the decoded `Arc` also owned by the document.

## Criterion stages

Command:

```console
cargo bench -p rohditor-core --bench pipeline_stages --locked -- --quick
```

The quick reference run used a 6048×4024 synthetic sensor for normalization and
2560×1703 typed buffers for the remaining stages.

| Stage | Criterion interval |
| --- | ---: |
| Normalize to 2560 long edge | 10.91–11.01 ms |
| Bilinear demosaic | 17.59–18.52 ms |
| Fused exposure/contrast/saturation | 3.75–3.84 ms |
| Rec.2020 to oriented sRGB8 | 26.06–27.07 ms |

Use the same command without `--quick` for a longer statistical run. Benchmark
fixtures are intentionally codec- and filesystem-independent so they measure
the named kernels rather than RAW decode or output I/O.

## Real RAW cached previews

CPU command:

```console
cargo test -p rohditor-desktop --release --locked \
  private_cached_cpu_preview_reports_bounded_memory_and_stage_skips \
  -- --ignored --nocapture
```

| CPU measurement | Result |
| --- | ---: |
| First developed preview after decode | 67.19 ms |
| 24 cached edits, median | 32.12 ms |
| 24 cached edits, maximum | 35.17 ms |
| Stable logical cache buffers | 175.6 MiB |
| Initial render peak estimate | 163.1 MiB |

The complete ignored workspace suite also runs the cache measurement alongside
the real desktop open/preview/export test. Under that deliberate CPU and memory
contention it measured 106.41 ms first, 33.96 ms cached median, and a 185.99 ms
cached maximum—still below the 250 ms target.

GPU command:

```console
cargo test -p rohditor-gpu --release --locked \
  private_arw_cached_gpu_adjustment_performance_is_reported \
  -- --ignored --nocapture
```

| RX 9070 XT / RADV measurement | Result |
| --- | ---: |
| 40 cached edits, queue-completion median | 0.159 ms |
| 40 cached edits, queue-completion maximum | 0.406 ms |
| Final CPU encode + submit | 0.044 ms |
| Resident source/working/display textures | 83.3 MiB |

When all four ignored GPU tests ran concurrently, the cached measurement was
0.167 ms median and 3.491 ms maximum. This contention run still remained well
below the 33 ms target while orientation parity and private-file parity ran on
the same adapter.

The shared eframe device reports `timestamp_queries=false`. Queue completion is
therefore a conservative wall latency: it includes work queued before the
callback and is not an isolated shader execution time. The benchmark waits only
to make that callback sample deterministic; the desktop never blocks on it.

A release Wayland/wgpu smoke run selected the same discrete Vulkan adapter and
reached a developed GPU preview about 285 ms after GPU application startup. Its
trace recorded a 39.8 ms CPU base, 28 ms half-float packing, and 0.400 ms first
encode/submit. The embedded placeholder precedes sensor decode but was not
timed independently in this run.

## Tuning decision

The existing 2560-pixel preview size remains appropriate: cached CPU and GPU
results are well below the 250 ms and 33 ms goals, and first preview latency is
well below two seconds. The GPU downstream stages remain fused in one dispatch.
CPU buffer lifetime changed to one reusable scene-linear workspace; cache and
GPU texture counts are fixed. Tiling would add complexity without improving the
measured interactive path, so it remains reserved for future full-resolution
GPU export or a higher-quality demosaic that changes the performance profile.
