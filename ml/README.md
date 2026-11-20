# rohditor-ml: neural demosaicing

A small U-Net that demosaics and denoises Bayer raws in one pass. It is meant to
replace RCD + noise reduction in Rohditor, running as ONNX. Epic #115: dataset #116,
training and export #117.

## Contract (`contract.json`)

- **Input:** an RGGB mosaic packed as `[1, 4, H/2, W/2]` (`R, G1, G2, B`). Black is
  subtracted and the result divided by `white - black`. Negative values are kept, and
  saturated photosites are exactly 1.0.
- **White balance:** as-shot gains, normalised to G = 1, are applied to the mosaic
  *before* the network.
- **Output:** white-balanced linear camera RGB, `[1, 3, H, W]`.
- **Sizes and tiling:** H and W are multiples of 16. Full images are processed in
  512 px tiles with a 32 px halo.

## Data (#116)

Allowed sources and their licenses:

| source | license | truth | files |
|---|---|---|---|
| PixelShift200 | CC BY 4.0 | pixel-shift, full resolution | 109 |
| raw.pixls.us | CC0 | 2×2-binned | 1243 |
| RawNIND | CC0 | 2×2-binned, plus real noisy/clean pairs | 309 |
| RawNIND | CC BY-SA 4.0 | 2×2-binned, plus real noisy/clean pairs | 1854 |

ELD, LED, MIT-Adobe FiveK and SID are **not** used.

The 256 px patches are as follows (clean patches get synthetic noise during training;
noisy patches are real pairs):

| split | clean | real noisy |
|---|---|---|
| train | 31,781 | 10,736 |
| val | 3,456 | 992 |
| test | 4,912 | 2,120 |
| test_unseen_camera (Nikon D40) | 272 | 64 |

**Noise model** (`data/noise_params.json`): per-camera Poisson-Gaussian noise, fitted
on RawNIND pairs. It stores a shot gain K and read noise σ at every measured ISO,
plus banding (row noise) and black-offset terms. During training, K is drawn
log-uniformly between the camera's lowest and highest *measured* ISO. Cameras without
a fit draw a random fitted camera ("pooled"). The held-out D40 is never used for
training noise.

**Private corpus** (`testdata/private`): visual check only. It never appears in shards
or manifests, and its crops must never be published (they stay in `runs/eval*/crops/private/`).

## Model and training (#117)

**Model:** a U-Net with channels 32/64/128/256: 2.14 M parameters, 48.4 GMAC per
megapixel. It predicts a residual on top of a bilinear demosaic, and its head is
initialised to zero, so training starts from plain bilinear output. The output is
upsampled with `PixelShuffle(2)`.

**Samples:**
- 240 px crops, augmented with flips and transposes that keep the RGGB phase.
- A random downward exposure scale in [0.25, 1] and random white-balance gains.
- 40% of clean samples come from pixel-shift truth.
- 10% of clean samples are procedural hard edges and gratings
  (`train/procedural.py`).

**Loss:** L1 on signed gamma, `sign(x)·|x|^(1/2.2)`. Real pairs are scored 2×2-binned,
with a validity mask.

**Optimisation:** AdamW, bf16, gradient clipping at 1.0, batch 32. Each stage has its
own linear warmup and cosine decay.

| variant | licenses | stage 1 (synthetic noise) | stage 2 (20–30% real pairs) | status |
|---|---|---|---|---|
| `shippable` | CC0, CC BY | 300k steps, lr 3e-4 | 10k steps, lr 3e-5* | GPL-3.0 weights, published as a release asset (not in git) |
| `full` | + CC BY-SA | 500k steps, lr 3e-4 | 50k steps, lr 3e-5 | **private, never published or shipped** |

\* The trained shippable model used lr 1e-4 in stage 2.

### Commands

These run from `ml/`, in the rocm distrobox.

```sh
UV_PROJECT_ENVIRONMENT=.venv-rocm uv sync --group train   # never a plain `uv sync` here: it removes torch
uv run python -m rohditor_ml.data.build --subset full     # download -> shards (host .venv is fine)
UV_PROJECT_ENVIRONMENT=.venv-rocm uv run --group train python -m rohditor_ml.train.store   # shards -> memmapped npy (~54 GB)

export UV_PROJECT_ENVIRONMENT=.venv-rocm PYTHONUNBUFFERED=1
nohup uv run --group train python -m rohditor_ml.train --config configs/unet_v1_full.yaml --resume > runs/full.log 2>&1 &
pkill -f rohditor_ml.train                                # stop: last.pt is saved every 2,500 steps
nohup runs/full/train.sh >> runs/full.log 2>&1 &          # resume
.venv-rocm/bin/tensorboard --logdir runs                  # losses, learning rate, validation previews

uv run --group train python -m rohditor_ml.train.export --run-dir runs/full   # model.onnx + model.json
uv run --group train python -m rohditor_ml.eval --run-dirs runs/shippable runs/full --out-dir runs/eval
```

`configs/unet_v1_smoke.yaml` is a 40k-step run on 1,000 patches per set, for checking
the pipeline end to end. Each run folder holds `config.yaml`, `train.sh` (the exact
command), `log.csv`, `tb/` and `checkpoints/`.

**Export:** ONNX opset 18 with dynamic H and W, checked against PyTorch with
onnxruntime (max error < 1e-4; shippable: 3.3e-6). `model.json` records the contract,
the license, dataset attribution and the training commit.

## Evaluation

The **baselines** are MHC, RCD and AMaZE run through `rohditor-cli demosaic-npy`, plus
`rcd+nr(best)`: RCD followed by Rohditor's noise reduction, at the best of five
strengths per ISO tier.

Scores are PSNR and SSIM, linear and sRGB, with an 8 px margin, on these sets:

- **PixelShift sweep:** synthetic noise at ISO 100 to 51200 of a Sony A7C.
- **Real pairs:** held-out RawNIND scenes, scored 2×2-binned.
- **Unseen camera:** the Nikon D40.
- **Charts:** a zone plate and a slanted edge.

**Gate:** at least +1 dB linear PSNR over `rcd+nr(best)` at the top ISO tier, and at
least −0.3 dB at base ISO.

## Results

These are for `shippable` (`runs/eval-shippable/report.md`), as gain in linear PSNR
over `rcd+nr(best)`:

| set | base ISO | top ISO |
|---|---|---|
| PixelShift sweep | +0.92 dB (ISO 100) | +12.71 dB (ISO 51200) |
| real pairs | +1.44 dB (ISO ≤800) | +8.11 dB (ISO 6400+) |
| unseen camera, real | +1.44 dB (ISO ≤800) | +2.20 dB (ISO 1600–3200) |

- **Gate:** passed.
- **High ISO:** at ISO 51200 the model keeps 50.7 dB against 38.0 for the baseline,
  and SSIM 0.97 against 0.64.
- **Zone plate:** 21.6 dB, the same as RCD (AMaZE: 27.8).
- **Slanted edge:** 43.9 dB against RCD's 64.0. There is a colour fringe one pixel
  wide on the perfect step. The procedural patches are aimed at this; they are not in
  the shippable model yet.

## Problems and fixes

- **PyTorch on ROCm:** the rocm10.0 wheel needs `rocm[device-all]`. We use the
  rocm7.2 index instead, with torch 2.14.1 (a self-contained wheel) in a ROCm 10.0
  container on an RX 9070 XT. `triton-rocm` has to be listed explicitly to resolve.
- **GPU crash:** one `HSA_STATUS_ERROR_ILLEGAL_INSTRUCTION` at about 23k steps of the
  smoke run, cause unknown. Resuming from `last.pt` worked. Checkpoints are atomic.
- **Data loading:** random reads from a cold page cache only reached about 95
  samples/s. The npz shards are now converted to memmapped `.npy` files and read
  sequentially once at start-up (about 46 s for 42 GB), which gives over 1,000
  samples/s.
- **Speed:** the GPU is the limit at about 360 samples/s whatever the batch size, so
  batch 32 (0.9 GB of VRAM) is kept. Training runs at about 9.6 steps/s.
- **Zone plate washed out to grey (smoke run):** K was drawn up to the fit's
  extrapolated `k_max` (about ISO 160k for the A7C), so the model learned to smooth
  hard. Limiting K to the measured ISO range fixed it (18.6 → 21.6 dB). The old run
  is kept in `runs/shippable-widenoise`.
- **Fake noise tiers in eval:** tiers derived from `k_min`/`k_max` gave inflated
  gains (+15 dB). Tiers now come from the measured K per ISO.
- **Loss and learning-rate jump at stage 2:** this is by design. Each stage restarts
  its warmup, and real pairs have a higher loss. Real-pair validation PSNR rose
  (+0.26 dB), but lr 1e-4 cost 0.07 dB on clean, hence 3e-5 from now on.
- **Crop size 240, not 248:** 248/2 = 124 does not survive three 2× poolings.
- **Flips:** a flip shifts the RGGB phase, so flipped crops start at an odd offset.
