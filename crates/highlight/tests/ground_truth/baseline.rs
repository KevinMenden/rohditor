//! Pinned baselines for the issue #63 harness.
//!
//! One row per (scene, sat, method) run of the matrix: an exact suspected
//! count, a bound on `raised`, and ceilings for the error metrics. The
//! matrix test checks every run against these rows, collects all
//! violations, and fails once with the full list. It also prints a
//! pasteable block in exactly the `BASELINES` row format, so
//! re-baselining is a verbatim copy-paste of the emitted lines.

use super::metrics::MeasuredRow;

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
    /// Ceiling on the maximum hue error in radians, 1.5x measured — the
    /// same convention and rounding rule as the med/p95 ceilings, because
    /// every measured row has a clearly non-zero hue maximum (the clipping
    /// itself moves the sampled channel). A run whose hue set is empty
    /// (no suspected pixel passes the chroma floor) satisfies the ceiling
    /// trivially; a row captured with no measurable hue pins `0.0`, so hue
    /// appearing there later forces a deliberate re-baseline. Median hue
    /// is printed but not asserted.
    pub hue_max_at_most: f64,
}

/// Slack added to `AtMost` raised ceilings for estimator-bit drift, and
/// the date of the measurement pass that set it (the date is part of the
/// meaning: it says which capture the ceiling tracks).
const RAISED_SLACK: usize = 2;
const RAISED_SLACK_DATE: &str = "2026-11-04";

/// Baseline bound for the `raised` column:
/// - Off/Clip: `Exact(0)` (identity / bit-exact no-op on pre-clamped input).
/// - Single-channel disc scenes: `AtMost(measured + RAISED_SLACK)` — the
///   #62 known failure (estimates are produced but never applied on large
///   single-channel regions); stays a ceiling even if measurements grow.
/// - Acting scenes (measured raised >= 5): `AtLeast(measured / 2)` floor.
/// - Anything else measured below the acting threshold: `AtMost(measured +
///   RAISED_SLACK)` (never a vacuous 0-floor).
///
/// The `AtMost` arms carry RAISED_SLACK units of slack — the same ±2
/// tolerance already allowed for `suspected`: a 1-bit estimator wobble on
/// another machine must not break the ceiling. `AtLeast` floors keep no
/// slack (under-acting is a real regression).
#[must_use]
pub fn raised_policy(scene: &str, method: &str, measured: usize) -> Bound {
    if matches!(method, "off" | "clip") {
        return Bound::Exact(0);
    }
    if scene == "disk_small" || scene == "disk_large" {
        return Bound::AtMost(measured + RAISED_SLACK);
    }
    if measured >= 5 {
        Bound::AtLeast(measured / 2)
    } else {
        Bound::AtMost(measured + RAISED_SLACK)
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

// Per-scene rationale for a ceiling (rather than floor) raised bound, kept
// as data so `render_baseline_row` stays one function that only formats.
const DISC_KNOWN_62_COMMENT: &[&str] = &[
    "Known failure #62: estimates produced (recon >> raised) are never applied",
    "on large single-channel regions. When #62 lands this row flips to AtLeast",
    "— that flip IS the progress signal.",
];
const SPECULARS_KNOWN_62_COMMENT: &[&str] = &[
    "Known #62 shape: estimates produced, none applied on isolated tiny",
    "speculars; flip to AtLeast when methods act here.",
];
const BELOW_ACTING_COMMENT: &[&str] = &[
    "raised below the acting threshold (5) at baseline; flip to AtLeast when",
    "methods act here.",
];

/// The explanation lines printed above one scene's `AtMost` rows.
fn ceiling_comment(scene: &str) -> &'static [&'static str] {
    match scene {
        "disk_small" | "disk_large" => DISC_KNOWN_62_COMMENT,
        "speculars" => SPECULARS_KNOWN_62_COMMENT,
        _ => BELOW_ACTING_COMMENT,
    }
}

/// Formats a ceiling as a Rust literal guaranteed >= `1.5 * measured`:
/// round the 1.5x value up to 6 decimals.
fn ceiling_literal(measured: f64) -> String {
    format!("{:.6}", (measured * 1.5 * 1e6).ceil() / 1e6)
}

/// One pasteable `BaselineRow` literal (plus its policy comments) for the
/// baseline capture workflow: the bound and the comment lines are decided
/// here, in one place, from `raised_policy` and the scene's rationale data.
#[must_use]
pub fn render_baseline_row(row: &MeasuredRow) -> String {
    let bound = raised_policy(row.scene, row.method, row.raised);
    let bound_text = match bound {
        Bound::Exact(value) => format!("Exact({value})"),
        Bound::AtLeast(value) => format!("AtLeast({value})"),
        Bound::AtMost(value) => format!("AtMost({value})"),
    };
    let mut out = String::new();
    if matches!(bound, Bound::AtMost(_)) {
        for line in ceiling_comment(row.scene) {
            out.push_str(&format!("    // {line}\n"));
        }
        // A ceiling that tracks a non-zero measured estimator count carries
        // a dated slack note so a capture run stays paste-verbatim.
        if row.raised > 0 {
            out.push_str(&format!(
                "    // {RAISED_SLACK_DATE}: measured {} + slack {RAISED_SLACK} for \
                 estimator-bit drift across codegen\n",
                row.raised
            ));
        }
    }
    out.push_str(&format!(
        "    BaselineRow {{ scene: {:?}, sat: {:.2}, method: {:?}, suspected: {}, raised: {}, \
         med_abs_logerr_max: {}, p95_abs_logerr_max: {}, hue_max_at_most: {} }},\n",
        row.scene,
        row.sat,
        row.method,
        row.suspected,
        bound_text,
        ceiling_literal(row.med),
        ceiling_literal(row.p95),
        row.hue_max
            .map_or_else(|| "0.0".to_owned(), ceiling_literal),
    ));
    out
}

/// Find the pinned row for `(scene, sat, method)`.
#[must_use]
pub fn find_baseline(scene: &str, sat: f32, method: &str) -> Option<&'static BaselineRow> {
    BASELINES.iter().find(|row| {
        row.scene == scene && (row.sat - sat).abs() < f32::EPSILON && row.method == method
    })
}

/// Every way one measured run violates its pinned row; empty means pass.
/// Collecting (instead of panicking per row) lets the driver report all
/// broken rows at once.
#[must_use]
pub fn check_baseline(row: &BaselineRow, measured: &MeasuredRow) -> Vec<String> {
    let mut violations = Vec::new();
    if measured.suspected != row.suspected {
        violations.push(format!(
            "{}@{} {}: suspected count moved ({} != pinned {})",
            row.scene, row.sat, row.method, measured.suspected, row.suspected
        ));
    }
    if !bound_holds(row.raised, measured.raised) {
        violations.push(format!(
            "{}@{} {}: raised = {} violates {:?}",
            row.scene, row.sat, row.method, measured.raised, row.raised
        ));
    }
    if measured.med > row.med_abs_logerr_max {
        violations.push(format!(
            "{}@{} {}: med |ln| {} exceeds ceiling {}",
            row.scene, row.sat, row.method, measured.med, row.med_abs_logerr_max
        ));
    }
    if measured.p95 > row.p95_abs_logerr_max {
        violations.push(format!(
            "{}@{} {}: p95 |ln| {} exceeds ceiling {}",
            row.scene, row.sat, row.method, measured.p95, row.p95_abs_logerr_max
        ));
    }
    // Empty hue sets (no suspected pixel passes the chroma floor) satisfy
    // the bound trivially; never assert on an empty maximum.
    if let Some(hue_max) = measured.hue_max.filter(|hue| *hue > row.hue_max_at_most) {
        violations.push(format!(
            "{}@{} {}: max hue {} rad exceeds ceiling {}",
            row.scene, row.sat, row.method, hue_max, row.hue_max_at_most
        ));
    }
    violations
}

// Measured 2026-11-04 on this branch (base commit d492784), debug, x86-64,
// default rayon threads.
// Suspected counts are bit-derived (see generator margin guard); the med,
// p95 and hue-max ceilings are all 1.5x measured per issue #63 "assert
// today's numbers loosely", and `raised` ceilings carry RAISED_SLACK units
// of estimator-drift slack (see `raised_policy`).
use Bound::{AtLeast, AtMost, Exact};

// Kept one row per line (matching the harness's pasteable output verbatim)
// so re-baselining stays a clean line-diff in PR review.
#[rustfmt::skip]
pub const BASELINES: &[BaselineRow] = &[
    BaselineRow { scene: "disk_small", sat: 1.00, method: "off", suspected: 9, raised: Exact(0), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 1.526664 },
    BaselineRow { scene: "disk_small", sat: 1.00, method: "clip", suspected: 9, raised: Exact(0), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 1.526664 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_small", sat: 1.00, method: "local_ratios", suspected: 9, raised: AtMost(2), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 1.526664 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_small", sat: 1.00, method: "opposed", suspected: 9, raised: AtMost(2), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 1.526664 },
    BaselineRow { scene: "disk_small", sat: 0.98, method: "off", suspected: 9, raised: Exact(0), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 1.589631 },
    BaselineRow { scene: "disk_small", sat: 0.98, method: "clip", suspected: 9, raised: Exact(0), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 1.589631 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_small", sat: 0.98, method: "local_ratios", suspected: 9, raised: AtMost(2), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 1.589631 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_small", sat: 0.98, method: "opposed", suspected: 9, raised: AtMost(2), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 1.589631 },
    BaselineRow { scene: "disk_large", sat: 1.00, method: "off", suspected: 8073, raised: Exact(0), med_abs_logerr_max: 2.403506, p95_abs_logerr_max: 2.753494, hue_max_at_most: 2.086022 },
    BaselineRow { scene: "disk_large", sat: 1.00, method: "clip", suspected: 8073, raised: Exact(0), med_abs_logerr_max: 2.403506, p95_abs_logerr_max: 2.753494, hue_max_at_most: 2.086022 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // 2026-11-04: measured 4 + slack 2 for estimator-bit drift across codegen
    BaselineRow { scene: "disk_large", sat: 1.00, method: "local_ratios", suspected: 8073, raised: AtMost(6), med_abs_logerr_max: 2.403506, p95_abs_logerr_max: 2.753494, hue_max_at_most: 2.086022 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_large", sat: 1.00, method: "opposed", suspected: 8073, raised: AtMost(2), med_abs_logerr_max: 2.403506, p95_abs_logerr_max: 2.753494, hue_max_at_most: 2.086022 },
    BaselineRow { scene: "disk_large", sat: 0.98, method: "off", suspected: 8086, raised: Exact(0), med_abs_logerr_max: 2.433040, p95_abs_logerr_max: 2.783764, hue_max_at_most: 2.150477 },
    BaselineRow { scene: "disk_large", sat: 0.98, method: "clip", suspected: 8086, raised: Exact(0), med_abs_logerr_max: 2.433040, p95_abs_logerr_max: 2.783764, hue_max_at_most: 2.150477 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // 2026-11-04: measured 2 + slack 2 for estimator-bit drift across codegen
    BaselineRow { scene: "disk_large", sat: 0.98, method: "local_ratios", suspected: 8086, raised: AtMost(4), med_abs_logerr_max: 2.433040, p95_abs_logerr_max: 2.783764, hue_max_at_most: 2.150477 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    BaselineRow { scene: "disk_large", sat: 0.98, method: "opposed", suspected: 8086, raised: AtMost(2), med_abs_logerr_max: 2.433040, p95_abs_logerr_max: 2.783764, hue_max_at_most: 2.150477 },
    BaselineRow { scene: "two_channel", sat: 1.00, method: "off", suspected: 42, raised: Exact(0), med_abs_logerr_max: 0.450329, p95_abs_logerr_max: 0.828959, hue_max_at_most: 0.612795 },
    BaselineRow { scene: "two_channel", sat: 1.00, method: "clip", suspected: 42, raised: Exact(0), med_abs_logerr_max: 0.450329, p95_abs_logerr_max: 0.828959, hue_max_at_most: 0.612795 },
    BaselineRow { scene: "two_channel", sat: 1.00, method: "local_ratios", suspected: 42, raised: AtLeast(11), med_abs_logerr_max: 0.333861, p95_abs_logerr_max: 0.695226, hue_max_at_most: 0.514760 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    BaselineRow { scene: "two_channel", sat: 1.00, method: "opposed", suspected: 42, raised: AtMost(2), med_abs_logerr_max: 0.450329, p95_abs_logerr_max: 0.828959, hue_max_at_most: 0.612795 },
    BaselineRow { scene: "two_channel", sat: 0.98, method: "off", suspected: 43, raised: Exact(0), med_abs_logerr_max: 0.477053, p95_abs_logerr_max: 0.859263, hue_max_at_most: 0.668729 },
    BaselineRow { scene: "two_channel", sat: 0.98, method: "clip", suspected: 43, raised: Exact(0), med_abs_logerr_max: 0.477053, p95_abs_logerr_max: 0.859263, hue_max_at_most: 0.668729 },
    BaselineRow { scene: "two_channel", sat: 0.98, method: "local_ratios", suspected: 43, raised: AtLeast(11), med_abs_logerr_max: 0.352046, p95_abs_logerr_max: 0.699968, hue_max_at_most: 0.514760 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    // 2026-11-04: measured 2 + slack 2 for estimator-bit drift across codegen
    BaselineRow { scene: "two_channel", sat: 0.98, method: "opposed", suspected: 43, raised: AtMost(4), med_abs_logerr_max: 0.477053, p95_abs_logerr_max: 0.859263, hue_max_at_most: 0.668729 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "off", suspected: 268, raised: Exact(0), med_abs_logerr_max: 0.704530, p95_abs_logerr_max: 1.989775, hue_max_at_most: 2.697371 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "clip", suspected: 268, raised: Exact(0), med_abs_logerr_max: 0.704530, p95_abs_logerr_max: 1.989775, hue_max_at_most: 2.697371 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "local_ratios", suspected: 268, raised: AtLeast(38), med_abs_logerr_max: 0.704530, p95_abs_logerr_max: 1.989775, hue_max_at_most: 2.697371 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "opposed", suspected: 268, raised: AtLeast(35), med_abs_logerr_max: 0.698491, p95_abs_logerr_max: 1.989775, hue_max_at_most: 2.697371 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "off", suspected: 276, raised: Exact(0), med_abs_logerr_max: 0.693285, p95_abs_logerr_max: 2.020079, hue_max_at_most: 2.716664 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "clip", suspected: 276, raised: Exact(0), med_abs_logerr_max: 0.693285, p95_abs_logerr_max: 2.020079, hue_max_at_most: 2.716664 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "local_ratios", suspected: 276, raised: AtLeast(40), med_abs_logerr_max: 0.693285, p95_abs_logerr_max: 2.020079, hue_max_at_most: 2.716664 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "opposed", suspected: 276, raised: AtLeast(37), med_abs_logerr_max: 0.634351, p95_abs_logerr_max: 2.020079, hue_max_at_most: 2.716664 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "off", suspected: 31, raised: Exact(0), med_abs_logerr_max: 0.473335, p95_abs_logerr_max: 0.921391, hue_max_at_most: 0.720534 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "clip", suspected: 31, raised: Exact(0), med_abs_logerr_max: 0.473335, p95_abs_logerr_max: 0.921391, hue_max_at_most: 0.720534 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "local_ratios", suspected: 31, raised: AtLeast(7), med_abs_logerr_max: 0.322892, p95_abs_logerr_max: 0.638511, hue_max_at_most: 0.561719 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "opposed", suspected: 31, raised: AtLeast(4), med_abs_logerr_max: 0.443842, p95_abs_logerr_max: 0.816886, hue_max_at_most: 0.720534 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "off", suspected: 35, raised: Exact(0), med_abs_logerr_max: 0.492658, p95_abs_logerr_max: 0.951695, hue_max_at_most: 0.765011 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "clip", suspected: 35, raised: Exact(0), med_abs_logerr_max: 0.492658, p95_abs_logerr_max: 0.951695, hue_max_at_most: 0.765011 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "local_ratios", suspected: 35, raised: AtLeast(7), med_abs_logerr_max: 0.348839, p95_abs_logerr_max: 0.670333, hue_max_at_most: 0.574164 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "opposed", suspected: 35, raised: AtLeast(3), med_abs_logerr_max: 0.458725, p95_abs_logerr_max: 0.847190, hue_max_at_most: 0.765011 },
    BaselineRow { scene: "border_clip", sat: 1.00, method: "off", suspected: 157, raised: Exact(0), med_abs_logerr_max: 0.861791, p95_abs_logerr_max: 0.892948, hue_max_at_most: 1.971580 },
    BaselineRow { scene: "border_clip", sat: 1.00, method: "clip", suspected: 157, raised: Exact(0), med_abs_logerr_max: 0.861791, p95_abs_logerr_max: 0.892948, hue_max_at_most: 1.971580 },
    BaselineRow { scene: "border_clip", sat: 1.00, method: "local_ratios", suspected: 157, raised: AtLeast(3), med_abs_logerr_max: 0.860542, p95_abs_logerr_max: 0.889181, hue_max_at_most: 1.971580 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    BaselineRow { scene: "border_clip", sat: 1.00, method: "opposed", suspected: 157, raised: AtMost(2), med_abs_logerr_max: 0.861791, p95_abs_logerr_max: 0.892948, hue_max_at_most: 1.971580 },
    BaselineRow { scene: "border_clip", sat: 0.98, method: "off", suspected: 157, raised: Exact(0), med_abs_logerr_max: 0.892095, p95_abs_logerr_max: 0.923252, hue_max_at_most: 2.018326 },
    BaselineRow { scene: "border_clip", sat: 0.98, method: "clip", suspected: 157, raised: Exact(0), med_abs_logerr_max: 0.892095, p95_abs_logerr_max: 0.923252, hue_max_at_most: 2.018326 },
    BaselineRow { scene: "border_clip", sat: 0.98, method: "local_ratios", suspected: 157, raised: AtLeast(4), med_abs_logerr_max: 0.889533, p95_abs_logerr_max: 0.918952, hue_max_at_most: 2.018326 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    BaselineRow { scene: "border_clip", sat: 0.98, method: "opposed", suspected: 157, raised: AtMost(2), med_abs_logerr_max: 0.892095, p95_abs_logerr_max: 0.923252, hue_max_at_most: 2.018326 },
    BaselineRow { scene: "speculars", sat: 1.00, method: "off", suspected: 14, raised: Exact(0), med_abs_logerr_max: 1.132601, p95_abs_logerr_max: 1.472744, hue_max_at_most: 1.880613 },
    BaselineRow { scene: "speculars", sat: 1.00, method: "clip", suspected: 14, raised: Exact(0), med_abs_logerr_max: 1.132601, p95_abs_logerr_max: 1.472744, hue_max_at_most: 1.880613 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    BaselineRow { scene: "speculars", sat: 1.00, method: "local_ratios", suspected: 14, raised: AtMost(2), med_abs_logerr_max: 1.132601, p95_abs_logerr_max: 1.472744, hue_max_at_most: 1.880613 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    BaselineRow { scene: "speculars", sat: 1.00, method: "opposed", suspected: 14, raised: AtMost(2), med_abs_logerr_max: 1.132601, p95_abs_logerr_max: 1.472744, hue_max_at_most: 1.880613 },
    BaselineRow { scene: "speculars", sat: 0.98, method: "off", suspected: 14, raised: Exact(0), med_abs_logerr_max: 1.162905, p95_abs_logerr_max: 1.503048, hue_max_at_most: 1.910010 },
    BaselineRow { scene: "speculars", sat: 0.98, method: "clip", suspected: 14, raised: Exact(0), med_abs_logerr_max: 1.162905, p95_abs_logerr_max: 1.503048, hue_max_at_most: 1.910010 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    BaselineRow { scene: "speculars", sat: 0.98, method: "local_ratios", suspected: 14, raised: AtMost(2), med_abs_logerr_max: 1.162905, p95_abs_logerr_max: 1.503048, hue_max_at_most: 1.910010 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    BaselineRow { scene: "speculars", sat: 0.98, method: "opposed", suspected: 14, raised: AtMost(2), med_abs_logerr_max: 1.162905, p95_abs_logerr_max: 1.503048, hue_max_at_most: 1.910010 },
];
