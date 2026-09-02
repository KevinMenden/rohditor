use rohditor_image::DisplayRgbImage;

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
        let mut histogram = Self::default();
        for row in image.data().chunks(image.row_stride()).take(image.height()) {
            for pixel in row[..image.width() * 3].as_chunks::<3>().0 {
                histogram.add_pixel(pixel[0], pixel[1], pixel[2]);
            }
        }
        histogram
    }

    /// Build a histogram from a packed RGBA8 display buffer.
    ///
    /// This is used by the debounced asynchronous GPU-preview analysis path.
    /// The GPU display texture remains the normal display source; the readback
    /// is consumed by analysis and never installed as the UI texture.
    pub fn from_rgba8(width: usize, height: usize, rgba: &[u8]) -> Option<Self> {
        let expected = width.checked_mul(height)?.checked_mul(4)?;
        if rgba.len() != expected {
            return None;
        }
        let mut histogram = Self::default();
        for pixel in rgba.as_chunks::<4>().0 {
            histogram.add_pixel(pixel[0], pixel[1], pixel[2]);
        }
        Some(histogram)
    }

    /// Return the luminance bin containing the requested cumulative fraction.
    #[must_use]
    pub fn luminance_percentile(&self, fraction: f32) -> u8 {
        let total = self.luminance.iter().sum::<u64>();
        if total == 0 {
            return 0;
        }
        let target = (f64::from(fraction.clamp(0.0, 1.0)) * (total - 1) as f64) as u64;
        let mut cumulative = 0_u64;
        for (index, count) in self.luminance.iter().enumerate() {
            cumulative = cumulative.saturating_add(*count);
            if cumulative > target {
                return index as u8;
            }
        }
        u8::MAX
    }

    fn add_pixel(&mut self, red: u8, green: u8, blue: u8) {
        self.red[usize::from(red)] += 1;
        self.green[usize::from(green)] += 1;
        self.blue[usize::from(blue)] += 1;
        let luminance =
            (0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue))
                .round()
                .clamp(0.0, 255.0) as usize;
        self.luminance[luminance] += 1;
        for (index, value) in [red, green, blue].into_iter().enumerate() {
            if value == 0 {
                self.shadow_clipped[index] += 1;
            }
            if value == u8::MAX {
                self.highlight_clipped[index] += 1;
            }
        }
    }
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
    use super::Histogram;
    use rohditor_image::{DisplayRgbImage, DisplayTransfer};

    #[test]
    fn histogram_counts_channels_luminance_and_clipping() {
        let image = DisplayRgbImage::new(
            2,
            1,
            6,
            DisplayTransfer::Srgb,
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
    fn rgba_histogram_rejects_wrong_buffer_size() {
        assert!(Histogram::from_rgba8(1, 1, &[0, 0, 0]).is_none());
    }

    #[test]
    fn luminance_percentiles_are_clamped_and_ordered() {
        let image = DisplayRgbImage::new(
            4,
            1,
            12,
            DisplayTransfer::Srgb,
            [0, 0, 0, 64, 64, 64, 128, 128, 128, 255, 255, 255].to_vec(),
        )
        .expect("valid image");
        let histogram = Histogram::from_display_rgb8(&image);
        assert_eq!(histogram.luminance_percentile(-1.0), 0);
        assert_eq!(histogram.luminance_percentile(1.0), 255);
        assert!(histogram.luminance_percentile(0.25) <= histogram.luminance_percentile(0.75));
    }
}
