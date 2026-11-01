//! Output sharpening choices for export.
//!
//! Output sharpening depends on the output (size and medium), not on the
//! photo, so it lives in [`ExportSettings`](super::ExportSettings) rather
//! than in the recipe. Presets keep it easy to qualify and rarely tuned.

use rohditor_detail::OutputSharpen;
use serde::{Deserialize, Serialize};

/// Where the exported image will be viewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMedium {
    /// Backlit display at roughly 100 to 200 PPI.
    Screen,
    /// Matte paper: more ink spread, so the strongest sharpening.
    Matte,
    /// Glossy or luster paper.
    Glossy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSharpeningStrength {
    Low,
    #[default]
    Standard,
    High,
}

/// Export-only sharpening, applied after resize on display-encoded values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSharpening {
    pub medium: OutputMedium,
    pub strength: OutputSharpeningStrength,
}

/// Luma halo limit beyond the local 3x3 range, shared by all presets.
const OVERSHOOT: f32 = 0.02;
/// Soft threshold that keeps flat-area noise from being sharpened.
const THRESHOLD: f32 = 0.005;

impl OutputSharpening {
    /// Unsharp-mask parameters for this preset.
    ///
    /// Screen Standard was tuned on a synthetic soft edge after a 4x area
    /// downscale (see the test below): sigma 0.5 left the rise distance 19%
    /// above the full-resolution edge, sigma 0.6 with amount 0.8 brings it to
    /// about 7%. Print presets use a larger sigma since print is viewed at
    /// higher PPI. They are not yet checked against real exports.
    #[must_use]
    pub const fn params(self) -> OutputSharpen {
        // (sigma in output pixels, amount at Standard strength)
        let (sigma, standard_amount) = match self.medium {
            OutputMedium::Screen => (0.6, 0.8),
            OutputMedium::Glossy => (0.8, 0.9),
            OutputMedium::Matte => (1.0, 1.1),
        };
        let multiplier = match self.strength {
            OutputSharpeningStrength::Low => 0.6,
            OutputSharpeningStrength::Standard => 1.0,
            OutputSharpeningStrength::High => 1.5,
        };
        OutputSharpen {
            sigma,
            amount: standard_amount * multiplier,
            threshold: THRESHOLD,
            overshoot: OVERSHOOT,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rohditor_detail::{NeverCancelled, sharpen_output};
    use rohditor_image::{DisplayRgbImage, DisplayTransfer};

    use super::*;

    #[test]
    fn strength_scales_amount_and_every_preset_is_valid() {
        for medium in [
            OutputMedium::Screen,
            OutputMedium::Matte,
            OutputMedium::Glossy,
        ] {
            let amount = |strength| OutputSharpening { medium, strength }.params().amount;
            assert!(
                amount(OutputSharpeningStrength::Low) < amount(OutputSharpeningStrength::Standard)
            );
            assert!(
                amount(OutputSharpeningStrength::Standard) < amount(OutputSharpeningStrength::High)
            );
        }
        let screen = OutputSharpening {
            medium: OutputMedium::Screen,
            strength: OutputSharpeningStrength::Standard,
        }
        .params();
        assert_eq!((screen.sigma, screen.amount), (0.6, 0.8));
    }

    /// 10-90% rise distance of a left-to-right edge, in pixels, with linear
    /// interpolation between samples.
    fn rise_distance(row: &[f32]) -> f32 {
        let (low, high) = (row[0], row[row.len() - 1]);
        let crossing = |level: f32| {
            let target = low + level * (high - low);
            let i = row
                .windows(2)
                .position(|w| w[0] <= target && w[1] > target)
                .unwrap();
            i as f32 + (target - row[i]) / (row[i + 1] - row[i])
        };
        crossing(0.9) - crossing(0.1)
    }

    #[test]
    fn screen_standard_restores_edge_rise_after_4x_downscale() {
        const FACTOR: usize = 4;
        // Full-resolution edge: a step softened like a lens would (about 3 px
        // rise), between 0.2 and 0.8 so the halo limit is visible.
        let full: Vec<f32> = (0..256)
            .map(|x| 0.5 + 0.3 * (1.2 * (x as f32 - 127.5) / (1.2 * 2.0_f32.sqrt())).tanh())
            .collect();
        let down: Vec<f32> = full
            .chunks(FACTOR)
            .map(|block| block.iter().sum::<f32>() / FACTOR as f32)
            .collect();
        let data = (0..down.len() * 8)
            .flat_map(|i| [down[i % down.len()]; 3])
            .collect();
        let mut image =
            DisplayRgbImage::new(down.len(), 8, down.len() * 3, DisplayTransfer::Srgb, data)
                .unwrap();
        let preset = OutputSharpening {
            medium: OutputMedium::Screen,
            strength: OutputSharpeningStrength::Standard,
        };
        sharpen_output(&mut image, preset.params(), &NeverCancelled).unwrap();
        let row: Vec<f32> = (0..down.len())
            .map(|x| image.pixel(x, 4).unwrap()[0])
            .collect();

        // Compare in full-resolution pixel units.
        let full_rise = rise_distance(&full);
        let sharpened_rise = rise_distance(&row) * FACTOR as f32;
        assert!(rise_distance(&down) * FACTOR as f32 > 1.4 * full_rise);
        assert!(
            (sharpened_rise - full_rise).abs() <= 0.15 * full_rise,
            "rise {sharpened_rise} vs full-resolution {full_rise}"
        );
        // The halo limit holds on the real edge too.
        let (low, high) = (down[0], down[down.len() - 1]);
        assert!(
            row.iter()
                .all(|&v| v >= low - 0.02 - 1e-6 && v <= high + 0.02 + 1e-6)
        );
    }
}
