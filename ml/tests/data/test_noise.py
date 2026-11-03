"""The noise model: statistics, saturation, banding, and calibration."""

import numpy as np

from rohditor_ml.data.noise import NoiseParams, NoiseSample, add_noise, apply_noise, sample_noise
from rohditor_ml.data.noise.fit import estimate_pair, fit_camera

K, SIGMA = 1e-3, 2e-3


def test_flat_patch_variance_matches_the_model():
    rng = np.random.default_rng(0)
    for level in (0.02, 0.3, 0.8):
        flat = np.full((512, 512), level, np.float32)
        noisy = apply_noise(flat, NoiseSample(K, SIGMA, 0.0, (0, 0, 0, 0)), rng)
        assert abs(noisy.var() / (K * level + SIGMA**2) - 1) < 0.03
        assert abs(noisy.mean() - level) < 5e-4


def test_saturated_photosites_stay_saturated_and_dark_ones_get_read_noise():
    rng = np.random.default_rng(0)
    clean = np.zeros((64, 64), np.float32)
    clean[:, :32] = 1.0
    clean[:, 32:] = -0.01  # black-level undershoot
    noisy = apply_noise(clean, NoiseSample(K, SIGMA, 0.0, (0, 0, 0, 0)), rng)
    assert (noisy[:, :32] == 1.0).all()
    assert abs(noisy[:, 32:].std() - SIGMA) < 3e-4 and abs(noisy[:, 32:].mean() + 0.01) < 3e-4


def test_row_noise_is_constant_along_rows_and_offsets_follow_the_cfa():
    rng = np.random.default_rng(0)
    flat = np.full((256, 256), 0.2, np.float32)
    banded = apply_noise(flat, NoiseSample(1e-9, 1e-6, 0.01, (0, 0, 0, 0)), rng)
    row_means = banded.mean(axis=1)
    assert abs(row_means.std() - 0.01) < 0.002 and banded.std(axis=1).max() < 1e-4
    offsets = apply_noise(flat, NoiseSample(1e-9, 1e-6, 0.0, (0.01, 0.02, 0.03, 0.04)), rng)
    np.testing.assert_allclose([offsets[0, 0], offsets[0, 1], offsets[1, 0], offsets[1, 1]],
                               [0.21, 0.22, 0.23, 0.24], atol=1e-4)


def test_add_noise_is_reproducible_and_samples_within_range():
    params = NoiseParams(1e-4, 1e-2, 0.5, -2.4, 0.05, 1e-3)
    mosaic = np.full((32, 32), 0.4, np.float32)
    np.testing.assert_array_equal(add_noise(mosaic, params, np.random.default_rng(5)),
                                  add_noise(mosaic, params, np.random.default_rng(5)))
    rng = np.random.default_rng(0)
    ks = [sample_noise(params, rng).k for _ in range(500)]
    assert 1e-4 <= min(ks) and max(ks) <= 1e-2


def test_calibration_recovers_known_parameters():
    rng = np.random.default_rng(0)
    yy, xx = np.mgrid[0:1024, 0:1024]
    clean = (0.02 + 0.6 * (xx / 1024) ** 1.5).astype(np.float32)
    estimates = []
    for k in (2e-4, 5e-4, 1e-3, 2e-3):
        sigma = 0.0949 * np.sqrt(k)  # the power law the fit should find: slope 0.5
        noisy = apply_noise(clean, NoiseSample(k, sigma, 0.0, (0, 0, 0, 0)), rng)
        estimate = estimate_pair(noisy, clean, 100 * k * 1e3, rng)
        assert abs(estimate.k / k - 1) < 0.05
        estimates.append(estimate)
    params = fit_camera(estimates)
    assert abs(params.sigma_log_slope - 0.5) < 0.1 and abs(params.sigma_log_intercept - np.log(0.0949)) < 0.3
    assert params.k_min < 2e-4 and params.k_max > 2e-3
