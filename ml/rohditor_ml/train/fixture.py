"""Write the tiny ONNX test fixture for `crates/neural` (on CPU):

    uv run --group train python -m rohditor_ml.train.fixture

A narrow U-Net with the v1 layout and seeded random weights (the head included,
so the output is not just the bilinear branch), exported exactly like a trained
model. Next to it go a seeded packed input and the PyTorch output for it, so the
Rust ONNX Runtime backend can be checked without the real model. The weights are
random: they carry no training data and no license constraints.
"""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np
import torch

from ..paths import ML_ROOT
from .export import export_onnx
from .model import UNet

OUT_DIR = ML_ROOT.parent / "crates" / "neural" / "tests" / "data"
CHANNELS = (4, 4, 8, 8)
# Full-resolution size of the reference input: multiples of 16, not square.
HEIGHT, WIDTH = 64, 96


def build() -> UNet:
    torch.manual_seed(0)
    model = UNet(CHANNELS)
    # Small random head, so the correction is visible but stays near bilinear.
    torch.nn.init.normal_(model.head.weight, std=0.05)
    torch.nn.init.normal_(model.head.bias, std=0.01)
    return model.eval()


def main() -> int:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    model = build()
    export_onnx(model, OUT_DIR / "tiny.onnx")
    rng = np.random.default_rng(0)
    packed = (rng.random((1, 4, HEIGHT // 2, WIDTH // 2)) * 1.2 - 0.05).astype(np.float32)
    with torch.no_grad():
        expected = model(torch.from_numpy(packed)).numpy()
    np.save(OUT_DIR / "tiny_input.npy", packed)
    np.save(OUT_DIR / "tiny_expected.npy", expected)
    for path in sorted(OUT_DIR.glob("tiny*")):
        print(f"{path} ({path.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
