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
  1024 px tiles with a 128 px halo. The halo covers the receptive field (about
  100 px), so stitched tiles equal one pass; a 32 px halo left seams of up to 1e-2.

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

| variant | licenses | stage 1 (synthetic noise) | stage 2 (real pairs mixed in) | status |
|---|---|---|---|---|
| `shippable` | CC0, CC BY | 300k steps, lr 3e-4 | 10k steps, 20% real, lr 3e-5 | GPL-3.0 weights, published as a release asset (not in git) |
| `full` | + CC BY-SA | 300k steps, lr 3e-4 | 30k steps, 30% real, lr 3e-5 | **private, never published or shipped** |

The current `shippable` model predates two config changes. It was trained without the
procedural images and with lr 1e-4 in stage 2. `full` has both changes.

## Setup

Training runs in the `rocm` distrobox on an RX 9070 XT. The home directory is shared
with the host, so the container has its own environment, `.venv-rocm`. Run everything
from `ml/`:

```sh
UV_PROJECT_ENVIRONMENT=.venv-rocm uv sync --group train   # torch, onnx, tensorboard
```

Never run a plain `uv sync` with `UV_PROJECT_ENVIRONMENT=.venv-rocm`: it removes torch.
The data tests and the CPU smoke test also run in the host `.venv`, without torch.

Data, once:

```sh
uv run python -m rohditor_ml.data.build --subset full     # download -> data/shards, manifest, noise fit
UV_PROJECT_ENVIRONMENT=.venv-rocm uv run --group train python -m rohditor_ml.train.store
# -> data/npy: memmapped arrays (~54 GB); unchanged shard groups are skipped on a rerun
```

## Training

```sh
export UV_PROJECT_ENVIRONMENT=.venv-rocm PYTHONUNBUFFERED=1

# start (in the background; output goes to runs/<variant>.log)
nohup uv run --group train python -m rohditor_ml.train --config configs/unet_v1_shippable.yaml --resume \
    > runs/shippable.log 2>&1 &

tail -f runs/shippable.log                     # progress
.venv-rocm/bin/tensorboard --logdir runs       # losses, lr, validation PSNR and preview images
pkill -f rohditor_ml.train                     # stop at any time
nohup runs/shippable/train.sh >> runs/shippable.log 2>&1 &   # resume
```

- **Run folders:** a run writes to `runs/<variant>/` (or `--run-dir`). The folder
  holds `config.yaml`, `train.sh` (the exact command and commit), `log.csv`, `tb/`,
  `checkpoints/` and, at the end, `model.pt`.
- **Checkpoints and resume:** `checkpoints/last.pt` is written atomically every
  2,500 steps, and `stage<i>-<name>.pt` at the end of each stage. `--resume` restores
  the model, optimizer, stage and step. It re-reads the schedule from the config file,
  so don't edit a config while its run is unfinished.
- **Starting over:** without `--resume` the trainer refuses to overwrite a run.
  Delete or rename the folder to start fresh.
- **GPU crashes:** a crash (seen once: `HSA_STATUS_ERROR_ILLEGAL_INSTRUCTION`) stops
  the run. Resume it as above.
- **Speed:** about 250–310 samples/s, which puts shippable at 9–10 h and full at
  11–13 h. The data loader runs on the CPU, so compiles and other load slow training.
  The full variant reads the whole 42 GB of patches, which should stay in the page
  cache, so close large applications if RAM is tight.
- **Smoke run:** `configs/unet_v1_smoke.yaml` is 40k steps on 1,000 patches per set,
  for checking the pipeline end to end in about an hour.

## Export

```sh
uv run --group train python -m rohditor_ml.train.export --run-dir runs/shippable   # -> runs/shippable/export/
```

- **Format:** ONNX opset 18 with dynamic H and W (multiples of 16). Export and the
  check run on the CPU, comparing onnxruntime with PyTorch at three sizes (it fails
  above 1e-4; shippable 3.3e-6, full 3.8e-6).
- **`model.json`:** records the contract, tiling, parameters and MACs, the ONNX
  `sha256`, the weights license, the training commit, licenses and stages, and every
  dataset source with its license and attribution.

## Evaluation

```sh
cargo build --release -p rohditor-cli          # the baselines (also built automatically if missing)
uv run --group train python -m rohditor_ml.eval --run-dirs runs/shippable runs/full --out-dir runs/eval
```

**Options:**
- `--crops N`: crops per set and tier (default 2).
- `--private-limit N`: how many raws from `testdata/private` to crop (0 for none).
- `--sweep-camera`: the camera whose noise drives the PixelShift sweep (default: Sony A7C).
- `--device`

Two models take about 10 min. It writes `report.md` (gate and tables), `scores.csv`
(per case), `summary.json` and `crops/` (labelled side-by-side 100% crops).

**Baselines:** MHC, RCD and AMaZE, run through `rohditor-cli demosaic-npy`, plus
`rcd+nr(best)`: RCD followed by Rohditor's noise reduction, at the best of five
strengths per set and ISO tier.

**Sets:** inputs are seeded, so every method and run sees the same cases.
- **PixelShift sweep:** 96 test patches with synthetic noise at ISO 100–51200 of the
  sweep camera, using its measured K per ISO.
- **Real pairs:** 384 held-out RawNIND patches, tiered by ISO and scored 2×2-binned
  with a mask.
- **Unseen camera (Nikon D40):** its real pairs, plus clean patches with synthetic
  noise at its own measured ISOs.
- **Charts:** a zone plate and a slanted edge, clean and at base-ISO noise. They have
  no numeric gate; check them in `crops/charts_*.png`.
- **Private corpus:** visual crops only, never published.

**Metrics:** PSNR and SSIM, linear and sRGB, with an 8 px margin.

**Gate (shippable):** at least +1 dB linear PSNR over `rcd+nr(best)` at the top ISO
tier, at least −0.3 dB at base ISO, and no visible artifacts on the charts.

## Results

These come from `runs/eval-full/report.md` and are linear PSNR in dB.

**Gate:** gain over `rcd+nr(best)`. Both variants pass.

| set | tier | shippable | full |
|---|---|---|---|
| PixelShift sweep | ISO 51200 | +12.71 | +12.67 |
| PixelShift sweep | ISO 100 | +0.92 | +0.91 |
| real pairs | ISO 6400+ | +8.11 | +8.85 |
| real pairs | ISO 0–800 | +1.44 | +2.05 |

**Absolute scores:**

| set | tier | rcd+nr(best) | shippable | full |
|---|---|---|---|---|
| PixelShift sweep | ISO 100 / 3200 / 51200 | 53.97 / 49.77 / 38.03 | 54.89 / 53.70 / 50.73 | 54.88 / 53.64 / 50.69 |
| real pairs | ISO 0–800 / 1600–3200 / 6400+ | 58.03 / 54.37 / 45.72 | 59.46 / 57.00 / 53.83 | 60.07 / 57.79 / 54.57 |
| unseen camera, real | ISO 0–800 / 1600–3200 | 49.56 / 45.21 | 51.00 / 47.41 | 51.35 / 47.47 |
| unseen camera, synthetic | ISO 400 / 3200 | 44.39 / 36.48 | 45.40 / 42.86 | 45.34 / 42.72 |
| zone plate (clean) | | RCD 21.6, AMaZE 27.8 | 21.6 | 24.2 |
| slanted edge (clean) | | RCD 64.0, AMaZE 74.1 | 43.9 | 45.7 |

- **High ISO:** at ISO 51200 the network keeps about 50.7 dB and SSIM 0.97, against
  38.0 dB and 0.64 for the baseline. At base ISO it is still about 1 dB better.
- **Full vs shippable:** full gains 0.6–0.8 dB on real pairs, and both are equal on
  synthetic noise. Part of full's lead, especially on the charts, comes from the
  procedural images and the lower stage-2 lr rather than the extra data, because
  shippable predates both.
- **Open: slanted edge.** Both models leave a colour fringe about one pixel wide on
  the perfect step, which RCD doesn't show. It is smaller in full. Retraining
  shippable with the current config is the next step. If the fringe remains, raise
  `procedural_fraction`.
- **Inference time** for a 24 MP image (network only, measured with 96 tiles of
  512 px and a 32 px halo; the current 1024/128 tiling costs about 30% more):
  - GPU, PyTorch fp32: 0.2 s (bf16 is slower on this card). The whole image in one
    pass needs 10.6 GB.
  - CPU, onnxruntime on a Ryzen 5 2600X: about 16 s.
  - For comparison, classic RCD takes 0.75 s and RCD + noise reduction 2.2 s on
    the CPU.

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
- **Speed:** the GPU alone manages about 360 samples/s whatever the batch size
  (32 to 256), so batch 32 is kept (0.9 GB of VRAM). Training reaches 250–310
  samples/s, limited by the CPU-side loader. A `rustc` build running alongside
  halved it.
- **RAM for `full`:** it reads all 42 GB of patches (shippable only touches a third).
  With about 20 GB of applications open, that no longer fits in 62 GB, and patches
  are read from disk again. Closing large applications helps.
- **Zone plate washed out to grey (smoke run):** K was drawn up to the fit's
  extrapolated `k_max` (about ISO 160k for the A7C), so the model learned to smooth
  hard. Limiting K to the measured ISO range fixed it (18.6 → 21.6 dB). The old run
  is kept in `runs/shippable-widenoise`.
- **Fake noise tiers in eval:** tiers derived from `k_min`/`k_max` gave inflated
  gains (+15 dB). Tiers now come from the measured K per ISO.
- **Loss and learning-rate jump at stage 2:** this is by design. Each stage restarts
  its warmup, and real pairs have a higher loss. Real-pair validation PSNR rose
  (+0.26 dB), but lr 1e-4 cost 0.07 dB on clean. With 3e-5 (full): +0.6 dB on real
  pairs and −0.04 dB on clean.
- **Crop size 240, not 248:** 248/2 = 124 does not survive three 2× poolings.
- **Flips:** a flip shifts the RGGB phase, so flipped crops start at an odd offset.
