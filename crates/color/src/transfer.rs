//! IEC sRGB transfer functions.

/// Apply the IEC sRGB transfer function to one linear-light component.
#[must_use]
pub fn linear_srgb_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        12.92 * value
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

/// Decode one sRGB component back to linear light.
#[must_use]
pub fn srgb_to_linear_srgb(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_transfer_round_trips_representative_values() {
        for linear in [0.0, 0.001, 0.003_130_8, 0.18, 0.5, 1.0] {
            let restored = srgb_to_linear_srgb(linear_srgb_to_srgb(linear));
            assert!((restored - linear).abs() < 1.0e-6, "{linear}: {restored}");
        }
    }
}
