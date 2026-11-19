"""Building one training sample from a stored patch.

Clean patches (synthetic noise), in the order that matters physically:

1. phase-safe geometry and crop (first, so row noise stays row noise);
2. a random downward exposure scale, so nothing exceeds saturation;
3. synthetic noise from the patch's noise profile, in sensor space;
4. random white-balance gains, applied to input and target;
5. packing to `[R, G1, G2, B]`.

Real pairs (RawNIND noisy shots) already carry real noise: they get the same
geometry and white balance, and keep their validity mask.

Every sample is `(input [4, c/2, c/2], target [3, c, c], mask [1, c, c], real)`.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from ..data.bayer import pack
from ..data.noise import add_noise, params_for
from .augment import apply_geometry, packed_gains, random_exposure, random_gains, random_geometry


@dataclass(frozen=True)
class Sample:
    input: np.ndarray
    target: np.ndarray
    mask: np.ndarray
    real: bool


def clean_sample(mosaic, rgb, profile: str, noise_doc: dict, crop: int, exposure_min: float,
                 rng: np.random.Generator) -> Sample:
    g = random_geometry(rng, mosaic.shape[-1], crop)
    mosaic = apply_geometry(np.asarray(mosaic, np.float32), g)
    target = apply_geometry(np.asarray(rgb, np.float32), g)
    scale = random_exposure(rng, exposure_min)
    mosaic, target = mosaic * scale, target * scale
    noisy = add_noise(mosaic, params_for(profile, noise_doc, rng), rng)
    gains = random_gains(rng)
    return Sample(
        input=pack(noisy) * packed_gains(gains),
        target=target * gains[:, None, None],
        mask=np.ones((1, crop, crop), np.float32),
        real=False,
    )


def real_sample(mosaic, rgb, mask, crop: int, rng: np.random.Generator) -> Sample:
    g = random_geometry(rng, mosaic.shape[-1], crop)
    gains = random_gains(rng)
    return Sample(
        input=pack(apply_geometry(np.asarray(mosaic, np.float32), g)) * packed_gains(gains),
        target=apply_geometry(np.asarray(rgb, np.float32), g) * gains[:, None, None],
        mask=apply_geometry(np.asarray(mask, np.float32), g)[None],
        real=True,
    )
