//! Camera-native Opposed / local-inpainting reconstruction.
//!
//! This is an original Bayer-domain implementation informed by the opposed
//! recovery family, rather than a port of a demosaiced RGB implementation.
//! It keeps the algorithm deliberately local and conservative: valid nearby
//! photosites provide a chrominance offset, while the target cell's surviving
//! channels provide the opposing reference for the missing channel.

use rayon::prelude::*;
use rohditor_image::MosaicImage;

use crate::cells::CellSummaries;
use crate::detect::is_suspected_clipped;
use crate::{
    ChannelDetectionLevels, HighlightError, OpposedOptions, OpposedOutput, OpposedStats, checkpoint,
};

const FIRST_RADIUS: usize = 1;
const LAST_RADIUS: usize = 3;
const MAX_CANDIDATES: usize = 48;
const MIN_CANDIDATES: usize = 3;
const DARK_SUPPORT_FRACTION: f32 = 0.2;
const CROSS_CHANNEL_TOLERANCE_EV: f32 = 0.75;
const MAXIMUM_ESTIMATE_MULTIPLIER: f32 = 4.0;

/// Logical bytes occupied by the immutable cell summaries used by Opposed.
#[must_use]
pub fn opposed_scratch_bytes(width: usize, height: usize) -> Option<usize> {
    crate::cells::scratch_bytes(width, height)
}

/// Reconstruct suspected-clipped Bayer sites using local opposing-channel
/// evidence.
pub fn reconstruct_opposed(
    mosaic: MosaicImage<f32>,
    options: OpposedOptions,
) -> Result<OpposedOutput, HighlightError> {
    reconstruct_opposed_cancellable(mosaic, options, &|| false)
}

/// Cancellable form of [`reconstruct_opposed`].
pub fn reconstruct_opposed_cancellable(
    mut mosaic: MosaicImage<f32>,
    options: OpposedOptions,
    cancellation: &dyn crate::CancellationCheck,
) -> Result<OpposedOutput, HighlightError> {
    options.detection_levels.validate()?;
    checkpoint(cancellation)?;
    let validation = validate_visible_samples(&mosaic, options.detection_levels, cancellation)?;
    if validation.suspected_clipped_sites == 0 {
        checkpoint(cancellation)?;
        return Ok(OpposedOutput {
            mosaic,
            stats: validation,
        });
    }

    let summaries = CellSummaries::build(&mosaic, options.detection_levels, cancellation)?;
    let width = mosaic.width();
    let row_stride = mosaic.row_stride();
    let pattern = mosaic.pattern();
    let mut reconstruction = mosaic
        .data_mut()
        .par_chunks_mut(row_stride)
        .enumerate()
        .try_fold(
            OpposedStats::default,
            |mut row_stats, (y, output_row)| -> Result<OpposedStats, HighlightError> {
                checkpoint(cancellation)?;
                for (x, sample) in output_row[..width].iter_mut().enumerate() {
                    let color = pattern.color_at(x, y);
                    let channel = color.channel_index();
                    let level = options.detection_levels.for_color(color);
                    if !is_suspected_clipped(*sample, level) {
                        continue;
                    }

                    let target_x = x / 2;
                    let target_y = y / 2;
                    let target = summaries.get(target_x, target_y);
                    let support_count = opposing_support_count(target, channel);
                    let estimate = estimate_site(
                        target,
                        channel,
                        options.detection_levels,
                        &summaries,
                        target_x,
                        target_y,
                        support_count,
                    );
                    let Some(estimate) = estimate else {
                        row_stats.fallback_sites += 1;
                        if support_count == 0 {
                            row_stats.fully_unsupported_sites += 1;
                        }
                        continue;
                    };

                    row_stats.reconstructed_sites += 1;
                    let bounded = estimate.min(MAXIMUM_ESTIMATE_MULTIPLIER * level);
                    if bounded > *sample {
                        *sample = bounded;
                        row_stats.changed_sites += 1;
                    }
                }
                Ok(row_stats)
            },
        )
        .try_reduce(
            OpposedStats::default,
            |mut left, right| -> Result<OpposedStats, HighlightError> {
                left.add_assign(right);
                Ok(left)
            },
        )?;
    reconstruction.suspected_clipped_sites = validation.suspected_clipped_sites;
    reconstruction.suspected_by_channel = validation.suspected_by_channel;
    debug_assert_eq!(
        reconstruction.suspected_clipped_sites,
        reconstruction.reconstructed_sites + reconstruction.fallback_sites
    );
    debug_assert!(reconstruction.changed_sites <= reconstruction.reconstructed_sites);
    debug_assert!(reconstruction.fully_unsupported_sites <= reconstruction.fallback_sites);
    checkpoint(cancellation)?;
    Ok(OpposedOutput {
        mosaic,
        stats: reconstruction,
    })
}

fn validate_visible_samples(
    mosaic: &MosaicImage<f32>,
    levels: ChannelDetectionLevels,
    cancellation: &dyn crate::CancellationCheck,
) -> Result<OpposedStats, HighlightError> {
    mosaic
        .data()
        .par_chunks(mosaic.row_stride())
        .enumerate()
        .try_fold(
            OpposedStats::default,
            |mut row_stats, (y, row)| -> Result<OpposedStats, HighlightError> {
                checkpoint(cancellation)?;
                for (x, sample) in row[..mosaic.width()].iter().copied().enumerate() {
                    if !sample.is_finite() {
                        return Err(HighlightError::NonFiniteSample { x, y });
                    }
                    let color = mosaic.pattern().color_at(x, y);
                    if is_suspected_clipped(sample, levels.for_color(color)) {
                        row_stats.suspected_clipped_sites += 1;
                        row_stats.suspected_by_channel[color.channel_index()] += 1;
                    }
                }
                Ok(row_stats)
            },
        )
        .try_reduce(
            OpposedStats::default,
            |mut left, right| -> Result<OpposedStats, HighlightError> {
                left.add_assign(right);
                Ok(left)
            },
        )
}

fn opposing_support_count(target: crate::cells::CellSummary, target_channel: usize) -> usize {
    (0..3)
        .filter(|channel| {
            *channel != target_channel && target.is_usable(*channel) && target.is_clean(*channel)
        })
        .count()
}

fn estimate_site(
    target: crate::cells::CellSummary,
    target_channel: usize,
    levels: ChannelDetectionLevels,
    summaries: &CellSummaries,
    target_x: usize,
    target_y: usize,
    support_count: usize,
) -> Option<f32> {
    if support_count == 0 {
        return None;
    }
    let target_reference = opposing_reference(target, target_channel)?;
    let mut corrections = [0.0_f32; MAX_CANDIDATES];
    let mut count = 0;

    for radius in FIRST_RADIUS..=LAST_RADIUS {
        for cell_y in target_y.saturating_sub(radius)
            ..=target_y.saturating_add(radius).min(summaries.height - 1)
        {
            for cell_x in target_x.saturating_sub(radius)
                ..=target_x.saturating_add(radius).min(summaries.width - 1)
            {
                if target_x.abs_diff(cell_x).max(target_y.abs_diff(cell_y)) != radius {
                    continue;
                }
                let candidate = summaries.get(cell_x, cell_y);
                if !candidate.is_usable(target_channel)
                    || !candidate.is_clean(target_channel)
                    || candidate.means[target_channel]
                        <= DARK_SUPPORT_FRACTION * levels.for_channel(target_channel)
                {
                    continue;
                }
                if !candidate_matches_support(
                    target,
                    candidate,
                    target_channel,
                    levels,
                    support_count,
                ) {
                    continue;
                }
                let Some(candidate_reference) = opposing_reference(candidate, target_channel)
                else {
                    continue;
                };
                let correction = candidate.means[target_channel] - candidate_reference;
                if correction.is_finite() && count < corrections.len() {
                    corrections[count] = correction;
                    count += 1;
                }
            }
        }
        if count >= MIN_CANDIDATES {
            break;
        }
    }

    if count < MIN_CANDIDATES {
        return None;
    }
    corrections[..count].sort_by(|left, right| left.total_cmp(right));
    let correction = median(&corrections[..count]);
    let estimate = target_reference + correction;
    (estimate.is_finite() && estimate > 0.0).then_some(estimate)
}

fn candidate_matches_support(
    target: crate::cells::CellSummary,
    candidate: crate::cells::CellSummary,
    target_channel: usize,
    levels: ChannelDetectionLevels,
    support_count: usize,
) -> bool {
    let mut checked = 0;
    for channel in 0..3 {
        if channel == target_channel || !target.is_usable(channel) {
            continue;
        }
        if !target.is_clean(channel)
            || !candidate.is_usable(channel)
            || !candidate.is_clean(channel)
        {
            return false;
        }
        let target_value = target.means[channel];
        let candidate_value = candidate.means[channel];
        if target_value <= DARK_SUPPORT_FRACTION * levels.for_channel(channel)
            || candidate_value <= DARK_SUPPORT_FRACTION * levels.for_channel(channel)
        {
            return false;
        }
        checked += 1;
    }

    // With two surviving support channels, their ratio is a useful edge guard.
    // With one support channel there is no chromaticity ratio to compare, so
    // the median and the minimum candidate count provide the conservative guard.
    if support_count >= 2 {
        let target_ratio =
            target.means[(target_channel + 1) % 3] / target.means[(target_channel + 2) % 3];
        let candidate_ratio =
            candidate.means[(target_channel + 1) % 3] / candidate.means[(target_channel + 2) % 3];
        target_ratio.is_finite()
            && candidate_ratio.is_finite()
            && target_ratio > 0.0
            && candidate_ratio > 0.0
            && (target_ratio.log2() - candidate_ratio.log2()).abs() <= CROSS_CHANNEL_TOLERANCE_EV
            && checked >= 2
    } else {
        checked >= 1
    }
}

fn opposing_reference(summary: crate::cells::CellSummary, target_channel: usize) -> Option<f32> {
    let mut sum = 0.0;
    let mut count = 0;
    for channel in 0..3 {
        if channel == target_channel || !summary.is_usable(channel) || !summary.is_clean(channel) {
            continue;
        }
        let value = summary.means[channel];
        if !value.is_finite() || value <= 0.0 {
            continue;
        }
        sum += value.cbrt();
        count += 1;
    }
    (count != 0).then(|| {
        let root = sum / count as f32;
        root * root * root
    })
}

fn median(values: &[f32]) -> f32 {
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    }
}
