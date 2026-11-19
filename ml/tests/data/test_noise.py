"""The noise model: statistics, saturation, banding, and calibration."""

import numpy as np

from rohditor_ml.data.noise import POOLED, NoiseParams, NoiseSample, add_noise, apply_noise, params_for, sample_noise
from rohditor_ml.data.noise.fit import estimate_pair, fit_camera, fit_manifest

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
    params = NoiseParams((np.log(1e-4), np.log(1e-2)), (np.log(3e-4), np.log(3e-2)), 0.05, 0.2)
    mosaic = np.full((32, 32), 0.4, np.float32)
    np.testing.assert_array_equal(add_noise(mosaic, params, np.random.default_rng(5)),
                                  add_noise(mosaic, params, np.random.default_rng(5)))
    rng = np.random.default_rng(0)
    ks = [sample_noise(params, rng).k for _ in range(500)]
    assert 1e-4 <= min(ks) and max(ks) <= 1e-2


def dual_gain_sigma(k: float) -> float:
    """A dual-gain sensor: 6 electrons of read noise below K = 1e-3, 2 above."""
    return (6 if k < 1e-3 else 2) * k


def gradient_clean() -> np.ndarray:
    _, xx = np.mgrid[0:1024, 0:1024]
    return (0.02 + 0.6 * (xx / 1024) ** 1.5).astype(np.float32)


def test_calibration_follows_a_dual_gain_read_noise_curve():
    rng = np.random.default_rng(0)
    clean = gradient_clean()
    ks = (2e-4, 5e-4, 8e-4, 1.6e-3, 3.2e-3, 6.4e-3)
    estimates = []
    for iso, k in enumerate(ks, 1):
        noisy = apply_noise(clean, NoiseSample(k, dual_gain_sigma(k), 0.0, (0, 0, 0, 0)), rng)
        estimate = estimate_pair(noisy, clean, 100 * iso, rng)
        assert abs(estimate.k / k - 1) < 0.05
        estimates.append(estimate)
    params = fit_camera(estimates)
    # Both regimes are kept: a single power-law line would be off by ~2x at the ends.
    for k in ks:
        assert abs(np.exp(params.log_sigma(np.log(k))) / dual_gain_sigma(k) - 1) < 0.2
    assert np.allclose(np.exp([params.log_k_points[0], params.log_k_points[-1]]), [ks[0], ks[-1]], rtol=0.05)
    # Beyond the measured range the read noise in electrons stays constant.
    assert np.isclose(np.exp(params.log_sigma(np.log(ks[-1] * 2))) / (ks[-1] * 2),
                      np.exp(params.log_sigma(np.log(ks[-1]))) / ks[-1])


def test_banding_is_fitted_relative_to_read_noise():
    rng = np.random.default_rng(2)
    clean = gradient_clean()
    estimates = []
    for iso, k in enumerate((5e-4, 1e-3, 2e-3, 4e-3), 1):
        sigma = 3 * k
        noisy = apply_noise(clean, NoiseSample(k, sigma, 0.5 * sigma, (0, 0, 0, 0)), rng)
        estimates.append(estimate_pair(noisy, clean, 100 * iso, rng))
    params = fit_camera(estimates)
    assert 0.35 < params.row_ratio_max < 0.75


def test_manifest_refit_assigns_profiles_and_pooled_draws_fitted_cameras():
    estimate = {"k": 1e-3, "sigma2": 9e-6, "row_var": 0.0, "gain": 1.0}
    noisy = [{"camera": cam, "noise_estimate": {**estimate, "k": k, "sigma2": (3 * k) ** 2, "iso": iso}}
             for cam in ("cam a", "cam b") for iso, k in ((100, 1e-4), (400, 4e-4), (1600, 1.6e-3))]
    manifest = {"files": noisy + [{"camera": "cam a"}, {"camera": "other"}]}
    doc = fit_manifest(manifest)
    assert sorted(doc["cameras"]) == ["cam a", "cam b"]
    assert [r["noise_profile"] for r in manifest["files"][-2:]] == ["cam a", POOLED]
    rng = np.random.default_rng(0)
    drawn = {params_for(POOLED, doc, rng) for _ in range(50)}
    assert drawn == {params_for("cam a", doc, rng), params_for("cam b", doc, rng)}


def test_calibration_resolves_read_noise_far_below_shot_noise():
    # The real-camera regime: 3 electrons of read noise (sigma = 3 K), so sigma^2 is
    # 10-100x below the shot noise of even the darkest flat areas. An unweighted
    # line fit loses the intercept here (it comes out negative or far too high).
    rng = np.random.default_rng(1)
    clean = gradient_clean()
    for k in (2e-4, 1e-3, 4e-3):
        noisy = apply_noise(clean, NoiseSample(k, 3 * k, 0.0, (0, 0, 0, 0)), rng)
        estimate = estimate_pair(noisy, clean, 1e3, rng)
        assert abs(np.sqrt(estimate.sigma2) / (3 * k) - 1) < 0.25
