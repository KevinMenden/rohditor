"""Ground truth: exact re-mosaicking, and OpenCV's demosaic orientation."""

import numpy as np

from rohditor_ml.data import bayer, truth
from rohditor_ml.data.decode import PixelShiftScene


def test_pixelshift_truth_reproduces_the_mosaic():
    rng = np.random.default_rng(0)
    planes = rng.random((10, 14, 4)).astype(np.float32)
    scene = PixelShiftScene(planes, np.ones(3, np.float32), "test")
    mosaic, rgb = truth.from_pixelshift(scene)
    remosaic = bayer.mosaic_from_rgb(rgb)
    # Red and blue photosites are reproduced bit-exactly ...
    for sl in [(slice(0, None, 2), slice(0, None, 2)), (slice(1, None, 2), slice(1, None, 2))]:
        np.testing.assert_array_equal(remosaic[sl], mosaic[sl])
    # ... green photosites hold the mean of the two shifted green measurements.
    green = (slice(0, None, 2), slice(1, None, 2))
    np.testing.assert_allclose(remosaic[green], 0.5 * (planes[green + (1,)] + planes[green + (2,)]), rtol=1e-6)
    # When Gr == Gb the whole mosaic is reproduced exactly.
    planes[..., 2] = planes[..., 1]
    mosaic, rgb = truth.from_pixelshift(PixelShiftScene(planes, np.ones(3, np.float32), "test"))
    np.testing.assert_array_equal(bayer.mosaic_from_rgb(rgb), mosaic)


def test_clean_mosaic_truth_reproduces_its_input():
    mosaic = np.random.default_rng(1).random((64, 96)).astype(np.float32)
    binned_mosaic, rgb = truth.from_clean_mosaic(mosaic)
    assert binned_mosaic.shape == (32, 48) and rgb.shape == (3, 32, 48)
    np.testing.assert_array_equal(bayer.mosaic_from_rgb(rgb), binned_mosaic)


def colour_quadrants():
    """RGB: red top-left, blue bottom-right, green elsewhere (asymmetric, edges on even pixels)."""
    rgb = np.zeros((3, 32, 32), np.float32)
    rgb[0, :16, :16] = 1
    rgb[2, 16:, 16:] = 1
    rgb[1, :16, 16:] = 1
    rgb[1, 16:, :16] = 1
    return rgb


def test_demosaic_keeps_red_and_blue_in_place():
    # Guards the OpenCV constant: a wrong Bayer pattern swaps red and blue.
    rgb = colour_quadrants()
    out = truth.demosaic_ea(bayer.mosaic_from_rgb(rgb) * 0.9)
    np.testing.assert_allclose(out[:, 4:12, 4:12].mean(axis=(1, 2)), [0.9, 0, 0], atol=0.02)
    np.testing.assert_allclose(out[:, 20:28, 20:28].mean(axis=(1, 2)), [0, 0, 0.9], atol=0.02)


def test_demosaic_keeps_negative_values():
    mosaic = bayer.mosaic_from_rgb(colour_quadrants()) * 0.5 - 0.01
    out = truth.demosaic_ea(mosaic)
    assert out.min() < -0.005 and abs(out[0, 8, 8] - 0.49) < 0.01


def test_window_matches_full_demosaic_at_odd_offsets():
    rng = np.random.default_rng(2)
    yy, xx = np.mgrid[0:96, 0:128]
    smooth = (0.3 + 0.2 * np.sin(yy / 9) * np.cos(xx / 13)).astype(np.float32)
    mosaic = smooth * (1 + 0.1 * rng.random(smooth.shape).astype(np.float32))
    full = truth.demosaic_ea(mosaic)
    for y, x in [(21, 33), (20, 34), (31, 47)]:  # odd and even offsets
        window = truth.demosaic_window(mosaic, y, x, 32)
        np.testing.assert_allclose(window, full[:, y : y + 32, x : x + 32], atol=2e-3)
