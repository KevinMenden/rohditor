# Demosaic reference testing

Goal: verify that Rohditor's demosaic algorithms are as good as the ones in
RawTherapee and darktable, using the same input mosaic and measuring the result
numerically instead of by eye.

Motivation (see `docs/open_issues.md`): AMaZE and RCD look worse than
RawTherapee's output on real images.

## Why not compare final images

The tools differ in white balance, camera matrix, tone curve, highlight
handling and default sharpening. A comparison of final JPEGs would mostly
measure those. The test must isolate the demosaic step.

## Hypotheses to test (one experiment at a time)

1. **White balance order.** `demosaic()` applies WB gains after reconstruction
   (`crates/demosaic/src/lib.rs`). RawTherapee and darktable, as far as we
   know, scale before demosaicing. AMaZE/RCD are gradient/ratio based, and
   AMaZE has clip thresholds (`CLIP_POINT = 1.0`), so unbalanced data may skew
   direction decisions. Test: pre-scale the mosaic, demosaic, divide out.
2. Divergences in the AMaZE port (re-tiled adaptation of `amaze_demosaic_RT.cc`)
   and RCD port.
3. RawTherapee's post-demosaic false-colour suppression.
4. Clipping behaviour near saturation.
5. MHC being the default algorithm. Neither tool implements MHC, so it can
   only be scored against ground truth, not against a reference tool.

## Levels

### Level 1: synthetic input with known ground truth (main test)

- Take a real Bayer file from `testdata/private`, bin each 2x2 RGGB quad into one
  full-RGB pixel (half resolution, real texture and noise, no external data).
  This is the ground truth.
- Re-mosaic it into a clean Bayer pattern and write it as a linear DNG: black
  level 0, fixed white level, identity as-shot white balance, identity colour
  matrix.
- Run RawTherapee, darktable and Rohditor on that DNG; score each against
  the ground truth (PSNR, max error, delta E, edge-region error, zipper and
  false-colour measures).
- Sanity check first: at measured CFA sites each tool's output must equal the
  input. If not, the harness configuration is wrong, not the algorithm.

### Level 2: real ARW, demosaic only

- Minimal RawTherapee pp3 (neutral WB, no highlight recovery, sharpening, CA,
  denoise; linear 16-bit TIFF) and minimal darktable XMP (rawprepare,
  temperature, demosaic, linear output; float TIFF).
- Compare Rohditor vs RawTherapee, darktable vs RawTherapee, interior only
  (exclude a 16 px border; border policies differ).
- If RawTherapee AMaZE ~ darktable AMaZE, the reference is trustworthy.
- Write amplified difference images to `testdata/generated/reference/`.

### Level 3: end-to-end look (deferred)

Neutral rendering vs RawTherapee neutral with a perceptual tolerance. Only
after Levels 1 and 2 are clean.

## Code layout

New dev-only crate `crates/reference` (`publish = false`, nothing depends on it;
`scripts/check.sh` asserts that):

- `dng`: minimal DNG writer for a mosaic.
- `external`: runs the AppImages. Paths come from `ROHDITOR_RAWTHERAPEE` and
  `ROHDITOR_DARKTABLE`; tests skip if unset. Profile templates live in
  `testdata/reference/`.
- `metrics`: PSNR, delta E, max error, region masks, zipper/false-colour.
- `tiff`: reader for tool outputs (dev-only `tiff` dependency).
- Tests needing external tools are `#[ignore]`. Pure-Rust parts (DNG writer,
  metrics) run in normal CI.
- Metric baselines are committed as JSON (not images).

## Expectations

- Different algorithms will not match each other. The same algorithm on the
  same mosaic from a faithful port should agree within ~1e-3 relative error;
  RawTherapee's 16-bit output sets a floor of 1/65535.
- Anything beyond that is a finding. Borders are excluded.

## Order of work

1. **Spike (go/no-go):** both AppImages run headless; minimal profiles really
   disable other steps; DNG round-trips with identity.
2. **Level 1:** crate, DNG writer, metrics, table of algorithm x tool.
3. **Level 2:** real-ARW comparison and difference images.
4. **Diagnose** hypotheses 1-4, fix, lock in baselines.
5. Check GPU demosaics against the CPU reference with the existing GPU suite.

## Decisions

- RawTherapee is the primary reference, darktable the cross-check.
- A dev-only `tiff` dependency is acceptable.
- First targets: AMaZE and RCD. MHC and bilinear are scored against ground
  truth only.

## Spike and Level 1 results

Status: white balance before demosaicing is implemented (see "Fix: channel
balance" below). Remaining work is listed under "Next steps".

Run with:

```
ROHDITOR_RAWTHERAPEE=~/Applications/RawTherapee_5.13_x86_64_release.AppImage \
ROHDITOR_DARKTABLE=~/Applications/Darktable-5.6.1-x86_64.AppImage \
cargo test --release -p rohditor-reference --test level1 -- --ignored --nocapture
```

`level1_real_images` runs the six private ARWs at two noise levels (2x2
binned, and 4x4 binned for roughly half the noise); `level1_clean_fixtures`
runs noise-free analytic images (zone plate, slanted edge, disc). Scores are
PSNR over all pixels, the 10% strongest-gradient "edge" pixels, and the rest
("flat").

### Harness notes

- Both tools run headless (~0.2 s RawTherapee, ~1.7 s darktable per
  1024x768). The CLIs live inside the AppImages, so the harness extracts them
  once to `testdata/generated/reference/tools/`.
- RawTherapee 5.13 has no linear 16-bit output preset. We use the sRGB output
  profile with an sRGB working space and invert the curve (exact up to
  quantization). It crops 4 px per side; `interior_window` aligns it. Its
  `.pp3` keys were verified to be honoured (`CcSteps` changes the output; a
  pp3 with only the demosaic method gives the same result).
- darktable: `--apply-custom-presets false` plus `workflow=none` leaves only
  the default modules. The demosaic method comes from a one-item XMP (params
  v4; 1 = AMaZE, 5 = RCD, its default), with a throwaway config dir and
  in-memory library.
- The DNG carries white-balance gains: AsShotNeutral plus a colour matrix with
  rows divided by the gains, so a tool's output is `gains * camera` in linear
  sRGB. Tools normalize multipliers to green = 1, so the truth is darkened by
  `1 / max(gain)` to keep balanced values <= 1 (no clipping).
  Lesson: with a unit neutral (an early version of the harness) RawTherapee
  and darktable disagreed by up to 3 dB with the *same* algorithm, because
  RawTherapee scales internally. Always give the tools a realistic neutral.
- Identity check: every tool reproduces the measured samples at sites below
  0.9 (RMS <= 1e-4). Near-saturated sites are skipped: darktable clips and
  adjusts them.

### Findings

1. **The references are trustworthy.** RawTherapee and darktable agree to
   within ~0.01 dB for AMaZE and RCD on all 12 real cases, and to within
   0.4 dB on clean fixtures.
2. **White balance after demosaicing costs 2-7 dB.** `demosaic()` applies gains
   after reconstruction (documented in `crates/demosaic/src/lib.rs`), while both
   tools scale before. Real files have R and B at 0.4-0.5x of green, and
   AMaZE/RCD build R and B from green plus smoothed colour differences, so
   green noise leaks into them and thresholds see unbalanced channels.
   Pre-balancing the mosaic (rows "preWB") recovers most of it, e.g. DSC03821
   4x4: AMaZE 49.98 -> 54.03 dB (reference 54.77), RCD 49.35 -> 54.07 (54.69);
   worst case DSC03270 4x4 RCD 47.25 -> 51.91 (reference 54.58). Bilinear and
   MHC are linear and unaffected.
2b. After pre-balancing, Rohditor is still 0.3-1.5 dB behind on AMaZE, and
   behind on RCD in a few cases (DSC03270 4x4: 2.7 dB; DSC01166 4x4: 0.9 dB).
3. **Rohditor's AMaZE is a simplification, not a port.** Noise-free, no white
   balance involved: slanted edge 51.3 dB vs ~60.9 (references), disc 47.4 vs
   49.4, zone plate 15.2 vs 16.7. Compared with RawTherapee's
   `amaze_demosaic_RT.cc` (5.13), the port has about 140 lines of green
   interpolation versus ~700 upstream, and ~40 lines of chroma versus ~500.
   Missing: Nyquist texture detection and area interpolation, G refinement
   from curvature, variance-weighted combination of the plus/minus diagonal
   estimates (`pmwt`), the second green pass using the new R+B values, and
   the "fancy chrominance" smoothing. Errors concentrate at colour edges:
   red at blue sites is off by 0.13 on a 0.5-step edge. Green at the same pixels
   already matches the references.
4. **RCD matches darktable on the slanted edge** (59.27 dB, identical) but trails
   on the disc (51.6 vs 53.4) and zone plate (16.48 vs 16.81), so a smaller
   divergence exists. Not investigated yet.
5. On the noisy 2x2 truth, bilinear often wins on "all" because per-pixel noise
   in the truth cannot be recovered and smoothing is rewarded. Prefer the 4x4
   rows and the edge column for algorithm quality.

An earlier version of this section reported RawTherapee 2-4 dB ahead of
darktable and attributed it to RawTherapee-specific smoothing; that was the
harness artifact described above.

### Fix: channel balance (done)

`demosaic(mosaic, balance, algorithm)` now takes the as-shot gains as a
*balance*. RCD and AMaZE reconstruct on the mosaic scaled by `balance / max`
(darktable's convention: all scaled samples stay <= 1, which RCD's `[0, 1]`
limits rely on) and scale back afterwards, so the output stays camera-native
and the user's white balance remains a separate pipeline stage. Bilinear and
MHC are linear and skip the scaling.

- `crates/demosaic/src/balance.rs`: `ChannelBalance` (public, so the GPU uses
  the same normalization).
- AMaZE's clip point is now `min(normalized gains)`, the level at which the
  first channel saturates, instead of the constant 1.0.
- Pipeline: `SensorDevelopmentDescription::demosaic_balance()` returns the
  file's as-shot gains, independent of the recipe, so a white-balance edit
  never invalidates the demosaiced image (RawTherapee does the same). The full-
  resolution export path now always applies white balance as its own stage; it
  used to fuse it into the demosaic write when optics and capture sharpening
  were off, which costs one extra parallel pass.
- GPU RCD applies the same balance (uniform buffer, scaled at load and
  unscaled at store); `rcd_matches_cpu_under_a_non_unit_channel_balance` covers it.
- `DemosaicContract::algorithm_version` is 2 for RCD and AMaZE (results changed).
- The old `gains` semantic of `demosaic()` (output multiplied by gains) is gone;
  nothing in the pipeline used it.

Result on the 12 real cases (PSNR vs balanced truth; rows are Rohditor as the
pipeline runs it, Rohditor without balance, RawTherapee/darktable):

| case | AMaZE old -> new (ref) | RCD old -> new (ref) |
|---|---|---|
| DSC03821 4x4 | 49.98 -> 54.03 (54.77) | 49.35 -> 54.07 (54.69) |
| DSC03270 4x4 | 50.91 -> 53.12 (54.55) | 47.25 -> 51.91 (54.58) |
| DSC01166 4x4 | 46.29 -> 49.62 (50.06) | 45.79 -> 49.23 (50.17) |
| DSC03821 2x2 | 48.65 -> 50.62 (51.37) | 48.38 -> 51.23 (51.72) |

Every case improves; remaining gaps are 0.3-1.5 dB for AMaZE and 0.1-2.7 dB
for RCD (see below). Run the `level1_real_images` test for the full table; the
"no balance" rows are kept as a regression baseline.

Cost (criterion, 2560x1703 mosaic, as-shot-like gains 2.1/1.0/2.6): RCD
405.8 -> 406.5 ms, AMaZE 392.6 -> 397.1 ms. The scaled copy of the mosaic is
within noise; bilinear and MHC are unchanged because they skip it. The bench
has `*_balanced` variants for every algorithm.

### Known issues from this work

- **GPU/CPU RCD outliers.** With balance, GPU and CPU RCD differ on 84 of 72M
  samples in DSC00851 (one small cluster, max camera-space error 1.8e-3); a
  discrete direction-selection step in RCD flips on f32 rounding differences.
  `private_rcd_camera_parity_and_full_resolution_timings` now tolerates at
  most 1e-5 of samples outside tolerance (scaled by the largest inverse
  gain) and a 5e-3 maximum. Worth understanding properly, e.g. by aligning
  the GPU's `fma` use with the CPU arithmetic.
- **Pre-existing failing GPU export parity tests** (they fail identically at
  HEAD, checked in a clean worktree): `private_full_resolution_export_parity`,
  `private_full_resolution_export_software_structure`,
  `private_full_resolution_capture_export_software_structure` (MHC, 16-bit
  ChromaCompress max=18 against a limit of 16) and
  `private_rcd_full_resolution_export_software_structure` (RCD: 8-bit max=6 at
  HEAD, now 2; 16-bit max=700 against a limit of 16 because of the outlier
  cluster above).
- darktable clips and adjusts near-saturated samples, so the identity check
  skips sites >= 0.9.

### Next steps

1. **Port AMaZE faithfully** from `amaze_demosaic_RT.cc` (GPL, already adapted
   here) and check each stage against the clean fixtures. This is the biggest
   remaining gap: noise-free slanted edge 51.3 dB vs ~60.9 for the references,
   disc 47.4 vs 49.4, zone plate 15.2 vs 16.7. Missing stages are listed in
   finding 3. Expect AMaZE to also need a GPU version later.
2. **Find the remaining RCD divergence** (noise-free disc 51.6 vs 53.4, zone
   plate 16.48 vs 16.81; DSC03270 4x4 51.9 vs 54.6 after balance). Rohditor
   RCD matches darktable exactly on the slanted edge, so a stage-level diff
   against darktable's `rcd` should find it quickly.
3. Settle the GPU/CPU RCD outliers and the failing export parity tests above.
4. **Level 2** on real ARWs (neutral pp3 / XMP, no synthetic truth), difference
   images in `testdata/generated/reference/`.
5. Lock in baselines as JSON so regressions fail `check.sh`-adjacent tests, and
   consider running a small version of the clean-fixture comparison without
   external tools (Rohditor-only thresholds) in normal CI.
6. Optionally let the user's white balance, not only the as-shot one, set the
   balance when it is far from as-shot; measure first, as it would make every
   white-balance edit re-demosaic.
