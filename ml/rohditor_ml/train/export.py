"""Export a trained variant as `model.onnx` + `model.json` (on CPU):

    uv run --group train python -m rohditor_ml.train.export --run-dir runs/shippable

The ONNX graph has dynamic height and width (opset 18), and is verified with
onnxruntime against PyTorch at a few sizes before anything is written.
`model.json` records the contract, cost, the ONNX hash, the training commit and
every dataset source with its license.

The `shippable` export is published as a GitHub release asset (weights GPL-3.0,
dataset attribution in `model.json`); `full` stays local and is never attached
to a release.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from collections import Counter
from pathlib import Path

import numpy as np
import torch

from ..contract import CONTRACT
from ..paths import ML_ROOT
from .trainer import build_model
from .model import macs_per_megapixel, parameter_count

OPSET = 18
TOLERANCE = 1e-4
# Full-resolution sizes the export is checked at (multiples of 16, not square).
CHECK_SIZES = ((64, 96), (256, 160), (512, 512))

# Attribution per source, recorded in `model.json`.
SOURCES = {
    "pixelshift200": {"title": "PixelShift200", "url": "https://huggingface.co/datasets/guochengqian/PixelShift200"},
    "pixls": {"title": "raw.pixls.us", "url": "https://raw.pixls.us"},
    "rawnind": {"title": "RawNIND", "url": "https://doi.org/10.14428/DVN/DEQCIM"},
}
WEIGHTS_LICENSE = {"shippable": "GPL-3.0-only", "full": "private: never published, attached to a release or shipped"}


def load_trained(run_dir: Path) -> tuple[torch.nn.Module, dict, str]:
    """The model at the end of the run (`model.pt`), its config and training commit."""
    checkpoints = sorted((run_dir / "checkpoints").glob("stage*.pt"))
    if not checkpoints:
        raise FileNotFoundError(f"{run_dir} has no finished stage checkpoint")
    state = torch.load(checkpoints[-1], map_location="cpu", weights_only=False)
    model = build_model(state["config"])
    model.load_state_dict(state["model"])
    return model.eval(), state["config"], state["git_commit"]


def export_onnx(model: torch.nn.Module, path: Path) -> None:
    example = torch.zeros(1, 4, 64, 64)
    height, width = torch.export.Dim("height", min=1), torch.export.Dim("width", min=1)
    # Half-resolution dims must stay multiples of 8 (three 2x2 pools).
    program = torch.onnx.export(
        model, (example,), dynamo=True, opset_version=OPSET, input_names=["mosaic"], output_names=["rgb"],
        dynamic_shapes={"packed": {2: 8 * height, 3: 8 * width}},
    )
    program.save(str(path))


def verify_onnx(model: torch.nn.Module, path: Path) -> float:
    """Largest absolute difference between onnxruntime and PyTorch over `CHECK_SIZES`."""
    import onnxruntime

    session = onnxruntime.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    rng = np.random.default_rng(0)
    worst = 0.0
    for h, w in CHECK_SIZES:
        packed = (rng.random((1, 4, h // 2, w // 2)) * 1.2 - 0.05).astype(np.float32)
        with torch.no_grad():
            expected = model(torch.from_numpy(packed)).numpy()
        (actual,) = session.run(None, {"mosaic": packed})
        if actual.shape != (1, 3, h, w):
            raise AssertionError(f"onnx output {actual.shape} for input {packed.shape}")
        worst = max(worst, float(np.abs(actual - expected).max()))
    if worst > TOLERANCE:
        raise AssertionError(f"onnxruntime differs from PyTorch by {worst:.2e} (> {TOLERANCE})")
    return worst


def dataset_sources(config: dict) -> list[dict]:
    """Every source/license combination the variant trained on, with file counts."""
    manifest = json.loads((ML_ROOT / config["data"]["npy_dir"] / "manifest.json").read_text(encoding="utf-8"))
    licenses = set(config["licenses"])
    counts = Counter((f["source"], f["license"]) for f in manifest["files"]
                     if f["split"] == "train" and f["license"] in licenses and f["patches"])
    return [{"source": s, **SOURCES[s], "license": lic, "train_files": n} for (s, lic), n in sorted(counts.items())]


def model_card(model: torch.nn.Module, config: dict, commit: str, onnx_path: Path, max_error: float) -> dict:
    variant = config["variant"]
    return {
        "name": config["model"]["name"],
        "variant": variant,
        "version": config["model"]["version"],
        "weights_license": WEIGHTS_LICENSE.get(variant, "private"),
        "contract": {k: CONTRACT[k] for k in ("version", "input", "output", "scaling", "white_balance")},
        "tiling": CONTRACT["tiling"],
        "dynamic_shapes": True,
        "size_multiple": model.size_multiple,
        "parameters": parameter_count(model),
        "macs_per_megapixel": macs_per_megapixel(model),
        "onnx": {"file": onnx_path.name, "opset": OPSET, "sha256": hashlib.sha256(onnx_path.read_bytes()).hexdigest(),
                 "max_abs_error_vs_pytorch": max_error},
        "training": {"git_commit": commit, "licenses": config["licenses"], "stages": config["stages"]},
        "datasets": dataset_sources(config),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="python -m rohditor_ml.train.export", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--run-dir", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, help="default: <run-dir>/export")
    args = parser.parse_args(argv)
    out = args.out_dir or args.run_dir / "export"
    out.mkdir(parents=True, exist_ok=True)

    model, config, commit = load_trained(args.run_dir)
    part = out / "model.onnx.part"
    export_onnx(model, part)
    max_error = verify_onnx(model, part)
    onnx_path = out / "model.onnx"
    part.replace(onnx_path)
    card = model_card(model, config, commit, onnx_path, max_error)
    (out / "model.json").write_text(json.dumps(card, indent=1) + "\n", encoding="utf-8")
    print(f"{onnx_path} (max error vs PyTorch {max_error:.1e}) and model.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
