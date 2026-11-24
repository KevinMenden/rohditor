//! The facts about a photo that frontends list.

use rohditor_image::Orientation;

use crate::model::RawFileInfo;

/// One labelled value. `None` where the file does not provide it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfoRow {
    pub label: &'static str,
    pub value: Option<String>,
}

impl RawFileInfo {
    /// How the photo was taken.
    #[must_use]
    pub fn capture_rows(&self) -> Vec<InfoRow> {
        let capture = &self.capture;
        vec![
            row("Captured", capture.captured_at_label()),
            row("Camera", present(self.camera_label())),
            row("Lens", capture.lens_label()),
            row("Focal length", capture.focal_length_label()),
            row("Shutter", capture.exposure_time_label()),
            row("Aperture", capture.aperture_label()),
            row(
                "ISO",
                capture
                    .iso
                    .filter(|iso| *iso > 0)
                    .map(|iso| iso.to_string()),
            ),
            row("Focus distance", capture.focus_distance_label()),
        ]
    }

    /// What the file holds.
    #[must_use]
    pub fn file_rows(&self) -> Vec<InfoRow> {
        vec![
            row("Format", present(self.format.clone())),
            row(
                "Dimensions",
                Some(format!("{} × {}", self.width, self.height)),
            ),
            row("Orientation", orientation_label(self.orientation)),
            row(
                "As-shot WB",
                white_balance_label(&self.as_shot_white_balance),
            ),
            row(
                "Bit depth",
                self.source_bits_per_sample
                    .map(|bits| format!("{bits} bit")),
            ),
            row("Compression", self.compression.clone()),
            row("File size", Some(file_size_label(self.source_size_bytes))),
        ]
    }
}

const fn row(label: &'static str, value: Option<String>) -> InfoRow {
    InfoRow { label, value }
}

fn present(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

fn orientation_label(orientation: Orientation) -> Option<String> {
    (orientation != Orientation::Unknown).then(|| orientation.to_string())
}

/// The as-shot channel multipliers, e.g. `2.10 · 1.00 · 1.58`.
fn white_balance_label(multipliers: &[Option<f32>; 4]) -> Option<String> {
    let present: Vec<String> = multipliers
        .iter()
        .flatten()
        .map(|value| format!("{value:.2}"))
        .collect();
    (!present.is_empty()).then(|| present.join(" · "))
}

/// Decimal megabytes, as file managers show them.
fn file_size_label(bytes: u64) -> String {
    // Precision loss above 2^52 bytes is irrelevant for a display label.
    #[allow(clippy::cast_precision_loss)]
    let megabytes = bytes as f64 / 1_000_000.0;
    format!("{megabytes:.1} MB")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_balance_lists_only_present_multipliers() {
        assert_eq!(
            white_balance_label(&[Some(2.1), Some(1.0), Some(1.583), None]).as_deref(),
            Some("2.10 · 1.00 · 1.58")
        );
        assert_eq!(white_balance_label(&[None; 4]), None);
    }

    #[test]
    fn file_size_is_in_megabytes() {
        assert_eq!(file_size_label(24_310_000), "24.3 MB");
        assert_eq!(file_size_label(0), "0.0 MB");
    }

    #[test]
    fn unknown_orientation_is_missing() {
        assert_eq!(orientation_label(Orientation::Unknown), None);
        assert!(orientation_label(Orientation::Rotate90).is_some());
    }
}
