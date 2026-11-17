//! Camera-native, region-aware Opposed reconstruction.
//!
//! This is an original Bayer-domain implementation informed by the opposed
//! recovery family, rather than a port of a demosaiced RGB implementation.
//! Every clipped site is estimated as
//! `opposing_reference(target) + correction`: the target cell's surviving
//! channels provide the reference, and a chrominance offset measured on clean
//! cells provides the correction. The correction comes from two sources:
//!
//! - **local**: clean candidate cells within [`LAST_RADIUS`] cells of the
//!   site, accurate near the edge of a clipped region;
//! - **region**: a robust statistic over the clean border ring of the
//!   site's clipped region (see [`crate::regions`] and
//!   [`crate::region_correction`]), falling back to an image-wide pool.
//!
//! Near the border the local value dominates; deeper inside, where the local
//! search runs out of candidates, the region value takes over, with a
//! distance-weighted blend in between.

use rayon::prelude::*;
use rohditor_image::MosaicImage;

use crate::cells::{CellSummaries, ceil_halved};
use crate::detect::is_suspected_clipped;
use crate::region_correction::RegionCorrections;
use crate::regions::Regions;
use crate::support::{self, ChannelMask};
use crate::{
    ChannelClipLevels, ChannelDetectionLevels, HighlightError, OpposedOptions, OpposedOutput,
    OpposedStats, checkpoint,
};

const FIRST_RADIUS: usize = 1;
const LAST_RADIUS: usize = 3;
const MAX_CANDIDATES: usize = 48;
const MIN_CANDIDATES: usize = 3;
const MAXIMUM_ESTIMATE_MULTIPLIER: f32 = 4.0;

/// Logical bytes occupied by the working memory of Opposed: the immutable
/// cell summaries, the per-cell region labels (plus segmentation scratch) and
/// the per-region correction accumulators, all for their worst case.
#[must_use]
pub fn opposed_scratch_bytes(width: usize, height: usize) -> Option<usize> {
    let (cell_width, cell_height) = (ceil_halved(width), ceil_halved(height));
    crate::cells::scratch_bytes(width, height)?
        .checked_add(crate::regions::scratch_bytes(cell_width, cell_height)?)?
        .checked_add(crate::region_correction::scratch_bytes(
            cell_width,
            cell_height,
        )?)
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
    if let Some(ceiling) = options.neutral_ceiling {
        // Validate before any pixel write: a NaN/zero ceiling would sail past
        // the input-only non-finite sample scans below.
        ceiling.validate()?;
    }
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
    let regions = Regions::build(&summaries, cancellation)?;
    let corrections =
        RegionCorrections::build(&summaries, &regions, options.detection_levels, cancellation)?;
    let context = EstimateContext {
        levels: options.detection_levels,
        summaries: &summaries,
        regions: &regions,
        corrections: &corrections,
    };
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
                    let estimate = if support_count == 0 {
                        None
                    } else {
                        context.estimate_site(target, channel, target_x, target_y)
                    };
                    let Some(estimate) = estimate else {
                        row_stats.fallback_sites += 1;
                        if support_count == 0 {
                            row_stats.fully_unsupported_sites += 1;
                        }
                        neutralise_to_ceiling(
                            sample,
                            options.neutral_ceiling,
                            channel,
                            &mut row_stats.neutralised_sites,
                        );
                        continue;
                    };

                    row_stats.reconstructed_sites += 1;
                    let bounded = estimate.min(MAXIMUM_ESTIMATE_MULTIPLIER * level);
                    if bounded > *sample {
                        *sample = bounded;
                        row_stats.changed_sites += 1;
                    } else {
                        // Estimate produced but not applied: the site keeps
                        // its raw value, so for the neutraliser it is a
                        // fallback like a `None` estimate (issue #65).
                        neutralise_to_ceiling(
                            sample,
                            options.neutral_ceiling,
                            channel,
                            &mut row_stats.neutralised_sites,
                        );
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
    debug_assert!(
        reconstruction.neutralised_sites
            <= reconstruction.suspected_clipped_sites - reconstruction.changed_sites
    );
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

/// Everything the per-site estimator reads; all of it is immutable, so rows
/// can be reconstructed in parallel.
struct EstimateContext<'a> {
    levels: ChannelDetectionLevels,
    summaries: &'a CellSummaries,
    regions: &'a Regions,
    corrections: &'a RegionCorrections,
}

impl EstimateContext<'_> {
    /// Estimate a suspected-clipped `target_channel` site in cell
    /// `(target_x, target_y)`, or `None` when neither the local neighbourhood
    /// nor any region border provides evidence.
    fn estimate_site(
        &self,
        target: crate::cells::CellSummary,
        target_channel: usize,
        target_x: usize,
        target_y: usize,
    ) -> Option<f32> {
        let support = support::support_mask(target, target_channel, self.levels);
        let target_reference = support::reference(target, support)?;
        let target_ratio = support::support_log_ratio(target, support);
        let local =
            self.local_correction(target_channel, support, target_ratio, target_x, target_y);
        let label = self.regions.label(target_x, target_y);
        debug_assert_ne!(label, 0, "a suspected site always lies inside a region");
        // A border whose colour differs from the target's belongs to a
        // neighbouring object: treat it as no evidence rather than bleed it.
        let region = self
            .corrections
            .lookup(label, target_channel, support)
            .filter(|border| {
                support::chromaticity_agrees(support, target_ratio, border.support_log_ratio)
            })
            .map(|border| border.offset);

        let correction = match (local, region) {
            (Some((local, radius)), Some(region)) => {
                let weight = local_weight(radius);
                weight * local + (1.0 - weight) * region
            }
            (Some((local, _)), None) => local,
            (None, Some(region)) => region,
            (None, None) => return None,
        };
        let estimate = target_reference + correction;
        (estimate.is_finite() && estimate > 0.0).then_some(estimate)
    }

    /// Median correction of the clean candidates nearest to the target, and
    /// the search radius (in cells) that supplied them.
    fn local_correction(
        &self,
        target_channel: usize,
        support: ChannelMask,
        target_ratio: Option<f32>,
        target_x: usize,
        target_y: usize,
    ) -> Option<(f32, usize)> {
        let summaries = self.summaries;
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
                    // Most cells next to a large region are clipped too:
                    // reject them from the flag byte before loading means.
                    if !summaries.is_usable_and_clean(cell_x, cell_y, target_channel) {
                        continue;
                    }
                    let candidate = summaries.get(cell_x, cell_y);
                    let Some(correction) =
                        support::correction(candidate, target_channel, support, self.levels)
                    else {
                        continue;
                    };
                    let candidate_ratio = support::support_log_ratio(candidate, support);
                    if !support::chromaticity_agrees(support, target_ratio, candidate_ratio) {
                        continue;
                    }
                    if count < corrections.len() {
                        corrections[count] = correction;
                        count += 1;
                    }
                }
            }
            if count >= MIN_CANDIDATES {
                corrections[..count].sort_by(|left, right| left.total_cmp(right));
                return Some((median(&corrections[..count]), radius));
            }
        }
        None
    }
}

/// Weight of the local correction when it was found at `radius` cells:
/// 1 right at the border, falling linearly towards the region estimate as
/// the nearest evidence gets further away.
fn local_weight(radius: usize) -> f32 {
    (LAST_RADIUS + 1 - radius) as f32 / LAST_RADIUS as f32
}

fn median(values: &[f32]) -> f32 {
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    }
}

/// Lower a suspected, not-raised site to its neutral ceiling with a strict
/// `>` (mirroring Clip's `is_changed`), so the site lands neutral after the
/// as-shot white balance instead of keeping its clipped chroma (issue #65).
/// Raised sites are deliberately excluded in v1: the estimator output above
/// the ceiling is still information, not raw clipping.
fn neutralise_to_ceiling(
    sample: &mut f32,
    ceiling: Option<ChannelClipLevels>,
    channel: usize,
    neutralised_sites: &mut usize,
) {
    let Some(ceiling) = ceiling else {
        return;
    };
    let limit = ceiling.for_channel(channel);
    if *sample > limit {
        *sample = limit;
        *neutralised_sites += 1;
    }
}
