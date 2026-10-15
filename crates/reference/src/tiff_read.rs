//! Read tool output TIFFs as normalized linear RGB.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use rohditor_image::{LinearRgbImage, LinearRgbSpace};
use tiff::decoder::{Decoder, DecodingResult};

use crate::ReferenceError;

/// Decode an RGB(A) TIFF; integer samples are scaled to 0..=1, float is kept.
/// The alpha channel, if present, is dropped.
pub fn read_rgb_tiff(path: &Path) -> Result<LinearRgbImage<f32>, ReferenceError> {
    let mut decoder = Decoder::new(BufReader::new(File::open(path)?))?;
    let (width, height) = decoder.dimensions()?;
    let (width, height) = (width as usize, height as usize);
    let samples: Vec<f32> = match decoder.read_image()? {
        DecodingResult::U8(v) => v.into_iter().map(|s| f32::from(s) / 255.0).collect(),
        DecodingResult::U16(v) => v.into_iter().map(|s| f32::from(s) / 65535.0).collect(),
        DecodingResult::F32(v) => v,
        _ => return Err(ReferenceError::Other("unsupported TIFF sample type".into())),
    };
    let per_pixel = samples.len() / (width * height);
    if per_pixel < 3 {
        return Err(ReferenceError::Other("TIFF is not RGB".into()));
    }
    let data = samples
        .chunks_exact(per_pixel)
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();
    Ok(LinearRgbImage::new(
        width,
        height,
        width * 3,
        LinearRgbSpace::CameraNative,
        data,
    )?)
}

/// Undo the sRGB transfer function in place (tools that cannot write linear
/// 16-bit output are decoded this way; the inverse is exact up to quantization).
pub fn srgb_to_linear(image: &mut LinearRgbImage<f32>) {
    for value in image.data_mut() {
        *value = if *value <= 0.040_45 {
            *value / 12.92
        } else {
            ((*value + 0.055) / 1.055).powf(2.4)
        };
    }
}
