use rayon::prelude::*;

use crate::{CancellationToken, LinearRgbImage, PipelineError};
use rohditor_image::allocate_zeroed_f32;

#[derive(Debug)]
struct AreaSample {
    first: usize,
    weights: Vec<f32>,
}

/// Reduce linear RGB with a separable pixel-area filter.
///
/// Each destination sample is the normalized overlap integral of source pixel
/// cells. Filtering remains in the image's existing linear color space and
/// deliberately performs no clipping.
pub(crate) fn resize_area_cancellable(
    image: LinearRgbImage<f32>,
    target_width: usize,
    target_height: usize,
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    cancellation.checkpoint()?;
    let source_width = image.width();
    let source_height = image.height();
    if target_width == 0
        || target_height == 0
        || target_width > source_width
        || target_height > source_height
    {
        return Err(PipelineError::InvalidDimensions {
            width: target_width,
            height: target_height,
            row_stride: 0,
            reason: format!(
                "area reduction target must be non-zero and no larger than {source_width}x{source_height}"
            ),
        });
    }
    if target_width == source_width && target_height == source_height {
        return Ok(image);
    }

    let horizontal_samples = area_samples(source_width, target_width);
    let vertical_samples = area_samples(source_height, target_height);
    let intermediate_stride = target_width.checked_mul(3).ok_or_else(|| {
        invalid_dimensions(target_width, source_height, 0, "RGB stride overflowed")
    })?;
    let intermediate_elements =
        intermediate_stride
            .checked_mul(source_height)
            .ok_or_else(|| {
                invalid_dimensions(
                    target_width,
                    source_height,
                    intermediate_stride,
                    "horizontal area-filter buffer overflowed",
                )
            })?;
    let mut intermediate = allocate_zeroed_f32(intermediate_elements)?;
    intermediate
        .par_chunks_mut(intermediate_stride)
        .enumerate()
        .try_for_each(|(source_y, output_row)| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            let source_row_start = source_y * image.row_stride();
            for (target_x, destination) in output_row.chunks_exact_mut(3).enumerate() {
                let sample = &horizontal_samples[target_x];
                for (offset, &weight) in sample.weights.iter().enumerate() {
                    let source_x = sample.first + offset;
                    let source_start = source_row_start + source_x * 3;
                    for (channel, destination) in destination.iter_mut().enumerate() {
                        *destination += image.data()[source_start + channel] * weight;
                    }
                }
            }
            Ok(())
        })?;
    cancellation.checkpoint()?;
    let space = image.space();
    drop(image);

    let output_stride = intermediate_stride;
    let output_elements = output_stride.checked_mul(target_height).ok_or_else(|| {
        invalid_dimensions(
            target_width,
            target_height,
            output_stride,
            "vertical area-filter buffer overflowed",
        )
    })?;
    let mut output = allocate_zeroed_f32(output_elements)?;
    output
        .par_chunks_mut(output_stride)
        .enumerate()
        .try_for_each(|(target_y, output_row)| -> Result<(), PipelineError> {
            cancellation.checkpoint()?;
            let sample = &vertical_samples[target_y];
            for (offset, &weight) in sample.weights.iter().enumerate() {
                let source_y = sample.first + offset;
                let source_row = &intermediate
                    [source_y * intermediate_stride..(source_y + 1) * intermediate_stride];
                for (destination, &source) in output_row.iter_mut().zip(source_row) {
                    *destination += source * weight;
                }
            }
            Ok(())
        })?;
    cancellation.checkpoint()?;

    LinearRgbImage::new(target_width, target_height, output_stride, space, output)
}

fn area_samples(source_length: usize, target_length: usize) -> Vec<AreaSample> {
    let scale = source_length as f64 / target_length as f64;
    (0..target_length)
        .map(|target| {
            let left = target as f64 * scale;
            let right = (target + 1) as f64 * scale;
            let first = left.floor() as usize;
            let end = (right.ceil() as usize).min(source_length);
            let weights = (first..end)
                .map(|source| {
                    let overlap = right.min((source + 1) as f64) - left.max(source as f64);
                    (overlap / scale) as f32
                })
                .collect();
            AreaSample { first, weights }
        })
        .collect()
}

fn invalid_dimensions(
    width: usize,
    height: usize,
    row_stride: usize,
    reason: &str,
) -> PipelineError {
    PipelineError::InvalidDimensions {
        width,
        height,
        row_stride,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use rayon::ThreadPoolBuilder;

    use super::*;
    use crate::LinearRgbSpace;

    #[test]
    fn exact_two_by_two_reduction_averages_each_source_block() {
        let mut data = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                let value = (y * 4 + x) as f32;
                data.extend_from_slice(&[value, value + 100.0, -value]);
            }
        }
        let image = LinearRgbImage::new(4, 4, 12, LinearRgbSpace::CameraNative, data)
            .expect("valid source");
        let reduced = resize_area_cancellable(image, 2, 2, &CancellationToken::new())
            .expect("valid reduction");
        assert_eq!(reduced.pixel(0, 0), Some(&[2.5, 102.5, -2.5][..]));
        assert_eq!(reduced.pixel(1, 0), Some(&[4.5, 104.5, -4.5][..]));
        assert_eq!(reduced.pixel(0, 1), Some(&[10.5, 110.5, -10.5][..]));
        assert_eq!(reduced.pixel(1, 1), Some(&[12.5, 112.5, -12.5][..]));
    }

    #[test]
    fn asymmetric_fractional_reduction_preserves_a_constant_and_space() {
        let image = LinearRgbImage::new(
            7,
            5,
            21,
            LinearRgbSpace::Rec2020D65,
            [0.25, -0.5, 1.5].repeat(35),
        )
        .expect("valid source");
        let reduced = resize_area_cancellable(image, 3, 2, &CancellationToken::new())
            .expect("valid reduction");
        assert_eq!(reduced.space(), LinearRgbSpace::Rec2020D65);
        for pixel in reduced.data().chunks_exact(3) {
            for (actual, expected) in pixel.iter().zip([0.25, -0.5, 1.5]) {
                assert!((actual - expected).abs() <= 2.0e-7);
            }
        }
    }

    #[test]
    fn area_reduction_is_identical_across_rayon_thread_counts() {
        let data = (0..31 * 23 * 3)
            .map(|index| (index % 101) as f32 / 100.0)
            .collect();
        let image = LinearRgbImage::new(31, 23, 93, LinearRgbSpace::CameraNative, data)
            .expect("valid source");
        let single_pool = ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("single-thread pool");
        let multi_pool = ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("multi-thread pool");
        let single = single_pool
            .install(|| resize_area_cancellable(image.clone(), 13, 11, &CancellationToken::new()))
            .expect("single-thread resize");
        let multiple = multi_pool
            .install(|| resize_area_cancellable(image, 13, 11, &CancellationToken::new()))
            .expect("multi-thread resize");
        assert_eq!(single, multiple);
    }

    #[test]
    fn area_reduction_honors_cancellation_and_rejects_upscaling() {
        let image = LinearRgbImage::new(4, 3, 12, LinearRgbSpace::CameraNative, vec![0.5; 36])
            .expect("valid source");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(matches!(
            resize_area_cancellable(image.clone(), 2, 2, &cancellation),
            Err(PipelineError::Cancelled)
        ));
        assert!(resize_area_cancellable(image, 5, 3, &CancellationToken::new()).is_err());
    }
}
