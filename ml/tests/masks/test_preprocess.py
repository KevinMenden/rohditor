"""Mask-model preprocessing: channel order, normalisation and output handling."""

import numpy as np
from PIL import Image

from rohditor_ml.masks.models import IMAGENET_MEAN, IMAGENET_STD, SKY, SUBJECT
from rohditor_ml.masks.preprocess import to_input, to_probability


def test_input_is_normalised_nchw_in_rgb_order():
    # Red in the top-left quadrant, blue in the bottom-right, black elsewhere:
    # asymmetric in x, in y and in channels, so a flipped or swapped axis shows up.
    rgb = np.zeros((8, 8, 3), np.uint8)
    rgb[:4, :4, 0] = 255
    rgb[4:, 4:, 2] = 255
    x = to_input(Image.fromarray(rgb), 8)  # 8x8 to 8x8: the resize is exact
    assert x.shape == (1, 3, 8, 8) and x.dtype == np.float32
    zero = [-m / s for m, s in zip(IMAGENET_MEAN, IMAGENET_STD)]
    one = [(1 - m) / s for m, s in zip(IMAGENET_MEAN, IMAGENET_STD)]
    np.testing.assert_allclose(x[0, :, 0, 0], [one[0], zero[1], zero[2]], rtol=1e-6)
    np.testing.assert_allclose(x[0, :, 7, 7], [zero[0], zero[1], one[2]], rtol=1e-6)
    np.testing.assert_allclose(x[0, :, 0, 7], zero, rtol=1e-6)
    np.testing.assert_allclose(x[0, :, 7, 0], zero, rtol=1e-6)


def test_input_is_resized_to_a_square():
    x = to_input(Image.new("RGB", (30, 20), (10, 20, 30)), 16)
    assert x.shape == (1, 3, 16, 16)


def test_logits_get_a_sigmoid_and_probabilities_are_clipped():
    with np.errstate(all="raise"):  # very negative logits must not overflow
        p = to_probability(np.array([0.0, 2.0, 100.0, -100.0]), SUBJECT)
    np.testing.assert_allclose(p, [0.5, 1 / (1 + np.exp(-2.0)), 1.0, 0.0], atol=1e-6)
    np.testing.assert_array_equal(to_probability(np.array([-0.1, 0.3, 1.2]), SKY), np.float32([0.0, 0.3, 1.0]))
