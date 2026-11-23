use rohditor_image::DisplayRgbImage;

use super::clipping::{is_highlight_clipped, is_shadow_clipped};

const HISTOGRAM_BINS: usize = 256;

/// Display-referred RGB and luminance distribution for one rendered image.
///
/// Histogram data is transient render analysis, not part of the serialized
/// edit recipe. Counts use `u64` so full-resolution exports can be analyzed
/// without a counter overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Histogram {
    pub red: [u64; HISTOGRAM_BINS],
    pub green: [u64; HISTOGRAM_BINS],
    pub blue: [u64; HISTOGRAM_BINS],
    pub luminance: [u64; HISTOGRAM_BINS],
    pub shadow_clipped: [u64; 3],
    pub highlight_clipped: [u64; 3],
}

impl Histogram {
    /// Build a histogram from an sRGB8 display image.
    #[must_use]
    pub fn from_display_rgb8(image: &DisplayRgbImage<u8>) -> Self {
        Self::count(
            image
                .data()
                .chunks(image.row_stride())
                .take(image.height())
                .flat_map(|row| row[..image.width() * 3].as_chunks::<3>().0)
                .copied(),
        )
    }

    /// Build a histogram from a packed RGBA8 display buffer.
    ///
    /// This is used by the throttled asynchronous GPU-preview analysis path.
    /// The GPU display texture remains the normal display source; the readback
    /// is consumed by analysis and never installed as the UI texture.
    pub fn from_rgba8(width: usize, height: usize, rgba: &[u8]) -> Option<Self> {
        let expected = width.checked_mul(height)?.checked_mul(4)?;
        if rgba.len() != expected {
            return None;
        }
        let pixels = rgba.as_chunks::<4>().0;
        Some(Self::count(
            pixels
                .iter()
                .map(|&[red, green, blue, _]| [red, green, blue]),
        ))
    }

    /// Return the luminance bin containing the requested cumulative fraction.
    #[must_use]
    pub fn luminance_percentile(&self, fraction: f32) -> u8 {
        percentile(&self.luminance, fraction)
    }

    /// Return the highest bin any of red, green, or blue has at the requested
    /// cumulative fraction. Unlike luminance, this sees a saturated colour
    /// approaching clipping in a single channel.
    #[must_use]
    pub fn brightest_channel_percentile(&self, fraction: f32) -> u8 {
        [&self.red, &self.green, &self.blue]
            .into_iter()
            .map(|counts| percentile(counts, fraction))
            .max()
            .unwrap_or(0)
    }

    fn count(pixels: impl Iterator<Item = [u8; 3]>) -> Self {
        let mut histogram = Self::default();
        // `for_each`, as it is much faster than a loop over the flattened rows.
        pixels.for_each(|[red, green, blue]| {
            histogram.red[usize::from(red)] += 1;
            histogram.green[usize::from(green)] += 1;
            histogram.blue[usize::from(blue)] += 1;
            histogram.luminance[usize::from(luminance(red, green, blue))] += 1;
        });
        for (index, bins) in [&histogram.red, &histogram.green, &histogram.blue]
            .into_iter()
            .enumerate()
        {
            for (value, &count) in (0..=u8::MAX).zip(bins) {
                if is_shadow_clipped(value) {
                    histogram.shadow_clipped[index] += count;
                }
                if is_highlight_clipped(value) {
                    histogram.highlight_clipped[index] += count;
                }
            }
        }
        histogram
    }
}

/// Rec. 709 luminance of display values, rounded. In integers, as this runs for every pixel of a
/// preview while a slider is dragged.
fn luminance(red: u8, green: u8, blue: u8) -> u8 {
    let weighted = 2126 * u32::from(red) + 7152 * u32::from(green) + 722 * u32::from(blue);
    // The weights add up to 10000, so this is at most 255.
    ((weighted + 5000) / 10_000) as u8
}

fn percentile(counts: &[u64; HISTOGRAM_BINS], fraction: f32) -> u8 {
    let total = counts.iter().sum::<u64>();
    if total == 0 {
        return 0;
    }
    let target = (f64::from(fraction.clamp(0.0, 1.0)) * (total - 1) as f64) as u64;
    let mut cumulative = 0_u64;
    for (index, count) in counts.iter().enumerate() {
        cumulative = cumulative.saturating_add(*count);
        if cumulative > target {
            return index as u8;
        }
    }
    u8::MAX
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            red: [0; HISTOGRAM_BINS],
            green: [0; HISTOGRAM_BINS],
            blue: [0; HISTOGRAM_BINS],
            luminance: [0; HISTOGRAM_BINS],
            shadow_clipped: [0; 3],
            highlight_clipped: [0; 3],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Histogram, luminance};
    use rohditor_image::{DisplayColorSpace, DisplayRgbImage};

    #[test]
    fn histogram_counts_channels_luminance_and_clipping() {
        let image = DisplayRgbImage::new(
            2,
            1,
            6,
            DisplayColorSpace::SRGB,
            vec![0, 10, 255, 255, 20, 30],
        )
        .expect("valid image");
        let histogram = Histogram::from_display_rgb8(&image);
        assert_eq!(histogram.red[0], 1);
        assert_eq!(histogram.red[255], 1);
        assert_eq!(histogram.green[10], 1);
        assert_eq!(histogram.green[20], 1);
        assert_eq!(histogram.blue[255], 1);
        assert_eq!(histogram.blue[30], 1);
        assert_eq!(histogram.shadow_clipped, [1, 0, 0]);
        assert_eq!(histogram.highlight_clipped, [1, 0, 1]);
        assert_eq!(histogram.luminance.iter().sum::<u64>(), 2);
    }

    #[test]
    fn luminance_weights_and_rounds_every_channel() {
        for value in 0..=u8::MAX {
            assert_eq!(luminance(value, value, value), value);
        }
        assert_eq!(luminance(255, 0, 0), 54);
        assert_eq!(luminance(0, 255, 0), 182);
        assert_eq!(luminance(0, 0, 255), 18);
        // 2.4344 and 2.5066 before rounding.
        assert_eq!(luminance(0, 3, 4), 2);
        assert_eq!(luminance(0, 3, 5), 3);
    }

    #[test]
    fn rgba_histogram_rejects_wrong_buffer_size() {
        assert!(Histogram::from_rgba8(1, 1, &[0, 0, 0]).is_none());
    }

    #[test]
    fn luminance_percentiles_are_clamped_and_ordered() {
        let image = DisplayRgbImage::new(
            4,
            1,
            12,
            DisplayColorSpace::SRGB,
            [0, 0, 0, 64, 64, 64, 128, 128, 128, 255, 255, 255].to_vec(),
        )
        .expect("valid image");
        let histogram = Histogram::from_display_rgb8(&image);
        assert_eq!(histogram.luminance_percentile(-1.0), 0);
        assert_eq!(histogram.luminance_percentile(1.0), 255);
        assert!(histogram.luminance_percentile(0.25) <= histogram.luminance_percentile(0.75));
    }

    #[test]
    fn brightest_channel_percentile_sees_a_single_saturated_channel() {
        let image = DisplayRgbImage::new(
            2,
            1,
            6,
            DisplayColorSpace::SRGB,
            vec![10, 20, 250, 12, 22, 252],
        )
        .expect("valid image");
        let histogram = Histogram::from_display_rgb8(&image);
        assert_eq!(histogram.brightest_channel_percentile(1.0), 252);
        assert!(histogram.luminance_percentile(1.0) < 50);
    }
}
