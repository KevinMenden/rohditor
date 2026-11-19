"""The methods under evaluation: Rohditor's classic demosaics (through
`rohditor-cli demosaic-npy`) and trained network variants.

Every method maps a batch of same-sized cases to balanced linear RGB
`[N, 3, H, W]`.
"""

from __future__ import annotations

import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import torch

from ..data.bayer import pack
from ..paths import ML_ROOT
from ..train.augment import packed_gains

CLI = ML_ROOT.parent / "target" / "release" / "rohditor-cli"


@dataclass(frozen=True)
class Classic:
    """A demosaic algorithm with optional noise reduction (strengths in [0, 1])."""

    algorithm: str  # mhc, rcd or amaze
    chroma: float = 0.0
    luminance: float = 0.0

    @property
    def name(self) -> str:
        if not (self.chroma or self.luminance):
            return self.algorithm
        return f"{self.algorithm}+nr(c{self.chroma:g},l{self.luminance:g})"

    def run(self, mosaics: np.ndarray, gains: np.ndarray) -> np.ndarray:
        with tempfile.TemporaryDirectory(prefix="rohditor-eval-") as tmp:
            tmp = Path(tmp)
            np.save(tmp / "mosaic.npy", mosaics.astype(np.float32))
            np.save(tmp / "wb.npy", gains.astype(np.float32))
            subprocess.run(
                [str(CLI), "demosaic-npy", str(tmp / "mosaic.npy"), "--wb", str(tmp / "wb.npy"),
                 "--output", str(tmp / "out.npy"), "--demosaic", self.algorithm,
                 "--chroma", str(self.chroma), "--luminance", str(self.luminance)],
                check=True,
            )
            return np.load(tmp / "out.npy")


@dataclass
class Network:
    """A trained variant, run in float32 on the given device."""

    name: str
    model: torch.nn.Module
    device: torch.device
    batch: int = 16

    def run(self, mosaics: np.ndarray, gains: np.ndarray) -> np.ndarray:
        packed = np.stack([pack(m) * packed_gains(g) for m, g in zip(mosaics, gains, strict=True)])
        outputs = []
        with torch.no_grad():
            for i in range(0, len(packed), self.batch):
                x = torch.from_numpy(packed[i : i + self.batch]).to(self.device)
                outputs.append(self.model(x).float().cpu().numpy())
        return np.concatenate(outputs)


def classic_methods(nr_settings: list[tuple[float, float]]) -> tuple[list[Classic], list[Classic]]:
    """Context methods (plain MHC, RCD, AMaZE) and the RCD + noise-reduction candidates."""
    plain = [Classic("mhc"), Classic("rcd"), Classic("amaze")]
    return plain, [Classic("rcd", c, l) for c, l in nr_settings]


def ensure_cli() -> None:
    """Build the release CLI the baselines run through (a no-op when it is up to date)."""
    subprocess.run(["cargo", "build", "--release", "-p", "rohditor-cli"], cwd=ML_ROOT.parent, check=True)
