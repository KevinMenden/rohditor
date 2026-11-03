"""Calibrating `NoiseParams` from real noisy/clean pairs (RawNIND).

Per noisy image we regress the residual variance against the signal on flat
areas of the clean image: `Var(y - a*g) = K * (a*g) + sigma^2`, where `g` is the
(smoothed) clean image and `a` the gain that matches its exposure to the noisy
one. Per camera we then fit `log sigma` linearly against `log K` across ISO (a straight
line in sigma^2 against K fits badly: sigma^2 spans orders of magnitude over the ISO
range, so the high-ISO end dominates and the intercept goes negative). Flat
areas make the estimate insensitive to the small misalignment between the two
shots.

Known approximations: the clean image's own noise is only reduced (3x3
smoothing), not removed, so use noisy images well above the clean ISO; row
noise is estimated from differences of neighbouring row means and may be
slightly high.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass

import numpy as np
from scipy.ndimage import uniform_filter

from ..bayer import pack
from .model import NoiseParams

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
    k, sigma2 = np.polyfit(means, variances, 1)
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
    """Ranges and the log sigma-vs-log K line for one camera (needs a few ISO levels)."""
    if len(estimates) < MIN_PAIRS_PER_CAMERA:
        return None
    k = np.array([e.k for e in estimates])
    # Pairs whose read noise is below what the regression can resolve (sigma^2 <= 0)
    # still say something about K, but not about sigma.
    resolved = [e for e in estimates if e.sigma2 > 0]
    if len(resolved) < MIN_PAIRS_PER_CAMERA:
        return None
    log_k = np.log([e.k for e in resolved])
    log_sigma = 0.5 * np.log([e.sigma2 for e in resolved])
    slope, intercept = np.polyfit(log_k, log_sigma, 1)
    log_std = float(np.std(log_sigma - (slope * log_k + intercept)))
    row_sigma = float(np.sqrt(max(e.row_var for e in estimates)))
    # A margin lets training cover levels between and a bit beyond the measured ISOs.
    return NoiseParams(float(k.min() * 0.8), float(k.max() * 1.25), float(slope), float(intercept), log_std, row_sigma)


def fit_all(estimates: dict[str, list[PairEstimate]]) -> dict:
    """The JSON document: per-camera params plus pooled params for unseen cameras."""
    cameras = {name: fit_camera(items) for name, items in estimates.items()}
    pooled_input = [e for items in estimates.values() for e in items]
    return {
        "cameras": {name: p.to_dict() for name, p in cameras.items() if p is not None},
        "pooled": (p.to_dict() if (p := fit_camera(pooled_input)) else None),
        "pair_counts": {name: len(items) for name, items in estimates.items()},
    }


def group_by_camera(items: list[tuple[str, PairEstimate]]) -> dict[str, list[PairEstimate]]:
    grouped: dict[str, list[PairEstimate]] = defaultdict(list)
    for camera, estimate in items:
        grouped[camera].append(estimate)
    return grouped
