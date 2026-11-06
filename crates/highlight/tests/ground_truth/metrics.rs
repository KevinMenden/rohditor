//! Reconstruction metrics for the issue #63 harness.
//!
//! Metric definitions live here: raised counting, per-site
//! log-ratio error with median/p95, and the after-WB hue error with chroma
//! floor. Per-channel log-ratios are WB-invariant by algebra, so the fixed
//! metric WB only enters through the hue metric. The pinned baseline table
//! lives next door in `baseline`.

// f64 metric math deliberately keeps mul/add separate; the harness scores
// bit-predictable quantities and never needs the contracted form (same
// margin-guard reasoning as `generator`).
#![allow(clippy::suboptimal_flops)]

use rohditor_image::BayerPattern;

/// Fixed metric white balance: the non-trivial per-channel scale hue is
/// scored after. Log-ratios are WB-invariant by algebra (see
/// [`abs_log_errors`]), so WB only matters here.
pub const METRIC_WB: [f32; 3] = [2.0, 1.0, 1.55];

/// Chroma floor below which a truth pixel is treated as hueless.
const CHROMA_FLOOR: f64 = 0.04;

/// Everything a single scene/mosaic view needs to score one method run.
/// All slices are row-major with `width` stride and equal length.
pub struct SiteSet<'a> {
    pub width: usize,
    pub truth: &'a [[f32; 3]],
    pub input: &'a [f32],
    pub output: &'a [f32],
    /// Expected suspected set, derived from the input bits.
    pub suspected: &'a [bool],
}

impl SiteSet<'_> {
    fn channel_at(&self, index: usize) -> usize {
        let (x, y) = (index % self.width, index / self.width);
        BayerPattern::Rggb.color_at(x, y).channel_index()
    }
}

/// First `(index, input, output)` where an output bit dropped below its
/// input bit — the "no site goes below its clipped input" invariant.
/// `None` means the invariant holds.
#[must_use]
pub fn site_below_input(sites: &SiteSet<'_>) -> Option<(usize, f32, f32)> {
    sites
        .input
        .iter()
        .zip(sites.output)
        .enumerate()
        .find_map(|(index, (&before, &after))| (after < before).then_some((index, before, after)))
}

/// Data-driven "raised": sites where the output bit actually got lifted
/// above the input bit (`out > in`). This is the honest reconstruction
/// column across all four methods.
#[must_use]
pub fn raised_count(sites: &SiteSet<'_>) -> usize {
    sites
        .input
        .iter()
        .zip(sites.output)
        .filter(|(before, after)| after > before)
        .count()
}

/// |ln(out / truth)| over every expected-suspected site. The margin guard
/// keeps truth above sat - 1e-4 at suspected sites. Per-channel log-ratios
/// are WB-invariant by algebra (scaling both sides of the ratio by one
/// per-channel WB vector cancels inside the log), so this metric already
/// covers the fixed-WB wording of the issue; hue is where WB matters.
#[must_use]
pub fn abs_log_errors(sites: &SiteSet<'_>) -> Vec<f64> {
    let mut errors = Vec::new();
    for (index, &suspected) in sites.suspected.iter().enumerate() {
        if !suspected {
            continue;
        }
        let channel = sites.channel_at(index);
        let truth = f64::from(sites.truth[index][channel]);
        let out = f64::from(sites.output[index]);
        errors.push((out / truth).ln().abs());
    }
    errors
}

/// |ln(out / truth)| over the *raised* sites only (printed, not baselined).
#[must_use]
pub fn raised_abs_log_errors(sites: &SiteSet<'_>) -> Vec<f64> {
    let mut errors = Vec::new();
    for (index, &suspected) in sites.suspected.iter().enumerate() {
        if !suspected || sites.output[index] <= sites.input[index] {
            continue;
        }
        let channel = sites.channel_at(index);
        let truth = f64::from(sites.truth[index][channel]);
        let out = f64::from(sites.output[index]);
        errors.push((out / truth).ln().abs());
    }
    errors
}

/// Hue of a WB-scaled RGB triple: `atan2(sqrt(3)*(G - B), 2R - G - B)`.
fn hue_angle(rgb: &[f32; 3]) -> f64 {
    let (r, g, b) = (f64::from(rgb[0]), f64::from(rgb[1]), f64::from(rgb[2]));
    f64::atan2(f64::sqrt(3.0) * (g - b), 2.0 * r - g - b)
}

/// Wrapped hue error per suspected pixel after the fixed metric WB:
/// the sampled channel comes from the method
/// output, the other two from truth — this isolates the reconstructed
/// channel's error. Pixels whose TRUTH triple fails the chroma floor carry
/// no meaningful hue and are skipped.
#[must_use]
pub fn hue_errors(sites: &SiteSet<'_>) -> Vec<f64> {
    let mut errors = Vec::new();
    for (index, &suspected) in sites.suspected.iter().enumerate() {
        if !suspected {
            continue;
        }
        let truth = sites.truth[index];
        let sum = f64::from(truth[0]) + f64::from(truth[1]) + f64::from(truth[2]);
        if sum <= 1e-3 {
            continue;
        }
        let mut min = f64::from(truth[0]);
        let mut max = min;
        for &value in &truth[1..] {
            min = min.min(f64::from(value));
            max = max.max(f64::from(value));
        }
        if (max - min) / sum < CHROMA_FLOOR {
            continue;
        }
        let channel = sites.channel_at(index);
        let mut from_output = truth;
        from_output[channel] = sites.output[index];
        let scaled_truth = [
            truth[0] * METRIC_WB[0],
            truth[1] * METRIC_WB[1],
            truth[2] * METRIC_WB[2],
        ];
        let scaled_out = [
            from_output[0] * METRIC_WB[0],
            from_output[1] * METRIC_WB[1],
            from_output[2] * METRIC_WB[2],
        ];
        let delta = hue_angle(&scaled_out) - hue_angle(&scaled_truth);
        errors.push(f64::atan2(delta.sin(), delta.cos()).abs());
    }
    errors
}

/// Median of an already `total_cmp`-sorted list.
#[must_use]
pub fn median_sorted(values: &[f64]) -> f64 {
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Nearest-rank p95 of an already sorted list: `v[ceil(0.95 * n) - 1]`.
/// Requires a non-empty list.
#[must_use]
pub fn p95_sorted(values: &[f64]) -> f64 {
    values[(95 * values.len()).div_ceil(100) - 1]
}

/// Sort by `total_cmp` and return `(median, p95)`; `None` for empty input.
#[must_use]
pub fn summarize(mut values: Vec<f64>) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some((median_sorted(&values), p95_sorted(&values)))
}

/// One measured matrix run, before it is folded into a baseline row.
pub struct MeasuredRow {
    pub scene: &'static str,
    pub sat: f32,
    pub method: &'static str,
    pub suspected: usize,
    pub raised: usize,
    pub med: f64,
    pub p95: f64,
    /// `None` when no suspected pixel passes the chroma floor; the hue
    /// bound is then treated as satisfied (nothing measurable to check).
    pub hue_max: Option<f64>,
}
