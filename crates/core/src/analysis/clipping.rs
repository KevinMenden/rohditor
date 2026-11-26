//! Which pixels of the displayed output are clipped.
//!
//! The rule lives here once ([`is_shadow_clipped`], [`is_highlight_clipped`])
//! and is used by both the histogram's clipped counts and [`ClippingMask`], so
//! the numbers in the histogram panel and the tinted pixels in the viewport
//! describe the same pixels. Like the histogram, a mask is transient render
//! analysis: it is never part of the recipe and never affects an export.

use rohditor_image::DisplayRgbImage;

/// A display channel at exactly this value is clipped to black.
pub(super) const SHADOW_CLIP_LEVEL: u8 = u8::MIN;
/// A display channel at exactly this value is clipped to white.
pub(super) const HIGHLIGHT_CLIP_LEVEL: u8 = u8::MAX;

pub(super) fn is_shadow_clipped(value: u8) -> bool {
    value == SHADOW_CLIP_LEVEL
}

pub(super) fn is_highlight_clipped(value: u8) -> bool {
    value == HIGHLIGHT_CLIP_LEVEL
}

/// Clipping state of one pixel: which ends of the range any channel reached.
///
/// A pixel can be both, for example pure red `(255, 0, 0)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClipFlags(u8);

impl ClipFlags {
    const SHADOW: u8 = 1;
    const HIGHLIGHT: u8 = 2;

    fn of_pixel(red: u8, green: u8, blue: u8) -> Self {
        let mut bits = 0;
        for value in [red, green, blue] {
            if is_shadow_clipped(value) {
                bits |= Self::SHADOW;
            }
            if is_highlight_clipped(value) {
                bits |= Self::HIGHLIGHT;
            }
        }
        Self(bits)
    }

    /// At least one channel is clipped to black.
    #[must_use]
    pub fn shadow(self) -> bool {
        self.0 & Self::SHADOW != 0
    }

    /// At least one channel is clipped to white.
    #[must_use]
    pub fn highlight(self) -> bool {
        self.0 & Self::HIGHLIGHT != 0
    }
}

/// Per-pixel clipping flags for one display image, row-major without padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClippingMask {
    width: usize,
    height: usize,
    flags: Vec<ClipFlags>,
}

impl ClippingMask {
    /// Build a mask from an sRGB8 display image, honouring its row stride.
    #[must_use]
    pub fn from_display_rgb8(image: &DisplayRgbImage<u8>) -> Self {
        let mut flags = Vec::with_capacity(image.width() * image.height());
        for row in image.data().chunks(image.row_stride()).take(image.height()) {
            flags.extend(
                row[..image.width() * 3]
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|pixel| ClipFlags::of_pixel(pixel[0], pixel[1], pixel[2])),
            );
        }
        Self {
            width: image.width(),
            height: image.height(),
            flags,
        }
    }

    /// Build a mask from a packed RGBA8 display buffer, ignoring alpha.
    ///
    /// This is the GPU-preview readback format.
    #[must_use]
    pub fn from_rgba8(width: usize, height: usize, rgba: &[u8]) -> Option<Self> {
        Self::from_packed::<4>(width, height, rgba)
    }

    fn from_packed<const N: usize>(width: usize, height: usize, data: &[u8]) -> Option<Self> {
        let expected = width.checked_mul(height)?.checked_mul(N)?;
        if data.len() != expected {
            return None;
        }
        let flags = data
            .as_chunks::<N>()
            .0
            .iter()
            .map(|pixel| ClipFlags::of_pixel(pixel[0], pixel[1], pixel[2]))
            .collect();
        Some(Self {
            width,
            height,
            flags,
        })
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Flags in row-major order, `width * height` entries.
    #[must_use]
    pub fn flags(&self) -> &[ClipFlags] {
        &self.flags
    }
}

#[cfg(test)]
mod tests {
    use rohditor_image::{DisplayColorSpace, DisplayRgbImage};

    use super::{ClipFlags, ClippingMask};
    use crate::Histogram;

    /// 3x2 image: clipped in one channel, in all channels, in none, both
    /// ends at once, plus a mid-grey and a near-white that is not clipped.
    const PIXELS: [[u8; 3]; 6] = [
        [255, 10, 20],
        [255, 255, 255],
        [10, 20, 30],
        [0, 0, 0],
        [255, 0, 128],
        [254, 1, 128],
    ];

    fn expected_flags() -> Vec<(bool, bool)> {
        // (shadow, highlight)
        vec![
            (false, true),
            (false, true),
            (false, false),
            (true, false),
            (true, true),
            (false, false),
        ]
    }

    fn assert_matches_expected(mask: &ClippingMask) {
        assert_eq!((mask.width(), mask.height()), (3, 2));
        let got: Vec<_> = mask
            .flags()
            .iter()
            .map(|flags| (flags.shadow(), flags.highlight()))
            .collect();
        assert_eq!(got, expected_flags());
    }

    #[test]
    fn display_image_mask_skips_row_stride_padding() {
        // Stride 12 bytes: 9 bytes of pixels then 3 bytes of padding that are
        // all 255 and must not be read as pixels.
        let mut data = Vec::new();
        for row in PIXELS.chunks(3) {
            data.extend(row.iter().flatten());
            data.extend([255, 255, 255]);
        }
        let image = DisplayRgbImage::new(3, 2, 12, DisplayColorSpace::SRGB, data).expect("image");
        assert_matches_expected(&ClippingMask::from_display_rgb8(&image));
    }

    #[test]
    fn packed_rgba_mask_ignores_alpha() {
        let rgba: Vec<u8> = PIXELS
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 0])
            .collect();
        assert_matches_expected(&ClippingMask::from_rgba8(3, 2, &rgba).expect("rgba"));
    }

    #[test]
    fn packed_masks_reject_wrong_buffer_size() {
        assert!(ClippingMask::from_rgba8(1, 1, &[0, 0, 0]).is_none());
        assert!(ClippingMask::from_rgba8(usize::MAX, 2, &[]).is_none());
    }

    #[test]
    fn mask_agrees_with_histogram_clipped_counts() {
        // Every pixel's mask flags must say "clipped" exactly when the
        // histogram counts any channel of that pixel as clipped.
        for pixel in PIXELS {
            let image = DisplayRgbImage::new(1, 1, 3, DisplayColorSpace::SRGB, pixel.to_vec())
                .expect("image");
            let histogram = Histogram::from_display_rgb8(&image);
            let flags = ClippingMask::from_display_rgb8(&image).flags()[0];
            assert_eq!(
                flags.shadow(),
                histogram.shadow_clipped.iter().any(|n| *n > 0)
            );
            assert_eq!(
                flags.highlight(),
                histogram.highlight_clipped.iter().any(|n| *n > 0)
            );
        }
    }

    #[test]
    fn default_flags_are_unclipped() {
        assert!(!ClipFlags::default().shadow());
        assert!(!ClipFlags::default().highlight());
    }
}
