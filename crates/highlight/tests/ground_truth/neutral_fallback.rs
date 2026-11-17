//! Cross-method neutral-fallback test for issue #65 (Done-when clauses A+B).
//!
//! Clause A ("Opposed's max post-WB hue error in fully blown cores matches
//! Clip's"): per (scene, sat) containing fully-blown 2x2 cells (R, G and B
//! sites all suspected), the maximum hue error over the core sites must
//! match the WB-aware Clip row TWO-SIDED: `|Δ hue_max| <= HUE_MATCH_TOL`
//! (a one-sided bound cannot fail — pre-#65 raw magenta cores sit *closer*
//! to the warm scene truth than the neutral landing: measured Δ ≈ −0.26).
//! Same form for LocalRatios.
//!
//! Clause B ("no scene renders more coloured than Clip"): the
//! load-bearing falsifier is the DIRECT chroma comparison over the
//! suspected, not-raised sites (the neutralisation set):
//! `max(chroma_method − chroma_clip) <= CHROMA_TOL`. Hue-vs-truth
//! conflates neutral with accurate, so the two-sided hue Δ is kept as a
//! print-only diagnostic while this chroma bound is asserted. No truth
//! chroma floor here: chroma is defined on grey truth, and with the
//! floor a grey blown-sky scene would silently skip the falsifier.
//!
//! An empty fallback set must be ANNOUNCED, not silently true: every
//! section prints its `|fallback|` / `|chroma-checked|` counts per method,
//! and the `sun_core_halo` rows additionally assert a non-empty
//! chroma-checked guard.

use rohditor_image::BayerPattern;

use super::{Method, detection_levels, generator, metrics, run, scenes, shared_ceilings};

/// Two-sided hue-match tolerance in radians for clause A, capture-pinned
/// 2026-11-09. Correct implementations give Δ = 0.0000 at fully-blown
/// cells (both methods land bit-identically on the shared ceiling), so
/// 1e-3 is generous AND fail-able: pre-#65 measured |Δ| = 0.2600
/// (sat 1.00) / 0.2511 (sat 0.98) on `sun_core_halo`.
const HUE_MATCH_TOL: f64 = 1e-3;

/// Clause-B chroma tolerance, capture-pinned 2026-11-09. A correct #65 is
/// bit-identical to Clip at fully-blown sites, but partial-blown fallback
/// content is only *bounded*, not pinned, so this stays a tolerance.
/// The pre-#65 discriminating margins that make it fail-able (measured on
/// the unmodified methods, same assertion code): two_channel 0.2242 /
/// 0.2165, sun_core_halo 0.2341 / 0.2339 (LR@0.98: 0.0670 — LR raises
/// more of the set there), warm_cool 0.2554 / 0.2556, speculars 0.2445 /
/// 0.2440, worst overall 0.2556 — roughly 200x the tolerance, so the
/// pinned value is neither flip-able by drift nor vacuously tight.
const CHROMA_TOL: f64 = 1e-3;

/// Fully-blown core sites: every one of the four sites of a 2x2 cell is
/// suspected (`clips: [true, true, true]` cells of the scene set). In
/// such a cell neither reconstruction method has clean opposing support,
/// so both take the fallback branch and land bit-identically on the
/// shared ceiling — the strongest parity set for clause A.
fn core_sites(suspected: &[bool], width: usize, height: usize) -> Vec<usize> {
    let mut core = Vec::new();
    for cell_y in 0..height / 2 {
        for cell_x in 0..width / 2 {
            let sites = [
                (cell_y * 2) * width + cell_x * 2,
                (cell_y * 2) * width + cell_x * 2 + 1,
                (cell_y * 2 + 1) * width + cell_x * 2,
                (cell_y * 2 + 1) * width + cell_x * 2 + 1,
            ];
            if sites
                .iter()
                .all(|&index| index < suspected.len() && suspected[index])
            {
                core.extend(sites);
            }
        }
    }
    core
}

/// Max hue error over an explicit site mask (existing `hue_errors`
/// convention: sampled channel from the method's output, the other two
/// from truth, chroma floor from truth).
fn hue_max_over(sites_base: &metrics::SiteSet<'_>, mask: &[bool], output: &[f32]) -> Option<f64> {
    let subset = metrics::SiteSet {
        width: sites_base.width,
        truth: sites_base.truth,
        input: sites_base.input,
        output,
        suspected: mask,
    };
    let mut errors = metrics::hue_errors(&subset);
    if errors.is_empty() {
        return None;
    }
    errors.sort_by(f64::total_cmp);
    Some(errors[errors.len() - 1])
}

#[test]
fn neutral_fallback_matches_clip() {
    for spec in scenes::ALL {
        for sat in scenes::SAT_LEVELS {
            let scene = generator::generate(&spec, sat);
            let width = spec.width;
            let mask = suspected_mask(&scene, sat);
            if mask.iter().filter(|&&hit| hit).count() == 0 {
                continue;
            }
            let ceilings = shared_ceilings(sat);
            let clip = run(Method::Clip, sat, ceilings, &scene.mosaic);
            let lr = run(Method::LocalRatios, sat, ceilings, &scene.mosaic);
            let opposed = run(Method::Opposed, sat, ceilings, &scene.mosaic);
            println!("\n--- #65 section {} @ sat {:.2} ---", spec.name, sat);

            // ---- Clause A: two-sided core-site hue match vs Clip. ----
            let core = core_sites(&mask, width, spec.height);
            if spec.name == "sun_core_halo" {
                assert!(
                    !core.is_empty(),
                    "sun_core_halo@{sat:.2}: fully-blown core guard — expected >= 1 core site"
                );
            }
            if core.is_empty() {
                println!("  core sites: 0 (clause A trivially satisfied here)");
            } else {
                let core_mask: Vec<bool> = {
                    let mut marks = vec![false; mask.len()];
                    for &index in &core {
                        marks[index] = true;
                    }
                    marks
                };
                let base = metrics::SiteSet {
                    width,
                    truth: &scene.truth,
                    input: scene.mosaic.data(),
                    output: clip.mosaic.data(),
                    suspected: &core_mask,
                };
                let clip_max = hue_max_over(&base, &core_mask, clip.mosaic.data());
                for (name, out) in [
                    ("local_ratios", lr.mosaic.data()),
                    ("opposed", opposed.mosaic.data()),
                ] {
                    let method_max = hue_max_over(&base, &core_mask, out);
                    match (clip_max, method_max) {
                        (Some(clip_hue), Some(method_hue)) => {
                            let delta = (method_hue - clip_hue).abs();
                            println!(
                                "  core sites {}: hue_max {name} {method_hue:.4} vs clip {clip_hue:.4} |Δ| {delta:.4}",
                                core.len()
                            );
                            assert!(
                                delta <= HUE_MATCH_TOL,
                                "{}@{:.2} {name}: |hue_max − clip hue_max| = {delta:.4} exceeds HUE_MATCH_TOL {HUE_MATCH_TOL}",
                                spec.name,
                                sat
                            );
                        }
                        (Some(_), None) => println!(
                            "  core sites {}: {name} hue set empty over cores (satisfied by the existing empty-set convention)",
                            core.len()
                        ),
                        _ => println!(
                            "  core sites {}: clip hue set empty; nothing to match against",
                            core.len()
                        ),
                    }
                }
            }

            // ---- Clause B: direct chroma parity on the fallback set. ----
            for (name, out) in [
                ("local_ratios", lr.mosaic.data()),
                ("opposed", opposed.mosaic.data()),
            ] {
                let mut fallback = 0;
                let mut checked = 0;
                let mut max_delta = 0.0_f64;
                let mut neutralised_sites = 0;
                let mut raised_sites = 0;
                for (index, &suspected_site) in mask.iter().enumerate() {
                    if !suspected_site {
                        continue;
                    }
                    let before = scene.mosaic.data()[index];
                    let after = out[index];
                    if after > before {
                        raised_sites += 1;
                        continue; // raised sites: estimator's domain (v1)
                    }
                    fallback += 1;
                    if after < before {
                        neutralised_sites += 1;
                    }
                    let (x, y) = (index % width, index / width);
                    let channel = BayerPattern::Rggb.color_at(x, y).channel_index();
                    let Some(method_chroma) =
                        metrics::site_chroma(&scene.truth[index], channel, after)
                    else {
                        continue; // sum <= 1e-3: nothing measurable
                    };
                    let clip_chroma = metrics::site_chroma(
                        &scene.truth[index],
                        channel,
                        clip.mosaic.data()[index],
                    )
                    .expect("fallback site is bright; the clip triple sum matches");
                    max_delta = max_delta.max(method_chroma - clip_chroma);
                    checked += 1;
                }
                println!(
                    "  {name}: |fallback| {fallback} · |chroma-checked| {checked} · neutralised {neutralised_sites} · raised {raised_sites} · max(chroma−chroma_clip) {max_delta:.6} (TOL {CHROMA_TOL})"
                );
                if spec.name == "sun_core_halo" {
                    assert!(
                        checked >= 1,
                        "sun_core_halo@{sat:.2} {name}: empty chroma-checked fallback set — clause B would be silently true"
                    );
                }
                assert!(
                    max_delta <= CHROMA_TOL,
                    "{}@{:.2} {name}: fallback chroma exceeds clip chroma by {max_delta:.6} (> CHROMA_TOL {CHROMA_TOL})",
                    spec.name,
                    sat
                );

                // ---- boundary step (print-only, feeds the #65
                // follow-up decision on reconstructed-vs-neutral smoothing). ----
                let mut boundary = BoundaryScan::default();
                scan_boundary_steps(
                    &mask,
                    scene.mosaic.data(),
                    out,
                    width,
                    spec.height,
                    &mut boundary,
                );
                println!(
                    "  {name}: boundary step (neutralised↔raised across a cell border): {}",
                    boundary.max.map_or_else(
                        || "— (no adjacent pair)".to_owned(),
                        |step| format!("{step:.4}")
                    )
                );
            }
        }
    }
}

fn suspected_mask(scene: &generator::Generated, sat: f32) -> Vec<bool> {
    rohditor_highlight::detect_local_ratios(&scene.mosaic, detection_levels(sat))
        .expect("harness mosaic and levels are valid")
        .data()
        .to_vec()
}

/// Max |output − output| across a 2x2-cell border where one side is a
/// neutralised site (`suspected, out < in`) and the other a raised site
/// (`suspected, out > in`). Per-channel f32 value suffices.
#[derive(Default)]
struct BoundaryScan {
    max: Option<f64>,
}

fn scan_boundary_steps(
    mask: &[bool],
    input: &[f32],
    output: &[f32],
    width: usize,
    height: usize,
    scan: &mut BoundaryScan,
) {
    let kind = |index: usize| -> Option<bool> {
        if !mask[index] {
            return None;
        }
        if output[index] < input[index] {
            Some(true) // neutralised
        } else if output[index] > input[index] {
            Some(false) // raised
        } else {
            None
        }
    };
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            for (nx, ny) in [(x + 1, y), (x, y + 1)] {
                if nx >= width || ny >= height {
                    continue;
                }
                if x / 2 == nx / 2 && y / 2 == ny / 2 {
                    continue; // same cell: no #65 boundary
                }
                let neighbour = ny * width + nx;
                if let (Some(a), Some(b)) = (kind(index), kind(neighbour))
                    && a != b
                {
                    let step = (f64::from(output[index]) - f64::from(output[neighbour])).abs();
                    scan.max = Some(scan.max.map_or(step, |m: f64| m.max(step)));
                }
            }
        }
    }
}
