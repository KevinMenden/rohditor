//! Export output with output sharpening between encoding and quantization.
//!
//! The fused conversions in `stages::display` encode and quantize in one
//! pass. Output sharpening needs the transfer-encoded floats in between, so
//! this path runs the same per-pixel encode, sharpens, then quantizes with
//! the same rounding and dither.

use rayon::prelude::*;
use rohditor_detail::{DetailError, OutputSharpen, sharpen_output};
use rohditor_image::{
    DisplayRgbImage, DisplayTransfer, LinearRgbImage, LinearRgbSpace, allocate_zeroed_f32,
    allocate_zeroed_u8, allocate_zeroed_u16,
};

use super::stages::display::quantization_dither;
use crate::color::{
    LINEAR_REC2020_TO_XYZ_D65, XYZ_D65_TO_LINEAR_SRGB, encode_rec2020_for_srgb_output,
};
use crate::{
    CancellationToken, DitherMode, ExportImage, GamutMappingDiagnostics, OutputBitDepth,
    OutputGeometry, OutputPolicy, PipelineError,
};

/// Encode `image` to sRGB floats, sharpen, and quantize to `bit_depth`.
pub(crate) fn render_sharpened_export(
    image: &LinearRgbImage<f32>,
    geometry: OutputGeometry,
    output_policy: OutputPolicy,
    dithering: DitherMode,
    bit_depth: OutputBitDepth,
    sharpening: OutputSharpen,
    cancellation: &CancellationToken,
) -> Result<(ExportImage, GamutMappingDiagnostics), PipelineError> {
    let (mut encoded, diagnostics) = encode_srgb_f32(image, geometry, output_policy, cancellation)?;
    sharpen_output(&mut encoded, sharpening, &|| cancellation.is_cancelled()).map_err(|error| {
        match error {
            DetailError::Cancelled => PipelineError::Cancelled,
            other => PipelineError::InvalidRecipe {
                field: "output sharpening",
                reason: other.to_string(),
            },
        }
    })?;
    let image = match bit_depth {
        OutputBitDepth::Eight => ExportImage::Rgb8(quantize_u8(&encoded, dithering)?),
        OutputBitDepth::Sixteen => ExportImage::Rgb16(quantize_u16(&encoded, dithering)?),
    };
    Ok((image, diagnostics))
}

/// Bytes of the transfer-encoded float buffer plus the sharpening planes.
/// `None` on overflow.
pub(crate) fn sharpened_output_bytes(width: usize, height: usize) -> Option<usize> {
    let encoded = width
        .checked_mul(height)?
        .checked_mul(3 * size_of::<f32>())?;
    encoded.checked_add(rohditor_detail::output_sharpen_scratch_bytes(
        width, height,
    )?)
}

fn encode_srgb_f32(
    image: &LinearRgbImage<f32>,
    geometry: OutputGeometry,
    output_policy: OutputPolicy,
    cancellation: &CancellationToken,
) -> Result<(DisplayRgbImage<f32>, GamutMappingDiagnostics), PipelineError> {
    cancellation.checkpoint()?;
    if image.space() != LinearRgbSpace::Rec2020D65 {
        return Err(PipelineError::WrongImageState {
            expected: LinearRgbSpace::Rec2020D65.description(),
            actual: image.space().description(),
        });
    }
    let (width, height) = geometry.output_dimensions();
    let stride = width
        .checked_mul(3)
        .ok_or_else(|| overflow(width, height))?;
    let elements = stride
        .checked_mul(height)
        .ok_or_else(|| overflow(width, height))?;
    let mut data = allocate_zeroed_f32(elements)?;
    let rec2020_to_srgb = LINEAR_REC2020_TO_XYZ_D65.then(XYZ_D65_TO_LINEAR_SRGB);
    let diagnostics = data
        .par_chunks_mut(stride)
        .enumerate()
        .try_fold(
            GamutMappingDiagnostics::default,
            |mut diagnostics, (y, row)| -> Result<_, PipelineError> {
                cancellation.checkpoint()?;
                for (x, destination) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                    let (source_x, source_y) = geometry.source_coordinate_in_bounds(x, y);
                    let start = source_y * image.row_stride() + source_x * 3;
                    let source = &image.data()[start..start + 3];
                    let (encoded, status) = encode_rec2020_for_srgb_output(
                        rec2020_to_srgb,
                        [source[0], source[1], source[2]],
                        output_policy,
                    );
                    diagnostics.record(status);
                    *destination = encoded;
                }
                Ok(diagnostics)
            },
        )
        .try_reduce(GamutMappingDiagnostics::default, |left, right| {
            Ok(left.merge(right))
        })?;
    let image = DisplayRgbImage::new(width, height, stride, DisplayTransfer::Srgb, data)?;
    Ok((image, diagnostics))
}

fn quantize_u8(
    image: &DisplayRgbImage<f32>,
    dithering: DitherMode,
) -> Result<DisplayRgbImage<u8>, PipelineError> {
    let (width, height, stride) = (image.width(), image.height(), image.row_stride());
    let mut output = allocate_zeroed_u8(image.data().len())?;
    output
        .par_chunks_mut(stride)
        .zip(image.data().par_chunks(stride))
        .enumerate()
        .for_each(|(y, (out_row, in_row))| {
            let pixels = out_row
                .as_chunks_mut::<3>()
                .0
                .iter_mut()
                .zip(in_row.as_chunks::<3>().0);
            for (x, (out, value)) in pixels.enumerate() {
                let dither = quantization_dither(dithering, x, y);
                for (out, value) in out.iter_mut().zip(value) {
                    *out = (value * 255.0 + dither).round().clamp(0.0, 255.0) as u8;
                }
            }
        });
    Ok(DisplayRgbImage::new(
        width,
        height,
        stride,
        DisplayTransfer::Srgb,
        output,
    )?)
}

fn quantize_u16(
    image: &DisplayRgbImage<f32>,
    dithering: DitherMode,
) -> Result<DisplayRgbImage<u16>, PipelineError> {
    let (width, height, stride) = (image.width(), image.height(), image.row_stride());
    let mut output = allocate_zeroed_u16(image.data().len())?;
    output
        .par_chunks_mut(stride)
        .zip(image.data().par_chunks(stride))
        .enumerate()
        .for_each(|(y, (out_row, in_row))| {
            let pixels = out_row
                .as_chunks_mut::<3>()
                .0
                .iter_mut()
                .zip(in_row.as_chunks::<3>().0);
            for (x, (out, value)) in pixels.enumerate() {
                let dither = quantization_dither(dithering, x, y);
                for (out, value) in out.iter_mut().zip(value) {
                    *out = (value * 65_535.0 + dither).round().clamp(0.0, 65_535.0) as u16;
                }
            }
        });
    Ok(DisplayRgbImage::new(
        width,
        height,
        stride,
        DisplayTransfer::Srgb,
        output,
    )?)
}

fn overflow(width: usize, height: usize) -> PipelineError {
    PipelineError::InvalidDimensions {
        width,
        height,
        row_stride: 0,
        reason: "sharpened export buffer size overflowed".to_owned(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rohditor_image::Orientation;

    use super::*;
    use crate::cpu::{
        render_display_srgb8_dithered_with_geometry_and_diagnostics,
        render_display_srgb16_with_geometry_and_diagnostics,
    };

    /// Asymmetric 5x3 linear image with values above 1, a negative sample
    /// and a saturated colour, so gamut mapping and clamping are exercised.
    fn test_image() -> LinearRgbImage<f32> {
        let data = (0..5 * 3 * 3)
            .map(|i| match i {
                4 => -0.05,
                7 => 1.6,
                _ => (i as f32 * 0.37).sin().abs(),
            })
            .collect();
        LinearRgbImage::new(5, 3, 15, LinearRgbSpace::Rec2020D65, data).unwrap()
    }

    /// With no amount the sharpened path must match the fused conversion
    /// bit for bit, so only the sharpening itself changes pixels.
    #[test]
    fn zero_amount_matches_the_fused_conversion() {
        let image = test_image();
        let geometry = OutputGeometry::new(5, 3, Orientation::Rotate90, None).unwrap();
        let none = OutputSharpen {
            sigma: 1.0,
            amount: 0.0,
            threshold: 0.0,
            overshoot: 0.0,
        };
        let policy = OutputPolicy::ClipToSrgb;
        let token = CancellationToken::new();

        let (sharpened, diagnostics) = render_sharpened_export(
            &image,
            geometry,
            policy,
            DitherMode::Ordered8x8,
            OutputBitDepth::Eight,
            none,
            &token,
        )
        .unwrap();
        let (fused, fused_diagnostics) =
            render_display_srgb8_dithered_with_geometry_and_diagnostics(
                &image,
                geometry,
                policy,
                DitherMode::Ordered8x8,
            )
            .unwrap();
        let ExportImage::Rgb8(sharpened) = sharpened else {
            panic!("expected 8-bit output");
        };
        assert_eq!(sharpened, fused);
        assert_eq!(diagnostics, fused_diagnostics);

        let (sharpened, _) = render_sharpened_export(
            &image,
            geometry,
            policy,
            DitherMode::None,
            OutputBitDepth::Sixteen,
            none,
            &token,
        )
        .unwrap();
        let (fused, _) = render_display_srgb16_with_geometry_and_diagnostics(
            &image,
            geometry,
            policy,
            DitherMode::None,
        )
        .unwrap();
        let ExportImage::Rgb16(sharpened) = sharpened else {
            panic!("expected 16-bit output");
        };
        assert_eq!(sharpened, fused);
    }

    #[test]
    fn cancellation_is_reported() {
        let token = CancellationToken::new();
        token.cancel();
        let geometry = OutputGeometry::new(5, 3, Orientation::Normal, None).unwrap();
        let result = render_sharpened_export(
            &test_image(),
            geometry,
            OutputPolicy::ClipToSrgb,
            DitherMode::None,
            OutputBitDepth::Eight,
            OutputSharpen {
                sigma: 1.0,
                amount: 0.5,
                threshold: 0.0,
                overshoot: 0.02,
            },
            &token,
        );
        assert!(matches!(result, Err(PipelineError::Cancelled)));
    }
}
