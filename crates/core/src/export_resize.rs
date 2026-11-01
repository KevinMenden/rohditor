//! Export-only resize of the cropped, oriented linear image.
//!
//! The output step is split into: apply [`OutputGeometry`] to linear pixels,
//! resize with the exact pixel-area filter, then encode and quantize. Exports
//! that keep the original size skip this module and use the fused path.

use rayon::prelude::*;
use rohditor_image::{LinearRgbImage, allocate_zeroed_f32};

use crate::resample::{AreaReductionPlan, resize_area_with_plan_cancellable};
use crate::{CancellationToken, OutputGeometry, PipelineError};

/// Bytes the resize needs beyond the source image: the cropped copy, the
/// horizontal-pass buffer, and the resized result. `None` on overflow.
pub(crate) fn resize_bytes(cropped: (usize, usize), target: (usize, usize)) -> Option<usize> {
    let f32_rgb = 3 * size_of::<f32>();
    let cropped_bytes = cropped.0.checked_mul(cropped.1)?.checked_mul(f32_rgb)?;
    let intermediate = target.0.checked_mul(cropped.1)?.checked_mul(f32_rgb)?;
    let resized = target.0.checked_mul(target.1)?.checked_mul(f32_rgb)?;
    cropped_bytes
        .checked_add(intermediate)?
        .checked_add(resized)
}

/// Crop and orient `image` through `geometry`, then area-reduce it to `target`.
///
/// `target` must not exceed the geometry's output size on either axis.
pub(crate) fn crop_and_resize(
    image: &LinearRgbImage<f32>,
    geometry: OutputGeometry,
    target: (usize, usize),
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    let cropped = crop_and_orient(image, geometry, cancellation)?;
    let plan = AreaReductionPlan::new(cropped.width(), cropped.height(), target.0, target.1)?;
    resize_area_with_plan_cancellable(cropped, &plan, cancellation)
}

fn crop_and_orient(
    image: &LinearRgbImage<f32>,
    geometry: OutputGeometry,
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    cancellation.checkpoint()?;
    let (width, height) = geometry.output_dimensions();
    let stride = width
        .checked_mul(3)
        .ok_or_else(|| overflow(width, height))?;
    let elements = stride
        .checked_mul(height)
        .ok_or_else(|| overflow(width, height))?;
    let mut data = allocate_zeroed_f32(elements)?;
    data.par_chunks_mut(stride).enumerate().try_for_each(
        |(y, row)| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            for (x, destination) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let (source_x, source_y) = geometry.source_coordinate_in_bounds(x, y);
                let start = source_y * image.row_stride() + source_x * 3;
                destination.copy_from_slice(&image.data()[start..start + 3]);
            }
            Ok(())
        },
    )?;
    LinearRgbImage::new(width, height, stride, image.space(), data).map_err(Into::into)
}

fn overflow(width: usize, height: usize) -> PipelineError {
    PipelineError::InvalidDimensions {
        width,
        height,
        row_stride: 0,
        reason: "export resize buffer size overflowed".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use rohditor_edit::NormalizedCropRect;
    use rohditor_image::{LinearRgbSpace, Orientation};

    use super::*;

    /// Channel 0 holds `x + 10 * y`; the others are constant.
    fn ramp(width: usize, height: usize) -> LinearRgbImage<f32> {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[(x + 10 * y) as f32, 1.0, -2.0]);
            }
        }
        LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::Rec2020D65, data)
            .expect("test setup")
    }

    #[test]
    fn area_weights_match_a_hand_computed_seven_by_five_to_three_by_two() {
        // Horizontal cells of width 7/3 average x to [5/7, 3, 37/7];
        // vertical cells of height 5/2 average y to [0.8, 3.2].
        let image = ramp(7, 5);
        let geometry = OutputGeometry::new(7, 5, Orientation::Normal, None).expect("test setup");
        let out = crop_and_resize(&image, geometry, (3, 2), &CancellationToken::new())
            .expect("test setup");
        assert_eq!((out.width(), out.height()), (3, 2));
        let x_means = [5.0 / 7.0, 3.0, 37.0 / 7.0];
        let y_means = [0.8, 3.2];
        for (y, y_mean) in y_means.iter().enumerate() {
            for (x, x_mean) in x_means.iter().enumerate() {
                let pixel = &out.data()[y * out.row_stride() + x * 3..][..3];
                let expected = x_mean + 10.0 * y_mean;
                assert!(
                    (pixel[0] - expected).abs() < 1e-4,
                    "{pixel:?} vs {expected}"
                );
                // Constant and negative channels pass through unclipped.
                assert!((pixel[1] - 1.0).abs() < 1e-5);
                assert!((pixel[2] + 2.0).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn crop_and_rotation_are_applied_before_the_resize() {
        // 8x4 source rotated 90 degrees is 4x8; the 50% crop keeps 4x4.
        let image = ramp(8, 4);
        let crop = NormalizedCropRect {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 0.5,
        };
        let geometry =
            OutputGeometry::new(8, 4, Orientation::Rotate90, Some(crop)).expect("test setup");
        assert_eq!(geometry.output_dimensions(), (4, 4));
        let target = crate::OutputSize::LongEdge { pixels: 2 }.resolve(4, 4);
        assert_eq!(target, (2, 2));
        let out = crop_and_resize(&image, geometry, target, &CancellationToken::new())
            .expect("test setup");
        assert_eq!((out.width(), out.height()), (2, 2));
        // Rotate90 puts source column 0 on the output's top row, reading
        // bottom-to-top; the mean of the top-left 2x2 block must match the
        // un-resized cropped image.
        let full =
            crop_and_orient(&image, geometry, &CancellationToken::new()).expect("test setup");
        let block = [(0, 0), (1, 0), (0, 1), (1, 1)]
            .iter()
            .map(|&(x, y)| full.data()[y * full.row_stride() + x * 3])
            .sum::<f32>()
            / 4.0;
        assert!((out.data()[0] - block).abs() < 1e-4);
    }
}
