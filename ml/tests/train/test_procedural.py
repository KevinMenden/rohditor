import numpy as np

from rohditor_ml.data.bayer import mosaic_from_rgb, pack
from rohditor_ml.train.procedural import LEVEL_RANGE, procedural_rgb
from rohditor_ml.train.samples import procedural_sample

NOISE = {"cameras": {"cam": {"log_k_points": [-9.0, -8.0], "log_sigma_points": [-8.0, -7.0]}}}


def test_images_are_seeded_in_range_and_varied():
    a = procedural_rgb(np.random.default_rng(1), 64)
    np.testing.assert_array_equal(a, procedural_rgb(np.random.default_rng(1), 64))
    assert a.shape == (3, 64, 64) and a.dtype == np.float32
    images = [procedural_rgb(np.random.default_rng(seed), 64) for seed in range(20)]
    assert all(LEVEL_RANGE[0] - 1e-6 <= im.min() and im.max() <= LEVEL_RANGE[1] + 1e-6 for im in images)
    # Some images hold steps far sharper than photos do: neighbours differing by over 2x.
    assert any((np.maximum(im[:, :, 1:], im[:, :, :-1]) > 2 * np.minimum(im[:, :, 1:], im[:, :, :-1])).any()
               for im in images)


def test_sample_is_balanced_like_the_photo_samples():
    sample = procedural_sample(NOISE, 64, 1.0, np.random.default_rng(3))
    assert sample.input.shape == (4, 32, 32) and sample.target.shape == (3, 64, 64)
    assert sample.mask.shape == (1, 64, 64) and not sample.real
    # Input and target are both balanced: the input is the target's mosaic plus (low) noise.
    clean = pack(mosaic_from_rgb(sample.target))
    assert float(np.abs(sample.input - clean).mean()) < 0.01
