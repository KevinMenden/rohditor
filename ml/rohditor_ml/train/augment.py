"""Per-sample augmentation of contract-scaled RGGB patches (numpy, in loader workers).

Geometry has to keep the CFA phase:

* transposing an RGGB patch keeps RGGB (R and B stay on the diagonal, the two
  greens swap), so it needs no correction;
* a flip of an even-sized patch moves row/column 0 to an odd index, which turns
  RGGB into GBRG/GRBG, so the crop on a flipped axis starts at an odd offset,
  which restores RGGB.

The crop is `crop` square out of the stored `PATCH_SIZE` patches.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from ..contract import WB_BLUE_RANGE, WB_RED_RANGE


@dataclass(frozen=True)
class Geometry:
    transpose: bool
    flip_y: bool
    flip_x: bool
    y0: int
    x0: int
    crop: int


def _offset(rng: np.random.Generator, room: int, odd: bool) -> int:
    """A random crop offset in `[0, room]` with the given parity (`room` is even)."""
    if odd:
        return 2 * int(rng.integers(0, room // 2)) + 1
    return 2 * int(rng.integers(0, room // 2 + 1))


def random_geometry(rng: np.random.Generator, size: int, crop: int) -> Geometry:
    room = size - crop
    if room < 2 or room % 2 or crop % 2:
        raise ValueError(f"crop {crop} from {size} needs an even margin of at least 2")
    transpose, flip_y, flip_x = (bool(v) for v in rng.integers(0, 2, 3))
    return Geometry(transpose, flip_y, flip_x, _offset(rng, room, flip_y), _offset(rng, room, flip_x), crop)


def apply_geometry(array: np.ndarray, g: Geometry) -> np.ndarray:
    """Transform the last two axes of `array` (a mosaic, an RGB image or a mask)."""
    if g.transpose:
        array = np.swapaxes(array, -1, -2)
    if g.flip_y:
        array = array[..., ::-1, :]
    if g.flip_x:
        array = array[..., :, ::-1]
    return np.ascontiguousarray(array[..., g.y0 : g.y0 + g.crop, g.x0 : g.x0 + g.crop])


def random_exposure(rng: np.random.Generator, minimum: float) -> float:
    """A downward-only exposure scale, log-uniform in `[minimum, 1]` (nothing exceeds saturation)."""
    return float(np.exp(rng.uniform(np.log(minimum), 0.0)))


def random_gains(rng: np.random.Generator) -> np.ndarray:
    """White-balance gains `[R, G, B]` (green = 1) from the contract's training range."""
    return np.array([rng.uniform(*WB_RED_RANGE), 1.0, rng.uniform(*WB_BLUE_RANGE)], np.float32)


def packed_gains(gains: np.ndarray) -> np.ndarray:
    """`[R, G, B]` gains as a `[4, 1, 1]` multiplier for packed `[R, G1, G2, B]` planes."""
    return np.array([gains[0], gains[1], gains[1], gains[2]], np.float32)[:, None, None]
