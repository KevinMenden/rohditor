"""Procedural training images: hard edges and fine periodic detail.

Photos rarely hold a perfectly sharp step or detail near the Nyquist frequency
(lenses and anti-aliasing filters blur them), so a model trained on photos alone
fringes on such edges, which sharp lenses on filterless sensors do produce. These
images are random shapes in random colours, some filled with gratings up to the
Nyquist frequency, rendered through a supersampled box filter: the pixel aperture
and nothing else.

Images are drawn in white-balanced RGB, so achromatic shapes are truly grey after
balance (where demosaicing fringes are most visible).
"""

from __future__ import annotations

import numpy as np

SUPERSAMPLE = 4
LEVEL_RANGE = (0.01, 0.9)  # linear, drawn log-uniformly so shadows get their share
GREY_PROBABILITY = 0.5  # share of achromatic colours
GRATING_PROBABILITY = 0.3  # share of shapes filled with a grating instead of a flat colour
FREQUENCY_RANGE = (0.05, 0.5)  # grating frequency in cycles per pixel; 0.5 is Nyquist
SHAPES_RANGE = (2, 7)  # shapes per image, upper end exclusive


def _colour(rng: np.random.Generator) -> np.ndarray:
    levels = np.exp(rng.uniform(*np.log(LEVEL_RANGE), 3))
    if rng.random() < GREY_PROBABILITY:
        levels[:] = levels[0]
    return levels.astype(np.float32)[:, None, None]


def _region(rng: np.random.Generator, yy: np.ndarray, xx: np.ndarray, size: int) -> np.ndarray:
    """A random half-plane or disc, as a boolean mask on the supersampled grid."""
    cy, cx = rng.uniform(0, size, 2)
    if rng.random() < 0.5:
        angle = rng.uniform(0, 2 * np.pi)
        return (yy - cy) * np.sin(angle) + (xx - cx) * np.cos(angle) > 0
    radius = np.exp(rng.uniform(np.log(2.0), np.log(size)))
    return (yy - cy) ** 2 + (xx - cx) ** 2 < radius**2


def _fill(rng: np.random.Generator, yy: np.ndarray, xx: np.ndarray) -> np.ndarray:
    """A flat colour, or a grating between two colours at a random frequency and angle."""
    if rng.random() >= GRATING_PROBABILITY:
        return _colour(rng)
    frequency = np.exp(rng.uniform(*np.log(FREQUENCY_RANGE)))
    angle = rng.uniform(0, np.pi)
    phase = 2 * np.pi * frequency * (yy * np.sin(angle) + xx * np.cos(angle)) + rng.uniform(0, 2 * np.pi)
    weight = (0.5 + 0.5 * np.cos(phase)).astype(np.float32)
    a, b = _colour(rng), _colour(rng)
    return a + (b - a) * weight


def procedural_rgb(rng: np.random.Generator, size: int) -> np.ndarray:
    """A `[3, size, size]` balanced linear RGB image of overlapping random shapes."""
    n = size * SUPERSAMPLE
    coords = ((np.arange(n) + 0.5) / SUPERSAMPLE).astype(np.float32)
    yy, xx = coords[:, None], coords[None, :]
    image = np.broadcast_to(_colour(rng), (3, n, n)).copy()
    for _ in range(rng.integers(*SHAPES_RANGE)):
        image = np.where(_region(rng, yy, xx, size), _fill(rng, yy, xx), image)
    return image.reshape(3, size, SUPERSAMPLE, size, SUPERSAMPLE).mean(axis=(2, 4))
