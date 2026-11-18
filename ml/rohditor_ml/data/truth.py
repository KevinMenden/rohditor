"""Ground-truth construction: pairs of (clean input mosaic, target RGB).

Three recipes, all returning `(mosaic [H, W], rgb [3, H, W])` in the same
scaling and with the mosaic exactly equal to the RGGB sampling of `rgb`'s
sources:

* `from_pixelshift`: pixel shift measured R, Gr, Gb and B at every pixel, so the
  target is real sensor data with no demosaic error. The mosaic is the RGGB
  sampling of the same planes.
* `from_clean_mosaic`: for ordinary clean raws. The mosaic is demosaiced at full
  size (OpenCV edge-aware, as RawNIND does) and 2x2-binned, which averages away
  most demosaic error. The binned RGB is the target and its RGGB re-sampling the
  input (so the pair has half the sensor's resolution).
* `demosaic_ea` / `demosaic_window`: the full-size edge-aware demosaic on its
  own, used as the target for real noisy RawNIND pairs (where the input must
  stay full size).
"""

from __future__ import annotations

import cv2
import numpy as np

from . import bayer
from .decode import PixelShiftScene


def from_pixelshift(scene: PixelShiftScene) -> tuple[np.ndarray, np.ndarray]:
    return bayer.mosaic_from_planes(scene.planes), bayer.rgb_from_planes(scene.planes)


def demosaic_ea(mosaic: np.ndarray) -> np.ndarray:
    """Edge-aware demosaic of a contract-scaled RGGB mosaic -> float32 `[3, H, W]`.

    OpenCV needs uint16, so values are shifted and scaled into range and mapped
    back afterwards; that keeps negative values and anything above 1.0.
    """
    low, high = min(float(mosaic.min()), 0.0), max(float(mosaic.max()), 1.0)
    scale = 65535.0 / (high - low)
    quantised = np.rint((mosaic - low) * scale).astype(np.uint16)
    rgb = cv2.demosaicing(quantised, cv2.COLOR_BayerRGGB2RGB_EA)  # [H, W, 3]
    return (rgb.transpose(2, 0, 1).astype(np.float32) / scale + low).astype(np.float32)


def demosaic_window(mosaic: np.ndarray, y: int, x: int, size: int, pad: int = 8) -> np.ndarray:
    """`demosaic_ea` of only the `size`-square window at any `(y, x)` -> `[3, size, size]`.

    The demosaiced crop must start on an even row/column to keep RGGB phase, so
    it begins at the next even offset below `(y - pad, x - pad)`; the padding
    keeps the edge-aware filter's border effects outside the returned window.
    """
    y0, x0 = max((y - pad) // 2 * 2, 0), max((x - pad) // 2 * 2, 0)
    y1, x1 = min(y + size + pad, mosaic.shape[0]), min(x + size + pad, mosaic.shape[1])
    rgb = demosaic_ea(mosaic[y0 : y1 // 2 * 2, x0 : x1 // 2 * 2])
    return rgb[:, y - y0 : y - y0 + size, x - x0 : x - x0 + size]


def from_clean_mosaic(mosaic: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    rgb = bayer.bin2(demosaic_ea(mosaic))
    return bayer.mosaic_from_rgb(rgb), rgb
