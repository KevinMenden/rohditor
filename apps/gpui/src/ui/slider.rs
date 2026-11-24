//! Slider arithmetic: values, formatting, typed text, steps, relative drags and coloured tracks.

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
    /// `1.05×`
    Factor,
}

impl Format {
    pub fn label(self, value: f32, range: ParameterRange) -> String {
        match self {
            Self::Stops => format!("{value:+.2} EV"),
            Self::Kelvin => format!("{value:.0} K"),
            Self::Factor => format!("{value:.2}×"),
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

    /// The value meant by `text`, in the unit of [`Self::label`]. Clamped to the range.
    pub fn parse(self, text: &str, range: ParameterRange) -> Option<f32> {
        let text = text.trim().to_lowercase().replace(',', ".");
        let number = ["ev", "k", "%", "×", "x"]
            .iter()
            .find_map(|unit| text.strip_suffix(unit))
            .unwrap_or(&text)
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|number| number.is_finite())?;
        let value = match self {
            Self::Stops | Self::Kelvin | Self::Factor => number,
            Self::Percent => number / 100.0 + range.neutral,
        };
        Some(value.clamp(range.minimum, range.maximum))
    }

    /// What an arrow key changes.
    pub const fn step(self) -> f64 {
        match self {
            Self::Stops => 0.05,
            Self::Kelvin => 50.0,
            Self::Percent | Self::Factor => 0.01,
        }
    }

    /// The value `steps` away, on a multiple of the step.
    pub fn stepped(self, range: ParameterRange, value: f32, steps: i8) -> f32 {
        let neutral = f64::from(range.neutral);
        let position = (f64::from(value) - neutral) / self.step();
        let next = (position + f64::from(steps)).round() * self.step() + neutral;
        (next as f32).clamp(range.minimum, range.maximum)
    }

    /// Temperature feels even in mireds, everything else is linear.
    fn track_at(self, value: f32) -> f32 {
        match self {
            Self::Kelvin => -1.0 / value,
            Self::Stops | Self::Percent | Self::Factor => value,
        }
    }

    fn value_at(self, track: f32) -> f32 {
        match self {
            Self::Kelvin => -1.0 / track,
            Self::Stops | Self::Percent | Self::Factor => track,
        }
    }
}

/// Colours along a track: at its start, at the neutral value and at its end.
#[derive(Debug, Clone, Copy)]
pub struct Ramp(pub [u32; 3]);

pub const TEMPERATURE_RAMP: Ramp = Ramp([0x488fde, 0xdce0d6, 0xefb13f]);
pub const TINT_RAMP: Ramp = Ramp([0x48b460, 0xdcdcdc, 0xcd4ba4]);

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
    use rohditor_edit::{
        CONTRAST_RANGE, EXPOSURE_EV_RANGE, HIGHLIGHT_THRESHOLD_RANGE, SATURATION_RANGE,
        TEMPERATURE_RANGE,
    };

    use super::*;

    #[test]
    fn labels_are_relative_to_neutral() {
        assert_eq!(Format::Percent.label(1.25, SATURATION_RANGE), "+25");
        assert_eq!(Format::Percent.label(-0.001, CONTRAST_RANGE), "0");
        assert_eq!(Format::Stops.label(-0.5, EXPOSURE_EV_RANGE), "-0.50 EV");
        assert_eq!(
            Format::Factor.label(0.95, HIGHLIGHT_THRESHOLD_RANGE),
            "0.95×"
        );
        assert_eq!(Format::Kelvin.label(5_200.4, TEMPERATURE_RANGE), "5200 K");
    }

    #[test]
    fn typed_text_is_in_the_unit_of_the_label() {
        let stops = |text| Format::Stops.parse(text, EXPOSURE_EV_RANGE);
        assert_eq!(stops(" +0.35 EV"), Some(0.35));
        assert_eq!(stops("-1,5"), Some(-1.5));
        assert_eq!(stops("99"), Some(EXPOSURE_EV_RANGE.maximum));
        assert_eq!(stops("bright"), None);
        assert_eq!(stops("nan"), None);
        assert_eq!(
            Format::Kelvin.parse("5200 K", TEMPERATURE_RANGE),
            Some(5_200.0)
        );
        let factor = |text| Format::Factor.parse(text, HIGHLIGHT_THRESHOLD_RANGE);
        assert_eq!(factor("1.2×"), Some(1.2));
        assert_eq!(factor("1.2 x"), Some(1.2));
        assert_eq!(Format::Percent.parse("25", SATURATION_RANGE), Some(1.25));
    }

    #[test]
    fn steps_stay_on_a_grid_and_in_range() {
        let range = EXPOSURE_EV_RANGE;
        let thrice = (0..3).fold(0.0, |value, _| Format::Stops.stepped(range, value, 1));
        assert_eq!(thrice, 0.15);
        assert_eq!(Format::Stops.stepped(range, 0.0, -10), -0.5);
        assert_eq!(
            Format::Stops.stepped(range, range.maximum, 1),
            range.maximum
        );
        let percent = |value, steps| Format::Percent.stepped(SATURATION_RANGE, value, steps);
        assert_eq!(percent(1.254, 1), 1.26);
        let kelvin = |value, steps| Format::Kelvin.stepped(TEMPERATURE_RANGE, value, steps);
        assert_eq!(kelvin(kelvin(5_500.0, 1), -1), 5_500.0);
        assert!(kelvin(4_918.0, 1) > 4_918.0 && kelvin(4_918.0, -1) < 4_918.0);
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
