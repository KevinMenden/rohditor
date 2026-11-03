"""RGGB phase handling: the one-pixel-shift-swaps-R-and-B failure mode."""

import numpy as np

from rohditor_ml.contract import PATCH_SIZE, SIZE_MULTIPLE
from rohditor_ml.data import bayer
from rohditor_ml.data.patches import cut, sample_origins

R, G1, G2, B = 1.0, 2.0, 3.0, 4.0


def rggb_pattern(h, w):
    """A mosaic whose value says which colour the photosite is."""
    m = np.empty((h, w), np.float32)
    m[0::2, 0::2], m[0::2, 1::2], m[1::2, 0::2], m[1::2, 1::2] = R, G1, G2, B
    return m


def test_contract_constants():
    assert PATCH_SIZE % 2 == 0 and PATCH_SIZE % SIZE_MULTIPLE == 0


def test_pack_unpack_roundtrip_non_square():
    mosaic = np.random.default_rng(0).random((12, 20)).astype(np.float32)
    packed = bayer.pack(mosaic)
    assert packed.shape == (4, 6, 10)
    np.testing.assert_array_equal(bayer.unpack(packed), mosaic)
    # Channel order is [R, G1, G2, B]: asymmetric values catch any swap.
    np.testing.assert_array_equal(bayer.pack(rggb_pattern(4, 4))[:, 0, 0], [R, G1, G2, B])


def test_origin_for_every_cfa_phase():
    # libraw colour indices: 0 = R, 1 = G, 2 = B, 3 = G2.
    layouts = {
        "RGGB": [[0, 1], [3, 2]],
        "GBRG": [[1, 2], [0, 3]],
        "GRBG": [[1, 0], [2, 3]],
        "BGGR": [[2, 1], [3, 0]],
    }
    for name, tile in layouts.items():
        colors = np.tile(np.array(tile), (4, 4))
        for shift in [(0, 0), (1, 0), (0, 1), (1, 1)]:  # the visible area may start anywhere
            shifted = colors[shift[0] :, shift[1] :]
            dy, dx = bayer.rggb_origin(shifted, r=0, g=(1, 3), b=2)
            cropped = shifted[dy:, dx:]
            assert cropped[0, 0] == 0 and cropped[1, 1] == 2, (name, shift)
            assert cropped[0, 1] in (1, 3) and cropped[1, 0] in (1, 3), (name, shift)


def test_patches_start_on_red_and_keep_phase():
    mosaic = rggb_pattern(700, 900) / 10  # not a multiple of the patch size; below the clipping level
    origins = sample_origins(mosaic, 10, np.random.default_rng(1))
    assert len(origins) == 10
    for origin in origins:
        assert origin[0] % 2 == 0 and origin[1] % 2 == 0
        patch = cut(mosaic, origin)
        assert patch.shape == (PATCH_SIZE, PATCH_SIZE)
        np.testing.assert_allclose(patch[:2, :2] * 10, [[R, G1], [G2, B]], rtol=1e-6)


def test_planes_land_on_the_right_photosites():
    planes = np.zeros((4, 6, 4), np.float32)
    planes[..., :] = [10, 20, 30, 40]  # R, Gr, Gb, B everywhere
    mosaic = bayer.mosaic_from_planes(planes)
    np.testing.assert_array_equal(bayer.pack(mosaic)[:, 0, 0], [10, 20, 30, 40])
    np.testing.assert_array_equal(bayer.rgb_from_planes(planes)[:, 0, 0], [10, 25, 40])


def test_bin2_averages_blocks():
    rgb = np.arange(3 * 4 * 6, dtype=np.float32).reshape(3, 4, 6)
    binned = bayer.bin2(rgb)
    assert binned.shape == (3, 2, 3)
    assert binned[1, 1, 2] == rgb[1, 2:4, 4:6].mean()
