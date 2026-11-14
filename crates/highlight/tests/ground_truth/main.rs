//! Ground-truth reconstruction harness for the highlight crate (issue #63).
//!
//! Generates synthetic scenes with a known unclipped truth, mosaics + clamps
//! them, runs Off / Clip / LocalRatios / Opposed at the crate boundary, and
//! scores reconstruction quality. Three tests:
//! - `scene_guards`: exact-sat margin + mask-from-bits guards, no method
//!   runs — fast.
//! - `margin_negative_control`: the #62 margin trap as a one-liner.
//! - `ground_truth_matrix`: 8 scenes x 2 sat levels x 4 methods = 64 runs,
//!   prints the human table, the pasteable baseline literal block, and the
//!   elapsed time (timing is printed, never asserted), then checks every run
//!   against the pinned `baseline::BASELINES` rows and fails once with the
//!   full violation list.
//!
//! Levels are pinned at the crate boundary (`{sat} x 3` for detection)
//! instead of going through `rohditor_core` resolution, which decouples
//! this harness from pending #64. Since issue #65 the Clip rows run with
//! production-shaped WB-aware ceilings (`sat * min(METRIC_WB) / gain`,
//! the METRIC_WB vector standing in for as-shot balance) and the
//! LocalRatios/Opposed rows receive the same vector as `neutral_ceiling`,
//! so clause A/B of #65 compare like-for-like. Off rows are unchanged.

mod baseline;
mod generator;
mod metrics;
mod neutral_fallback;
mod region_opposed;
mod scenes;

use std::time::Instant;

use rohditor_highlight::{
    ChannelClipLevels, ChannelDetectionLevels, HighlightExecution, HighlightExecutionOutput,
    detect_local_ratios,
};
use rohditor_image::BayerPattern;

use metrics::{METRIC_WB, MeasuredRow, SiteSet, abs_log_errors};

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

/// WB-aware per-channel ceilings used by BOTH the Clip rows (as Clip
/// limits) and the reconstruction rows (as `neutral_ceiling`):
/// `sat * min(METRIC_WB) / METRIC_WB_c`, mirroring production
/// `sensor.rs` Clip resolution with the metric vector standing in for
/// the as-shot balance (issue #65: the harness as-shot *is*
/// `METRIC_WB`, so "neutral under the ceilings" and the hue metric
/// agree). Computed ONCE per (scene, sat) section and shared, so the
/// Clip↔neutraliser bit-parity claim never depends on recomputation.
fn shared_ceilings(sat: f32) -> [f32; 3] {
    let min = METRIC_WB[0].min(METRIC_WB[1]).min(METRIC_WB[2]);
    METRIC_WB.map(|gain| sat * min / gain)
}

fn execution(
    method: Method,
    ceilings: [f32; 3],
    detection: ChannelDetectionLevels,
) -> HighlightExecution {
    match method {
        Method::Off => HighlightExecution::Off,
        // WB-aware Clip (issue #65): limits below sat on R and B, a
        // real Clip like production sensor.rs — no longer a bit-exact
        // no-op on the pre-clamped mosaic.
        Method::Clip => HighlightExecution::Clip(ChannelClipLevels {
            red: ceilings[0],
            green: ceilings[1],
            blue: ceilings[2],
        }),
        Method::LocalRatios => {
            HighlightExecution::LocalRatios(rohditor_highlight::LocalRatioOptions {
                detection_levels: detection,
                neutral_ceiling: Some(ChannelClipLevels {
                    red: ceilings[0],
                    green: ceilings[1],
                    blue: ceilings[2],
                }),
            })
        }
        Method::Opposed => HighlightExecution::Opposed(rohditor_highlight::OpposedOptions {
            detection_levels: detection,
            neutral_ceiling: Some(ChannelClipLevels {
                red: ceilings[0],
                green: ceilings[1],
                blue: ceilings[2],
            }),
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
    /// `stats.neutralised_sites` (issue #65); zero for Off/Clip, which
    /// have no neutraliser.
    neutralised_sites: usize,
}

fn run(
    method: Method,
    sat: f32,
    ceilings: [f32; 3],
    input: &rohditor_image::MosaicImage<f32>,
) -> RunResult {
    let detection = detection_levels(sat);
    let output = execution(method, ceilings, detection)
        .apply_cancellable(input.clone(), &|| false)
        .expect("harness mosaic and levels are valid");
    match output {
        HighlightExecutionOutput::Off(mosaic) => RunResult {
            mosaic,
            changed_sites: 0,
            recon: None,
            stats_suspected: None,
            by_channel: None,
            neutralised_sites: 0,
        },
        HighlightExecutionOutput::Clip(output) => RunResult {
            mosaic: output.mosaic,
            changed_sites: output.stats.changed_sites,
            recon: None,
            stats_suspected: Some(output.stats.affected_sites),
            by_channel: None,
            neutralised_sites: 0,
        },
        HighlightExecutionOutput::LocalRatios(output) => RunResult {
            mosaic: output.mosaic,
            changed_sites: output.stats.changed_sites,
            recon: Some(output.stats.reconstructed_sites),
            stats_suspected: Some(output.stats.suspected_clipped_sites),
            by_channel: Some(output.stats.suspected_by_channel),
            neutralised_sites: output.stats.neutralised_sites,
        },
        HighlightExecutionOutput::Opposed(output) => RunResult {
            mosaic: output.mosaic,
            changed_sites: output.stats.changed_sites,
            recon: Some(output.stats.reconstructed_sites),
            stats_suspected: Some(output.stats.suspected_clipped_sites),
            by_channel: Some(output.stats.suspected_by_channel),
            neutralised_sites: output.stats.neutralised_sites,
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

/// The 64-run matrix: prints the human table (16 sections of 4 rows), the
/// pasteable baseline block, and elapsed time, then asserts invariants and
/// pinned baselines for every run.
#[test]
fn ground_truth_matrix() {
    // Framing: recon and raised MUST read apart.
    println!(
        "recon = estimates produced (stats) · raised = pixels actually changed (data-driven, out > in)"
    );
    println!("(hue_max / hue_med printed; only hue_max <= the row's 1.5x ceiling is asserted)");
    let start = Instant::now();
    let mut measured_rows: Vec<MeasuredRow> = Vec::new();
    for spec in scenes::ALL {
        for sat in scenes::SAT_LEVELS {
            let scene = generator::generate(&spec, sat);
            let ceilings = shared_ceilings(sat);
            let mask = detect_local_ratios(&scene.mosaic, detection_levels(sat))
                .expect("harness mosaic and levels are valid");
            let suspected = mask.data();
            let per_channel = suspected_by_channel(suspected, spec.width);
            let suspected_total = per_channel[0] + per_channel[1] + per_channel[2];
            println!(
                "\n=== scene {} @ sat {:.2} — suspected {} (R{} G{} B{}) ===",
                spec.name, sat, suspected_total, per_channel[0], per_channel[1], per_channel[2]
            );
            if suspected_total == 0 {
                println!("no suspected sites at this level; matrix skips the section");
                continue;
            }
            // Clip's own affected set derived from the input bits: with
            // WB-aware limits it catches below-sat falloff content too, so
            // the mask-from-bits expectation is replaced for Clip rows.
            let clip_allowed: Vec<bool> = scene
                .mosaic
                .data()
                .iter()
                .enumerate()
                .map(|(index, sample)| {
                    let (x, y) = (index % spec.width, index / spec.width);
                    let channel = BayerPattern::Rggb.color_at(x, y).channel_index();
                    *sample >= ceilings[channel]
                })
                .collect();
            println!(
                "  {:<12} {:>6} {:>6} {:>7} {:>8} {:>11} {:>7} {:>9} {:>9} {:>9} {:>9}",
                "method",
                "sus",
                "recon",
                "raised",
                "lowered",
                "neutralised",
                "frac",
                "med|ln|",
                "p95|ln|",
                "hue_max",
                "hue_med",
            );
            for method in METHODS {
                let method_name = execution(method, ceilings, detection_levels(sat)).stable_name();
                let out = run(method, sat, ceilings, &scene.mosaic);
                let sites = SiteSet {
                    width: spec.width,
                    truth: &scene.truth,
                    input: scene.mosaic.data(),
                    output: out.mosaic.data(),
                    suspected,
                };
                // Invariants, asserted for every run — per-method matrix
                // from the #65 contract (the pre-#65 "no method ever
                // lowers" asserts encoded exactly what #65 overturns).
                let floors = if matches!(method, Method::Off) {
                    [sat; 3]
                } else {
                    ceilings
                };
                if let Some((index, before, after)) = metrics::below_floor(&sites, floors) {
                    panic!(
                        "{}@{} {}: site {index} dropped below min(input, ceiling) ({before} -> {after})",
                        spec.name, sat, method_name
                    );
                }
                let raised = metrics::raised_count(&sites);
                let lowered = metrics::lowered_count(&sites);
                match method {
                    Method::Off => {
                        assert_eq!(lowered, 0, "{}@{} off: nothing may lower", spec.name, sat);
                        assert_eq!(
                            out.mosaic.data(),
                            scene.mosaic.data(),
                            "{}@{} off: output must be bit-identical to input",
                            spec.name,
                            sat
                        );
                    }
                    Method::Clip => {
                        // Clip's changed_sites counts LOWERED samples.
                        assert_eq!(
                            lowered, out.changed_sites,
                            "{}@{} clip: lowered != stats.changed_sites",
                            spec.name, sat
                        );
                        assert_eq!(
                            out.stats_suspected.expect("clip reports affected_sites"),
                            metrics::affected_by_limits(&sites, ceilings),
                            "{}@{} clip: affected_sites != input>=limit count",
                            spec.name,
                            sat
                        );
                        if let Some(index) = metrics::moves_outside(&sites, &clip_allowed) {
                            panic!("{}@{} clip: unsuspected site {index} moved", spec.name, sat);
                        }
                        if let Some((index, before, after)) =
                            metrics::clip_landing_violations(&sites, ceilings)
                        {
                            panic!(
                                "{}@{} clip: site {index} above its limit did not land exactly on it ({before} -> {after})",
                                spec.name, sat
                            );
                        }
                    }
                    Method::LocalRatios | Method::Opposed => {
                        assert_eq!(
                            raised, out.changed_sites,
                            "{}@{} {}: raised != stats.changed_sites",
                            spec.name, sat, method_name
                        );
                        // Neutralisation is the only lowering pass here
                        // Backstop: the neutralisation pass must be asserted, never print-only.
                        assert_eq!(
                            lowered, out.neutralised_sites,
                            "{}@{} {}: neutralised != stats.neutralised_sites",
                            spec.name, sat, method_name
                        );
                        assert_eq!(
                            out.stats_suspected.expect("recon reports suspected"),
                            suspected_total,
                            "{}@{} {}: stats suspected != mask-from-bits",
                            spec.name,
                            sat,
                            method_name
                        );
                        if let Some(by_channel) = out.by_channel {
                            assert_eq!(
                                by_channel[0] + by_channel[1] + by_channel[2],
                                suspected_total,
                                "{}@{} {}: suspected_by_channel sums != mask count",
                                spec.name,
                                sat,
                                method_name
                            );
                        }
                        if let Some(index) = metrics::moves_outside(&sites, suspected) {
                            panic!(
                                "{}@{} {}: unsuspected site {index} moved",
                                spec.name, sat, method_name
                            );
                        }
                        if let Some((index, before, after)) =
                            metrics::neutral_landing_violations(&sites, ceilings)
                        {
                            panic!(
                                "{}@{} {}: fallback site {index} above the ceiling did not land exactly on it ({before} -> {after})",
                                spec.name, sat, method_name
                            );
                        }
                    }
                }
                // Log-ratio error over ALL suspected sites.
                let (med, p95) = metrics::summarize(abs_log_errors(&sites))
                    .expect("skipped sections guarantee non-empty");
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
                let (hue_max_text, hue_med_text) = hue_text
                    .map_or(("—".to_owned(), "—".to_owned()), |(hmax, hmed)| {
                        (format!("{hmax:.4}"), format!("{hmed:.4}"))
                    });
                println!(
                    "  {:<12} {:>6} {:>6} {:>7} {:>8} {:>11} {:>7.3} {:>9.4} {:>9.4} {:>9} {:>9}  raised med/p95: {}",
                    method_name,
                    suspected_total,
                    out.recon.map_or("—".to_owned(), |v| v.to_string()),
                    raised,
                    lowered,
                    out.neutralised_sites,
                    raised as f64 / suspected_total as f64,
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
                    suspected: suspected_total,
                    raised,
                    lowered,
                    neutralised: out.neutralised_sites,
                    med,
                    p95,
                    hue_max: hue_text.map(|(hmax, _)| hmax),
                });
            }
        }
    }
    let elapsed = start.elapsed();
    println!("\n// ---- BASELINE_ROWS (paste into baseline::BASELINES) ----");
    for row in &measured_rows {
        print!("{}", baseline::render_baseline_row(row));
    }
    println!("// ---- END BASELINE_ROWS ----");
    // Timing is evidence, never an assertion: the goal of finishing the
    // matrix "under a second" must stay comfortably above this number.
    println!(
        "matrix elapsed: {:.1} ms (print-only)",
        elapsed.as_secs_f64() * 1000.0
    );
    // Pinned baselines, checked after all printing so a capture run still
    // emits the full table. Every row is checked even after a violation so
    // one run reports the complete picture.
    let mut violations: Vec<String> = Vec::new();
    for row in &measured_rows {
        match baseline::find_baseline(row.scene, row.sat, row.method) {
            Some(pinned) => violations.extend(baseline::check_baseline(pinned, row)),
            None => violations.push(format!(
                "{}@{} {}: no baseline row pinned",
                row.scene, row.sat, row.method
            )),
        }
    }
    if !violations.is_empty() {
        panic!(
            "{} baseline violation(s):\n{}",
            violations.len(),
            violations.join("\n")
        );
    }
    println!(
        "all {} runs match their pinned baselines",
        measured_rows.len()
    );
    println!(
        "note: AtMost ceilings on the disk and speculars rows track known #62 (estimates \
         produced, none applied) — see the BASELINES comment blocks in baseline.rs"
    );
    println!(
        "note: 2026-11-09 — WB-aware Clip + neutral fallback (#65) landed. The raised \
         ceilings stay valid for RAISED only; the lowering-side signal is asserted \
         per run (lowered==clip changed, neutralised==stats) and floored in the \
         lowered/neutralised baseline columns."
    );
}
