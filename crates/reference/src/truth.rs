//! Ground-truth synthesis from a real Bayer file.
//!
//! Each 2x2 CFA quad of a real raw frame is binned into one full-RGB pixel
//! (green is the mean of the two green sites). The result has real texture
//! and noise, and re-mosaicing it gives an input whose correct demosaic
//! output is known exactly.

use std::path::Path;

use rohditor_image::{BayerPattern, CfaColor, LinearRgbImage, LinearRgbSpace, MosaicImage};
use rohditor_raw::{PhotometricInterpretation, RawDecoder, RawlerDecoder};

use crate::ReferenceError;

/// Decode `path` and bin a centred `width` x `height` (output pixels) region.
pub fn load_ground_truth(
    path: &Path,
    width: usize,
    height: usize,
) -> Result<LinearRgbImage<f32>, ReferenceError> {
    let frame = RawlerDecoder::default().decode(path)?;
    let info = &frame.info;
    let PhotometricInterpretation::Cfa { pattern } = &info.photometric_interpretation else {
        return Err(ReferenceError::Other("source is not a CFA image".into()));
    };
    let source_pattern = BayerPattern::parse(&pattern.name, pattern.width, pattern.height)?;
    let (src_w, src_h) = (info.width, info.height);
    if width * 2 > src_w || height * 2 > src_h {
        return Err(ReferenceError::Other("requested crop exceeds frame".into()));
    }
    // Even offsets keep the Bayer phase of the source.
    let x0 = (src_w - width * 2) / 4 * 2;
    let y0 = (src_h - height * 2) / 4 * 2;
    let white = info.white_levels.first().copied().unwrap_or(65535.0);
    let black = &info.black_levels;

    let sample = |x: usize, y: usize| -> f32 {
        let level = black
            .values
            .get((y % black.repeat_height) * black.repeat_width + x % black.repeat_width)
            .copied()
            .unwrap_or(0.0);
        let raw = f32::from(frame.mosaic[y * frame.row_stride + x]);
        ((raw - level) / (white - level)).clamp(0.0, 1.0)
    };

    let mut data = Vec::with_capacity(width * height * 3);
    for j in 0..height {
        for i in 0..width {
            let mut rgb = [0.0_f32; 3];
            let mut greens = 0.0_f32;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (x, y) = (x0 + 2 * i + dx, y0 + 2 * j + dy);
                let value = sample(x, y);
                match source_pattern.color_at(x, y) {
                    CfaColor::Green => greens += value * 0.5,
                    color => rgb[color.channel_index()] = value,
                }
            }
            rgb[1] = greens;
            data.extend_from_slice(&rgb);
        }
    }
    Ok(LinearRgbImage::new(
        width,
        height,
        width * 3,
        LinearRgbSpace::CameraNative,
        data,
    )?)
}

/// Sample `truth` through a CFA, producing the mosaic a sensor would record.
pub fn remosaic(
    truth: &LinearRgbImage<f32>,
    pattern: BayerPattern,
) -> Result<MosaicImage<f32>, ReferenceError> {
    let (width, height) = (truth.width(), truth.height());
    let mut data = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let channel = pattern.color_at(x, y).channel_index();
            data.push(truth.data()[y * truth.row_stride() + x * 3 + channel]);
        }
    }
    Ok(MosaicImage::new(width, height, width, pattern, data)?)
}

/// Copy the `width` x `height` window at (`x0`, `y0`) out of `image`.
pub fn crop(
    image: &LinearRgbImage<f32>,
    x0: usize,
    y0: usize,
    width: usize,
    height: usize,
) -> Result<LinearRgbImage<f32>, ReferenceError> {
    let mut data = Vec::with_capacity(width * height * 3);
    for y in y0..y0 + height {
        let start = y * image.row_stride() + x0 * 3;
        data.extend_from_slice(&image.data()[start..start + width * 3]);
    }
    Ok(LinearRgbImage::new(
        width,
        height,
        width * 3,
        image.space(),
        data,
    )?)
}

/// Cut the `margin`-pixel interior of a `full_size` frame out of `image`,
/// which may be a centred crop of that frame (RawTherapee drops a few border
/// pixels). Lets differently cropped tool outputs be compared pixel for pixel.
pub fn interior_window(
    image: &LinearRgbImage<f32>,
    full_size: (usize, usize),
    margin: usize,
) -> Result<LinearRgbImage<f32>, ReferenceError> {
    let (dx, dy) = (
        full_size.0.checked_sub(image.width()),
        full_size.1.checked_sub(image.height()),
    );
    let (Some(dx), Some(dy)) = (dx, dy) else {
        return Err(ReferenceError::Other(
            "tool output exceeds the input".into(),
        ));
    };
    if dx % 2 != 0 || dy % 2 != 0 || dx / 2 > margin || dy / 2 > margin {
        return Err(ReferenceError::Other(
            "tool output is not a centred crop within the margin".into(),
        ));
    }
    crop(
        image,
        margin - dx / 2,
        margin - dy / 2,
        full_size.0 - 2 * margin,
        full_size.1 - 2 * margin,
    )
}

/// Halve the resolution with a 2x2 box filter. Averaging four independent
/// noisy samples halves the noise, so the truth better reflects the scene
/// and demosaic errors stop being dominated by sensor noise.
pub fn downsample2(image: &LinearRgbImage<f32>) -> Result<LinearRgbImage<f32>, ReferenceError> {
    let (width, height) = (image.width() / 2, image.height() / 2);
    let mut data = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            for c in 0..3 {
                let at = |dx: usize, dy: usize| {
                    image.data()[(2 * y + dy) * image.row_stride() + (2 * x + dx) * 3 + c]
                };
                data.push((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) * 0.25);
            }
        }
    }
    Ok(LinearRgbImage::new(
        width,
        height,
        width * 3,
        image.space(),
        data,
    )?)
}
