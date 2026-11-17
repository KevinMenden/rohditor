//! Human-readable labels for capture metadata, shared by every frontend.
//!
//! Each label is `None` when the value is missing or unusable (for example a
//! zero denominator), so callers pick their own placeholder.

use crate::model::{CaptureMetadata, RationalValue, RawFileInfo};

/// Reciprocals this close to a whole number are shown as `1/N`.
const RECIPROCAL_TOLERANCE: f64 = 0.05;

impl CaptureMetadata {
    /// Shutter speed: `1/250 s` below one second, decimal seconds otherwise.
    #[must_use]
    pub fn exposure_time_label(&self) -> Option<String> {
        let seconds = positive(self.exposure_time?)?;
        if seconds >= 1.0 {
            return Some(format!("{} s", trimmed(seconds)));
        }
        let reciprocal = 1.0 / seconds;
        let whole = reciprocal.round();
        // 0.4 s or 0.8 s have no whole reciprocal; `1/2.5` reads worse than `0.4 s`.
        if (reciprocal - whole).abs() <= RECIPROCAL_TOLERANCE * whole {
            Some(format!("1/{whole} s"))
        } else {
            Some(format!("{} s", trimmed(seconds)))
        }
    }

    /// F-number such as `f/2.8` or `f/8`.
    #[must_use]
    pub fn aperture_label(&self) -> Option<String> {
        positive(self.aperture?).map(|value| format!("f/{}", trimmed(value)))
    }

    /// Focal length such as `35 mm` or `24.5 mm`.
    #[must_use]
    pub fn focal_length_label(&self) -> Option<String> {
        positive(self.focal_length?).map(|value| format!("{} mm", trimmed(value)))
    }

    /// Subject distance such as `1.2 m`.
    #[must_use]
    pub fn focus_distance_label(&self) -> Option<String> {
        positive(self.focus_distance?).map(|value| format!("{} m", trimmed(value)))
    }

    /// EXIF stores `YYYY:MM:DD HH:MM:SS`; this shows `YYYY-MM-DD HH:MM:SS`.
    /// Other layouts are returned as stored.
    #[must_use]
    pub fn captured_at_label(&self) -> Option<String> {
        let stored = self.captured_at.as_deref()?.trim();
        if stored.is_empty() {
            return None;
        }
        let bytes = stored.as_bytes();
        let is_exif_date = bytes.len() >= 10
            && bytes[4] == b':'
            && bytes[7] == b':'
            && bytes[..10]
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit());
        if !is_exif_date {
            return Some(stored.to_owned());
        }
        Some(format!(
            "{}-{}-{}{}",
            &stored[..4],
            &stored[5..7],
            &stored[8..10],
            &stored[10..]
        ))
    }

    /// Lens make and model, without repeating a make the model already names.
    #[must_use]
    pub fn lens_label(&self) -> Option<String> {
        let model = self.lens_model.as_deref()?;
        Some(match self.lens_make.as_deref() {
            Some(make) if !model.starts_with(make) => format!("{make} {model}"),
            _ => model.to_owned(),
        })
    }
}

impl RawFileInfo {
    /// Normalized camera make and model, e.g. `Sony ILCE-7M4`.
    #[must_use]
    pub fn camera_label(&self) -> String {
        format!("{} {}", self.clean_make, self.clean_model)
            .trim()
            .to_owned()
    }
}

fn positive(value: RationalValue) -> Option<f64> {
    value
        .as_f64()
        .filter(|value| *value > 0.0 && value.is_finite())
}

/// One decimal place, without a trailing `.0`.
fn trimmed(value: f64) -> String {
    let text = format!("{value:.1}");
    match text.strip_suffix(".0") {
        Some(whole) => whole.to_owned(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rational(numerator: u32, denominator: u32) -> Option<RationalValue> {
        Some(RationalValue {
            numerator,
            denominator,
        })
    }

    fn shutter(numerator: u32, denominator: u32) -> Option<String> {
        CaptureMetadata {
            exposure_time: rational(numerator, denominator),
            ..CaptureMetadata::default()
        }
        .exposure_time_label()
    }

    #[test]
    fn sub_second_shutter_is_a_reciprocal() {
        assert_eq!(shutter(1, 250).as_deref(), Some("1/250 s"));
        // Stored unreduced.
        assert_eq!(shutter(10, 2500).as_deref(), Some("1/250 s"));
        assert_eq!(shutter(1, 3).as_deref(), Some("1/3 s"));
    }

    #[test]
    fn shutter_without_a_whole_reciprocal_is_decimal() {
        assert_eq!(shutter(4, 10).as_deref(), Some("0.4 s"));
        assert_eq!(shutter(8, 10).as_deref(), Some("0.8 s"));
    }

    #[test]
    fn multi_second_shutter_is_decimal_seconds() {
        assert_eq!(shutter(1, 1).as_deref(), Some("1 s"));
        assert_eq!(shutter(25, 10).as_deref(), Some("2.5 s"));
        assert_eq!(shutter(30, 1).as_deref(), Some("30 s"));
    }

    #[test]
    fn unusable_rationals_have_no_label() {
        assert_eq!(shutter(1, 0), None);
        assert_eq!(shutter(0, 1), None);
        assert_eq!(CaptureMetadata::default().exposure_time_label(), None);
    }

    #[test]
    fn aperture_and_focal_length_trim_whole_numbers() {
        let metadata = CaptureMetadata {
            aperture: rational(28, 10),
            focal_length: rational(35, 1),
            focus_distance: rational(12, 10),
            ..CaptureMetadata::default()
        };
        assert_eq!(metadata.aperture_label().as_deref(), Some("f/2.8"));
        assert_eq!(metadata.focal_length_label().as_deref(), Some("35 mm"));
        assert_eq!(metadata.focus_distance_label().as_deref(), Some("1.2 m"));

        let metadata = CaptureMetadata {
            aperture: rational(8, 1),
            focal_length: rational(245, 10),
            ..CaptureMetadata::default()
        };
        assert_eq!(metadata.aperture_label().as_deref(), Some("f/8"));
        assert_eq!(metadata.focal_length_label().as_deref(), Some("24.5 mm"));
    }

    #[test]
    fn exif_dates_become_readable_and_others_pass_through() {
        let label = |text: &str| {
            CaptureMetadata {
                captured_at: Some(text.to_owned()),
                ..CaptureMetadata::default()
            }
            .captured_at_label()
        };
        assert_eq!(
            label("2024:05:01 12:34:56").as_deref(),
            Some("2024-05-01 12:34:56")
        );
        assert_eq!(label("yesterday").as_deref(), Some("yesterday"));
        assert_eq!(label("  "), None);
    }

    #[test]
    fn lens_label_does_not_repeat_the_make() {
        let label = |make: Option<&str>, model: Option<&str>| {
            CaptureMetadata {
                lens_make: make.map(str::to_owned),
                lens_model: model.map(str::to_owned),
                ..CaptureMetadata::default()
            }
            .lens_label()
        };
        assert_eq!(
            label(Some("Sony"), Some("FE 35mm F1.8")).as_deref(),
            Some("Sony FE 35mm F1.8")
        );
        assert_eq!(
            label(Some("Sony"), Some("Sony FE 35mm F1.8")).as_deref(),
            Some("Sony FE 35mm F1.8")
        );
        assert_eq!(label(Some("Sony"), None), None);
    }
}
