# Phase 9 image-quality foundation

Phase 9 replaces the MVP's reconstruction baseline and establishes a repeatable
way to judge RAW image quality. Demosaicing is the first implementation target,
but it is not treated as the only possible cause of a disappointing image.
Sensor decoding, preview reduction, color calibration, scene-to-display tone,
sharpening, noise handling, lens correction, and display color management all
affect the result at different stages.

## Current-state diagnosis

The current full-resolution path is technically coherent: `rawler` decodes the
Sony mosaic and metadata, Rohditor applies the recommended crop, per-phase
black-level and white-level normalization, as-shot white balance, bilinear
Bayer demosaic, a camera-matrix transform into linear Rec.2020/D65, global
edits, and an sRGB output transform. Normalized values below zero and above one
remain available until output.

Four current choices materially limit perceived quality:

1. Bilinear interpolation blurs fine chroma detail and can produce zippering,
   false color, or colored edges around high-frequency structure.
2. The 6000x4000 A6400 crop is point-selected to 2560x1707 before the desktop
   demosaic. This retains about 18 percent of the source sample count without an
   antialiasing low-pass filter. It can alias or discard detail independently of
   the chosen demosaic.
3. The viewport's current `100%` command means one preview-texture pixel per
   screen pixel. It is not yet a one-sensor-pixel inspection mode and therefore
   cannot be used to judge demosaic quality honestly.
4. The neutral rendering has no camera-style base curve, highlight roll-off,
   gamut compression, capture sharpening, denoising, or lens correction. Its
   hard-clipped scene-linear rendering will look flatter and less polished than
   Sony's embedded JPEG even when reconstruction is correct.

A comparison of `DSC00851.ARW` with its embedded 1616x1080 JPEG confirmed a
large difference in contrast, saturation, highlight handling, and apparent
sharpening. That comparison is useful for diagnosing the overall look, but the
embedded JPEG is processed and too small to serve as demosaic ground truth.

## Algorithm decision

Implement Malvar-He-Cutler (MHC) as Rohditor's first high-quality demosaic and
make it the default after the quality gates pass. Keep bilinear as a selectable
fast/reference implementation.

MHC is the right next step because it is specified by a compact published set
of 5x5 linear filters, is straightforward to test for every Bayer phase, maps
cleanly to row-parallel CPU work and a later GPU kernel, and was reported by its
authors to improve PSNR by more than 5.5 dB over bilinear. It is not presented
as the final word in demosaicing. Modern processors commonly prefer algorithms
such as AMaZE, RCD, or noise-oriented LMMSE for particular images, but their
implementations and tuning are substantially more complex. Available AMaZE and
RCD reference code is also commonly GPL-licensed, while Rohditor's own license
has not been selected.

The MHC implementation must be written from the paper's filter definition and
Rohditor-owned tests. Do not copy an incompatible reference implementation.

Primary references:

- [Malvar, He, and Cutler, High-quality linear interpolation for demosaicing of Bayer-patterned color images](https://www.microsoft.com/en-us/research/publication/high-quality-linear-interpolation-for-demosaicing-of-bayer-patterned-color-images/)
- [Hirakawa and Parks, Adaptive homogeneity-directed demosaicing algorithm](https://pubmed.ncbi.nlm.nih.gov/15762333/)
- [RawTherapee demosaicing documentation](https://rawpedia.rawtherapee.com/Demosaicing)
- [Sony ILCE-6400 file-format guide](https://helpguide.sony.net/ilc/1810/v1/en/contents/TP0002279217.html)

## Implementation sequence

### 9A. Establish a quality baseline

1. Add a repeatable development command that emits named 100% and 200% crops
   from the private corpus using a fixed neutral recipe. Record the source
   identity, crop coordinates, algorithm, pipeline version, and timings without
   committing private pixels.
2. Classify private examples by what they test: low-ISO fine detail, diagonal
   and curved edges, repeating texture or moire risk, saturated boundaries,
   highlight clipping, high ISO, foliage, and skin tones. Capture one missing
   scene if the current six files do not cover a category.
3. Add generated linear-RGB fixtures such as slanted edges, zone plates,
   one-pixel lines, saturated color boundaries, smooth gradients, and noise.
   Mosaic them through all four Bayer layouts so the original RGB image remains
   known ground truth and redistributable.
4. Record RGB PSNR and channel error for bilinear on those fixtures. Use
   external RawTherapee AMaZE/RCD renders as a visual ceiling when available,
   not as byte-exact goldens. Use the camera JPEG only to compare overall look.
5. Cross-check at least one 12-bit and one 14-bit decoded sensor mosaic against
   an independent decoder such as LibRaw before attributing compression or
   unpacking artifacts to the demosaic stage.

### 9B. Add the MHC CPU reference

1. Move demosaic-specific code out of the broad `cpu.rs` module into:

   ```text
   crates/core/src/demosaic/
       mod.rs
       bilinear.rs
       malvar_he_cutler.rs
   ```

   Shared allocation, cancellation, image-state, and validation contracts stay
   in the core crate. Algorithm modules do not know about RAW decoding, UI,
   caches, or export codecs.
2. Add `DemosaicAlgorithm::MalvarHeCutler`. Use `mhc` as the stable CLI value
   and retain `bilinear` for comparison and recovery.
3. Evaluate the published 5x5 kernels in normalized camera RGB. Preserve the
   measured CFA component exactly at every interior sensor site. Apply
   per-channel white-balance gains after channel reconstruction, with no
   clipping of negative or over-range linear values.
4. Declare a two-pixel neighborhood halo. Use a documented deterministic border
   policy: bilinear reconstruction for the outer two rows and columns during
   the first implementation. A later mirrored-kernel border may replace it only
   with tests showing an improvement.
5. Parallelize independent rows with Rayon and retain cooperative cancellation
   at row boundaries. Start with readable scalar `f32`; benchmark before SIMD,
   unsafe code, or hand-tuned cache blocking.
6. Add exact coefficient/impulse tests, constant and affine-field tests,
   observed-site preservation, all four Bayer layouts, border dimensions,
   unclipped highlights, non-finite rejection, cancellation, and identical
   one-thread/multi-thread output.
7. Extend the existing Criterion demosaic benchmark with bilinear and MHC at
   preview and full A6400 dimensions.

### 9C. Make the desktop preview quality representative

1. Stop using phase-preserving point selection as the settled developed
   preview. Keep the embedded JPEG as the immediate loading placeholder.
2. For the first correct implementation, normalize and MHC-demosaic the full
   recommended crop, then reduce camera-linear RGB to the 2560-edge preview with
   a separable antialiased area filter. White balance and the camera matrix are
   linear and can be applied after this reduction. Discard the transient
   full-resolution RGB buffer after the reduced camera-RGB base is cached.
3. Keep the reduced, unbalanced camera-RGB result as its own cache stage. Its
   key is source identity, crop, demosaic algorithm, and preview dimensions.
   White-balance and downstream edits must not repeat demosaic or resampling.
4. Do not regenerate that base when the window is resized. The existing fixed
   2560-edge target remains the fit-preview source until measurements justify a
   change.
5. Make `100%` mean one developed source pixel per screen pixel. Initially this
   may request a background full-resolution developed texture only when needed;
   a visible-region tiled path can replace it if retained memory or upload time
   exceeds the Phase 9 budget. Until true 1:1 is ready, label the existing mode
   as preview-pixel 100% rather than implying sensor scale.
6. Report `embedded`, `fast`, `high-quality`, or `1:1` source state in the
   viewport/diagnostics. Never leave a point-sampled provisional preview labeled
   as the final developed result.
7. Preserve the existing direct egui-wgpu texture path. Phase 9 changes how the
   CPU camera-RGB base is reconstructed; it does not add a GPU readback or make
   GPU processing mandatory.

The simple full-frame implementation is preferred first because the target is
a 24 MP A6400 and the workstation has ample RAM. If measured transient memory
or latency misses the budget, optimize the same result with a tile/halo-aware
demosaic feeding the resampler; do not fall back to unfiltered point selection.

### 9D. Audit the rest of the RAW pipeline

The following checks are part of Phase 9 diagnosis. Findings that require new
processing tools become explicit follow-up phases rather than silent tweaks to
MHC:

- **Decode and sensor calibration:** verify 12/14-bit scaling, Sony compressed
  ARW behavior, crop/CFA parity, row stride, black-level stability, white level,
  optical-black information, hot/dead pixels, and any Sony PDAF correction
  requirement. Any artifacts already present in the camera's compressed RAW
  samples are input limitations and must not be misreported as demosaic defects.
- **White balance and input color:** validate as-shot gains and DNG matrix
  direction against an independent processor; measure the current fixed-D65
  choice on daylight, tungsten, and skin-tone images. Plan dual-illuminant
  interpolation or a camera profile if the errors are material.
- **Scene-to-display rendering:** measure clipped channel counts and compare a
  neutral exposure with a gentle filmic/base curve. Highlight roll-off,
  reconstruction, and gamut compression should follow reconstruction quality
  because they explain harsh highlights and flat appearance, not zippering.
- **Detail and noise:** evaluate false color, chroma noise, and acuity before
  adding capture sharpening. Add denoising and bad-pixel/PDAF cleanup ahead of
  sharpening when high-ISO artifacts demand it.
- **Optics and display:** separate lateral chromatic aberration, distortion,
  vignetting, and monitor-ICC differences from demosaic errors. Lens correction
  and color-managed display remain later stages with their own profiles and
  tests.

### 9E. Integrate and change the default

1. Add `mhc` to the CLI and diagnostics. The desktop does not need a prominent
   normal-user algorithm control; a diagnostics/developer selection is enough
   while both paths are being compared.
2. Include the algorithm and preview-reconstruction policy in cache keys,
   traces, diagnostic reports, and private golden metadata.
3. Run both algorithms through CPU preview, CPU export, GPU-adjusted preview,
   cancellation, cache invalidation, and all six private files. GPU adjustments
   continue to consume the CPU-created linear base.
4. Change `RenderOptions::default()` and CLI development default to MHC only
   after every acceptance gate passes. Update hashes and measurements as an
   intentional pipeline-version change.
5. Document visible differences and retain bilinear for troubleshooting and
   fast synthetic tests.

## Acceptance gates

Phase 9 is complete only when:

- MHC is independently unit-tested for every Bayer phase and is the default for
  desktop previews and full-resolution export.
- On the generated ground-truth suite, aggregate RGB PSNR improves by at least
  3 dB over bilinear, no fixture regresses by more than 0.25 dB without a
  documented artifact tradeoff, and measured CFA samples remain unchanged.
- Reviewed 100%/200% private crops show less false color and zippering on fine
  detail and saturated edges. Low-ISO and high-ISO images are reviewed
  separately.
- The settled fit preview is antialiased and no longer uses point selection.
  The viewport offers honest source-scale 1:1 inspection or clearly labels a
  temporary preview-pixel mode.
- A 12-bit and 14-bit mosaic cross-check finds no material decoder scaling,
  crop, or CFA discrepancy.
- The initial high-quality developed preview appears within two seconds after
  decode on the reference workstation, cached downstream GPU adjustments remain
  within the existing 33 ms budget, and CPU adjustments remain within 250 ms.
- Full-resolution MHC demosaic is no slower than twice the recorded bilinear
  demosaic median unless the visual review explicitly accepts the tradeoff.
- Transient memory for a 6000x4000 A6400 image remains below 600 MiB. If it does
  not, tile the MHC/resample path before making it the desktop default.
- CPU fallback, cancellation, newest-wins scheduling, export snapshot behavior,
  and direct GPU texture display retain their existing tests.

## Document boundary

This document covers only the Phase 9 image-quality effort. Tone mapping,
dual-illuminant profiles, sensor cleanup, denoising, sharpening, lens profiles,
monitor ICC support, and any later demosaic algorithm should each receive their
own focused document when selected. They are deliberately not a backlog here.
