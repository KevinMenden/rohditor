use rohditor_image::DisplayRgbImage;

use crate::CropRect;

/// Number of 8-bit display levels.
pub const LUMA_BINS: usize = 256;

/// Counts of 8-bit display luminance values.
pub type LuminanceHistogram = [u64; LUMA_BINS];

/// Luminance histogram of a region of an sRGB8 image.
///
/// Luminance is Rec.709 weighting of the encoded values, matching the editor's
/// own display histogram, so both sides of a fit are measured the same way.
/// The rectangle is clamped to the image.
#[must_use]
pub fn luminance_histogram(image: &DisplayRgbImage<u8>, rect: CropRect) -> LuminanceHistogram {
    let mut histogram = [0_u64; LUMA_BINS];
    let x_end = (rect.x + rect.width).min(image.width());
    let y_end = (rect.y + rect.height).min(image.height());
    for row in rect.y..y_end {
        let start = row * image.row_stride() + rect.x * 3;
        let end = row * image.row_stride() + x_end * 3;
        for pixel in image.data()[start..end].as_chunks::<3>().0 {
            let luma = 0.2126 * f32::from(pixel[0])
                + 0.7152 * f32::from(pixel[1])
                + 0.0722 * f32::from(pixel[2]);
            histogram[luma.round().clamp(0.0, 255.0) as usize] += 1;
        }
    }
    histogram
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_image::DisplayTransfer;

    #[test]
    fn counts_only_the_requested_rectangle() {
        // 3x2 image: left column white, the rest black.
        let mut data = vec![0_u8; 3 * 2 * 3];
        for row in 0..2 {
            data[row * 9..row * 9 + 3].fill(255);
        }
        let image = DisplayRgbImage::new(3, 2, 9, DisplayTransfer::Srgb, data).expect("image");
        let all = luminance_histogram(&image, CropRect::full(&image));
        assert_eq!((all[0], all[255]), (4, 2));
        let right = luminance_histogram(
            &image,
            CropRect {
                x: 1,
                y: 0,
                width: 2,
                height: 2,
            },
        );
        assert_eq!((right[0], right[255]), (4, 0));
    }
}
