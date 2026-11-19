"""Image-quality metrics on `[3, H, W]` balanced linear RGB.

Each metric is reported in linear (peak 1.0, values unclipped) and in a
display-referred view (clipped to [0, 1], sRGB transfer curve), which weights
shadows the way a viewer sees them. SSIM is the usual Gaussian-window form
(sigma 1.5), averaged over channels.
"""

from __future__ import annotations

import numpy as np
from scipy.ndimage import gaussian_filter

MARGIN = 8  # border pixels left out of every metric, the same for every method


def to_display(rgb: np.ndarray) -> np.ndarray:
    x = np.clip(rgb, 0.0, 1.0)
    return np.where(x <= 0.0031308, 12.92 * x, 1.055 * np.power(x, 1 / 2.4) - 0.055)


def psnr(output: np.ndarray, target: np.ndarray, mask: np.ndarray | None = None) -> float:
    err = (output - target) ** 2
    mse = float(err.mean()) if mask is None else float((err * mask).sum() / max(mask.sum() * len(err), 1))
    return float(10 * np.log10(1.0 / max(mse, 1e-12)))


def ssim(output: np.ndarray, target: np.ndarray, mask: np.ndarray | None = None) -> float:
    c1, c2 = 0.01**2, 0.03**2

    def blur(x):
        return gaussian_filter(x, sigma=(0, 1.5, 1.5), truncate=3.5)

    mu_o, mu_t = blur(output), blur(target)
    var_o = blur(output * output) - mu_o**2
    var_t = blur(target * target) - mu_t**2
    cov = blur(output * target) - mu_o * mu_t
    value = ((2 * mu_o * mu_t + c1) * (2 * cov + c2)) / ((mu_o**2 + mu_t**2 + c1) * (var_o + var_t + c2))
    if mask is None:
        return float(value.mean())
    return float((value * mask).sum() / max(mask.sum() * len(value), 1))


def bin2(x: np.ndarray) -> np.ndarray:
    c, h, w = x.shape
    return x.reshape(c, h // 2, 2, w // 2, 2).mean(axis=(2, 4))


def scores(output: np.ndarray, target: np.ndarray, mask: np.ndarray | None = None, binned: bool = False) -> dict:
    """All four metrics for one image. Real pairs are scored 2x2-binned, as in training
    (their target carries its own demosaic error); a binned pixel needs all four valid."""
    m = MARGIN
    output, target = output[:, m:-m, m:-m].astype(np.float64), target[:, m:-m, m:-m].astype(np.float64)
    if mask is not None:
        mask = mask[None, m:-m, m:-m].astype(np.float64)
    if binned:
        output, target = bin2(output), bin2(target)
        if mask is not None:
            mask = (bin2(mask) > 0.999).astype(np.float64)
    out_d, tgt_d = to_display(output), to_display(target)
    return {"psnr": psnr(output, target, mask), "ssim": ssim(output, target, mask),
            "psnr_display": psnr(out_d, tgt_d, mask), "ssim_display": ssim(out_d, tgt_d, mask)}
