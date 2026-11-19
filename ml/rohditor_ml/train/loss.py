"""Training loss: L1 after a signed gamma, masked, full-res or 2x2-binned per sample.

Linear L1 overweights highlights, so both sides go through
`sign(x) * |x|^(1/2.2)` first. The contract keeps negative values, so a plain
gamma or a log would fail; a small offset keeps the gradient finite at 0
while staying continuous.

Real pairs are compared after 2x2 binning: their target is an edge-aware
demosaic of the clean shot, which has its own demosaic error. Binning keeps the
real-noise signal without teaching the model the reference demosaicer's
artifacts. A binned pixel is valid only if all four of its pixels are.
"""

from __future__ import annotations

import torch
import torch.nn.functional as F

GAMMA = 1 / 2.2
EPS = 1e-4


def signed_gamma(x: torch.Tensor) -> torch.Tensor:
    return torch.sign(x) * ((x.abs() + EPS) ** GAMMA - EPS**GAMMA)


def _masked_l1(output: torch.Tensor, target: torch.Tensor, mask: torch.Tensor) -> torch.Tensor:
    """Per-sample mean |output - target| over valid pixels -> `[B]`."""
    diff = (signed_gamma(output) - signed_gamma(target)).abs() * mask
    return diff.sum(dim=(1, 2, 3)) / (mask.sum(dim=(1, 2, 3)) * output.shape[1]).clamp_min(1.0)


def bin2(x: torch.Tensor) -> torch.Tensor:
    return F.avg_pool2d(x, 2)


def loss_per_sample(output: torch.Tensor, target: torch.Tensor, mask: torch.Tensor, real: torch.Tensor) -> torch.Tensor:
    """`[B]` losses: full resolution for synthetic samples, binned for real pairs (`real` = 1)."""
    output, target = output.float(), target.float()
    full = _masked_l1(output, target, mask)
    if not bool(real.any()):
        return full
    binned = _masked_l1(bin2(output), bin2(target), -F.max_pool2d(-mask, 2))  # min-pool: all four valid
    return torch.where(real > 0.5, binned, full)


def psnr(output: torch.Tensor, target: torch.Tensor, mask: torch.Tensor, real: torch.Tensor) -> torch.Tensor:
    """`[B]` linear PSNR (peak 1), on the same resolution the loss uses for each sample."""
    output, target = output.float(), target.float()

    def mse(o, t, m):
        return ((o - t) ** 2 * m).sum(dim=(1, 2, 3)) / (m.sum(dim=(1, 2, 3)) * o.shape[1]).clamp_min(1.0)

    error = torch.where(real > 0.5, mse(bin2(output), bin2(target), -F.max_pool2d(-mask, 2)),
                        mse(output, target, mask))
    return -10 * torch.log10(error.clamp_min(1e-12))
