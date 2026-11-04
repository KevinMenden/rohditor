//! Reconstruction metrics + pinned baselines for the issue #63 harness.
//!
//! Metric definitions live here: raised counting, per-site
//! log-ratio error with median/p95, the WB-invariance self-check, and the
//! after-WB hue error with chroma floor. The `BASELINES` const table pins
//! today's numbers loosely so future improvements show up as test changes.

// f64 metric math deliberately keeps mul/add separate; the harness scores
// bit-predictable quantities and never needs the contracted form (same
// margin-guard reasoning as `generator`).
#![allow(clippy::suboptimal_flops)]

use rohditor_image::BayerPattern;

/// Fixed metric white balance: the non-trivial per-channel scale used for
/// the WB-invariance self-check and for scoring hue.
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

/// |ln(out / truth)| over every expected-suspected site. Truth is
/// > 0 at suspected sites by the margin guard (truth > sat - 1e-4).
#[must_use]
pub fn abs_log_errors(sites: &SiteSet<'_>) -> Vec<f64> {
    abs_log_errors_wb(sites, [1.0, 1.0, 1.0])
}

/// Same errors after scaling both sides by a per-channel WB vector; the
/// values coincide with [`abs_log_errors`] up to f32 rounding ulps, which is
/// the WB-invariance self-check.
#[must_use]
pub fn abs_log_errors_wb(sites: &SiteSet<'_>, wb: [f32; 3]) -> Vec<f64> {
    let mut errors = Vec::new();
    for (index, &suspected) in sites.suspected.iter().enumerate() {
        if !suspected {
            continue;
        }
        let channel = sites.channel_at(index);
        let truth = f64::from(sites.truth[index][channel] * wb[channel]);
        let out = f64::from(sites.output[index] * wb[channel]);
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

/// Loose bound on one baseline quantity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bound {
    Exact(usize),
    AtLeast(usize),
    AtMost(usize),
}

/// One pinned (scene, sat, method) baseline row.
pub struct BaselineRow {
    pub scene: &'static str,
    pub sat: f32,
    /// Matches `HighlightExecution::stable_name()`.
    pub method: &'static str,
    /// Exact, always: input-only quantity, bit-robust via the generator
    /// margin guard. If the maintainer's ARM box ever drifts, widen to
    /// +/-2 with an updated dated comment.
    pub suspected: usize,
    pub raised: Bound,
    /// Ceiling, 1.5x measured (all-suspected-sites definition).
    pub med_abs_logerr_max: f64,
    /// Ceiling, 1.5x measured.
    pub p95_abs_logerr_max: f64,
    /// Generous fixed 3.0 rad ceiling on max hue for EVERY row;
    /// median hue is printed but not asserted.
    pub hue_max_at_most: f64,
}

/// Baseline bound for the `raised` column:
/// - Off/Clip: `Exact(0)` (identity / bit-exact no-op on pre-clamped input).
/// - Single-channel disc scenes: `AtMost(measured + 2)` — the #62 known
///   failure, estimates are produced but never applied on large
///   single-channel regions; stays a ceiling even if measurements grow.
/// - Acting scenes (measured raised >= 5): `AtLeast(measured / 2)` floor.
/// - Anything else measured below the acting threshold: `AtMost` the
///   measured value + 2 (never a vacuous 0-floor).
///
/// 2026-11-04 review fix: both `AtMost` arms are measured + 2 (subsuming
/// the old floor of 2, since 0 + 2 = 2) — 2 units of slack for
/// estimator-bit drift, the same ±2 tolerance already allowed for
/// `suspected`: a 1-bit estimator wobble on another machine must not
/// break the ceiling. `AtLeast` floors keep no slack (under-acting is a
/// real regression).
#[must_use]
pub fn raised_policy(scene: &str, method: &str, measured: usize) -> Bound {
    if matches!(method, "off" | "clip") {
        return Bound::Exact(0);
    }
    if scene == "disk_small" || scene == "disk_large" {
        return Bound::AtMost(measured + 2);
    }
    if measured >= 5 {
        Bound::AtLeast(measured / 2)
    } else {
        Bound::AtMost(measured + 2)
    }
}

/// True if `measured` satisfies the bound.
#[must_use]
pub fn bound_holds(bound: Bound, measured: usize) -> bool {
    match bound {
        Bound::Exact(expected) => measured == expected,
        Bound::AtLeast(floor) => measured >= floor,
        Bound::AtMost(ceiling) => measured <= ceiling,
    }
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

/// Find the pinned row for `(scene, sat, method)`.
#[must_use]
pub fn find_baseline(scene: &str, sat: f32, method: &str) -> Option<&'static BaselineRow> {
    BASELINES.iter().find(|row| {
        row.scene == scene && (row.sat - sat).abs() < f32::EPSILON && row.method == method
    })
}

/// Check one measured run against its pinned row.
pub fn check_baseline(row: &BaselineRow, measured: &MeasuredRow) {
    assert_eq!(
        measured.suspected, row.suspected,
        "{}@{} {}: suspected count moved",
        row.scene, row.sat, row.method
    );
    assert!(
        bound_holds(row.raised, measured.raised),
        "{}@{} {}: raised = {} violates {:?}",
        row.scene,
        row.sat,
        row.method,
        measured.raised,
        row.raised
    );
    assert!(
        measured.med <= row.med_abs_logerr_max,
        "{}@{} {}: med |ln| {} exceeds ceiling {}",
        row.scene,
        row.sat,
        row.method,
        measured.med,
        row.med_abs_logerr_max
    );
    assert!(
        measured.p95 <= row.p95_abs_logerr_max,
        "{}@{} {}: p95 |ln| {} exceeds ceiling {}",
        row.scene,
        row.sat,
        row.method,
        measured.p95,
        row.p95_abs_logerr_max
    );
    // Empty hue sets (no suspected pixel passes the chroma floor) satisfy
    // the bound trivially; never assert on an empty maximum.
    if let Some(hue_max) = measured.hue_max {
        assert!(
            hue_max <= row.hue_max_at_most,
            "{}@{} {}: max hue {} rad exceeds ceiling {}",
            row.scene,
            row.sat,
            row.method,
            hue_max,
            row.hue_max_at_most
        );
    }
}

// Measured 2026-11-04, commit d492784, debug, x86-64, default rayon threads.
// Suspected counts are bit-derived (see generator margin guard); ceilings are
// 1.5x measured per issue #63 "assert today's numbers loosely".
// `hue_max_at_most` is NOT 1.5x-derived: it is the fixed generous 3.0 rad
// ceiling asserted on every row - clipping itself dominates the
// worst site, so a tighter value would pin reconstruction noise. Median hue
// is printed but not asserted.
// Reality note: disk_large measured LocalRatios raised = 10 @ 1.0 /
// 14 @ 0.98 (edge-ring sites), above the 0-1 expected at design time, so
// those two known-#62 rows cap at the measured value + 2 estimator-drift
// slack (2026-11-04, see `raised_policy`) instead of a literal AtMost(2).
// Still ceilings, never floors.
use Bound::{AtLeast, AtMost, Exact};

// Kept one row per line (matching the harness's pasteable output verbatim)
// so re-baselining stays a clean line-diff in PR review.
#[rustfmt::skip]
pub const BASELINES: &[BaselineRow] = &[
    BaselineRow { scene: "disk_small", sat: 1.00, method: "off", suspected: 9, raised: Exact(0), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 3.0 },
    BaselineRow { scene: "disk_small", sat: 1.00, method: "clip", suspected: 9, raised: Exact(0), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 3.0 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_small", sat: 1.00, method: "local_ratios", suspected: 9, raised: AtMost(2), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 3.0 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_small", sat: 1.00, method: "opposed", suspected: 9, raised: AtMost(2), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 3.0 },
    BaselineRow { scene: "disk_small", sat: 0.98, method: "off", suspected: 9, raised: Exact(0), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 3.0 },
    BaselineRow { scene: "disk_small", sat: 0.98, method: "clip", suspected: 9, raised: Exact(0), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 3.0 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_small", sat: 0.98, method: "local_ratios", suspected: 9, raised: AtMost(2), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 3.0 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_small", sat: 0.98, method: "opposed", suspected: 9, raised: AtMost(2), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 3.0 },
    BaselineRow { scene: "disk_large", sat: 1.00, method: "off", suspected: 1481, raised: Exact(0), med_abs_logerr_max: 1.223497, p95_abs_logerr_max: 1.479790, hue_max_at_most: 3.0 },
    BaselineRow { scene: "disk_large", sat: 1.00, method: "clip", suspected: 1481, raised: Exact(0), med_abs_logerr_max: 1.223497, p95_abs_logerr_max: 1.479790, hue_max_at_most: 3.0 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // 2026-11-04: measured 10 + slack 2 for estimator-bit drift across codegen
    BaselineRow { scene: "disk_large", sat: 1.00, method: "local_ratios", suspected: 1481, raised: AtMost(12), med_abs_logerr_max: 1.223497, p95_abs_logerr_max: 1.479790, hue_max_at_most: 3.0 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_large", sat: 1.00, method: "opposed", suspected: 1481, raised: AtMost(2), med_abs_logerr_max: 1.223497, p95_abs_logerr_max: 1.479790, hue_max_at_most: 3.0 },
    BaselineRow { scene: "disk_large", sat: 0.98, method: "off", suspected: 1489, raised: Exact(0), med_abs_logerr_max: 1.250317, p95_abs_logerr_max: 1.510094, hue_max_at_most: 3.0 },
    BaselineRow { scene: "disk_large", sat: 0.98, method: "clip", suspected: 1489, raised: Exact(0), med_abs_logerr_max: 1.250317, p95_abs_logerr_max: 1.510094, hue_max_at_most: 3.0 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // 2026-11-04: measured 14 + slack 2 for estimator-bit drift across codegen
    BaselineRow { scene: "disk_large", sat: 0.98, method: "local_ratios", suspected: 1489, raised: AtMost(16), med_abs_logerr_max: 1.250317, p95_abs_logerr_max: 1.510094, hue_max_at_most: 3.0 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_large", sat: 0.98, method: "opposed", suspected: 1489, raised: AtMost(2), med_abs_logerr_max: 1.250317, p95_abs_logerr_max: 1.510094, hue_max_at_most: 3.0 },
    BaselineRow { scene: "two_channel", sat: 1.00, method: "off", suspected: 42, raised: Exact(0), med_abs_logerr_max: 0.450329, p95_abs_logerr_max: 0.828959, hue_max_at_most: 3.0 },
    BaselineRow { scene: "two_channel", sat: 1.00, method: "clip", suspected: 42, raised: Exact(0), med_abs_logerr_max: 0.450329, p95_abs_logerr_max: 0.828959, hue_max_at_most: 3.0 },
    BaselineRow { scene: "two_channel", sat: 1.00, method: "local_ratios", suspected: 42, raised: AtLeast(11), med_abs_logerr_max: 0.333861, p95_abs_logerr_max: 0.695226, hue_max_at_most: 3.0 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    BaselineRow { scene: "two_channel", sat: 1.00, method: "opposed", suspected: 42, raised: AtMost(2), med_abs_logerr_max: 0.450329, p95_abs_logerr_max: 0.828959, hue_max_at_most: 3.0 },
    BaselineRow { scene: "two_channel", sat: 0.98, method: "off", suspected: 43, raised: Exact(0), med_abs_logerr_max: 0.477053, p95_abs_logerr_max: 0.859263, hue_max_at_most: 3.0 },
    BaselineRow { scene: "two_channel", sat: 0.98, method: "clip", suspected: 43, raised: Exact(0), med_abs_logerr_max: 0.477053, p95_abs_logerr_max: 0.859263, hue_max_at_most: 3.0 },
    BaselineRow { scene: "two_channel", sat: 0.98, method: "local_ratios", suspected: 43, raised: AtLeast(11), med_abs_logerr_max: 0.352046, p95_abs_logerr_max: 0.699968, hue_max_at_most: 3.0 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    // 2026-11-04: measured 2 + slack 2 for estimator-bit drift across codegen
    BaselineRow { scene: "two_channel", sat: 0.98, method: "opposed", suspected: 43, raised: AtMost(4), med_abs_logerr_max: 0.477053, p95_abs_logerr_max: 0.859263, hue_max_at_most: 3.0 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "off", suspected: 268, raised: Exact(0), med_abs_logerr_max: 0.704530, p95_abs_logerr_max: 1.989775, hue_max_at_most: 3.0 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "clip", suspected: 268, raised: Exact(0), med_abs_logerr_max: 0.704530, p95_abs_logerr_max: 1.989775, hue_max_at_most: 3.0 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "local_ratios", suspected: 268, raised: AtLeast(38), med_abs_logerr_max: 0.704530, p95_abs_logerr_max: 1.989775, hue_max_at_most: 3.0 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "opposed", suspected: 268, raised: AtLeast(35), med_abs_logerr_max: 0.698491, p95_abs_logerr_max: 1.989775, hue_max_at_most: 3.0 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "off", suspected: 276, raised: Exact(0), med_abs_logerr_max: 0.693285, p95_abs_logerr_max: 2.020079, hue_max_at_most: 3.0 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "clip", suspected: 276, raised: Exact(0), med_abs_logerr_max: 0.693285, p95_abs_logerr_max: 2.020079, hue_max_at_most: 3.0 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "local_ratios", suspected: 276, raised: AtLeast(40), med_abs_logerr_max: 0.693285, p95_abs_logerr_max: 2.020079, hue_max_at_most: 3.0 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "opposed", suspected: 276, raised: AtLeast(37), med_abs_logerr_max: 0.634351, p95_abs_logerr_max: 2.020079, hue_max_at_most: 3.0 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "off", suspected: 31, raised: Exact(0), med_abs_logerr_max: 0.473335, p95_abs_logerr_max: 0.921391, hue_max_at_most: 3.0 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "clip", suspected: 31, raised: Exact(0), med_abs_logerr_max: 0.473335, p95_abs_logerr_max: 0.921391, hue_max_at_most: 3.0 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "local_ratios", suspected: 31, raised: AtLeast(7), med_abs_logerr_max: 0.322892, p95_abs_logerr_max: 0.638511, hue_max_at_most: 3.0 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "opposed", suspected: 31, raised: AtLeast(4), med_abs_logerr_max: 0.443842, p95_abs_logerr_max: 0.816886, hue_max_at_most: 3.0 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "off", suspected: 35, raised: Exact(0), med_abs_logerr_max: 0.492658, p95_abs_logerr_max: 0.951695, hue_max_at_most: 3.0 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "clip", suspected: 35, raised: Exact(0), med_abs_logerr_max: 0.492658, p95_abs_logerr_max: 0.951695, hue_max_at_most: 3.0 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "local_ratios", suspected: 35, raised: AtLeast(7), med_abs_logerr_max: 0.348839, p95_abs_logerr_max: 0.670333, hue_max_at_most: 3.0 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "opposed", suspected: 35, raised: AtLeast(3), med_abs_logerr_max: 0.458725, p95_abs_logerr_max: 0.847190, hue_max_at_most: 3.0 },
    BaselineRow { scene: "border_clip", sat: 1.00, method: "off", suspected: 157, raised: Exact(0), med_abs_logerr_max: 0.861791, p95_abs_logerr_max: 0.892948, hue_max_at_most: 3.0 },
    BaselineRow { scene: "border_clip", sat: 1.00, method: "clip", suspected: 157, raised: Exact(0), med_abs_logerr_max: 0.861791, p95_abs_logerr_max: 0.892948, hue_max_at_most: 3.0 },
    BaselineRow { scene: "border_clip", sat: 1.00, method: "local_ratios", suspected: 157, raised: AtLeast(3), med_abs_logerr_max: 0.860542, p95_abs_logerr_max: 0.889181, hue_max_at_most: 3.0 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    BaselineRow { scene: "border_clip", sat: 1.00, method: "opposed", suspected: 157, raised: AtMost(2), med_abs_logerr_max: 0.861791, p95_abs_logerr_max: 0.892948, hue_max_at_most: 3.0 },
    BaselineRow { scene: "border_clip", sat: 0.98, method: "off", suspected: 157, raised: Exact(0), med_abs_logerr_max: 0.892095, p95_abs_logerr_max: 0.923252, hue_max_at_most: 3.0 },
    BaselineRow { scene: "border_clip", sat: 0.98, method: "clip", suspected: 157, raised: Exact(0), med_abs_logerr_max: 0.892095, p95_abs_logerr_max: 0.923252, hue_max_at_most: 3.0 },
    BaselineRow { scene: "border_clip", sat: 0.98, method: "local_ratios", suspected: 157, raised: AtLeast(4), med_abs_logerr_max: 0.889533, p95_abs_logerr_max: 0.918952, hue_max_at_most: 3.0 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    BaselineRow { scene: "border_clip", sat: 0.98, method: "opposed", suspected: 157, raised: AtMost(2), med_abs_logerr_max: 0.892095, p95_abs_logerr_max: 0.923252, hue_max_at_most: 3.0 },
    BaselineRow { scene: "speculars", sat: 1.00, method: "off", suspected: 14, raised: Exact(0), med_abs_logerr_max: 1.132601, p95_abs_logerr_max: 1.472744, hue_max_at_most: 3.0 },
    BaselineRow { scene: "speculars", sat: 1.00, method: "clip", suspected: 14, raised: Exact(0), med_abs_logerr_max: 1.132601, p95_abs_logerr_max: 1.472744, hue_max_at_most: 3.0 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    BaselineRow { scene: "speculars", sat: 1.00, method: "local_ratios", suspected: 14, raised: AtMost(2), med_abs_logerr_max: 1.132601, p95_abs_logerr_max: 1.472744, hue_max_at_most: 3.0 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    BaselineRow { scene: "speculars", sat: 1.00, method: "opposed", suspected: 14, raised: AtMost(2), med_abs_logerr_max: 1.132601, p95_abs_logerr_max: 1.472744, hue_max_at_most: 3.0 },
    BaselineRow { scene: "speculars", sat: 0.98, method: "off", suspected: 14, raised: Exact(0), med_abs_logerr_max: 1.162905, p95_abs_logerr_max: 1.503048, hue_max_at_most: 3.0 },
    BaselineRow { scene: "speculars", sat: 0.98, method: "clip", suspected: 14, raised: Exact(0), med_abs_logerr_max: 1.162905, p95_abs_logerr_max: 1.503048, hue_max_at_most: 3.0 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    BaselineRow { scene: "speculars", sat: 0.98, method: "local_ratios", suspected: 14, raised: AtMost(2), med_abs_logerr_max: 1.162905, p95_abs_logerr_max: 1.503048, hue_max_at_most: 3.0 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    BaselineRow { scene: "speculars", sat: 0.98, method: "opposed", suspected: 14, raised: AtMost(2), med_abs_logerr_max: 1.162905, p95_abs_logerr_max: 1.503048, hue_max_at_most: 3.0 },
];
