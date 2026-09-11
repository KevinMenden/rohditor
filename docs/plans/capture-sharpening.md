# Capture sharpening

Status: first useful version implemented, opt-in; 2026-09-11.
The implementation below is complete through application integration. Broader
qualification and hardware GPU validation remain follow-up work.

## Recommendation and alternatives

Start with **contrast-masked Richardson–Lucy (RL) deconvolution**, using a small
Gaussian point-spread function (PSF: the assumed shape of capture blur), a fixed
iteration budget, and an amount blend. Ship it opt-in until visual qualification.
The aim is modest recovery of detail softened by optics, the sensor filter and
demosaicing; severe defocus, motion blur and missing detail are outside scope.
Output sharpening after resizing remains a separate future feature.

| Approach | Strengths | Limitations / role for Rohditor |
| --- | --- | --- |
| Unsharp masking: add a scaled difference between the image and a blurred copy | Simple, fast, easy to threshold | Boosts edge contrast; readily amplifies noise and halos. Useful comparison baseline, or a smaller first release if RL proves too expensive. |
| RL deconvolution | Explicitly estimates an image before small-scale blur; radius has a physical interpretation | Iterative cost, noise amplification and ringing when the assumed blur is wrong. Recommended with masking and conservative limits. |
| Multiscale / wavelet detail enhancement | Independent control over fine and coarse detail | More controls and tuning; better suited to later texture/local-contrast work. |
| Edge-aware diffusion / inverse diffusion | Flexible deblurring with edge and noise controls | Substantially more mathematics, computation and tuning than this first feature needs. |

RawTherapee documents USM, RL and multiscale alternatives in its
[sharpening guide](https://rawpedia.rawtherapee.com/Sharpening). Its dedicated
[capture tool](https://rawpedia.rawtherapee.com/Capture_Sharpening) works immediately
after demosaicing in linear data, with contrast masking and iteration limits.
Darktable also provides early
[capture sharpening](https://darktable-org.github.io/dtdocs/en/module-reference/processing-modules/demosaic/);
its broader [diffusion tool](https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/diffuse/)
illustrates the more complex alternative. These are references, not a commitment
to reproduce either application's implementation.

## Processing contract

Implemented order:

```text
normalize → highlight reconstruction → demosaic → capture sharpening
          → optics → preview reduction → white balance / camera transform
          → exposure / rendering / creative edits → output
```

Process full-resolution camera-linear RGB before geometric resampling. Define
radius in source pixels, independent of zoom and export size. This preserves a
simple source-space blur model; it can amplify existing chromatic aberration,
so test strong TCA cases with optics enabled. Splitting optics to move TCA earlier
is deferred unless those tests demonstrate a need.

In `crates/core/src/pipeline/orchestration.rs`, fit preview already demosaics with
identity WB gains, applies optics, then reduces into `ReconstructedPreview`.
Full-resolution preparation currently applies WB during demosaic when optics is
off. For sharpening-enabled paths, make both feed the same camera-native stage
and apply WB afterwards. Preserve disabled-path results and verify enabled-path
consistency. Do not insert sharpening into the later, already reduced
`DemosaicedBase` adjustment pass.

Algorithm v1 uses the mean of positive camera channels as its intensity guide
(floor 1e-6), eight RL iterations, a normalized Gaussian truncated at ceil(3 sigma)
and half-sample mirrored borders. Estimates are bounded to 0.5–2 times the input
guide on every iteration; the final common RGB gain blends this ratio through
Amount and the protection mask. Camera RGB is not standard-space luminance.
Signed RGB and values above one are preserved; near-black or unreliable values
fall back to unchanged RGB.

Controls: Enabled (default Off), Amount 0–1 (default 0.5), Radius/sigma 0.3–1.2
source pixels (default 0.6), Noise protection 0–1 (default 0.5). Recipe schema 11
reads existing recipes with sharpening Off. The mask uses local high-pass
contrast, a scene-linear shadow fade and softened per-channel highlight
protection; Clip uses the same WB-dependent ceilings as highlight processing.

Use a smooth local-contrast mask with shadow/noise suppression, plus protection
around clipped highlights. Masking reduces noise amplification; it is not
denoising, which is also currently missing. Keep high-ISO use conservative.
If denoising is added, explicitly review its ordering with capture sharpening.

## Implementation plan

1. **Qualify the algorithm.** Add an isolated `core::sharpening` module with a
   cancellable CPU implementation, separable Gaussian convolution and reusable
   scratch buffers. Start experiments at sigma 0.6 source pixels and 8 iterations;
   these are provisional, not camera-calibrated defaults. Compare Off, small-radius
   USM and masked RL on identical source crops. Freeze the numerical contract and
   parameter ranges before exposing controls. No generic filter framework or new
   crate is needed initially.
2. **Integrate the recipe and pipeline.** Add validated `CaptureSharpening`
   settings in `rohditor-edit`, with Enabled, Amount, Radius (sigma) and Noise
   protection; keep iteration count algorithm-owned initially. Bump the current
   schema (10), default missing settings to Off through existing readers, and
   version the algorithm. Share processing across fit preview, Source 1:1 and
   8/16-bit export. Off and zero amount must bypass exactly. Include scratch
   memory in checked allocation estimates and report stage timing/provenance.
3. **Complete application integration.** Add settings and algorithm identity to
   desktop `ReconstructedCameraRgbKey` and core cached-base compatibility checks.
   Changes invalidate reconstructed and downstream results, retaining decoded
   RAW; ordinary creative edits reuse the sharpened base. Add desktop Detail
   controls, undo/redo/reset, sidecar round trips and CLI equivalents. Coalesce
   slider work, cancel superseded jobs and reject stale document/revision results.
   Initially GPU preview consumes the CPU-sharpened base, retaining CPU fallback;
   a WGSL sharpening implementation is a later, measured optimization.
4. **Validate before enabling by default.** Test asymmetric impulses/edges,
   constants, tiny images/borders, signed and HDR inputs, noise, cancellation,
   deterministic results and cache invalidation. Known-blur fixtures must show
   improved reconstruction error, with measured edge overshoot and flat-patch
   noise gain. Review real Sony RAW 100%/200% crops: foliage, hair, fabric/moire,
   skin, sky, high ISO, clipped highlights and lens corners. Verify preview
   reduction against the shared full-resolution stage and Source 1:1/export
   agreement. Set explicit accepted artifact limits from these comparisons.

Benchmark representative 24 MP and larger frames, recording latency, peak memory
and cancellation responsiveness separately from UI rendering. If full-frame cost
is excessive, investigate bounded tiling with iteration-aware halos and full-frame
equivalence tests before adding another persistent full-resolution cache.

Implementation acceptance requires `./scripts/check.sh`, the ignored private
full-resolution suite, and the ignored GPU suite for shared-base integration.
Record unavailable corpus/hardware checks; software Vulkan is not hardware GPU
qualification. Auto radius, corner compensation, motion deblur, output sharpening
and a general denoiser are deferred.

## First-release validation

- Normal `./scripts/check.sh` and the ignored private full-resolution suite pass.
  Tests cover exact bypass, borders/tiny images, signed/HDR data, deterministic
  threads, cancellation, invalid inputs, schema/sidecar round trips, cache
  invalidation, slider undo/redo/reset, preview reduction and 8/16-bit export.
- Known-blur fixture MSE: Off 0.0004567, USM 0.0002660, masked RL 0.0001755.
  Overshoot 0.00872 and undershoot 0.00128 in normalized linear units; the test
  limit is 0.025 for each. Flat-patch noise gain is 1.0, with a 1.01 limit.
  These are fixture-specific regression limits, not universal image-quality guarantees.
- All six private Sony A6400 files render through the capture stage and match
  the independently exercised full pipeline. The existing crop manifest produces
  Off/USM/RL comparisons at 100% and 200%; foliage, fur, rope and clipped-sun crops
  show conservative detail enhancement. Noise protection is not denoising.
- Isolated release measurements: 24 MP 741 ms, 48 MP 1490 ms; cancellation
  8.6/12.7 ms. Scratch is 549/1099 MiB; RGB plus scratch is 824/1648 MiB,
  before decoded RAW and other resident application buffers. Checked working-set
  limits remain enforced. No additional persistent full-resolution cache was added.
- The ignored GPU suite was run. Capture-source parity and stale-source rejection
  execute on llvmpipe; hardware-only cases skip because this environment has no
  exposed GPU device. Hardware parity/performance are **not qualified**.

Enable **Detail → Capture sharpening**, or use
`rohditor-cli develop input.ARW output.png --capture-sharpening`.
Optional flags: `--capture-amount`, `--capture-radius`,
`--capture-noise-protection`. Review results at Source 1:1.

Reproduce comparison crops by setting `ROHDITOR_CAPTURE_ARTIFACTS` to a new output
directory and running
`cargo test --release -p rohditor-core private_capture_comparison -- --ignored --nocapture`.
Run the isolated stage benchmark with
`cargo test --release -p rohditor-core benchmark_capture_sharpening -- --ignored --nocapture`.

Keep default Off. Remaining qualification includes real hardware GPU integration,
portraits/skin and human hair, more severe TCA/lens-corner cases, and a broader
noise/camera corpus. These do not block the opt-in first release.
