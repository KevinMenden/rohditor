"""Validation previews for TensorBoard: bilinear input | network output | target.

Images are balanced linear RGB; the strip is shown through the sRGB transfer
curve, with one exposure per strip so the target's 99th percentile lands at 0.9.
"""

from __future__ import annotations

import torch


def to_display(rgb: torch.Tensor) -> torch.Tensor:
    x = rgb.clamp(0.0, 1.0)
    return torch.where(x <= 0.0031308, 12.92 * x, 1.055 * x.clamp_min(1e-12) ** (1 / 2.4) - 0.055)


def preview_strip(bilinear: torch.Tensor, output: torch.Tensor, target: torch.Tensor) -> torch.Tensor:
    """Three `[3, H, W]` images side by side -> one `[3, H, 3W]` display image in [0, 1]."""
    gain = 0.9 / torch.quantile(target.float().flatten(), 0.99).clamp_min(1e-4)
    return torch.cat([to_display(x.float() * gain) for x in (bilinear, output, target)], dim=2)
