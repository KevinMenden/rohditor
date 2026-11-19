"""The evaluation sets, as lists of `Case`s every method is run on.

* `pixelshift_sweep`: PixelShift200 test patches (real full-resolution truth)
  with synthetic noise at the nominal ISOs `SWEEP_ISOS` of a reference camera.
* `real_pairs`: real noisy/clean pairs of the held-out RawNIND scenes, tiered
  by ISO, scored 2x2-binned and masked (see `metrics.scores`).
* `unseen_camera`: the held-out camera (`test_unseen_camera`): its real pairs,
  plus its clean patches with synthetic noise at its own measured ISOs.
* `charts`: a synthetic zone plate and slanted edge, where aliasing and
  zippering show.

Inputs are contract-scaled RGGB mosaics *before* white balance plus the gains,
so the classic baselines get exactly what Rohditor's pipeline would; targets are
balanced linear RGB, like the network output. Noise and selection are seeded,
so every method and every run sees the same cases.
"""

from __future__ import annotations

import json
import re
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from ..data.bayer import mosaic_from_rgb
from ..data.noise import NoiseParams, NoiseSample, apply_noise
from ..train.store import PatchArrays

SWEEP_ISOS = (100, 800, 3200, 12800, 51200)
ISO_TIERS = ((0, 800, "iso0-800"), (801, 3200, "iso1600-3200"), (3201, 10**9, "iso6400+"))
CHART_GAINS = np.array([2.1, 1.0, 1.7], np.float32)


@dataclass
class Case:
    set: str
    tier: str
    id: str
    mosaic: np.ndarray  # [H, W] noisy, not white balanced
    gains: np.ndarray  # [3]
    target: np.ndarray  # [3, H, W], balanced
    mask: np.ndarray | None = None  # [H, W], real pairs only
    binned: bool = False  # score 2x2-binned (real pairs)


def tier_key(tier: str) -> tuple[int, int]:
    """Sort order of tier labels: `clean` first, then by the first ISO in the label."""
    if tier == "clean":
        return (0, 0)
    match = re.search(r"\d+", tier)
    return (1, int(match.group()) if match else 0)


def measured_k(files: list[dict], camera: str) -> dict[float, float]:
    """Median shot-noise gain K per ISO from the camera's real pairs (manifest noise estimates)."""
    by_iso: dict[float, list[float]] = defaultdict(list)
    for record in files:
        estimate = record.get("noise_estimate")
        if record["camera"] == camera and estimate:
            by_iso[estimate["iso"]].append(estimate["k"])
    return {iso: float(np.median(ks)) for iso, ks in sorted(by_iso.items())}


def iso_noise(params: NoiseParams, k_by_iso: dict[float, float], isos) -> list[tuple[str, NoiseSample]]:
    """One fixed noise level per nominal ISO: K interpolated log-log between the measured
    ISOs, read noise from the fitted curve, row noise at half the fitted maximum and no
    black offset, so tiers differ only in level.

    The tiers follow measured ISOs rather than the fit's `[k_min, k_max]`, whose ends
    carry a margin and single outlier pairs (far beyond any real ISO).
    """
    measured = np.log(np.array(list(k_by_iso.items())))
    result = []
    for iso in isos:
        log_k = float(np.interp(np.log(iso), measured[:, 0], measured[:, 1]))
        sigma = float(np.exp(params.log_sigma(log_k)))
        result.append((f"iso{int(iso)}", NoiseSample(float(np.exp(log_k)), sigma, 0.5 * params.row_ratio_max * sigma, (0.0,) * 4)))
    return result


def _choose(indices: np.ndarray, count: int, seed: int) -> np.ndarray:
    if len(indices) <= count:
        return indices
    return np.sort(np.random.default_rng(seed).choice(indices, count, replace=False))


def _synthetic(name: str, arrays: PatchArrays, indices, tiers: list[tuple[str, NoiseSample]], seed: int) -> list[Case]:
    cases = []
    for tier_index, (tier, noise) in enumerate(tiers):
        for i in indices:
            mosaic = np.asarray(arrays.mosaic[i], np.float32)
            gains = np.asarray(arrays.wb[i], np.float32)
            rng = np.random.default_rng([seed, tier_index, int(i)])
            cases.append(Case(name, tier, f"{i}", apply_noise(mosaic, noise, rng), gains,
                              np.asarray(arrays.rgb[i], np.float32) * gains[:, None, None]))
    return cases


def _real(name: str, arrays: PatchArrays, indices, files: list[dict]) -> list[Case]:
    cases = []
    for i in indices:
        iso = files[int(arrays.file_index[i])].get("iso") or 0
        tier = next(label for lo, hi, label in ISO_TIERS if lo <= iso <= hi)
        gains = np.asarray(arrays.wb[i], np.float32)
        cases.append(Case(name, tier, f"{i}", np.asarray(arrays.mosaic[i], np.float32), gains,
                          np.asarray(arrays.rgb[i], np.float32) * gains[:, None, None],
                          np.asarray(arrays.mask[i], np.float32), binned=True))
    return cases


def zone_plate(size: int, supersample: int = 4) -> np.ndarray:
    """Achromatic `cos(k r^2)` rings reaching the Nyquist frequency at the edge midpoints."""
    n = size * supersample
    c = (np.arange(n) + 0.5) / supersample - size / 2
    r2 = c[:, None] ** 2 + c[None, :] ** 2
    k = np.pi / size  # local frequency k*r/pi cycles/px: 0.5 (Nyquist) at r = size/2
    fine = 0.3 + 0.25 * np.cos(k * r2)
    return fine.reshape(size, supersample, size, supersample).mean(axis=(1, 3))


def slanted_edge(size: int, angle_deg: float = 5.0, supersample: int = 8) -> np.ndarray:
    n = size * supersample
    c = (np.arange(n) + 0.5) / supersample - size / 2
    t = np.tan(np.radians(angle_deg))
    fine = np.where(c[None, :] - t * c[:, None] > 0, 0.6, 0.05)
    return fine.reshape(size, supersample, size, supersample).mean(axis=(1, 3))


def charts(size: int, base: NoiseSample, seed: int) -> list[Case]:
    """Both charts, clean and at the reference camera's base-ISO noise; grey in balanced RGB."""
    cases = []
    for chart, image in (("zone_plate", zone_plate(size)), ("slanted_edge", slanted_edge(size))):
        target = np.repeat(image[None].astype(np.float32), 3, axis=0)
        mosaic = mosaic_from_rgb(target / CHART_GAINS[:, None, None])
        cases.append(Case("charts", "clean", chart, mosaic, CHART_GAINS, target))
        noisy = apply_noise(mosaic, base, np.random.default_rng([seed, len(cases)]))
        cases.append(Case("charts", "base", chart, noisy, CHART_GAINS, target))
    return cases


def build_sets(npy_dir: Path, noise_doc: dict, sweep_camera: str, counts: dict[str, int], seed: int = 0) -> list[Case]:
    manifest = json.loads((npy_dir / "manifest.json").read_text(encoding="utf-8"))
    files, holdout = manifest["files"], manifest["holdout_camera"]
    pixelshift_file = np.array([f["truth"] == "pixelshift" for f in files])
    sweep = iso_noise(NoiseParams.from_dict(noise_doc["cameras"][sweep_camera]),
                      measured_k(files, sweep_camera), SWEEP_ISOS)
    cases: list[Case] = []

    test_clean = PatchArrays.open(npy_dir, "test", "clean")
    ps = np.flatnonzero(pixelshift_file[np.asarray(test_clean.file_index)])
    cases += _synthetic("pixelshift_sweep", test_clean, _choose(ps, counts["pixelshift_sweep"], seed), sweep, seed)

    test_real = PatchArrays.open(npy_dir, "test", "noisy")
    cases += _real("real_pairs", test_real, _choose(np.arange(len(test_real)), counts["real_pairs"], seed), files)

    unseen_clean = PatchArrays.open(npy_dir, "test_unseen_camera", "clean")
    unseen_real = PatchArrays.open(npy_dir, "test_unseen_camera", "noisy")
    if holdout in noise_doc["cameras"]:
        k_by_iso = measured_k(files, holdout)
        tiers = iso_noise(NoiseParams.from_dict(noise_doc["cameras"][holdout]), k_by_iso, list(k_by_iso))
        chosen = _choose(np.arange(len(unseen_clean)), counts["unseen_camera"], seed)
        cases += _synthetic("unseen_camera_synthetic", unseen_clean, chosen, tiers, seed)
    cases += _real("unseen_camera_real", unseen_real, np.arange(len(unseen_real)), files)

    cases += charts(256, sweep[0][1], seed)
    return cases
