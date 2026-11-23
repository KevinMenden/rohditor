//! Slider arithmetic: values, formatting and relative drags.

use rohditor_edit::ParameterRange;

/// How a value is written next to its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `+0.35 EV`
    Stops,
    /// `5200 K`
    Kelvin,
    /// Distance from neutral in percent, signed.
    Percent,
}

impl Format {
    pub fn label(self, value: f32, range: ParameterRange) -> String {
        match self {
            Self::Stops => format!("{value:+.2} EV"),
            Self::Kelvin => format!("{value:.0} K"),
            Self::Percent => {
                let percent = ((value - range.neutral) * 100.0).round();
                // Avoid "-0".
                if percent == 0.0 {
                    "0".to_owned()
                } else {
                    format!("{percent:+.0}")
                }
            }
        }
    }

    /// Temperature feels even in mireds, everything else is linear.
    fn track_at(self, value: f32) -> f32 {
        match self {
            Self::Kelvin => -1.0 / value,
            Self::Stops | Self::Percent => value,
        }
    }

    fn value_at(self, track: f32) -> f32 {
        match self {
            Self::Kelvin => -1.0 / track,
            Self::Stops | Self::Percent => track,
        }
    }
}

/// Position of `value` on the track, 0 to 1.
pub fn fraction(format: Format, range: ParameterRange, value: f32) -> f32 {
    let (low, high) = (
        format.track_at(range.minimum),
        format.track_at(range.maximum),
    );
    ((format.track_at(value) - low) / (high - low)).clamp(0.0, 1.0)
}

/// The value after dragging by `delta` track widths from `start`.
pub fn dragged(format: Format, range: ParameterRange, start: f32, delta: f32) -> f32 {
    let (low, high) = (
        format.track_at(range.minimum),
        format.track_at(range.maximum),
    );
    let track = (format.track_at(start) + delta * (high - low)).clamp(low, high);
    format.value_at(track).clamp(range.minimum, range.maximum)
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{CONTRAST_RANGE, EXPOSURE_EV_RANGE, SATURATION_RANGE, TEMPERATURE_RANGE};

    use super::*;

    #[test]
    fn labels_are_relative_to_neutral() {
        assert_eq!(Format::Percent.label(1.25, SATURATION_RANGE), "+25");
        assert_eq!(Format::Percent.label(-0.001, CONTRAST_RANGE), "0");
        assert_eq!(Format::Stops.label(-0.5, EXPOSURE_EV_RANGE), "-0.50 EV");
        assert_eq!(Format::Kelvin.label(5_200.4, TEMPERATURE_RANGE), "5200 K");
    }

    #[test]
    fn drags_are_relative_and_clamped() {
        let drag = |start, delta| dragged(Format::Stops, EXPOSURE_EV_RANGE, start, delta);
        assert!((drag(1.0, 0.1) - 2.0).abs() < 1e-5);
        assert_eq!(drag(1.0, 9.0), EXPOSURE_EV_RANGE.maximum);
        assert_eq!(drag(1.0, -9.0), EXPOSURE_EV_RANGE.minimum);
    }

    #[test]
    fn temperature_moves_evenly_in_mireds() {
        let start = 5_000.0;
        let warm = dragged(Format::Kelvin, TEMPERATURE_RANGE, start, -0.1);
        let cool = dragged(Format::Kelvin, TEMPERATURE_RANGE, start, 0.1);
        assert!((1.0 / warm - 1.0 / start - (1.0 / start - 1.0 / cool)).abs() < 1e-7);
        assert!(start - warm < cool - start);
        let at = fraction(Format::Kelvin, TEMPERATURE_RANGE, cool);
        assert!((at - fraction(Format::Kelvin, TEMPERATURE_RANGE, start) - 0.1).abs() < 1e-4);
    }
}
