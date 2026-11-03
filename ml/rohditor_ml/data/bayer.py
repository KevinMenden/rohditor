"""RGGB Bayer helpers.

Everything here assumes the contract's phase: the pixel at (0, 0) is red,
(0, 1) is green ("G1"/Gr), (1, 0) is green ("G2"/Gb) and (1, 1) is blue. Mosaics are
`[H, W]` arrays; full-sampled planes are `[H, W, 4]` in the order R, Gr, Gb, B;
RGB images are `[3, H, W]`.
"""

from __future__ import annotations

import numpy as np

R, G1, G2, B = 0, 1, 2, 3


def pack(mosaic: np.ndarray) -> np.ndarray:
    """`[H, W]` mosaic -> `[4, H/2, W/2]` channels [R, G1, G2, B] (the network input)."""
    return np.stack([mosaic[0::2, 0::2], mosaic[0::2, 1::2], mosaic[1::2, 0::2], mosaic[1::2, 1::2]])


def unpack(packed: np.ndarray) -> np.ndarray:
    """Inverse of `pack`."""
    _, h, w = packed.shape
    mosaic = np.empty((2 * h, 2 * w), packed.dtype)
    mosaic[0::2, 0::2], mosaic[0::2, 1::2] = packed[R], packed[G1]
    mosaic[1::2, 0::2], mosaic[1::2, 1::2] = packed[G2], packed[B]
    return mosaic


def mosaic_from_planes(planes: np.ndarray) -> np.ndarray:
    """Sample a full-sampled `[H, W, 4]` (R, Gr, Gb, B) image through the RGGB filter."""
    mosaic = np.empty(planes.shape[:2], planes.dtype)
    mosaic[0::2, 0::2] = planes[0::2, 0::2, R]
    mosaic[0::2, 1::2] = planes[0::2, 1::2, G1]
    mosaic[1::2, 0::2] = planes[1::2, 0::2, G2]
    mosaic[1::2, 1::2] = planes[1::2, 1::2, B]
    return mosaic


def rgb_from_planes(planes: np.ndarray) -> np.ndarray:
    """Full-sampled planes -> `[3, H, W]` RGB, with green the mean of Gr and Gb."""
    return np.stack([planes[..., R], 0.5 * (planes[..., G1] + planes[..., G2]), planes[..., B]])


def mosaic_from_rgb(rgb: np.ndarray) -> np.ndarray:
    """Sample a `[3, H, W]` RGB image through the RGGB filter."""
    _, h, w = rgb.shape
    mosaic = np.empty((h, w), rgb.dtype)
    mosaic[0::2, 0::2] = rgb[0, 0::2, 0::2]
    mosaic[0::2, 1::2] = rgb[1, 0::2, 1::2]
    mosaic[1::2, 0::2] = rgb[1, 1::2, 0::2]
    mosaic[1::2, 1::2] = rgb[2, 1::2, 1::2]
    return mosaic


def bin2(rgb: np.ndarray) -> np.ndarray:
    """2x2 average of a `[3, H, W]` image (H and W even) -> `[3, H/2, W/2]`."""
    c, h, w = rgb.shape
    return rgb.reshape(c, h // 2, 2, w // 2, 2).mean(axis=(2, 4))


def rggb_origin(colors: np.ndarray, r: int, g: tuple[int, ...], b: int) -> tuple[int, int]:
    """Find the offset `(dy, dx)` in {0, 1}^2 where the CFA reads R G / G B.

    `colors` is the per-photosite colour index of the sensor (libraw's
    `raw_colors_visible`); `r`, `g`, `b` are the indices that mean red, green
    and blue. Cropping the mosaic at that offset gives RGGB phase. Skipping this
    step on a GBRG or BGGR sensor silently swaps red and blue.
    """
    for dy in (0, 1):
        for dx in (0, 1):
            if (
                colors[dy, dx] == r
                and colors[dy, dx + 1] in g
                and colors[dy + 1, dx] in g
                and colors[dy + 1, dx + 1] == b
            ):
                return dy, dx
    raise ValueError("sensor is not a 2x2 Bayer pattern with R, G, G, B")
