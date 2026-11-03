"""Aligning a noisy RawNIND capture to its clean reference.

The two shots of a scene differ by a few pixels of camera/subject shift and by
exposure. Like `rawnind_jddc`, we search for the best integer shift on
demosaiced data and match the gain. Unlike it, the noisy mosaic is never moved
(shifting by an odd amount would change the CFA phase); the *clean RGB target*
is shifted instead, since an RGB image can be cropped at any offset. Only
small windows are demosaiced, never whole images.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
from scipy.ndimage import uniform_filter

from .truth import demosaic_window

SEARCH = 3  # pixels; the captures are tripod shots
CROP = 768  # side of the central region used for the search (even)


@dataclass(frozen=True)
class Alignment:
    dy: int  # clean RGB pixel (y + dy, x + dx) corresponds to noisy pixel (y, x)
    dx: int
    gain: float  # noisy ~ gain * clean
    score: float  # mean absolute luma residual; lower is a better match


def _luma(rgb: np.ndarray) -> np.ndarray:
    return uniform_filter(rgb.mean(axis=0), size=5)


def find_alignment(noisy: np.ndarray, clean: np.ndarray) -> Alignment | None:
    """Best integer shift of the `clean` mosaic onto the `noisy` mosaic (same geometry)."""
    h, w = noisy.shape
    if clean.shape != (h, w) or min(h, w) < CROP + 2 * SEARCH + 40:
        return None
    y0, x0 = (h - CROP) // 4 * 2, (w - CROP) // 4 * 2  # even, so the crops keep RGGB phase
    noisy_luma = _luma(demosaic_window(noisy, y0, x0, CROP))
    padded = _luma(demosaic_window(clean, y0 - SEARCH, x0 - SEARCH, CROP + 2 * SEARCH))

    best: Alignment | None = None
    for dy in range(-SEARCH, SEARCH + 1):
        for dx in range(-SEARCH, SEARCH + 1):
            clean_luma = padded[SEARCH + dy : SEARCH + dy + CROP, SEARCH + dx : SEARCH + dx + CROP]
            gain = float((noisy_luma * clean_luma).sum() / max((clean_luma**2).sum(), 1e-12))
            score = float(np.abs(noisy_luma - gain * clean_luma).mean())
            if best is None or score < best.score:
                best = Alignment(dy, dx, gain, score)
    return best


def loss_mask(noisy: np.ndarray, clean_rgb_shifted: np.ndarray, gain: float) -> np.ndarray:
    """Per-pixel validity (uint8 `[H, W]`) of a noisy/clean patch pair.

    Rejects pixels that are overexposed in the clean image and areas where the
    smoothed luma disagrees much more than the typical noise-plus-misalignment
    level (moving objects, specular shifts). A simplified version of
    `rawnind_jddc`'s loss mask, computed at half resolution.
    """
    noisy_luma = uniform_filter(noisy.reshape(noisy.shape[0] // 2, 2, noisy.shape[1] // 2, 2).mean(axis=(1, 3)), 3)
    half_clean = clean_rgb_shifted.reshape(3, noisy.shape[0] // 2, 2, noisy.shape[1] // 2, 2).mean(axis=(2, 4))
    clean_luma = uniform_filter(half_clean.mean(axis=0) * gain, 3)
    diff = np.abs(noisy_luma - clean_luma)
    robust_sigma = 1.4826 * np.median(diff) + 1e-4
    valid = diff < 6 * robust_sigma
    valid &= (half_clean.max(axis=0) < 0.98)
    return np.repeat(np.repeat(valid, 2, axis=0), 2, axis=1).astype(np.uint8)
