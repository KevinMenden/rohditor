"""Export the AI-mask models to ONNX, with a sample input/output pair:

    uv run --group masks python -m rohditor_ml.masks.export            # all models
    uv run --group masks python -m rohditor_ml.masks.export --model sky

Writes `ml/runs/masks/<id>/`: `model.onnx`, `model.json` (source, revision,
checksums, licence, contract) and `sample_input.npy` / `sample_output.npy` (a
CC0 photo, preprocessed, and the probability map onnxruntime gives for it).

- `birefnet-lite` (subject) is exported from PyTorch at opset 19, the first
  opset with `DeformConv`, so it needs onnxruntime >= 1.25. It is checked
  against PyTorch (on noise and on the sample photo) before anything is written.
- `skyseg` (sky) is only published as ONNX; the pinned file is used as is.

Everything is downloaded at a pinned revision and checked against a SHA-256,
including the model code that `trust_remote_code` runs.
The weights stay local: never commit, attach or ship them.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
from pathlib import Path

import numpy as np
from PIL import Image

from ..paths import ML_ROOT
from .models import IMAGENET_MEAN, IMAGENET_STD, MODELS, HubFile, MaskModel
from .preprocess import to_input, to_probability

SUBJECT_OPSET = 19
# Largest allowed difference between onnxruntime and PyTorch, after the sigmoid.
TOLERANCE = 1e-4


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch(file: HubFile) -> Path:
    """Download a pinned file into the Hugging Face cache and check its checksum."""
    from huggingface_hub import hf_hub_download

    path = Path(hf_hub_download(file.repo, file.filename, revision=file.revision, repo_type=file.repo_type))
    actual = sha256(path)
    if actual != file.sha256:
        raise ValueError(f"{file.repo}/{file.filename}@{file.revision}: sha256 {actual}, expected {file.sha256}")
    return path


def export_subject(model: MaskModel, path: Path, sample: np.ndarray) -> float:
    """Export BiRefNet from PyTorch; return the largest difference to PyTorch.

    The comparison runs on random noise and on the real sample photo.
    """
    import torch
    from transformers import AutoModelForImageSegmentation

    # Checks the weights and the model code that from_pretrained loads and runs below.
    for file in (model.weights, *model.code):
        fetch(file)
    net = AutoModelForImageSegmentation.from_pretrained(
        model.weights.repo, revision=model.weights.revision, trust_remote_code=True).eval()
    example = torch.zeros(1, 3, model.size, model.size)
    program = torch.onnx.export(net, (example,), dynamo=True, opset_version=SUBJECT_OPSET,
                                input_names=["image"], output_names=["logits"])
    program.save(str(path))

    noise = np.random.default_rng(0).standard_normal((1, 3, model.size, model.size)).astype(np.float32)
    worst = 0.0
    for probe in (noise, sample):
        with torch.no_grad():
            expected = net(torch.from_numpy(probe))
        expected = expected[-1] if isinstance(expected, (list, tuple)) else expected
        actual = run_onnx(path, probe, model)
        worst = max(worst, float(np.abs(actual - to_probability(expected.numpy(), model)).max()))
    return worst


def run_onnx(path: Path, x: np.ndarray, model: MaskModel) -> np.ndarray:
    """The model's probability map for one preprocessed input, on CPU."""
    import onnxruntime

    session = onnxruntime.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    outputs = session.run(None, {session.get_inputs()[0].name: x})
    return to_probability(outputs[model.output_index], model)


def model_card(model: MaskModel, onnx_path: Path, max_error: float | None) -> dict:
    import onnx

    graph = onnx.load(str(onnx_path), load_external_data=False)
    w = model.weights
    return {
        "id": model.id,
        "task": model.task,
        "source": {"repo": w.repo, "revision": w.revision, "file": w.filename, "sha256": w.sha256},
        "license": model.license,
        "training_data": model.training_data,
        "contract": {
            "input": {"shape": [1, 3, model.size, model.size], "dtype": "float32",
                      "layout": "sRGB, bilinear square resize (aspect ratio not kept), scaled to [0, 1]",
                      "mean": IMAGENET_MEAN, "std": IMAGENET_STD},
            "output": {"index": model.output_index, "shape": [1, 1, model.size, model.size],
                       "kind": model.output_kind, "postprocess": "sigmoid" if model.output_kind == "logits" else "none"},
        },
        "onnx": {"file": onnx_path.name, "sha256": sha256(onnx_path),
                 "opsets": {(o.domain or "ai.onnx"): o.version for o in graph.opset_import},
                 "max_abs_error_vs_pytorch": max_error},
        "sample": {"photo": f"{model.sample_photo.repo}/{model.sample_photo.filename}@{model.sample_photo.revision}",
                   "license": "CC0-1.0", "input": "sample_input.npy", "output": "sample_output.npy (probability)"},
    }


def export(model: MaskModel, out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    part = out / "model.onnx.part"
    sample = to_input(Image.open(fetch(model.sample_photo)), model.size)
    if model.task == "subject":
        max_error = export_subject(model, part, sample)
        if max_error > TOLERANCE:
            raise AssertionError(f"{model.id}: onnxruntime differs from PyTorch by {max_error:.2e} (> {TOLERANCE})")
    else:
        shutil.copyfile(fetch(model.weights), part)
        max_error = None
    onnx_path = out / "model.onnx"
    part.replace(onnx_path)

    np.save(out / "sample_input.npy", sample)
    np.save(out / "sample_output.npy", run_onnx(onnx_path, sample, model))
    card = model_card(model, onnx_path, max_error)
    (out / "model.json").write_text(json.dumps(card, indent=1) + "\n", encoding="utf-8")
    error = "" if max_error is None else f", max error vs PyTorch {max_error:.1e}"
    print(f"{model.id}: {onnx_path}{error}")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m rohditor_ml.masks.export", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--model", choices=["subject", "sky", "all"], default="all")
    parser.add_argument("--out-dir", type=Path, default=ML_ROOT / "runs" / "masks")
    args = parser.parse_args(argv)
    for model in MODELS.values():
        if args.model in ("all", model.task):
            export(model, args.out_dir / model.id)
    return 0


if __name__ == "__main__":
    sys.exit(main())
