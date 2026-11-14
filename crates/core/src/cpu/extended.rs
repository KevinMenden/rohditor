//! Extended-range linear display output for the HDR rendition.
//!
//! The result is unquantized and linear, in the same primaries as the SDR base,
//! with values relative to diffuse white (1.0) up to the requested headroom.
//! A gain map is then a plain per-pixel division of this image by the SDR base.

use rayon::prelude::*;
use rohditor_image::{
    ExtendedLinearRgbImage, LinearRgbImage, LinearRgbSpace, RgbPrimaries, allocate_zeroed_f32,
};

use crate::color::DisplayOutputTransform;
use crate::{
    CancellationToken, GamutMappingDiagnostics, OutputGeometry, OutputPolicy, PipelineError,
};

/// Convert linear Rec.2020 to extended-range linear RGB in `primaries`,
/// physically applying the geometry's orientation and crop.
///
/// Channels stay within `0..=headroom`: out-of-gamut chroma is mapped into the
/// target gamut per `output_policy`, but nothing is clamped at diffuse white.
///
/// # Errors
///
/// Fails when the input is not linear Rec.2020, when `headroom` is not finite
/// and at least 1.0, on dimension overflow, or when cancelled.
pub fn render_extended_linear_with_geometry(
    image: &LinearRgbImage<f32>,
    geometry: OutputGeometry,
    primaries: RgbPrimaries,
    headroom: f32,
    output_policy: OutputPolicy,
    cancellation: &CancellationToken,
) -> Result<(ExtendedLinearRgbImage<f32>, GamutMappingDiagnostics), PipelineError> {
    let span = tracing::info_span!(
        "cpu.extended_output_conversion",
        width = image.width(),
        height = image.height(),
        headroom
    );
    let _guard = span.enter();
    cancellation.checkpoint()?;
    // Checked up front: the gamut mapper assumes a usable ceiling.
    if !headroom.is_finite() || headroom < 1.0 {
        return Err(PipelineError::InvalidHeadroom { headroom });
    }
    if image.space() != LinearRgbSpace::Rec2020D65 {
        return Err(PipelineError::WrongImageState {
            expected: LinearRgbSpace::Rec2020D65.description(),
            actual: image.space().description(),
        });
    }
    let (output_width, output_height) = geometry.output_dimensions();
    let row_stride =
        output_width
            .checked_mul(3)
            .ok_or_else(|| PipelineError::InvalidDimensions {
                width: output_width,
                height: output_height,
                row_stride: 0,
                reason: "RGB stride overflowed".to_owned(),
            })?;
    let elements =
        row_stride
            .checked_mul(output_height)
            .ok_or_else(|| PipelineError::InvalidDimensions {
                width: output_width,
                height: output_height,
                row_stride,
                reason: "RGB sample count overflowed".to_owned(),
            })?;
    let mut output = allocate_zeroed_f32(elements)?;
    let transform = DisplayOutputTransform::new(primaries);
    let diagnostics = output
        .par_chunks_mut(row_stride)
        .enumerate()
        .try_fold(
            GamutMappingDiagnostics::default,
            |mut diagnostics, (output_y, output_row)| -> Result<_, PipelineError> {
                cancellation.checkpoint()?;
                for (output_x, destination) in
                    output_row.as_chunks_mut::<3>().0.iter_mut().enumerate()
                {
                    let (source_x, source_y) =
                        geometry.source_coordinate_in_bounds(output_x, output_y);
                    let start = source_y * image.row_stride() + source_x * 3;
                    let source = &image.data()[start..start + 3];
                    let mapped =
                        transform.map([source[0], source[1], source[2]], output_policy, headroom);
                    diagnostics.record(mapped.status);
                    *destination = mapped.linear_rgb;
                }
                Ok(diagnostics)
            },
        )
        .try_reduce(GamutMappingDiagnostics::default, |left, right| {
            Ok(left.merge(right))
        })?;
    cancellation.checkpoint()?;
    let image = ExtendedLinearRgbImage::new(
        output_width,
        output_height,
        row_stride,
        primaries,
        headroom,
        output,
    )?;
    Ok((image, diagnostics))
}
