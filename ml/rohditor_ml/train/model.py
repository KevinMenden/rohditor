"""The v1 joint demosaic + denoise U-Net.

Input is the contract's packed, white-balanced mosaic `[B, 4, H/2, W/2]`
(`[R, G1, G2, B]`); output is balanced linear RGB `[B, 3, H, W]`.

Layout: a 3-level U-Net on the half-resolution packed planes (two 3x3
convolutions per level, LeakyReLU(0.2), 2x2 max-pool down, bilinear upsample +
3x3 convolution up, skips by concatenation), then a 12-channel head and
`PixelShuffle(2)` to full resolution. A fixed bilinear demosaic of the input is
added at the end, so the network only learns the correction.

Only convolution, LeakyReLU, max-pool, upsample, concatenation, PixelShuffle and
(for the fixed bilinear branch) reflect padding are used, so the model can later
be ported to shaders. No normalisation layers, no attention.
"""

from __future__ import annotations

import torch
import torch.nn.functional as F
from torch import nn

DEFAULT_CHANNELS = (32, 64, 128, 256)


def _conv(cin: int, cout: int) -> nn.Sequential:
    return nn.Sequential(nn.Conv2d(cin, cout, 3, padding=1), nn.LeakyReLU(0.2))


def _block(cin: int, cout: int) -> nn.Sequential:
    return nn.Sequential(_conv(cin, cout), _conv(cout, cout))


class BilinearDemosaic(nn.Module):
    """Fixed (non-trained) bilinear demosaic of a packed RGGB mosaic.

    The packed channels are scattered into three sparse colour planes (a fixed
    1x1 convolution, then `PixelShuffle(2)`, whose sub-pixel order is exactly
    R, G1, G2, B), which a 3x3 depthwise kernel per colour fills in. Reflect
    padding keeps the CFA phase at the border (pixel -1 mirrors pixel 1).
    """

    def __init__(self):
        super().__init__()
        # Output channel `colour * 4 + subpixel` takes packed channel `subpixel`
        # when that photosite has that colour.
        scatter = torch.zeros(12, 4, 1, 1)
        for subpixel, colour in enumerate((0, 1, 1, 2)):
            scatter[colour * 4 + subpixel, subpixel] = 1.0
        rb = torch.tensor([[1.0, 2.0, 1.0], [2.0, 4.0, 2.0], [1.0, 2.0, 1.0]]) / 4
        g = torch.tensor([[0.0, 1.0, 0.0], [1.0, 4.0, 1.0], [0.0, 1.0, 0.0]]) / 4
        self.register_buffer("scatter", scatter, persistent=False)
        self.register_buffer("kernel", torch.stack([rb, g, rb])[:, None], persistent=False)

    def forward(self, packed: torch.Tensor) -> torch.Tensor:
        sparse = F.pixel_shuffle(F.conv2d(packed, self.scatter), 2)
        return F.conv2d(F.pad(sparse, (1, 1, 1, 1), mode="reflect"), self.kernel, groups=3)


class UNet(nn.Module):
    def __init__(self, channels: tuple[int, ...] = DEFAULT_CHANNELS):
        super().__init__()
        self.encoders = nn.ModuleList()
        cin = 4
        for cout in channels:
            self.encoders.append(_block(cin, cout))
            cin = cout
        self.pool = nn.MaxPool2d(2)
        self.ups = nn.ModuleList()
        self.decoders = nn.ModuleList()
        for cout in reversed(channels[:-1]):
            self.ups.append(nn.Sequential(nn.Upsample(scale_factor=2, mode="bilinear"), _conv(cin, cout)))
            self.decoders.append(_block(2 * cout, cout))
            cin = cout
        self.head = nn.Conv2d(cin, 12, 3, padding=1)
        self.shuffle = nn.PixelShuffle(2)
        self.bilinear = BilinearDemosaic()
        # Start as the bilinear demosaic: the correction begins at exactly zero.
        nn.init.zeros_(self.head.weight)
        nn.init.zeros_(self.head.bias)

    @property
    def size_multiple(self) -> int:
        """Full-resolution H and W must be multiples of this (2 for packing, 2 per pool)."""
        return 2 ** len(self.encoders)

    def forward(self, packed: torch.Tensor) -> torch.Tensor:
        skips = []
        x = packed
        for i, encoder in enumerate(self.encoders):
            if i:
                x = self.pool(x)
            x = encoder(x)
            skips.append(x)
        for up, decoder, skip in zip(self.ups, self.decoders, reversed(skips[:-1]), strict=True):
            x = decoder(torch.cat([up(x), skip], dim=1))
        return self.shuffle(self.head(x)) + self.bilinear(packed)


def parameter_count(model: nn.Module) -> int:
    return sum(p.numel() for p in model.parameters())


def macs_per_megapixel(model: UNet, size: int = 256) -> float:
    """Multiply-accumulates per output megapixel, counted over every convolution.

    Measured on a `size`-square output with forward hooks; the model is fully
    convolutional, so the per-pixel cost is the same at any size (upsampling and
    pooling are not counted).
    """
    total = 0

    def hook(module: nn.Conv2d, _inputs, output: torch.Tensor) -> None:
        nonlocal total
        kh, kw = module.kernel_size
        total += output.numel() * (module.in_channels // module.groups) * kh * kw

    handles = [m.register_forward_hook(hook) for m in model.modules() if isinstance(m, nn.Conv2d)]
    try:
        with torch.no_grad():
            model(torch.zeros(1, 4, size // 2, size // 2, device=next(model.parameters()).device))
    finally:
        for handle in handles:
            handle.remove()
    return total / (size * size / 1e6)
