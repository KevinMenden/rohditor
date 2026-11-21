# AI masks: model choice (#170)

Part of epic #160. This is a decision, not an implementation. The export script is
`rohditor_ml/masks/` (see "Export" below).

## Decision

| mask | model | input | why |
|---|---|---|---|
| subject | **BiRefNet_lite** (BiRefNet, Swin-T backbone) | 1024² | best edges per second; matches full BiRefNet on our photos |
| sky | **skyseg** (U²-Net trained for sky) | 320² | finest masks in foliage gaps; fast |
| promptable (later) | SAM 2.1 Hiera-T | 1024² | Apache-2.0 weights; out of scope for #160 |

**Use policy:** weights are only used locally. They are never committed, attached to
a release or shipped, so research and non-commercial training-data terms are
acceptable. Each model's `model.json` records its terms.

## Licences

"Local only" means acceptable under the policy above, but not for distribution.

| model | code | weights | training data | verdict |
|---|---|---|---|---|
| BiRefNet_lite | MIT | MIT | DIS5K (non-commercial, no redistribution), DUTS, HRSOD, UHRSD, HRS10K, P3M-10k, humans (Apache-2.0, synthetic) | **chosen** (local only) |
| BiRefNet (Swin-L) | MIT | MIT | same | rejected: cost |
| BEN2 base | MIT | MIT | DIS5K + 22k proprietary images | rejected: quality |
| RMBG-2.0 (BRIA) | MIT | CC BY-NC 4.0, gated | BRIA's private data | rejected: gated download (account + agreement) |
| RMBG-1.4 (BRIA) | — | BRIA licence agreement | private | rejected: needs an agreement |
| IS-Net / U²-Net (rembg) | Apache-2.0 | Google Drive (the ONNX re-host on HF is AGPL-3.0) | DIS5K / DUTS | rejected: weaker than BiRefNet in published DIS results; weights on Drive |
| InSPyReNet | MIT | Google Drive | DUTS, HRSOD, DIS5K | not tested: weights on Drive only |
| MODNet | Apache-2.0 | Apache-2.0 | portrait matting | rejected: portraits only |
| ormbg | Apache-2.0 | Apache-2.0 | synthetic humans only | not tested: humans only, but the only clean-data option |
| skyseg | MIT | MIT (re-host of xiongzhu666's ONNX) | not documented | **chosen** (local only) |
| SegFormer-B2 ADE20K | NVIDIA source code licence (non-commercial) | same | ADE20K (research terms) | rejected: blurry edges |
| Mask2Former / OneFormer ADE20K | MIT | various | ADE20K | not tested: heavier, same sky class as SegFormer |
| SAM 2.1 Hiera-T | Apache-2.0 | Apache-2.0 | SA-1B (Meta) | later, for click-to-select |

## Quality

**Test set:** 20 CC0 phone photos (`Dima786/Photos_For_Ai`) covering portraits with
loose hair against sky, pets (fur and whiskers), street scenes, harbour and sea
horizons with haze, sunsets, tree canopies against sky, glass bottles and a vase.

**Limits:** there is no ground truth, so this is a visual comparison. There is no
real wildlife or mountain haze, and the private RAW corpus was not used. Check
the chosen models on it before building the UI.

**Subject:**
- **Hair:** BiRefNet_lite keeps single strands against sky. Swin-L is no better.
- **Fur and whiskers:** clean; lite even kept more whiskers than Swin-L.
- **Glass:** bottles and the vase are segmented whole. The mask is not
  transparent: this is segmentation, not matting.
- **No clear subject** (canopy, open sky): lite and Swin-L return an almost empty
  mask. BEN2 invents subjects (trees, rooftops, a whole cat carrier). The UI must
  handle "no subject found".
- **Ambiguous scenes:** lite picked a building on a sunset and a hand with a cup.
  This is what "salient" means, and it is a reason to add click-to-select later.

**Sky:**
- **Gaps in foliage:** skyseg resolves holes in the canopy. SegFormer's logits are
  at 1/4 of 512 px and come out blurry.
- **Horizons, haze and sunsets:** both models are clean.
- **Hair and thin branches against sky:** both are coarse at 320 or 512 px.
  Full-resolution edge refinement (guided filter, #89) is required, not optional.
- **False positives:** in one dim indoor photo, skyseg marks a patch near the top
  with a probability of up to 0.92. SegFormer, which understands the scene, does
  not. Use skyseg's raw sigmoid output: the min-max normalisation in VGGT's
  reference code would make this worse on photos without sky. A "no sky" gate is
  an open point.

## Cost

**Setup:** CPU, 4 vCPUs (cloud container), onnxruntime 1.32, fp32, median of 19
photos. Peak RSS is for the whole process, measured with the onnxruntime memory
arena on and off.

| model | input | params | ONNX | latency | peak RSS (arena on / off) |
|---|---|---|---|---|---|
| BiRefNet_lite | 1024² | 45.8 M | 188 MB | **3.5 s** | 4.4 / 2.4 GB |
| BiRefNet Swin-L | 1024² | 232 M | 973 MB | 20 s | out of memory at 15 GB / 7.9 GB |
| BEN2 base (fp16) | 1024² | — | 219 MB | 8.3 s | 4.6 GB / — |
| skyseg | 320² | 44.0 M | 176 MB | **0.30 s** | 0.96 GB / — |
| SegFormer-B2 | 512² | 27.5 M | 112 MB | 0.49 s | 1.2 GB / — |
| SAM 2.1 Hiera-T encoder (PyTorch) | 1024² | 27.2 M | — | 1.2 s | 1.4 GB |

- **Memory:** run the subject model with the CPU memory arena off. It halves peak
  memory for about 8% more time.
- **GPU:** not measured, because this environment has no GPU. The `ort` crate
  (2.0.0-rc.15) bundles onnxruntime 1.31 with Linux builds for CUDA 13/TensorRT,
  MIGraphX (AMD), OpenVINO and WebGPU. Measure MIGraphX and WebGPU on the
  RX 9070 XT when `crates/neural` exists (#118).
- **`DeformConv` on GPU:** support on the GPU execution providers is not verified.
  onnxruntime assigns unsupported nodes to the CPU, so results stay correct, but
  there are extra copies.

## ONNX

- **BiRefNet_lite:** exports cleanly with the dynamo exporter at **opset 19**, the
  first opset with `DeformConv`. The graph has 1,888 nodes, against 16,400 for the
  community export, which decomposes `DeformConv` into Gather/ScatterND at
  opset 17.
  - Max error against PyTorch after the sigmoid: 1e-7 on noise, 3.8e-5 on the sample photo.
  - Needs **onnxruntime ≥ 1.25** for the `DeformConv` kernel. 1.20–1.24 refuse
    the model. The `ort` crate's bundled 1.31 is fine.
  - The input is a fixed 1024², as upstream: photos are squashed to a square and
    the mask is stretched back.
- **skyseg:** published only as ONNX (opset 11, fixed 320²). It has two outputs;
  use output 0, the fused sigmoid map. It runs on any recent onnxruntime.

## Delivery

**Weights never enter git and are not downloaded by the app.** The user creates the
models once with the export script below, and the app finds them by path (the
inference service, #160 step 3, takes a model directory).

- **Pinning:** every upstream file is downloaded at a fixed Hugging Face commit
  and checked against a SHA-256 in `rohditor_ml/masks/models.py`.
- **ONNX checksum:** the exported ONNX's own SHA-256 depends on the PyTorch
  version, so it is recorded in `model.json`, not pinned. The engine should key its
  mask cache on (model id, ONNX SHA-256).
- **Rejected: bundling.** 364 MB together, and the training data does not allow
  distribution.
- **Rejected: download on first use.** The app would fetch weights with
  non-commercial training data on the user's behalf. It would also need network
  code in the engine.

## Export

From `ml/`:

```sh
uv run --group masks python -m rohditor_ml.masks.export        # or --model subject / --model sky
```

This writes `ml/runs/masks/{birefnet-lite,skyseg}/` (git-ignored), each with:

- `model.onnx`
- `model.json`: source, revision, checksums, licence, training data, and the
  input/output contract (ImageNet mean and std, sigmoid or none)
- `sample_input.npy` and `sample_output.npy`: a CC0 photo after preprocessing, and
  onnxruntime's probability map for it

The sample input is the already-normalised tensor, so the Rust parity test does not
depend on matching PIL's resize.
