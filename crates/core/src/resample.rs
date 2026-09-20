use rayon::prelude::*;
use rohditor_image::{LinearRgbImage, allocate_zeroed_f32};

use crate::{CancellationToken, PipelineError};

/// Version of the exact separable pixel-area reduction contract.
pub const AREA_REDUCTION_ALGORITHM_VERSION: u16 = 1;

/// One destination-axis sample into a flattened overlap-weight table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AreaAxisSample {
    first: usize,
    weight_offset: usize,
    weight_count: usize,
}

impl AreaAxisSample {
    #[must_use]
    pub const fn first(&self) -> usize {
        self.first
    }

    #[must_use]
    pub const fn weight_offset(&self) -> usize {
        self.weight_offset
    }

    #[must_use]
    pub const fn weight_count(&self) -> usize {
        self.weight_count
    }
}

/// Immutable overlap table for one axis of exact area reduction.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaReductionAxis {
    source_length: usize,
    target_length: usize,
    samples: Vec<AreaAxisSample>,
    weights: Vec<f32>,
    fingerprint: u64,
}

impl AreaReductionAxis {
    pub fn new(source_length: usize, target_length: usize) -> Result<Self, PipelineError> {
        if source_length == 0 || target_length == 0 || target_length > source_length {
            return Err(invalid_dimensions(
                target_length,
                1,
                0,
                "area reduction axis must be non-zero and cannot upscale",
            ));
        }
        // Each source sample contributes once, plus at most two boundary
        // contributions per destination. Reject impossible public inputs
        // before floating-point conversion or infallible vector growth.
        let allocation_error = || {
            invalid_dimensions(
                target_length,
                1,
                0,
                "area weight table exceeds the CPU budget",
            )
        };
        let weight_capacity = target_length
            .checked_mul(2)
            .and_then(|count| source_length.checked_add(count))
            .ok_or_else(allocation_error)?;
        let table_bytes = weight_capacity
            .checked_mul(size_of::<f32>())
            .and_then(|bytes| {
                target_length
                    .checked_mul(size_of::<AreaAxisSample>())
                    .and_then(|samples| bytes.checked_add(samples))
            })
            .ok_or_else(allocation_error)?;
        if table_bytes > crate::CPU_WORKING_SET_LIMIT_BYTES {
            return Err(allocation_error());
        }
        let scale = source_length as f64 / target_length as f64;
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(target_length)
            .map_err(|_| allocation_error())?;
        let mut weights = Vec::new();
        weights
            .try_reserve_exact(weight_capacity)
            .map_err(|_| allocation_error())?;
        for target in 0..target_length {
            let left = target as f64 * scale;
            let right = (target + 1) as f64 * scale;
            let first = left.floor() as usize;
            let end = (right.ceil() as usize).min(source_length);
            let weight_offset = weights.len();
            for source in first..end {
                let overlap = right.min((source + 1) as f64) - left.max(source as f64);
                weights.push((overlap / scale) as f32);
            }
            samples.push(AreaAxisSample {
                first,
                weight_offset,
                weight_count: end - first,
            });
        }
        let fingerprint = area_axis_fingerprint(source_length, target_length, &samples, &weights);
        Ok(Self {
            source_length,
            target_length,
            samples,
            weights,
            fingerprint,
        })
    }

    #[must_use]
    pub const fn source_length(&self) -> usize {
        self.source_length
    }

    #[must_use]
    pub const fn target_length(&self) -> usize {
        self.target_length
    }

    #[must_use]
    pub fn samples(&self) -> &[AreaAxisSample] {
        &self.samples
    }

    #[must_use]
    pub fn weights(&self) -> &[f32] {
        &self.weights
    }

    #[must_use]
    pub const fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    fn weights_for(&self, sample: AreaAxisSample) -> &[f32] {
        &self.weights[sample.weight_offset..sample.weight_offset + sample.weight_count]
    }
}

/// Shared horizontal and vertical tables for one exact area reduction.
#[derive(Debug, Clone, PartialEq)]
pub struct AreaReductionPlan {
    horizontal: AreaReductionAxis,
    vertical: AreaReductionAxis,
    fingerprint: u64,
}

impl AreaReductionPlan {
    pub fn new(
        source_width: usize,
        source_height: usize,
        target_width: usize,
        target_height: usize,
    ) -> Result<Self, PipelineError> {
        let horizontal = AreaReductionAxis::new(source_width, target_width)?;
        let vertical = AreaReductionAxis::new(source_height, target_height)?;
        let fingerprint = horizontal.fingerprint().rotate_left(17)
            ^ vertical.fingerprint()
            ^ u64::from(AREA_REDUCTION_ALGORITHM_VERSION);
        Ok(Self {
            horizontal,
            vertical,
            fingerprint,
        })
    }

    #[must_use]
    pub const fn horizontal(&self) -> &AreaReductionAxis {
        &self.horizontal
    }

    #[must_use]
    pub const fn vertical(&self) -> &AreaReductionAxis {
        &self.vertical
    }

    #[must_use]
    pub const fn source_dimensions(&self) -> (usize, usize) {
        (self.horizontal.source_length, self.vertical.source_length)
    }

    #[must_use]
    pub const fn target_dimensions(&self) -> (usize, usize) {
        (self.horizontal.target_length, self.vertical.target_length)
    }

    #[must_use]
    pub const fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

/// Reduce linear RGB with a separable pixel-area filter.
///
/// Each destination sample is the normalized overlap integral of source pixel
/// cells. Filtering remains in the image's existing linear color space and
/// deliberately performs no clipping.
#[cfg(test)]
fn resize_area_cancellable(
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

    let plan = AreaReductionPlan::new(source_width, source_height, target_width, target_height)?;
    resize_area_with_plan_cancellable(image, &plan, cancellation)
}

pub(crate) fn resize_area_with_plan_cancellable(
    image: LinearRgbImage<f32>,
    plan: &AreaReductionPlan,
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    cancellation.checkpoint()?;
    let source_width = image.width();
    let source_height = image.height();
    let (planned_width, planned_height) = plan.source_dimensions();
    let (target_width, target_height) = plan.target_dimensions();
    if (source_width, source_height) != (planned_width, planned_height) {
        return Err(invalid_dimensions(
            source_width,
            source_height,
            image.row_stride(),
            "area reduction plan does not match its source image",
        ));
    }
    if (source_width, source_height) == (target_width, target_height) {
        return Ok(image);
    }
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
            for (target_x, destination) in output_row.as_chunks_mut::<3>().0.iter_mut().enumerate()
            {
                let sample = plan.horizontal.samples[target_x];
                for (offset, &weight) in plan.horizontal.weights_for(sample).iter().enumerate() {
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
            let sample = plan.vertical.samples[target_y];
            for (offset, &weight) in plan.vertical.weights_for(sample).iter().enumerate() {
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
        .map_err(Into::into)
}

fn area_axis_fingerprint(
    source_length: usize,
    target_length: usize,
    samples: &[AreaAxisSample],
    weights: &[f32],
) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    let mut add = |word: u64| {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    add(source_length as u64);
    add(target_length as u64);
    add(u64::from(AREA_REDUCTION_ALGORITHM_VERSION));
    for sample in samples {
        add(sample.first as u64);
        add(sample.weight_offset as u64);
        add(sample.weight_count as u64);
    }
    for weight in weights {
        add(u64::from(weight.to_bits()));
    }
    hash
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
    use rohditor_image::LinearRgbSpace;

    #[test]
    fn area_tables_reject_overflow_and_excessive_allocations() {
        assert!(AreaReductionAxis::new(usize::MAX, 1).is_err());
        assert!(AreaReductionAxis::new(usize::MAX, usize::MAX).is_err());
        assert!(AreaReductionAxis::new(crate::CPU_WORKING_SET_LIMIT_BYTES, 1).is_err());
    }

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
        for pixel in reduced.data().as_chunks::<3>().0 {
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
