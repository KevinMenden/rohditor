//! Relative temperature/tint shift for local adjustments.
//!
//! Local adjustments (masks) do not know the scene illuminant, so their Temperature and Tint
//! sliders are *relative shifts of the white point*, not Kelvin. The shift is a von Kries style
//! scaling of the linear Rec.2020 channels: moving the white towards a warmer (or cooler, or
//! more magenta, or greener) colour is the same as scaling the channels by the gains that map
//! the old white onto the new one.

/// Luminance weights of linear Rec.2020 (the working space of the adjustment stage).
pub const REC2020_LUMINANCE: [f32; 3] = [0.2627, 0.6780, 0.0593];

/// Log2 channel gains for a temperature of `+1` (warm: red up, blue down by the same amount).
/// At the slider extremes the red/blue ratio therefore changes by `2^(2 * 0.5) = 2`.
const TEMPERATURE_AXIS_LOG2: [f32; 3] = [0.5, 0.0, -0.5];
/// Log2 channel gains for a tint of `+1` (magenta: green down, red and blue up equally).
/// At the slider extremes the green/magenta balance changes by `2^0.75`.
const TINT_AXIS_LOG2: [f32; 3] = [0.25, -0.5, 0.25];

/// Per-channel gains (linear Rec.2020) for a relative temperature and tint shift.
///
/// * `temperature`: `-1.0..=1.0` on one layer, `+` is warmer (more yellow/orange), `-` cooler.
/// * `tint`: `-1.0..=1.0` on one layer, `+` is more magenta, `-` more green.
///
/// The pipeline sums the alpha-weighted values of all layers before calling this, so values
/// outside `-1..=1` are valid and simply extend the shift.
///
/// Properties (all covered by tests):
/// * `(0, 0)` returns exactly `[1, 1, 1]`.
/// * The gains are normalized so a white input keeps its Rec.2020 luminance: the shift changes
///   colour, not brightness.
/// * Symmetric: negating both inputs inverts the (un-normalized) gains, so `(t, s)` and
///   `(-t, -s)` give reciprocal red/blue and green/magenta ratios.
/// * Monotone: the red/blue ratio rises with `temperature`, and green falls relative to red
///   and blue with `tint`.
#[must_use]
pub fn local_white_balance_gains(temperature: f32, tint: f32) -> [f32; 3] {
    if temperature == 0.0 && tint == 0.0 {
        return [1.0; 3];
    }
    let raw: [f32; 3] = std::array::from_fn(|c| {
        (temperature * TEMPERATURE_AXIS_LOG2[c] + tint * TINT_AXIS_LOG2[c]).exp2()
    });
    let white_luminance: f32 = raw.iter().zip(REC2020_LUMINANCE).map(|(g, w)| g * w).sum();
    raw.map(|gain| gain / white_luminance)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(gains: [f32; 3]) -> f32 {
        gains
            .iter()
            .zip(REC2020_LUMINANCE)
            .map(|(g, w)| g * w)
            .sum()
    }

    #[test]
    fn zero_is_gain_free() {
        assert_eq!(local_white_balance_gains(0.0, 0.0), [1.0; 3]);
    }

    #[test]
    fn white_keeps_its_luminance() {
        for (t, s) in [(1.0, 0.0), (-1.0, 0.3), (0.4, -0.9), (2.5, 2.5)] {
            let gains = local_white_balance_gains(t, s);
            assert!((luminance(gains) - 1.0).abs() < 1e-6, "{t} {s}: {gains:?}");
        }
    }

    #[test]
    fn warmer_raises_red_over_blue_monotonically() {
        let mut previous = f32::NEG_INFINITY;
        for step in -10..=10 {
            let gains = local_white_balance_gains(step as f32 / 10.0, 0.0);
            let ratio = gains[0] / gains[2];
            assert!(ratio > previous, "step {step}");
            previous = ratio;
        }
        let warm = local_white_balance_gains(1.0, 0.0);
        assert!(warm[0] > 1.0 && warm[2] < 1.0);
    }

    #[test]
    fn magenta_tint_lowers_green_monotonically() {
        let mut previous = f32::INFINITY;
        for step in -10..=10 {
            let gains = local_white_balance_gains(0.0, step as f32 / 10.0);
            let ratio = gains[1] / (gains[0] * gains[2]).sqrt();
            assert!(ratio < previous, "step {step}");
            previous = ratio;
        }
    }

    #[test]
    fn opposite_shifts_have_reciprocal_ratios() {
        let (t, s) = (0.7, -0.4);
        let a = local_white_balance_gains(t, s);
        let b = local_white_balance_gains(-t, -s);
        assert!((a[0] / a[2] * (b[0] / b[2]) - 1.0).abs() < 1e-5);
        assert!((a[1] / a[0] * (b[1] / b[0]) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn temperature_does_not_move_the_green_magenta_axis() {
        let gains = local_white_balance_gains(0.8, 0.0);
        // Green sits between red and blue on the temperature axis (log gain 0 before
        // normalization), so red and blue move by reciprocal factors.
        assert!((gains[0] * gains[2] - gains[1] * gains[1]).abs() < 1e-5);
    }
}
