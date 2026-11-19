import pytest

torch = pytest.importorskip("torch")

from rohditor_ml.train.loss import loss_per_sample, signed_gamma  # noqa: E402


def test_signed_gamma_is_odd_continuous_and_has_finite_gradient():
    x = torch.tensor([-0.5, -1e-6, 0.0, 1e-6, 0.5, 2.0], requires_grad=True)
    y = signed_gamma(x)
    torch.testing.assert_close(signed_gamma(-x), -y)
    assert float(y[2].detach()) == 0.0 and abs(float(y[1].detach())) < 1e-3
    y.sum().backward()
    assert torch.isfinite(x.grad).all()


def test_real_pairs_are_scored_binned_and_masked():
    target = torch.zeros(2, 3, 4, 4)
    output = target.clone()
    # A zero-mean checkerboard error vanishes after 2x2 binning, but not at full resolution.
    output[:, :, 0::2, 0::2] += 0.1
    output[:, :, 1::2, 1::2] += 0.1
    output[:, :, 0::2, 1::2] -= 0.1
    output[:, :, 1::2, 0::2] -= 0.1
    mask = torch.ones(2, 1, 4, 4)
    losses = loss_per_sample(output, target, mask, torch.tensor([0.0, 1.0]))
    assert losses[0] > 0.1 and float(losses[1]) == pytest.approx(0.0, abs=1e-6)


def test_masked_pixels_do_not_count():
    target, output = torch.zeros(1, 3, 4, 4), torch.zeros(1, 3, 4, 4)
    output[..., :2, :2] = 5.0  # a bad block, masked out
    mask = torch.ones(1, 1, 4, 4)
    mask[..., 0, 0] = 0  # one invalid pixel invalidates its whole binned pixel
    assert float(loss_per_sample(output, target, mask, torch.tensor([1.0]))[0]) == 0.0
