//! Shared "opposing support" helpers for the Opposed estimator.
//!
//! A clipped site of channel `c` is estimated as
//! `reference(cell, support) + correction`, where `support` is the set of
//! *other* channels in the same cell that survived clipping and are bright
//! enough to be trusted. Local candidates and region border cells measure the
//! `correction` with exactly the same support set, so the reference cancels.

use crate::ChannelDetectionLevels;
use crate::cells::CellSummary;

/// A channel mean at or below this fraction of its detection level is treated
/// as noise-floor dark and never used as evidence.
pub(crate) const DARK_SUPPORT_FRACTION: f32 = 0.2;

/// Largest allowed difference, in EV, between the opposing-channel ratio of a
/// target and of the evidence used for it. Beyond this the evidence belongs
/// to a differently coloured neighbour and must not bleed its colour in.
pub(crate) const CROSS_CHANNEL_TOLERANCE_EV: f32 = 0.75;

/// Bitmask over channels (bit `c` set = channel `c` is in the set).
pub(crate) type ChannelMask = u8;

/// True when `channel` of `cell` is usable, unclipped and above the dark floor.
#[inline]
pub(crate) fn is_bright_clean(
    cell: CellSummary,
    channel: usize,
    levels: ChannelDetectionLevels,
) -> bool {
    cell.is_usable(channel)
        && cell.is_clean(channel)
        && cell.means[channel] > DARK_SUPPORT_FRACTION * levels.for_channel(channel)
}

/// The channels other than `target_channel` that can act as opposing support
/// in `cell`.
#[inline]
pub(crate) fn support_mask(
    cell: CellSummary,
    target_channel: usize,
    levels: ChannelDetectionLevels,
) -> ChannelMask {
    (0..3)
        .filter(|&channel| channel != target_channel && is_bright_clean(cell, channel, levels))
        .fold(0, |mask, channel| mask | (1 << channel))
}

/// Cube-root mean of the `support` channels of `cell` (darktable-style
/// opposed reference). `None` when the set is empty or any channel is not a
/// positive finite mean.
pub(crate) fn reference(cell: CellSummary, support: ChannelMask) -> Option<f32> {
    // A single channel is its own cube-root mean; skip the round trip.
    if support.count_ones() == 1 {
        let value = cell.means[support.trailing_zeros() as usize];
        return (value.is_finite() && value > 0.0).then_some(value);
    }
    let mut sum = 0.0;
    let mut count = 0;
    for channel in (0..3).filter(|channel| support & (1 << channel) != 0) {
        let value = cell.means[channel];
        if !value.is_finite() || value <= 0.0 {
            return None;
        }
        sum += value.cbrt();
        count += 1;
    }
    (count != 0).then(|| {
        let root = sum / count as f32;
        root * root * root
    })
}

/// Offset of `target_channel` above the opposing reference of a clean cell,
/// using `support` as the reference set. `None` if `cell` cannot provide
/// every channel involved.
#[inline]
pub(crate) fn correction(
    cell: CellSummary,
    target_channel: usize,
    support: ChannelMask,
    levels: ChannelDetectionLevels,
) -> Option<f32> {
    if !is_bright_clean(cell, target_channel, levels) {
        return None;
    }
    for channel in (0..3).filter(|channel| support & (1 << channel) != 0) {
        if !is_bright_clean(cell, channel, levels) {
            return None;
        }
    }
    let value = cell.means[target_channel] - reference(cell, support)?;
    value.is_finite().then_some(value)
}

/// `log2` ratio of the two support channels of `cell`: the cell's opposing
/// chromaticity. `None` unless `support` holds exactly two channels (with
/// one there is no ratio to compare) or the ratio is not positive and finite.
pub(crate) fn support_log_ratio(cell: CellSummary, support: ChannelMask) -> Option<f32> {
    if support.count_ones() != 2 {
        return None;
    }
    let mut channels = (0..3).filter(|channel| support & (1 << channel) != 0);
    let (first, second) = (channels.next()?, channels.next()?);
    let ratio = cell.means[first] / cell.means[second];
    (ratio.is_finite() && ratio > 0.0).then(|| ratio.log2())
}

/// Edge guard shared by local candidates and region borders. With two
/// support channels their chromaticities must agree; with fewer there is no
/// ratio to compare, so the median and the minimum evidence count are the
/// guard instead.
pub(crate) fn chromaticity_agrees(
    support: ChannelMask,
    target: Option<f32>,
    evidence: Option<f32>,
) -> bool {
    if support.count_ones() < 2 {
        return true;
    }
    match (target, evidence) {
        (Some(target), Some(evidence)) => (target - evidence).abs() <= CROSS_CHANNEL_TOLERANCE_EV,
        _ => false,
    }
}
