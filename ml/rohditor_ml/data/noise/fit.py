"""Calibrating `NoiseParams` from real noisy/clean pairs (RawNIND).

Per noisy image we regress the residual variance against the signal on flat
areas of the clean image: `Var(y - a*g) = K * (a*g) + sigma^2`, where `g` is the
(smoothed) clean image and `a` the gain that matches its exposure to the noisy
one (a weighted line fit per image). Per camera, the per-ISO medians of
`(log K, log sigma)` then form the read-noise curve (see `fit_camera`). Flat
areas make the estimate insensitive to the small misalignment between the two
shots.

The per-pair estimates are stored in the manifest, so `refit` can redo the
per-camera step in seconds without rebuilding the shards:

    uv run python -m rohditor_ml.data.noise.fit

Known approximations: the clean image's own noise is only reduced (3x3
smoothing), not removed, so use noisy images well above the clean ISO; row
noise is estimated from differences of neighbouring row means and may be
slightly high.
"""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from dataclasses import dataclass, replace
from pathlib import Path

import numpy as np
from scipy.ndimage import uniform_filter

from ...paths import ML_ROOT
from ..bayer import pack
from ..manifest import write_manifest
from .model import NoiseParams
from .profiles import profile_for

NOISE_PARAMS = ML_ROOT / "data" / "noise_params.json"
SHARDS_DIR = ML_ROOT / "data" / "shards"

MIN_BIN_PIXELS = 2000
BINS = 16
MAX_PIXELS = 4_000_000
MIN_PAIRS_PER_CAMERA = 3


@dataclass(frozen=True)
class PairEstimate:
    k: float
    sigma2: float
    row_var: float  # variance of the per-row offset, 0 when not detectable
    gain: float  # exposure ratio noisy / clean
    iso: float


def flat_mask(clean_planes: np.ndarray, smooth: np.ndarray) -> np.ndarray:
    """Pixels whose 5x5 neighbourhood (in each colour plane) is flat and well exposed."""
    mean = uniform_filter(smooth, size=(1, 5, 5))
    var = uniform_filter(smooth**2, size=(1, 5, 5)) - mean**2
    flat = np.sqrt(np.maximum(var, 0)) < 0.02 * smooth + 3e-4
    return flat & (smooth > 0.001) & (smooth < 0.9)


def estimate_pair(noisy: np.ndarray, clean: np.ndarray, iso: float, rng: np.random.Generator) -> PairEstimate | None:
    """Estimate (K, sigma^2, row variance) of one noisy image against its aligned clean one."""
    y, g = pack(noisy), pack(clean)
    smooth = uniform_filter(g, size=(1, 3, 3))
    mask = flat_mask(g, smooth) & (y < 0.95)
    if mask.sum() < MIN_BIN_PIXELS * 4:
        return None

    gain = float((y[mask] * smooth[mask]).sum() / (smooth[mask] ** 2).sum())
    resid = y - gain * smooth
    x_all, r_all = (gain * smooth)[mask], resid[mask]
    if x_all.size > MAX_PIXELS:
        keep = rng.choice(x_all.size, MAX_PIXELS, replace=False)
        x_all, r_all = x_all[keep], r_all[keep]

    # Mean signal vs residual variance in equally populated signal bins.
    order = np.argsort(x_all)
    means, variances = [], []
    for chunk in np.array_split(order, BINS):
        if chunk.size >= MIN_BIN_PIXELS:
            means.append(x_all[chunk].mean())
            variances.append(r_all[chunk].var())
    if len(means) < 4:
        return None
    # A bin's sample variance has an error proportional to the variance itself, so
    # weight by 1/variance (polyfit squares `w`). Unweighted, the bright, noisy bins
    # dominate and the intercept (the read noise, often 100x smaller than the shot
    # noise of the darkest bin) is lost in their error, even going negative.
    k, sigma2 = np.polyfit(means, variances, 1, w=1.0 / np.asarray(variances))
    if k <= 0:
        return None
    return PairEstimate(float(k), float(sigma2), _row_variance(resid, mask, float(np.mean(variances))), gain, iso)


def _row_variance(resid: np.ndarray, mask: np.ndarray, pixel_var: float) -> float:
    """Variance of per-row offsets (banding), from differences of neighbouring row means."""
    counts = mask.sum(axis=2)  # [4, h] valid pixels per plane row
    sums = np.where(mask, resid, 0.0).sum(axis=2)
    ok = counts >= 50
    means = np.where(ok, sums / np.maximum(counts, 1), np.nan)
    diff = means[:, 1:] - means[:, :-1]
    both = ok[:, 1:] & ok[:, :-1]
    if both.sum() < 100:
        return 0.0
    # Var(diff) = 2 * (row_var + pixel_var / n)
    shot_part = (pixel_var / np.minimum(counts[:, 1:], counts[:, :-1]))[both].mean()
    return float(max(0.0, np.var(diff[both]) / 2 - shot_part))


def fit_camera(estimates: list[PairEstimate]) -> NoiseParams | None:
    """Ranges and the read-noise curve for one camera (needs a few ISO levels).

    The curve's points are the median log K and log sigma of each ISO level, so
    a camera whose read noise drops at one ISO (dual conversion gain) keeps both
    regimes instead of one line misfitting each. Banding is the 90th percentile of
    row-noise std relative to read noise: robust to a few bad pairs, and it scales
    with ISO the way the read noise does.
    """
    if len(estimates) < MIN_PAIRS_PER_CAMERA:
        return None
    # Pairs whose read noise is below what the regression can resolve (sigma^2 <= 0)
    # still say something about K, but not about sigma.
    resolved = [e for e in estimates if e.sigma2 > 0]
    if len(resolved) < MIN_PAIRS_PER_CAMERA:
        return None
    by_iso: dict[float, list[PairEstimate]] = defaultdict(list)
    for e in resolved:
        by_iso[e.iso].append(e)
    if len(by_iso) < 2:
        return None
    points = sorted(
        (float(np.median(np.log([e.k for e in group]))), float(np.median(0.5 * np.log([e.sigma2 for e in group]))))
        for group in by_iso.values()
    )
    log_k_points, log_sigma_points = (tuple(v) for v in zip(*points, strict=True))
    params = NoiseParams(log_k_points, log_sigma_points)
    log_sigma = 0.5 * np.log([e.sigma2 for e in resolved])
    curve = np.array([params.log_sigma(np.log(e.k)) for e in resolved])
    row_ratio = np.sqrt([e.row_var for e in resolved]) / np.exp(log_sigma)
    return replace(params, sigma_log_std=float(np.std(log_sigma - curve)),
                   row_ratio_max=float(np.percentile(row_ratio, 90)))


def fit_all(estimates: dict[str, list[PairEstimate]]) -> dict:
    """The JSON document: per-camera params, plus the pair counts behind them.

    There is deliberately no pooled fit across cameras: files from a camera without
    a fit get the profile `"pooled"`, which means a random fitted camera per sample
    (see `profiles.params_for`). That keeps each draw a physically real sensor.
    """
    cameras = {name: fit_camera(items) for name, items in estimates.items()}
    return {
        "cameras": {name: p.to_dict() for name, p in sorted(cameras.items()) if p is not None},
        "pair_counts": {name: len(items) for name, items in sorted(estimates.items())},
    }


def fit_manifest(manifest: dict) -> dict:
    """Fit every camera from the per-pair estimates stored in the manifest, then point
    each file record at its `noise_profile` (its camera's fit, else pooled)."""
    estimates: dict[str, list[PairEstimate]] = defaultdict(list)
    for record in manifest["files"]:
        if record.get("noise_estimate"):
            estimates[record["camera"]].append(PairEstimate(**record["noise_estimate"]))
    doc = fit_all(estimates)
    for record in manifest["files"]:
        record["noise_profile"] = profile_for(record["camera"], doc)
    return doc


def write_params(doc: dict, path: Path = NOISE_PARAMS) -> None:
    path.write_text(json.dumps(doc, indent=1) + "\n", encoding="utf-8")


def main(argv: list[str] | None = None) -> int:
    """Refit the noise parameters from a built manifest (no shard rebuild needed)."""
    parser = argparse.ArgumentParser(prog="python -m rohditor_ml.data.noise.fit", description=main.__doc__)
    parser.add_argument("--manifest", type=Path, default=SHARDS_DIR / "manifest.json")
    args = parser.parse_args(argv)
    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    doc = fit_manifest(manifest)
    doc.update({"subset": manifest["subset"], "seed": manifest["seed"]})
    write_params(doc)
    write_manifest(manifest, args.manifest)
    print(f"fitted {len(doc['cameras'])} cameras -> {NOISE_PARAMS}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
