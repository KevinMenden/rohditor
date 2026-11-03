"""Synthetic noise for contract-scaled RGGB mosaics.

A photosite with clean value `x` (normalised: 1.0 is saturation) becomes

    y = K * Poisson(x / K) + Normal(0, sigma^2)

so `Var(y) = K * x + sigma^2`: `K` is the shot-noise gain and `sigma` the read
noise (which, across ISO, follows a power law of `K`). On top of that come optional per-row offsets (banding) and a small random
black-level offset per colour plane. `NoiseParams` are *ranges* per camera
(saved in `ml/data/noise_params.json`); each call to `add_noise` draws one
concrete `NoiseSample` from them, so every training patch gets its own ISO-like
noise level.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass

import numpy as np


@dataclass(frozen=True)
class NoiseParams:
    k_min: float
    k_max: float
    # Read noise follows a power law of the shot gain across ISO:
    # log(sigma) = sigma_log_slope * log(K) + sigma_log_intercept.
    sigma_log_slope: float
    sigma_log_intercept: float
    sigma_log_std: float = 0.0  # lognormal jitter of sigma around that line
    row_sigma_max: float = 0.0  # row-noise std is drawn uniformly from [0, max]
    black_offset_std: float = 3e-4  # a prior (about 5 counts at 14 bit), not fitted

    @classmethod
    def from_dict(cls, values: dict) -> NoiseParams:
        return cls(**values)

    def to_dict(self) -> dict:
        return asdict(self)


@dataclass(frozen=True)
class NoiseSample:
    k: float
    sigma: float
    row_sigma: float
    black_offsets: tuple[float, float, float, float]  # per 2x2 position: R, G1, G2, B


def sample_noise(params: NoiseParams, rng: np.random.Generator) -> NoiseSample:
    """Draw one noise level: K log-uniform, sigma from the fitted line in log-log space."""
    log_k = rng.uniform(np.log(params.k_min), np.log(params.k_max))
    log_sigma = params.sigma_log_slope * log_k + params.sigma_log_intercept + rng.normal(0.0, params.sigma_log_std)
    k, sigma = float(np.exp(log_k)), float(np.exp(log_sigma))
    row_sigma = float(rng.uniform(0.0, params.row_sigma_max))
    offsets = tuple(float(v) for v in rng.normal(0.0, params.black_offset_std, 4))
    return NoiseSample(k, sigma, row_sigma, offsets)


def apply_noise(mosaic: np.ndarray, sample: NoiseSample, rng: np.random.Generator) -> np.ndarray:
    """Add the given noise to a clean `[H, W]` mosaic (float32 in, float32 out)."""
    h, w = mosaic.shape
    # Shot noise needs a non-negative mean; negative clean values (black-level
    # undershoot) just get read noise.
    shot = sample.k * rng.poisson(np.maximum(mosaic, 0.0) / sample.k)
    noisy = np.where(mosaic > 0, shot, mosaic) + rng.normal(0.0, sample.sigma, mosaic.shape)
    if sample.row_sigma > 0:
        noisy += rng.normal(0.0, sample.row_sigma, (h, 1))
    offsets = np.array(sample.black_offsets).reshape(2, 2)
    noisy += np.tile(offsets, (h // 2, w // 2))
    # A photosite that was saturated stays saturated.
    noisy[mosaic >= 1.0] = 1.0
    return noisy.astype(np.float32)


def add_noise(mosaic: np.ndarray, params: NoiseParams, rng: np.random.Generator) -> np.ndarray:
    return apply_noise(mosaic, sample_noise(params, rng), rng)
