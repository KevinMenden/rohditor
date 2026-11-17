//! Robust chroma corrections measured on the clean border ring of each
//! clipped region, plus an image-wide pool for regions without enough
//! border evidence.
//!
//! The Opposed model is `estimate = reference(target) + correction`. A
//! border cell supplies one correction sample per `(channel, support set)`
//! pair (see [`crate::support`]); the support set must match the target's
//! because the reference differs by set. Samples are reduced with a trimmed
//! mean using fixed-size accumulators per region, so memory stays bounded
//! by the region count however large a region's ring is.
//!
//! Determinism: the ring is scanned sequentially in raster order, so the
//! floating-point accumulation order never depends on the thread count.

use crate::cells::{CellSummaries, allocation_error};
use crate::regions::Regions;
use crate::support::{self, ChannelMask};
use crate::{CancellationCheck, ChannelDetectionLevels, HighlightError, checkpoint};

/// Non-empty subsets of the two opposing channels, per target channel.
const PATTERNS_PER_CHANNEL: usize = 3;
/// Accumulators per region: 3 target channels x 3 support sets.
const SLOTS_PER_REGION: usize = 3 * PATTERNS_PER_CHANNEL;
/// A region (or the global pool) needs this many border samples to be used.
const MIN_BORDER_CELLS: u32 = 3;
/// Samples further than this many standard deviations from the mean are
/// dropped from the trimmed mean (edge pollution from a differently coloured
/// neighbour, noise outliers).
const TRIM_DEVIATIONS: f64 = 2.0;
/// Absolute floor on the trimming tolerance so identical samples all survive
/// f32 rounding.
const TRIM_FLOOR: f32 = 1e-5;
/// A border cell only describes the clipped object if its own value in the
/// corrected channel is near the clip level: right outside a continuous
/// shoulder the channel has only just dropped below clipping. A much darker
/// ring cell sits across a sharp edge in the background, where the
/// chromatic offset says nothing about the blown interior.
const BORDER_MIN_FRACTION: f32 = 0.5;
const CHECKPOINT_ROWS: usize = 64;

/// The two scans over the ring: moments first, then the trimmed sums.
#[derive(Clone, Copy)]
enum Pass {
    Moments,
    Trim,
}

/// Fixed-size accumulator for one `(region, channel, support set)` trimmed
/// mean. `accum` is reused by each pass (sum, then sum of the kept samples)
/// and so is `second_accum` (sum of squares, then the sum of the kept
/// samples' support-channel log ratios).
#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    count: u32,
    kept: u32,
    accum: f64,
    /// Pass 1: sum of squares. Pass 2: sum of the kept samples' log ratios
    /// of the two support channels (two-channel support only), so the target
    /// can check that the border is the same colour.
    second_accum: f64,
    mean: f32,
    tolerance: f32,
}

impl Slot {
    fn add(&mut self, pass: Pass, value: f32, log_ratio: Option<f32>) {
        let wide = f64::from(value);
        match pass {
            Pass::Moments => {
                self.count += 1;
                self.accum += wide;
                self.second_accum += wide * wide;
            }
            Pass::Trim => {
                if (value - self.mean).abs() <= self.tolerance {
                    self.kept += 1;
                    self.accum += wide;
                    self.second_accum += f64::from(log_ratio.unwrap_or(0.0));
                }
            }
        }
    }

    /// Turn the pass-1 moments into the trimming window and clear the
    /// accumulators for pass 2.
    fn finish_moments(&mut self) {
        if self.count != 0 {
            let count = f64::from(self.count);
            let mean = self.accum / count;
            let variance = (self.second_accum / count - mean * mean).max(0.0);
            self.mean = mean as f32;
            self.tolerance = (TRIM_DEVIATIONS * variance.sqrt()) as f32 + TRIM_FLOOR;
        }
        self.accum = 0.0;
        self.second_accum = 0.0;
    }

    /// The trimmed mean (and mean support ratio), if there were enough
    /// samples.
    fn value(&self, has_ratio: bool) -> Option<BorderCorrection> {
        if self.count < MIN_BORDER_CELLS || self.kept == 0 {
            return None;
        }
        let kept = f64::from(self.kept);
        Some(BorderCorrection {
            offset: (self.accum / kept) as f32,
            support_log_ratio: has_ratio.then(|| (self.second_accum / kept) as f32),
        })
    }
}

/// A region's (or the pool's) robust border estimate for one target.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BorderCorrection {
    /// Trimmed-mean offset of the target channel above its opposing reference.
    pub(crate) offset: f32,
    /// Mean `log2` ratio of the two support channels over the same border
    /// samples; `None` unless the support set has two channels.
    pub(crate) support_log_ratio: Option<f32>,
}

/// Which `(channel, support set)` slots are ever looked up: a region only
/// needs the slots its own clipped sites use, and the pool only those used
/// anywhere. Border samples for all other slots would be wasted work.
struct WantedSlots {
    /// Per region label, bit `offset` = slot `offset` is looked up.
    per_region: Vec<u16>,
    /// Union over all regions.
    any: u16,
}

impl WantedSlots {
    fn scan(
        summaries: &CellSummaries,
        regions: &Regions,
        levels: ChannelDetectionLevels,
        cancellation: &dyn CancellationCheck,
    ) -> Result<Self, HighlightError> {
        let entries = regions.count() as usize + 1;
        let mut per_region = Vec::new();
        per_region
            .try_reserve_exact(entries)
            .map_err(|_| allocation_error(entries))?;
        per_region.resize(entries, 0_u16);
        let mut any = 0_u16;
        for y in 0..summaries.height {
            if y % CHECKPOINT_ROWS == 0 {
                checkpoint(cancellation)?;
            }
            for (x, &label) in regions.row(y).iter().enumerate() {
                let clipped = summaries.clipped_channels_at(x, y);
                if label == 0 || clipped == 0 {
                    continue;
                }
                // Fully blown cells (no clean opposing channel by flags) have
                // no support: skip them without loading their means.
                let has_clean_channel = |channel: usize| {
                    (0..3)
                        .any(|other| other != channel && summaries.is_usable_and_clean(x, y, other))
                };
                let candidates =
                    |channel: usize| clipped & (1 << channel) != 0 && has_clean_channel(channel);
                if !(0..3).any(candidates) {
                    continue;
                }
                let cell = summaries.get(x, y);
                for channel in (0..3).filter(|&c| candidates(c)) {
                    let support = support::support_mask(cell, channel, levels);
                    if let Some(pattern) = pattern_position(channel, support) {
                        let bit = 1_u16 << (channel * PATTERNS_PER_CHANNEL + pattern);
                        per_region[label as usize] |= bit;
                        any |= bit;
                    }
                }
            }
        }
        Ok(Self { per_region, any })
    }

    fn region_wants(&self, label: u32, offset: usize) -> bool {
        self.per_region[label as usize] & (1 << offset) != 0
    }

    fn pool_wants(&self, offset: usize) -> bool {
        self.any & (1 << offset) != 0
    }
}

/// Border corrections for every region plus the global pool (group 0).
#[derive(Debug)]
pub(crate) struct RegionCorrections {
    slots: Vec<Slot>,
}

impl RegionCorrections {
    pub(crate) fn build(
        summaries: &CellSummaries,
        regions: &Regions,
        levels: ChannelDetectionLevels,
        cancellation: &dyn CancellationCheck,
    ) -> Result<Self, HighlightError> {
        let slot_count = (regions.count() as usize + 1)
            .checked_mul(SLOTS_PER_REGION)
            .ok_or_else(|| allocation_error(usize::MAX))?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(slot_count)
            .map_err(|_| allocation_error(slot_count))?;
        slots.resize(slot_count, Slot::default());
        let wanted = WantedSlots::scan(summaries, regions, levels, cancellation)?;

        for pass in [Pass::Moments, Pass::Trim] {
            for y in 0..summaries.height {
                if y % CHECKPOINT_ROWS == 0 {
                    checkpoint(cancellation)?;
                }
                for (x, &label) in regions.row(y).iter().enumerate() {
                    // Border ring = inside a region's dilation, outside its mask.
                    if label == 0 || summaries.clipped_channels_at(x, y) != 0 {
                        continue;
                    }
                    let cell = summaries.get(x, y);
                    let region_base = label as usize * SLOTS_PER_REGION;
                    for channel in
                        (0..3).filter(|c| regions.clipped_channels(label) & (1 << c) != 0)
                    {
                        if cell.means[channel] < BORDER_MIN_FRACTION * levels.for_channel(channel) {
                            continue;
                        }
                        for (pattern, support) in support_patterns(channel).into_iter().enumerate()
                        {
                            let offset = channel * PATTERNS_PER_CHANNEL + pattern;
                            let for_region = wanted.region_wants(label, offset);
                            let for_pool = wanted.pool_wants(offset);
                            if !for_region && !for_pool {
                                continue;
                            }
                            let Some(value) = support::correction(cell, channel, support, levels)
                            else {
                                continue;
                            };
                            let ratio = support::support_log_ratio(cell, support);
                            if for_region {
                                slots[region_base + offset].add(pass, value, ratio);
                            }
                            if for_pool {
                                slots[offset].add(pass, value, ratio);
                            }
                        }
                    }
                }
            }
            if matches!(pass, Pass::Moments) {
                for slot in &mut slots {
                    slot.finish_moments();
                }
            }
        }
        checkpoint(cancellation)?;
        Ok(Self { slots })
    }

    /// Correction for a target of `channel` with opposing `support` inside
    /// region `label`: the region's own border estimate, else the global
    /// pool, else `None`.
    pub(crate) fn lookup(
        &self,
        label: u32,
        channel: usize,
        support: ChannelMask,
    ) -> Option<BorderCorrection> {
        let offset = channel * PATTERNS_PER_CHANNEL + pattern_position(channel, support)?;
        let has_ratio = support.count_ones() == 2;
        self.slots[label as usize * SLOTS_PER_REGION + offset]
            .value(has_ratio)
            .or_else(|| self.slots[offset].value(has_ratio))
    }
}

/// Logical bytes of the accumulators for the worst-case region count.
pub(crate) fn scratch_bytes(cell_width: usize, cell_height: usize) -> Option<usize> {
    let per_region = SLOTS_PER_REGION
        .checked_mul(std::mem::size_of::<Slot>())?
        .checked_add(std::mem::size_of::<u16>())?;
    crate::regions::max_regions(cell_width, cell_height)?.checked_mul(per_region)
}

/// The three non-empty subsets of the channels opposing `channel`, in a
/// fixed order matching [`pattern_position`].
fn support_patterns(channel: usize) -> [ChannelMask; PATTERNS_PER_CHANNEL] {
    let mut others = (0..3).filter(|&other| other != channel);
    let (first, second) = (others.next().unwrap_or(0), others.next().unwrap_or(0));
    [1 << first, 1 << second, (1 << first) | (1 << second)]
}

fn pattern_position(channel: usize, support: ChannelMask) -> Option<usize> {
    support_patterns(channel)
        .iter()
        .position(|&pattern| pattern == support)
}
