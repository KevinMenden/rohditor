//! Ground-truth reconstruction harness for the highlight crate (issue #63).
//!
//! Generates synthetic scenes with a known unclipped truth, mosaics + clamps
//! them, runs Off / Clip / LocalRatios / Opposed at the crate boundary, and
//! scores reconstruction quality. Three tests:
//! - `scene_guards`: exact-sat margin + mask-from-bits guards, no method
//!   runs — fast.
//! - `margin_negative_control`: the #62 margin trap as a one-liner.
//! - `ground_truth_matrix`: 7 scenes x 2 sat levels x 4 methods = 56 runs,
//!   prints the human table, the pasteable baseline literal block, and the
//!   elapsed time (timing is printed, never asserted), then checks every run
//!   against the pinned `metrics::BASELINES` rows.
//!
//! Levels are pinned at the crate boundary (`{sat} x 3` for both detection
//! and Clip ceilings) instead of going through `rohditor_core` resolution,
//! which decouples this harness from pending #64.

mod generator;
mod metrics;
mod scenes;

use std::time::Instant;

use rohditor_highlight::{
    ChannelClipLevels, ChannelDetectionLevels, HighlightExecution, HighlightExecutionOutput,
    detect_local_ratios,
};
use rohditor_image::BayerPattern;

use metrics::{METRIC_WB, MeasuredRow, SiteSet, abs_log_errors, abs_log_errors_wb};

/// The four methods named by the issue, driven at the crate boundary.
#[derive(Clone, Copy)]
enum Method {
    Off,
    Clip,
    LocalRatios,
    Opposed,
}

const METHODS: [Method; 4] = [
    Method::Off,
    Method::Clip,
    Method::LocalRatios,
    Method::Opposed,
];

fn detection_levels(sat: f32) -> ChannelDetectionLevels {
    ChannelDetectionLevels {
        red: sat,
        green: sat,
        blue: sat,
    }
}

fn execution(method: Method, sat: f32) -> HighlightExecution {
    let detection = detection_levels(sat);
    match method {
        Method::Off => HighlightExecution::Off,
        // Clip ceilings == sat: on an already-clamped mosaic Clip is a
        // bit-exact no-op (only strictly-above is lowered; pinned by
        // crates/highlight/tests/clip.rs:72-92).
        Method::Clip => HighlightExecution::Clip(ChannelClipLevels {
            red: sat,
            green: sat,
            blue: sat,
        }),
        Method::LocalRatios => {
            HighlightExecution::LocalRatios(rohditor_highlight::LocalRatioOptions {
                detection_levels: detection,
            })
        }
        Method::Opposed => HighlightExecution::Opposed(rohditor_highlight::OpposedOptions {
            detection_levels: detection,
        }),
    }
}

/// One method run: output mosaic + the stats worth threading through.
struct RunResult {
    mosaic: rohditor_image::MosaicImage<f32>,
    changed_sites: usize,
    /// `stats.reconstructed_sites` (estimates *produced*); `None` for
    /// Off/Clip, which produce none.
    recon: Option<usize>,
    /// Stats-reported suspected count for comparison against the
    /// mask-from-bits set; `None` for Off.
    stats_suspected: Option<usize>,
    /// `suspected_by_channel`, LocalRatios/Opposed only.
    by_channel: Option<[usize; 3]>,
}

fn run(method: Method, sat: f32, input: &rohditor_image::MosaicImage<f32>) -> RunResult {
    let output = execution(method, sat)
        .apply_cancellable(input.clone(), &|| false)
        .expect("harness mosaic and levels are valid");
    match output {
        HighlightExecutionOutput::Off(mosaic) => RunResult {
            mosaic,
            changed_sites: 0,
            recon: None,
            stats_suspected: None,
            by_channel: None,
        },
        HighlightExecutionOutput::Clip(output) => RunResult {
            mosaic: output.mosaic,
            changed_sites: output.stats.changed_sites,
            recon: None,
            stats_suspected: Some(output.stats.affected_sites),
            by_channel: None,
        },
        HighlightExecutionOutput::LocalRatios(output) => RunResult {
            mosaic: output.mosaic,
            changed_sites: output.stats.changed_sites,
            recon: Some(output.stats.reconstructed_sites),
            stats_suspected: Some(output.stats.suspected_clipped_sites),
            by_channel: Some(output.stats.suspected_by_channel),
        },
        HighlightExecutionOutput::Opposed(output) => RunResult {
            mosaic: output.mosaic,
            changed_sites: output.stats.changed_sites,
            recon: Some(output.stats.reconstructed_sites),
            stats_suspected: Some(output.stats.suspected_clipped_sites),
            by_channel: Some(output.stats.suspected_by_channel),
        },
    }
}

/// Suspected sites grouped by Bayer channel for reporting.
fn suspected_by_channel(mask: &[bool], width: usize) -> [usize; 3] {
    let mut counts = [0usize; 3];
    for (index, &hit) in mask.iter().enumerate() {
        if hit {
            let (x, y) = (index % width, index / width);
            counts[BayerPattern::Rggb.color_at(x, y).channel_index()] += 1;
        }
    }
    counts
}

/// Guards run for every (scene, sat) pair BEFORE any baseline is captured:
/// a guard-failing scene has an ill-defined expected mask.
#[test]
fn scene_guards() {
    for spec in scenes::ALL {
        for sat in scenes::SAT_LEVELS {
            let scene = generator::generate(&spec, sat);
            // Guard A, exact-sat check first for an actionable message.
            let exact = generator::exact_sat_violations(&scene, &spec, sat);
            assert!(
                exact.is_empty(),
                "{}@{}: truth lands exactly on sat at {exact:?}",
                spec.name,
                sat
            );
            let close = generator::margin_violations(&scene, &spec, sat);
            assert!(
                close.is_empty(),
                "{}@{}: truth within margin {} of sat at {close:?}",
                spec.name,
                sat,
                generator::MARGIN
            );
            // Design rules (see `scenes`) on the scaled truth.
            let maxima = generator::channel_maxima(&scene);
            for (channel, &declared) in spec.clips.iter().enumerate() {
                if declared {
                    assert!(
                        maxima[channel] >= 1.2,
                        "{}: channel {channel} must clip but peaks at only {}",
                        spec.name,
                        maxima[channel]
                    );
                } else {
                    assert!(
                        maxima[channel] <= 0.90,
                        "{}: undeclared channel {channel} reaches {} (> 0.90)",
                        spec.name,
                        maxima[channel]
                    );
                }
            }
            // Guard B: expected set derived from the generated bits with the
            // crate's own predicate, never from analytic geometry.
            let mask = detect_local_ratios(&scene.mosaic, detection_levels(sat))
                .expect("harness mosaic and levels are valid");
            let bit_count = scene
                .mosaic
                .data()
                .iter()
                .filter(|&&sample| sample >= sat)
                .count();
            assert_eq!(
                mask.data().iter().filter(|&&hit| hit).count(),
                bit_count,
                "{}@{}: mask disagrees with mosaic bits",
                spec.name,
                sat
            );
            let per_channel = suspected_by_channel(mask.data(), spec.width);
            for (channel, &declared) in spec.clips.iter().enumerate() {
                if declared {
                    assert!(
                        per_channel[channel] >= 1,
                        "{}@{}: declared-clip channel {channel} has no suspected site",
                        spec.name,
                        sat
                    );
                } else {
                    assert_eq!(
                        per_channel[channel], 0,
                        "{}@{}: undeclared channel {channel} clips",
                        spec.name, sat
                    );
                }
            }
        }
    }
}

/// The #62 margin trap: a mosaic clamped at 0.98 detected at level 1.0
/// yields zero suspected sites. Documents why sat and level are swept
/// together in the matrix.
#[test]
fn margin_negative_control() {
    let scene = generator::generate(&scenes::DISK_LARGE, 0.98);
    let mask = detect_local_ratios(&scene.mosaic, detection_levels(1.0))
        .expect("harness mosaic and levels are valid");
    assert_eq!(
        mask.data().iter().filter(|&&hit| hit).count(),
        0,
        "green blown at 0.98 must give 0 suspected sites at level 1.0"
    );
}

/// Formats a ceiling as a Rust literal guaranteed >= `1.5 * measured`:
/// round the 1.5x value up to 6 decimals.
fn ceiling_literal(measured: f64) -> String {
    format!("{:.6}", (measured * 1.5 * 1e6).ceil() / 1e6)
}

/// One pasteable `BaselineRow` literal (plus its policy comment) for the
/// baseline capture workflow.
fn render_baseline_row(row: &MeasuredRow) -> String {
    let bound = metrics::raised_policy(row.scene, row.method, row.raised);
    let bound_text = match bound {
        metrics::Bound::Exact(value) => format!("Exact({value})"),
        metrics::Bound::AtLeast(value) => format!("AtLeast({value})"),
        metrics::Bound::AtMost(value) => format!("AtMost({value})"),
    };
    let mut out = String::new();
    if row.scene == "disk_small" || row.scene == "disk_large" {
        if !matches!(bound, metrics::Bound::Exact(_)) {
            out.push_str("    // Known failure #62: estimates produced (recon >> raised) are never applied\n");
            out.push_str("    // on large single-channel regions. When #62 lands this row flips to AtLeast\n");
            out.push_str("    // — that flip IS the progress signal.\n");
        }
    } else if row.scene == "speculars" {
        if !matches!(bound, metrics::Bound::Exact(_)) {
            out.push_str(
                "    // Known #62 shape: estimates produced, none applied on isolated tiny\n",
            );
            out.push_str("    // speculars; flip to AtLeast when methods act here.\n");
        }
    } else if matches!(bound, metrics::Bound::AtMost(_)) {
        out.push_str(
            "    // raised below the acting threshold (5) at baseline; flip to AtLeast when\n",
        );
        out.push_str("    // methods act here.\n");
    }
    // 2026-11-04 review fix: an AtMost ceiling that tracks a non-zero
    // measured estimator count carries a dated slack note (the policy's
    // +2), so a capture run stays paste-verbatim.
    if matches!(bound, metrics::Bound::AtMost(_)) && row.raised > 0 {
        out.push_str(&format!(
            "    // 2026-11-04: measured {} + slack 2 for estimator-bit drift across codegen\n",
            row.raised
        ));
    }
    out.push_str(&format!(
        "    BaselineRow {{ scene: {:?}, sat: {:.2}, method: {:?}, suspected: {}, raised: {}, \
         med_abs_logerr_max: {}, p95_abs_logerr_max: {}, hue_max_at_most: 3.0 }},\n",
        row.scene,
        row.sat,
        row.method,
        row.suspected,
        bound_text,
        ceiling_literal(row.med),
        ceiling_literal(row.p95),
    ));
    out
}

/// The 56-run matrix: prints the human table (14 sections of 4 rows), the
/// pasteable baseline block, and elapsed time, then asserts invariants and
/// pinned baselines for every run.
#[test]
fn ground_truth_matrix() {
    // Framing: recon and raised MUST read apart.
    println!(
        "recon = estimates produced (stats) · raised = pixels actually changed (data-driven, out > in)"
    );
    println!("(hue_max / hue_med printed; only hue_max <= row ceiling 3.0 rad is asserted)");
    let start = Instant::now();
    let mut measured_rows: Vec<MeasuredRow> = Vec::new();
    for spec in scenes::ALL {
        for sat in scenes::SAT_LEVELS {
            let scene = generator::generate(&spec, sat);
            let mask = detect_local_ratios(&scene.mosaic, detection_levels(sat))
                .expect("harness mosaic and levels are valid");
            let suspected = mask.data();
            let per_channel = suspected_by_channel(suspected, spec.width);
            println!(
                "\n=== scene {} @ sat {:.2} — suspected {} (R{} G{} B{}) ===",
                spec.name,
                sat,
                per_channel[0] + per_channel[1] + per_channel[2],
                per_channel[0],
                per_channel[1],
                per_channel[2]
            );
            if suspected.iter().filter(|&&hit| hit).count() == 0 {
                println!("no suspected sites at this level; matrix skips the section");
                continue;
            }
            println!(
                "  {:<12} {:>6} {:>6} {:>7} {:>7} {:>9} {:>9} {:>9} {:>9}",
                "method",
                "sus",
                "recon",
                "raised",
                "frac",
                "med|ln|",
                "p95|ln|",
                "hue_max",
                "hue_med"
            );
            for method in METHODS {
                let method_name = execution(method, sat).stable_name();
                let out = run(method, sat, &scene.mosaic);
                let sites = SiteSet {
                    width: spec.width,
                    truth: &scene.truth,
                    input: scene.mosaic.data(),
                    output: out.mosaic.data(),
                    suspected,
                };
                // Invariants, asserted for every run.
                if let Some((index, before, after)) = metrics::site_below_input(&sites) {
                    panic!(
                        "{}@{} {}: site {index} dropped below its clipped input ({before} -> {after})",
                        spec.name, sat, method_name
                    );
                }
                let raised = metrics::raised_count(&sites);
                assert_eq!(
                    raised, out.changed_sites,
                    "{}@{} {}: raised != stats.changed_sites",
                    spec.name, sat, method_name
                );
                if let Some(stats_suspected) = out.stats_suspected {
                    assert_eq!(
                        stats_suspected,
                        suspected.iter().filter(|&&hit| hit).count(),
                        "{}@{} {}: stats suspected != mask-from-bits",
                        spec.name,
                        sat,
                        method_name
                    );
                }
                if let Some(by_channel) = out.by_channel {
                    assert_eq!(
                        by_channel[0] + by_channel[1] + by_channel[2],
                        suspected.iter().filter(|&&hit| hit).count(),
                        "{}@{} {}: suspected_by_channel sums != mask count",
                        spec.name,
                        sat,
                        method_name
                    );
                }
                if matches!(method, Method::Off) {
                    assert_eq!(
                        out.mosaic.data(),
                        scene.mosaic.data(),
                        "{}@{} off: output must be bit-identical to input",
                        spec.name,
                        sat
                    );
                }
                // Log-ratio error over ALL suspected sites.
                let (med, p95) = metrics::summarize(abs_log_errors(&sites))
                    .expect("skipped sections guarantee non-empty");
                // WB-invariance self-check: per-channel ln is WB-invariant;
                // the re-run must agree to within f32 ulps.
                let (wb_med, wb_p95) = metrics::summarize(abs_log_errors_wb(&sites, METRIC_WB))
                    .expect("skipped sections guarantee non-empty");
                assert!(
                    (wb_med - med).abs() + (wb_p95 - p95).abs() < 1e-5,
                    "{}@{} {}: WB-invariance violated: med/p95 moved {} + {}",
                    spec.name,
                    sat,
                    method_name,
                    (wb_med - med).abs(),
                    (wb_p95 - p95).abs()
                );
                // Raised-only variant, printed but not baselined.
                let raised_summary = metrics::summarize(metrics::raised_abs_log_errors(&sites));
                // Hue after fixed WB; empty sets print '—' and satisfy the
                // bound without any assertion (a known residual risk).
                let mut hue = metrics::hue_errors(&sites);
                let hue_text = if hue.is_empty() {
                    None
                } else {
                    hue.sort_by(f64::total_cmp);
                    Some((hue[hue.len() - 1], metrics::median_sorted(&hue)))
                };
                let raised_text = raised_summary.map_or_else(
                    || "—".to_owned(),
                    |(rmed, rp95)| format!("{rmed:.4}/{rp95:.4}"),
                );
                let (hue_max_text, hue_med_text, hue_max) = hue_text.map_or(
                    ("—".to_owned(), "—".to_owned(), None),
                    |(hmax, hmed)| (format!("{hmax:.4}"), format!("{hmed:.4}"), Some(hmax)),
                );
                println!(
                    "  {:<12} {:>6} {:>6} {:>7} {:>7.3} {:>9.4} {:>9.4} {:>9} {:>9}  raised med/p95: {}",
                    method_name,
                    suspected.iter().filter(|&&hit| hit).count(),
                    out.recon.map_or("—".to_owned(), |v| v.to_string()),
                    raised,
                    raised as f64 / suspected.iter().filter(|&&hit| hit).count() as f64,
                    med,
                    p95,
                    hue_max_text,
                    hue_med_text,
                    raised_text,
                );
                measured_rows.push(MeasuredRow {
                    scene: spec.name,
                    sat,
                    method: method_name,
                    suspected: suspected.iter().filter(|&&hit| hit).count(),
                    raised,
                    med,
                    p95,
                    hue_max,
                });
            }
        }
    }
    let elapsed = start.elapsed();
    println!("\n// ---- BASELINE_ROWS (paste into metrics::BASELINES) ----");
    for row in &measured_rows {
        print!("{}", render_baseline_row(row));
    }
    println!("// ---- END BASELINE_ROWS ----");
    // Timing is evidence, never an assertion: the goal of finishing the
    // matrix "under a second" must stay comfortably above this number.
    println!(
        "matrix elapsed: {:.1} ms (print-only)",
        elapsed.as_secs_f64() * 1000.0
    );
    // Pinned baselines, checked after all printing so a capture run still
    // emits the full table.
    for row in &measured_rows {
        let baseline =
            metrics::find_baseline(row.scene, row.sat, row.method).unwrap_or_else(|| {
                panic!(
                    "no baseline row pinned for {}@{} {}",
                    row.scene, row.sat, row.method
                )
            });
        metrics::check_baseline(baseline, row);
    }
    println!(
        "all {} runs match their pinned baselines",
        measured_rows.len()
    );
    // 2026-11-04 review fix: plain pointer for table readers tying the
    // AtMost ceilings on disk/speculars rows to the known failure.
    println!(
        "note: AtMost ceilings on the disk and speculars rows track known #62 (estimates \
         produced, none applied) — see the BASELINES comment blocks in metrics.rs"
    );
}
