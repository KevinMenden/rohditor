import numpy as np
import pytest

from rohditor_ml.data.noise import NoiseParams, NoiseSample
from rohditor_ml.eval.metrics import scores
from rohditor_ml.eval.report import BASELINE, add_best_baseline, aggregate, gate
from rohditor_ml.eval.sets import charts, iso_noise, measured_k, slanted_edge, tier_key, zone_plate

PARAMS = NoiseParams((-9.0, -5.0), (-8.0, -5.0), 0.0, 0.4)
BASE_NOISE = NoiseSample(1e-4, 3e-4, 0.0, (0.0,) * 4)


def test_scores_rank_closer_outputs_higher():
    rng = np.random.default_rng(0)
    target = rng.random((3, 48, 48)).astype(np.float32) * 0.5
    near, far = target + 0.01 * rng.standard_normal(target.shape), target + 0.05 * rng.standard_normal(target.shape)
    a, b = scores(near, target), scores(far, target)
    assert all(a[m] > b[m] for m in a)
    assert scores(target, target)["ssim"] == pytest.approx(1.0)


def test_binned_scoring_ignores_masked_pixels():
    target = np.full((3, 48, 48), 0.3, np.float32)
    output = target.copy()
    output[:, 20:22, 20:22] = 5.0
    mask = np.ones((48, 48), np.float32)
    mask[21, 21] = 0  # invalidates the whole binned pixel holding the bad block
    assert scores(output, target, mask, binned=True)["psnr"] > 100


def test_iso_tiers_follow_the_measured_gains():
    files = [{"camera": "cam", "noise_estimate": {"iso": iso, "k": k}}
             for iso, k in ((100, 1e-4), (100, 1.2e-4), (1600, 1.6e-3), (6400, 6.4e-3))]
    files.append({"camera": "other", "noise_estimate": {"iso": 100, "k": 1.0}})
    k_by_iso = measured_k(files, "cam")
    assert k_by_iso[100] == pytest.approx(1.1e-4)
    tiers = iso_noise(PARAMS, k_by_iso, (100, 400, 6400))
    assert [t for t, _ in tiers] == ["iso100", "iso400", "iso6400"]
    assert tiers[1][1].k == pytest.approx(np.exp(np.interp(np.log(400), np.log([100, 1600]), np.log([1.1e-4, 1.6e-3]))))
    assert tiers[2][1].k == pytest.approx(6.4e-3) and tiers[2][1].sigma > tiers[0][1].sigma


def test_tier_labels_sort_by_iso():
    labels = ["iso6400+", "iso12800", "clean", "iso0-800", "iso100", "iso1600-3200"]
    assert sorted(labels, key=tier_key) == ["clean", "iso0-800", "iso100", "iso1600-3200", "iso6400+", "iso12800"]


def test_charts_are_in_range_and_grey_after_balance():
    for image in (zone_plate(64), slanted_edge(64)):
        assert image.min() >= 0.0 and image.max() <= 0.6
    for case in charts(64, BASE_NOISE, 0):
        assert case.mosaic.shape == (64, 64)
        np.testing.assert_array_equal(case.target[0], case.target[2])


def test_baseline_takes_the_best_setting_and_gate_compares_to_it():
    rows = []
    for method, value in (("rcd", 30.0), ("rcd+nr(a)", 33.0), ("rcd+nr(b)", 32.0), ("net", 34.5)):
        for tier in ("iso100", "iso51200"):
            v = value if tier == "iso51200" else 40.0
            rows.append({"set": "pixelshift_sweep", "tier": tier, "id": "0", "method": method,
                         "psnr": v, "ssim": 0.9, "psnr_display": v, "ssim_display": 0.9})
    table = aggregate(rows)
    chosen = add_best_baseline(table, ["rcd", "rcd+nr(a)", "rcd+nr(b)"])
    assert chosen[("pixelshift_sweep", "iso51200")] == "rcd+nr(a)"
    assert table[("pixelshift_sweep", "iso51200", BASELINE)]["psnr"] == 33.0
    checks = {c.tier: c for c in gate(table, "net")}
    assert checks["iso51200"].gain_db == pytest.approx(1.5) and checks["iso51200"].passed
    assert checks["iso100"].passed


def test_classic_baseline_through_the_cli():
    pytest.importorskip("torch")
    from rohditor_ml.eval.methods import CLI, Classic

    if not CLI.exists():
        pytest.skip("build the release CLI first: cargo build --release -p rohditor-cli")
    case = charts(64, BASE_NOISE, 0)[0]  # clean zone plate
    out = Classic("rcd").run(case.mosaic[None], case.gains[None])
    assert out.shape == (1, 3, 64, 64)
    assert scores(out[0], case.target)["psnr"] > 20
    nr = Classic("rcd", 0.5, 0.5).run(case.mosaic[None], case.gains[None])
    assert nr.shape == out.shape
