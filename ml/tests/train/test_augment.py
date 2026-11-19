"""Phase-safe geometry: after any transpose/flip/crop, the mosaic must still be
the RGGB sampling of the transformed RGB. A one-pixel phase error swaps R and B."""

import itertools

import numpy as np
import pytest

from rohditor_ml.data.bayer import mosaic_from_rgb
from rohditor_ml.train.augment import Geometry, apply_geometry, random_geometry

SIZE, CROP = 32, 24


def _pair(seed=0):
    rng = np.random.default_rng(seed)
    rgb = rng.random((3, SIZE, SIZE)).astype(np.float32)
    rgb[:, :, : SIZE // 3] *= 0.5  # asymmetric, so a flip is never a no-op
    return mosaic_from_rgb(rgb), rgb


@pytest.mark.parametrize("transpose,flip_y,flip_x", list(itertools.product([False, True], repeat=3)))
def test_every_geometry_keeps_rggb_phase(transpose, flip_y, flip_x):
    mosaic, rgb = _pair()
    for y0 in range(SIZE - CROP + 1):
        for x0 in range(SIZE - CROP + 1):
            if y0 % 2 != flip_y or x0 % 2 != flip_x:
                continue
            g = Geometry(transpose, flip_y, flip_x, y0, x0, CROP)
            np.testing.assert_array_equal(apply_geometry(mosaic, g), mosaic_from_rgb(apply_geometry(rgb, g)))


def test_transpose_keeps_phase_without_shift():
    mosaic, rgb = _pair(1)
    g = Geometry(True, False, False, 0, 0, CROP)
    out = apply_geometry(mosaic, g)
    # Red stays on even/even, blue on odd/odd; the greens swap places.
    np.testing.assert_array_equal(out[0::2, 0::2], mosaic[0:CROP:2, 0:CROP:2].T)
    np.testing.assert_array_equal(out[1::2, 1::2], mosaic[1:CROP:2, 1:CROP:2].T)
    np.testing.assert_array_equal(out, mosaic_from_rgb(apply_geometry(rgb, g)))


def test_flip_without_odd_offset_breaks_phase():
    """The check above is not vacuous: a flip cropped at an even offset is GRBG, not RGGB."""
    mosaic, rgb = _pair(2)
    g = Geometry(False, False, True, 0, 0, CROP)
    assert not np.array_equal(apply_geometry(mosaic, g), mosaic_from_rgb(apply_geometry(rgb, g)))


def test_random_geometry_picks_restoring_offsets():
    rng = np.random.default_rng(3)
    mosaic, rgb = _pair(3)
    for _ in range(200):
        g = random_geometry(rng, SIZE, CROP)
        assert g.y0 % 2 == g.flip_y and g.x0 % 2 == g.flip_x
        assert 0 <= g.y0 <= SIZE - CROP and 0 <= g.x0 <= SIZE - CROP
        np.testing.assert_array_equal(apply_geometry(mosaic, g), mosaic_from_rgb(apply_geometry(rgb, g)))


def test_random_geometry_rejects_odd_margins():
    with pytest.raises(ValueError):
        random_geometry(np.random.default_rng(0), SIZE, SIZE - 1)
