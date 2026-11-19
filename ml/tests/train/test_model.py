import numpy as np
import pytest

torch = pytest.importorskip("torch")

from rohditor_ml.data.bayer import mosaic_from_rgb, pack  # noqa: E402
from rohditor_ml.train.model import BilinearDemosaic, UNet, macs_per_megapixel, parameter_count  # noqa: E402


def test_shapes_and_budget():
    model = UNet()
    out = model(torch.zeros(2, 4, 24, 40))
    assert out.shape == (2, 3, 48, 80)
    params, macs = parameter_count(model), macs_per_megapixel(model)
    # Issue #117 budget: at most ~4M parameters and 60 GMAC/MP; the layout estimate is ~2.1M / ~48.
    assert 2.0e6 < params < 2.3e6
    assert 45e9 < macs < 52e9


def test_starts_as_bilinear():
    model = UNet()
    x = torch.rand(1, 4, 16, 16)
    torch.testing.assert_close(model(x), BilinearDemosaic()(x))


def _bilinear(rgb: np.ndarray) -> np.ndarray:
    packed = torch.from_numpy(pack(mosaic_from_rgb(rgb.astype(np.float32))))[None]
    return BilinearDemosaic()(packed)[0].numpy()


def test_bilinear_reproduces_flat_colour_everywhere():
    # Reflect padding keeps the CFA phase, so even the border is exact.
    rgb = np.ones((3, 12, 16)) * np.array([0.7, 0.3, 0.1])[:, None, None]
    np.testing.assert_allclose(_bilinear(rgb), rgb, atol=1e-6)


def test_bilinear_keeps_measured_samples_and_is_exact_on_ramps():
    y, x = np.mgrid[0:12, 0:16].astype(np.float32)
    rgb = np.stack([0.01 * x + 0.02 * y, 0.03 * x, 0.5 - 0.01 * y])
    out = _bilinear(rgb)
    mosaic = mosaic_from_rgb(rgb.astype(np.float32))
    np.testing.assert_allclose(mosaic_from_rgb(out), mosaic, atol=1e-6)
    # A linear ramp is interpolated exactly away from the (mirrored) border.
    np.testing.assert_allclose(out[:, 1:-1, 1:-1], rgb[:, 1:-1, 1:-1], atol=1e-5)


def test_red_and_blue_are_not_swapped():
    rgb = np.zeros((3, 8, 8))
    rgb[0] = 1.0  # pure red scene
    out = _bilinear(rgb)
    assert out[0].min() > 0.99 and out[2].max() < 1e-6
