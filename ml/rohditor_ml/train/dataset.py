"""Training and validation data for one model variant.

A variant is a license filter on the manifest's per-file `license`: the patch
arrays are shared, and each variant only indexes the patches whose source file
has an allowed license.

`SampleStream` is an endless, worker-safe stream of augmented samples; the mix
of clean (synthetic noise) and real-pair samples is set per training stage.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
import torch
from torch.utils.data import IterableDataset, get_worker_info

from .samples import Sample, clean_sample, real_sample
from .store import PatchArrays


def training_noise_doc(noise_doc: dict, holdout_camera: str | None) -> dict:
    """The noise parameters without the held-out camera.

    Its pairs were measured for the fit, but a pooled draw during training must
    not use that sensor's noise, or the unseen-camera test isn't unseen.
    """
    cameras = {k: v for k, v in noise_doc["cameras"].items() if k != holdout_camera}
    return {**noise_doc, "cameras": cameras}


@dataclass
class SplitData:
    """Patch arrays of one split, with the indices this variant may use."""

    clean: PatchArrays | None
    real: PatchArrays | None
    pixelshift: np.ndarray = field(default_factory=lambda: np.zeros(0, np.int64))  # clean indices, full-res truth
    binned: np.ndarray = field(default_factory=lambda: np.zeros(0, np.int64))  # clean indices, 2x2-binned truth
    real_indices: np.ndarray = field(default_factory=lambda: np.zeros(0, np.int64))
    profiles: list[str] = field(default_factory=list)  # noise profile per manifest file

    @classmethod
    def load(cls, root: Path, split: str, licenses: set[str], limit: int | None = None, seed: int = 0) -> SplitData:
        manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
        files = manifest["files"]
        allowed = np.array([f["license"] in licenses for f in files])
        pixelshift_file = np.array([f["truth"] == "pixelshift" for f in files])
        data = cls(PatchArrays.open(root, split, "clean"), PatchArrays.open(root, split, "noisy"),
                   profiles=[f.get("noise_profile", "pooled") for f in files])
        rng = np.random.default_rng(seed)

        def pick(indices: np.ndarray) -> np.ndarray:
            if limit is not None and len(indices) > limit:
                indices = np.sort(rng.choice(indices, limit, replace=False))
            return indices

        if data.clean is not None:
            fi = np.asarray(data.clean.file_index)
            ok = allowed[fi]
            data.pixelshift = pick(np.flatnonzero(ok & pixelshift_file[fi]))
            data.binned = pick(np.flatnonzero(ok & ~pixelshift_file[fi]))
        if data.real is not None:
            data.real_indices = pick(np.flatnonzero(allowed[np.asarray(data.real.file_index)]))
        return data

    def describe(self) -> str:
        return f"{len(self.pixelshift)} pixel-shift + {len(self.binned)} binned clean, {len(self.real_indices)} real pairs"

    def clean_sample(self, index: int, noise_doc: dict, crop: int, exposure_min: float, rng) -> Sample:
        c = self.clean
        profile = self.profiles[int(c.file_index[index])]
        return clean_sample(c.mosaic[index], c.rgb[index], profile, noise_doc, crop, exposure_min, rng)

    def real_sample(self, index: int, crop: int, rng) -> Sample:
        r = self.real
        return real_sample(r.mosaic[index], r.rgb[index], r.mask[index], crop, rng)


@dataclass(frozen=True)
class Mix:
    """How a stage draws samples."""

    real_fraction: float  # share of real pairs
    pixelshift_fraction: float  # share of pixel-shift patches among the clean ones
    crop: int
    exposure_min: float


def draw(data: SplitData, mix: Mix, noise_doc: dict, rng: np.random.Generator) -> Sample:
    if len(data.real_indices) and rng.random() < mix.real_fraction:
        return data.real_sample(int(rng.choice(data.real_indices)), mix.crop, rng)
    use_pixelshift = len(data.pixelshift) and (not len(data.binned) or rng.random() < mix.pixelshift_fraction)
    pool = data.pixelshift if use_pixelshift else data.binned
    return data.clean_sample(int(rng.choice(pool)), noise_doc, mix.crop, mix.exposure_min, rng)


def to_tensors(sample: Sample) -> tuple[torch.Tensor, torch.Tensor, torch.Tensor, torch.Tensor]:
    return (torch.from_numpy(np.ascontiguousarray(sample.input, np.float32)),
            torch.from_numpy(np.ascontiguousarray(sample.target, np.float32)),
            torch.from_numpy(np.ascontiguousarray(sample.mask, np.float32)),
            torch.tensor(float(sample.real)))


class SampleStream(IterableDataset):
    """Endless random samples; each loader worker gets its own seed."""

    def __init__(self, data: SplitData, mix: Mix, noise_doc: dict, seed: int):
        self.data, self.mix, self.noise_doc, self.seed = data, mix, noise_doc, seed

    def __iter__(self):
        info = get_worker_info()
        worker = info.id if info is not None else 0
        rng = np.random.default_rng([self.seed, worker])
        while True:
            yield to_tensors(draw(self.data, self.mix, self.noise_doc, rng))


def fixed_samples(data: SplitData, mix: Mix, noise_doc: dict, n_clean: int, n_real: int, seed: int) -> list[tuple]:
    """A deterministic validation set: the same patches, noise and gains on every call."""
    rng = np.random.default_rng(seed)
    clean = np.concatenate([data.pixelshift, data.binned])
    clean = rng.choice(clean, min(n_clean, len(clean)), replace=False) if len(clean) else clean
    real = data.real_indices
    real = rng.choice(real, min(n_real, len(real)), replace=False) if len(real) else real
    samples = [to_tensors(data.clean_sample(int(i), noise_doc, mix.crop, mix.exposure_min, np.random.default_rng([seed, int(i)])))
               for i in clean]
    samples += [to_tensors(data.real_sample(int(i), mix.crop, np.random.default_rng([seed, 1, int(i)]))) for i in real]
    return samples
