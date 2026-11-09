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
    /// Data-lowering bound (`out < in`): Clip's WB-aware ceiling pass
    /// (issue #65). Off rows pin `Exact(0)`; Clip and reconstruction rows
    /// floor at half the measured count once it is non-zero.
    pub lowered: Bound,
    /// `stats.neutralised_sites` bound: the #65 neutraliser's progress
    /// floor. Off/Clip never neutralise (`Exact(0)`).
    pub neutralised: Bound,
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
/// - Off/Clip: `Exact(0)` — raises nothing; WB-aware Clip only *lowers*
///   strictly-above samples to the ceiling, and the #63 bit-exact no-op
///   wording is superseded (2026-11-09: WB-aware Clip + neutral fallback
///   landed, issue #65). The lowering side is bounded by `lowered`.
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

/// Floor for the `lowered` column (data `out < in`). Off moves nothing:
/// `Exact(0)`. For every other method a non-zero measurement is #65's
/// progress signal, floored at half the measured value (machine-decidable
/// from the measured count, so the capture stays a verbatim paste — same
/// decision shape as [`raised_policy`]). Cross-arch rounding of the
/// WB-aware ceilings argues for the /2 floor instead of an Exact pin.
#[must_use]
pub fn lowered_policy(method: &str, measured: usize) -> Bound {
    if method == "off" {
        return Bound::Exact(0);
    }
    if measured > 0 {
        Bound::AtLeast(measured / 2)
    } else {
        Bound::Exact(0)
    }
}

/// Floor for the `neutralised` column (`stats.neutralised_sites`). Only
/// LocalRatios/Opposed run a neutraliser; Off and Clip pin `Exact(0)`.
#[must_use]
pub fn neutralised_policy(method: &str, measured: usize) -> Bound {
    if matches!(method, "off" | "clip") {
        return Bound::Exact(0);
    }
    if measured > 0 {
        Bound::AtLeast(measured / 2)
    } else {
        Bound::Exact(0)
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

/// Renders `Bound` as the literal used in a baseline row.
fn bound_literal(bound: Bound) -> String {
    match bound {
        Bound::Exact(value) => format!("Exact({value})"),
        Bound::AtLeast(value) => format!("AtLeast({value})"),
        Bound::AtMost(value) => format!("AtMost({value})"),
    }
}

/// One pasteable `BaselineRow` literal (plus its policy comments) for the
/// baseline capture workflow: the bounds and the comment lines are decided
/// here, in one place, from `raised_policy`, `lowered_policy`,
/// `neutralised_policy` and the scene's rationale data.
#[must_use]
pub fn render_baseline_row(row: &MeasuredRow) -> String {
    let bound = raised_policy(row.scene, row.method, row.raised);
    let lowered_bound = lowered_policy(row.method, row.lowered);
    let neutralised_bound = neutralised_policy(row.method, row.neutralised);
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
        // #65 sibling note: the raised AtMost stays valid for RAISED only;
        // the lowering side progress signal lives in `neutralised`.
        if matches!(row.method, "local_ratios" | "opposed") {
            out.push_str(
                "    // #65: this raised ceiling is unaffected by neutralisation; see the\n    // neutralised floor on the same row for the lowering-side signal.\n",
            );
        }
    }
    out.push_str(&format!(
        "    BaselineRow {{ scene: {:?}, sat: {:.2}, method: {:?}, suspected: {}, raised: {}, \
         lowered: {}, neutralised: {}, med_abs_logerr_max: {}, p95_abs_logerr_max: {}, \
         hue_max_at_most: {} }},\n",
        row.scene,
        row.sat,
        row.method,
        row.suspected,
        bound_literal(bound),
        bound_literal(lowered_bound),
        bound_literal(neutralised_bound),
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
    if !bound_holds(row.lowered, measured.lowered) {
        violations.push(format!(
            "{}@{} {}: lowered = {} violates {:?}",
            row.scene, row.sat, row.method, measured.lowered, row.lowered
        ));
    }
    if !bound_holds(row.neutralised, measured.neutralised) {
        violations.push(format!(
            "{}@{} {}: neutralised = {} violates {:?}",
            row.scene, row.sat, row.method, measured.neutralised, row.neutralised
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

// Measured 2026-11-09 on this branch (base f0a2895 + issue #65), debug,
// x86-64, default rayon threads. Full re-baseline: Clip rows now run with
// WB-aware ceilings (sat * min(METRIC_WB) / gain) and the reconstruction
// rows share that vector as `neutral_ceiling`, so all 14 Clip rows and the
// R/B-fallback LR/Opposed rows moved (issue #65).
// Suspected counts are bit-derived (see generator margin guard); the med,
// p95 and hue-max ceilings are all 1.5x measured per issue #63 "assert
// today's numbers loosely", `raised` ceilings carry RAISED_SLACK units of
// estimator-drift slack, and the lowered/neutralised floors are machine-
// decided by `lowered_policy` / `neutralised_policy` (#65 progress signal).
use Bound::{AtLeast, AtMost, Exact};

// Kept one row per line (matching the harness's pasteable output verbatim)
// so re-baselining stays a clean line-diff in PR review.
#[rustfmt::skip]
pub const BASELINES: &[BaselineRow] = &[
    BaselineRow { scene: "disk_small", sat: 1.00, method: "off", suspected: 9, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 1.526664 },
    BaselineRow { scene: "disk_small", sat: 1.00, method: "clip", suspected: 9, raised: Exact(0), lowered: AtLeast(1), neutralised: Exact(0), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 1.526664 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "disk_small", sat: 1.00, method: "local_ratios", suspected: 9, raised: AtMost(2), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 1.526664 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "disk_small", sat: 1.00, method: "opposed", suspected: 9, raised: AtMost(2), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.754080, p95_abs_logerr_max: 1.028429, hue_max_at_most: 1.526664 },
    BaselineRow { scene: "disk_small", sat: 0.98, method: "off", suspected: 9, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 1.589631 },
    BaselineRow { scene: "disk_small", sat: 0.98, method: "clip", suspected: 9, raised: Exact(0), lowered: AtLeast(1), neutralised: Exact(0), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 1.589631 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "disk_small", sat: 0.98, method: "local_ratios", suspected: 9, raised: AtMost(2), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 1.589631 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "disk_small", sat: 0.98, method: "opposed", suspected: 9, raised: AtMost(2), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.784384, p95_abs_logerr_max: 1.058733, hue_max_at_most: 1.589631 },
    BaselineRow { scene: "disk_large", sat: 1.00, method: "off", suspected: 8073, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 2.403506, p95_abs_logerr_max: 2.753494, hue_max_at_most: 2.086022 },
    BaselineRow { scene: "disk_large", sat: 1.00, method: "clip", suspected: 8073, raised: Exact(0), lowered: AtLeast(1247), neutralised: Exact(0), med_abs_logerr_max: 2.403506, p95_abs_logerr_max: 2.753494, hue_max_at_most: 2.086022 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // 2026-11-04: measured 4 + slack 2 for estimator-bit drift across codegen
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "disk_large", sat: 1.00, method: "local_ratios", suspected: 8073, raised: AtMost(6), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 2.403506, p95_abs_logerr_max: 2.753494, hue_max_at_most: 2.086022 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "disk_large", sat: 1.00, method: "opposed", suspected: 8073, raised: AtMost(2), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 2.403506, p95_abs_logerr_max: 2.753494, hue_max_at_most: 2.086022 },
    BaselineRow { scene: "disk_large", sat: 0.98, method: "off", suspected: 8086, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 2.433040, p95_abs_logerr_max: 2.783764, hue_max_at_most: 2.150477 },
    BaselineRow { scene: "disk_large", sat: 0.98, method: "clip", suspected: 8086, raised: Exact(0), lowered: AtLeast(1306), neutralised: Exact(0), med_abs_logerr_max: 2.433040, p95_abs_logerr_max: 2.783764, hue_max_at_most: 2.150477 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // 2026-11-04: measured 2 + slack 2 for estimator-bit drift across codegen
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "disk_large", sat: 0.98, method: "local_ratios", suspected: 8086, raised: AtMost(4), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 2.433040, p95_abs_logerr_max: 2.783764, hue_max_at_most: 2.150477 },
    // Known failure #62: estimates produced (recon >> raised) are never applied
    // on large single-channel regions. When #62 lands this row flips to AtLeast
    // — that flip IS the progress signal.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "disk_large", sat: 0.98, method: "opposed", suspected: 8086, raised: AtMost(2), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 2.433040, p95_abs_logerr_max: 2.783764, hue_max_at_most: 2.150477 },
    BaselineRow { scene: "two_channel", sat: 1.00, method: "off", suspected: 42, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.450329, p95_abs_logerr_max: 0.828959, hue_max_at_most: 0.612795 },
    BaselineRow { scene: "two_channel", sat: 1.00, method: "clip", suspected: 42, raised: Exact(0), lowered: AtLeast(21), neutralised: Exact(0), med_abs_logerr_max: 0.719794, p95_abs_logerr_max: 1.800280, hue_max_at_most: 3.657449 },
    BaselineRow { scene: "two_channel", sat: 1.00, method: "local_ratios", suspected: 42, raised: AtLeast(11), lowered: AtLeast(3), neutralised: AtLeast(3), med_abs_logerr_max: 0.419187, p95_abs_logerr_max: 1.371914, hue_max_at_most: 2.984909 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "two_channel", sat: 1.00, method: "opposed", suspected: 42, raised: AtMost(2), lowered: AtLeast(8), neutralised: AtLeast(8), med_abs_logerr_max: 0.719794, p95_abs_logerr_max: 1.800280, hue_max_at_most: 3.657449 },
    BaselineRow { scene: "two_channel", sat: 0.98, method: "off", suspected: 43, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.477053, p95_abs_logerr_max: 0.859263, hue_max_at_most: 0.668729 },
    BaselineRow { scene: "two_channel", sat: 0.98, method: "clip", suspected: 43, raised: Exact(0), lowered: AtLeast(21), neutralised: Exact(0), med_abs_logerr_max: 0.737333, p95_abs_logerr_max: 1.830584, hue_max_at_most: 3.684205 },
    BaselineRow { scene: "two_channel", sat: 0.98, method: "local_ratios", suspected: 43, raised: AtLeast(11), lowered: AtLeast(3), neutralised: AtLeast(3), med_abs_logerr_max: 0.405345, p95_abs_logerr_max: 1.402218, hue_max_at_most: 3.124926 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    // 2026-11-04: measured 2 + slack 2 for estimator-bit drift across codegen
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "two_channel", sat: 0.98, method: "opposed", suspected: 43, raised: AtMost(4), lowered: AtLeast(8), neutralised: AtLeast(8), med_abs_logerr_max: 0.729825, p95_abs_logerr_max: 1.830584, hue_max_at_most: 3.684205 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "off", suspected: 268, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.704530, p95_abs_logerr_max: 1.989775, hue_max_at_most: 2.697371 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "clip", suspected: 268, raised: Exact(0), lowered: AtLeast(399), neutralised: Exact(0), med_abs_logerr_max: 1.245692, p95_abs_logerr_max: 3.029496, hue_max_at_most: 3.087307 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "local_ratios", suspected: 268, raised: AtLeast(38), lowered: AtLeast(53), neutralised: AtLeast(53), med_abs_logerr_max: 1.105516, p95_abs_logerr_max: 3.029496, hue_max_at_most: 3.087307 },
    BaselineRow { scene: "sun_core_halo", sat: 1.00, method: "opposed", suspected: 268, raised: AtLeast(35), lowered: AtLeast(56), neutralised: AtLeast(56), med_abs_logerr_max: 1.067033, p95_abs_logerr_max: 3.029496, hue_max_at_most: 3.087307 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "off", suspected: 276, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.693285, p95_abs_logerr_max: 2.020079, hue_max_at_most: 2.716664 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "clip", suspected: 276, raised: Exact(0), lowered: AtLeast(410), neutralised: Exact(0), med_abs_logerr_max: 1.263992, p95_abs_logerr_max: 3.059800, hue_max_at_most: 3.093362 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "local_ratios", suspected: 276, raised: AtLeast(40), lowered: AtLeast(54), neutralised: AtLeast(54), med_abs_logerr_max: 1.031028, p95_abs_logerr_max: 3.059800, hue_max_at_most: 3.093362 },
    BaselineRow { scene: "sun_core_halo", sat: 0.98, method: "opposed", suspected: 276, raised: AtLeast(37), lowered: AtLeast(57), neutralised: AtLeast(57), med_abs_logerr_max: 1.034050, p95_abs_logerr_max: 3.059800, hue_max_at_most: 3.093362 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "off", suspected: 31, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.473335, p95_abs_logerr_max: 0.921391, hue_max_at_most: 0.720534 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "clip", suspected: 31, raised: Exact(0), lowered: AtLeast(37), neutralised: Exact(0), med_abs_logerr_max: 1.359861, p95_abs_logerr_max: 1.961111, hue_max_at_most: 1.907816 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "local_ratios", suspected: 31, raised: AtLeast(7), lowered: AtLeast(8), neutralised: AtLeast(8), med_abs_logerr_max: 0.680135, p95_abs_logerr_max: 1.338226, hue_max_at_most: 1.655399 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 1.00, method: "opposed", suspected: 31, raised: AtLeast(4), lowered: AtLeast(11), neutralised: AtLeast(11), med_abs_logerr_max: 1.085803, p95_abs_logerr_max: 1.821018, hue_max_at_most: 1.907816 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "off", suspected: 35, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.492658, p95_abs_logerr_max: 0.951695, hue_max_at_most: 0.765011 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "clip", suspected: 35, raised: Exact(0), lowered: AtLeast(38), neutralised: Exact(0), med_abs_logerr_max: 1.365733, p95_abs_logerr_max: 1.991415, hue_max_at_most: 1.962792 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "local_ratios", suspected: 35, raised: AtLeast(7), lowered: AtLeast(10), neutralised: AtLeast(10), med_abs_logerr_max: 0.717195, p95_abs_logerr_max: 1.550801, hue_max_at_most: 1.712002 },
    BaselineRow { scene: "warm_cool_adjacent", sat: 0.98, method: "opposed", suspected: 35, raised: AtLeast(3), lowered: AtLeast(14), neutralised: AtLeast(14), med_abs_logerr_max: 1.116107, p95_abs_logerr_max: 1.851322, hue_max_at_most: 1.962792 },
    BaselineRow { scene: "border_clip", sat: 1.00, method: "off", suspected: 157, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.861791, p95_abs_logerr_max: 0.892948, hue_max_at_most: 1.971580 },
    BaselineRow { scene: "border_clip", sat: 1.00, method: "clip", suspected: 157, raised: Exact(0), lowered: AtLeast(54), neutralised: Exact(0), med_abs_logerr_max: 0.861791, p95_abs_logerr_max: 0.892948, hue_max_at_most: 1.971580 },
    BaselineRow { scene: "border_clip", sat: 1.00, method: "local_ratios", suspected: 157, raised: AtLeast(3), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.860542, p95_abs_logerr_max: 0.889181, hue_max_at_most: 1.971580 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "border_clip", sat: 1.00, method: "opposed", suspected: 157, raised: AtMost(2), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.861791, p95_abs_logerr_max: 0.892948, hue_max_at_most: 1.971580 },
    BaselineRow { scene: "border_clip", sat: 0.98, method: "off", suspected: 157, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.892095, p95_abs_logerr_max: 0.923252, hue_max_at_most: 2.018326 },
    BaselineRow { scene: "border_clip", sat: 0.98, method: "clip", suspected: 157, raised: Exact(0), lowered: AtLeast(63), neutralised: Exact(0), med_abs_logerr_max: 0.892095, p95_abs_logerr_max: 0.923252, hue_max_at_most: 2.018326 },
    BaselineRow { scene: "border_clip", sat: 0.98, method: "local_ratios", suspected: 157, raised: AtLeast(4), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.889533, p95_abs_logerr_max: 0.918952, hue_max_at_most: 2.018326 },
    // raised below the acting threshold (5) at baseline; flip to AtLeast when
    // methods act here.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "border_clip", sat: 0.98, method: "opposed", suspected: 157, raised: AtMost(2), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 0.892095, p95_abs_logerr_max: 0.923252, hue_max_at_most: 2.018326 },
    BaselineRow { scene: "speculars", sat: 1.00, method: "off", suspected: 14, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 1.132601, p95_abs_logerr_max: 1.472744, hue_max_at_most: 1.880613 },
    BaselineRow { scene: "speculars", sat: 1.00, method: "clip", suspected: 14, raised: Exact(0), lowered: AtLeast(12), neutralised: Exact(0), med_abs_logerr_max: 1.789983, p95_abs_logerr_max: 2.512464, hue_max_at_most: 2.277433 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "speculars", sat: 1.00, method: "local_ratios", suspected: 14, raised: AtMost(2), lowered: AtLeast(5), neutralised: AtLeast(5), med_abs_logerr_max: 1.789983, p95_abs_logerr_max: 2.512464, hue_max_at_most: 2.277433 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "speculars", sat: 1.00, method: "opposed", suspected: 14, raised: AtMost(2), lowered: AtLeast(5), neutralised: AtLeast(5), med_abs_logerr_max: 1.789983, p95_abs_logerr_max: 2.512464, hue_max_at_most: 2.277433 },
    BaselineRow { scene: "speculars", sat: 0.98, method: "off", suspected: 14, raised: Exact(0), lowered: Exact(0), neutralised: Exact(0), med_abs_logerr_max: 1.162905, p95_abs_logerr_max: 1.503048, hue_max_at_most: 1.910010 },
    BaselineRow { scene: "speculars", sat: 0.98, method: "clip", suspected: 14, raised: Exact(0), lowered: AtLeast(14), neutralised: Exact(0), med_abs_logerr_max: 1.820287, p95_abs_logerr_max: 2.542768, hue_max_at_most: 2.315410 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "speculars", sat: 0.98, method: "local_ratios", suspected: 14, raised: AtMost(2), lowered: AtLeast(5), neutralised: AtLeast(5), med_abs_logerr_max: 1.820287, p95_abs_logerr_max: 2.542768, hue_max_at_most: 2.315410 },
    // Known #62 shape: estimates produced, none applied on isolated tiny
    // speculars; flip to AtLeast when methods act here.
    // #65: this raised ceiling is unaffected by neutralisation; see the
    // neutralised floor on the same row for the lowering-side signal.
    BaselineRow { scene: "speculars", sat: 0.98, method: "opposed", suspected: 14, raised: AtMost(2), lowered: AtLeast(5), neutralised: AtLeast(5), med_abs_logerr_max: 1.820287, p95_abs_logerr_max: 2.542768, hue_max_at_most: 2.315410 },
];
