use rayon::prelude::*;
use rohditor_image::MosaicImage;

use crate::cells::CellSummaries;
use crate::detect::is_suspected_clipped;
use crate::{
    ChannelDetectionLevels, HighlightError, LocalRatioOptions, LocalRatioOutput,
    ReconstructionStats, checkpoint,
};

const FIRST_RADIUS: usize = 1;
const LAST_RADIUS: usize = 2;
const MAX_CANDIDATES: usize = 24;
const TWO_SUPPORT_MINIMUM: usize = 3;
const ONE_SUPPORT_MINIMUM: usize = 5;
const CROSS_CHANNEL_TOLERANCE_EV: f32 = 0.5;
const ESTIMATE_TOLERANCE_EV: f32 = 1.0;
const MAXIMUM_ESTIMATE_MULTIPLIER: f32 = 4.0;

/// Logical bytes occupied by the two compact cell-summary arrays. This is a
/// deterministic working-set estimate; allocator bookkeeping is not included.
#[must_use]
pub fn local_ratio_scratch_bytes(width: usize, height: usize) -> Option<usize> {
    let cell_width = crate::cells::ceil_halved(width);
    let cell_height = crate::cells::ceil_halved(height);
    cell_width
        .checked_mul(cell_height)?
        .checked_mul(std::mem::size_of::<[f32; 3]>() + std::mem::size_of::<u8>())
}

/// Reconstruct suspected-clipped Bayer sites from bounded local channel
/// ratios. The input is consumed so no output-sized mosaic clone is needed.
pub fn reconstruct_local_ratios(
    mosaic: MosaicImage<f32>,
    options: LocalRatioOptions,
) -> Result<LocalRatioOutput, HighlightError> {
    reconstruct_local_ratios_cancellable(mosaic, options, &|| false)
}

/// Cancellable form of [`reconstruct_local_ratios`].
pub fn reconstruct_local_ratios_cancellable(
    mut mosaic: MosaicImage<f32>,
    options: LocalRatioOptions,
    cancellation: &dyn crate::CancellationCheck,
) -> Result<LocalRatioOutput, HighlightError> {
    options.detection_levels.validate()?;
    checkpoint(cancellation)?;
    let validation = validate_visible_samples(&mosaic, options.detection_levels, cancellation)?;
    if validation.suspected_clipped_sites == 0 {
        checkpoint(cancellation)?;
        return Ok(LocalRatioOutput {
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
            ReconstructionStats::default,
            |mut row_stats, (y, output_row)| -> Result<ReconstructionStats, HighlightError> {
                checkpoint(cancellation)?;
                for (x, sample) in output_row[..width].iter_mut().enumerate() {
                    let color = pattern.color_at(x, y);
                    let channel = color.channel_index();
                    let level = options.detection_levels.for_color(color);
                    if !is_suspected_clipped(*sample, level) {
                        continue;
                    }

                    let target = summaries.get(x / 2, y / 2);
                    let support_count = usable_support_count(target, channel);
                    if support_count == 0 {
                        row_stats.fallback_sites += 1;
                        row_stats.fully_unsupported_sites += 1;
                        continue;
                    }

                    let estimate = estimate_site(
                        target,
                        channel,
                        options.detection_levels,
                        &summaries,
                        x / 2,
                        y / 2,
                        support_count,
                    );
                    let Some(estimate) = estimate else {
                        row_stats.fallback_sites += 1;
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
            ReconstructionStats::default,
            |mut left, right| -> Result<ReconstructionStats, HighlightError> {
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
    Ok(LocalRatioOutput {
        mosaic,
        stats: reconstruction,
    })
}

fn validate_visible_samples(
    mosaic: &MosaicImage<f32>,
    levels: ChannelDetectionLevels,
    cancellation: &dyn crate::CancellationCheck,
) -> Result<ReconstructionStats, HighlightError> {
    mosaic
        .data()
        .par_chunks(mosaic.row_stride())
        .enumerate()
        .try_fold(
            ReconstructionStats::default,
            |mut row_stats, (y, row)| -> Result<ReconstructionStats, HighlightError> {
                checkpoint(cancellation)?;
                for (x, sample) in row[..mosaic.width()].iter().copied().enumerate() {
                    if !sample.is_finite() {
                        return Err(HighlightError::NonFiniteSample { x, y });
                    }
                    let channel = mosaic.pattern().color_at(x, y).channel_index();
                    if is_suspected_clipped(
                        sample,
                        levels.for_color(mosaic.pattern().color_at(x, y)),
                    ) {
                        row_stats.suspected_clipped_sites += 1;
                        row_stats.suspected_by_channel[channel] += 1;
                    }
                }
                Ok(row_stats)
            },
        )
        .try_reduce(
            ReconstructionStats::default,
            |mut left, right| -> Result<ReconstructionStats, HighlightError> {
                left.add_assign(right);
                Ok(left)
            },
        )
}

fn usable_support_count(target: crate::cells::CellSummary, target_channel: usize) -> usize {
    (0..3)
        .filter(|channel| *channel != target_channel && target.is_usable(*channel))
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
    let minimum = if support_count == 1 {
        ONE_SUPPORT_MINIMUM
    } else {
        TWO_SUPPORT_MINIMUM
    };
    let mut estimates = [0.0_f32; 2];
    let mut estimate_count = 0;
    for support_channel in 0..3 {
        if support_channel == target_channel || !target.is_usable(support_channel) {
            continue;
        }
        let mut candidates = [0.0_f32; MAX_CANDIDATES];
        let count = collect_candidates(
            target,
            target_channel,
            support_channel,
            levels,
            summaries,
            target_x,
            target_y,
            support_count,
            &mut candidates,
            minimum,
        );
        if count < minimum {
            continue;
        }
        candidates[..count].sort_by(|left, right| left.total_cmp(right));
        let median = median(&candidates[..count]);
        let estimate = target.means[support_channel] * median;
        if estimate.is_finite() && estimate > 0.0 {
            estimates[estimate_count] = estimate;
            estimate_count += 1;
        }
    }

    match estimate_count {
        0 => None,
        1 => Some(estimates[0]),
        2 => {
            if estimates[0] <= 0.0 || estimates[1] <= 0.0 {
                return None;
            }
            let disagreement = (estimates[0].log2() - estimates[1].log2()).abs();
            (disagreement <= ESTIMATE_TOLERANCE_EV).then(|| (estimates[0] * estimates[1]).sqrt())
        }
        _ => unreachable!("a Bayer target has at most two supporting channels"),
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_candidates(
    target: crate::cells::CellSummary,
    target_channel: usize,
    support_channel: usize,
    levels: ChannelDetectionLevels,
    summaries: &CellSummaries,
    target_x: usize,
    target_y: usize,
    support_count: usize,
    candidates: &mut [f32; MAX_CANDIDATES],
    minimum: usize,
) -> usize {
    let other_support = (0..3).find(|channel| {
        *channel != target_channel && *channel != support_channel && target.is_usable(*channel)
    });
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
                if !candidate.is_clean(target_channel) || !candidate.is_usable(support_channel) {
                    continue;
                }
                if support_count == 2 {
                    let Some(other_support) = other_support else {
                        continue;
                    };
                    if !candidate.is_usable(other_support) {
                        continue;
                    }
                    if !cross_channel_matches(target, candidate, support_channel, other_support) {
                        continue;
                    }
                }
                let numerator = candidate.means[target_channel];
                let denominator = candidate.means[support_channel];
                if !numerator.is_finite()
                    || !denominator.is_finite()
                    || numerator <= 0.0
                    || denominator <= 0.02 * levels.for_channel(support_channel)
                {
                    continue;
                }
                if count < candidates.len() {
                    candidates[count] = numerator / denominator;
                    count += 1;
                }
            }
        }
        if count >= minimum {
            break;
        }
    }
    count
}

fn cross_channel_matches(
    target: crate::cells::CellSummary,
    candidate: crate::cells::CellSummary,
    first: usize,
    second: usize,
) -> bool {
    let target_ratio = target.means[first] / target.means[second];
    let candidate_ratio = candidate.means[first] / candidate.means[second];
    if !target_ratio.is_finite()
        || !candidate_ratio.is_finite()
        || target_ratio <= 0.0
        || candidate_ratio <= 0.0
    {
        return false;
    }
    (target_ratio.log2() - candidate_ratio.log2()).abs() <= CROSS_CHANNEL_TOLERANCE_EV
}

fn median(values: &[f32]) -> f32 {
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    }
}

trait DetectionLevelsForChannel {
    fn for_channel(self, channel: usize) -> f32;
}

impl DetectionLevelsForChannel for ChannelDetectionLevels {
    fn for_channel(self, channel: usize) -> f32 {
        match channel {
            0 => self.red,
            1 => self.green,
            2 => self.blue,
            _ => unreachable!("Bayer channel index is always in range"),
        }
    }
}
