//! Acceptance tests for region-aware Opposed (issue #66), measured on the
//! ground-truth harness.
//!
//! - `soft_disk_is_reconstructed_from_its_border`: the large-region regime.
//!   An ~50 px blown highlight sits far beyond the ~12 px local search
//!   reach; the interior must be reconstructed from the region's border.
//! - `adjacent_objects_keep_their_own_hue`: two differently coloured
//!   clipped objects must not borrow each other's chroma.

use rohditor_image::BayerPattern;

use super::{Method, detection_levels, generator, metrics, run, scenes, shared_ceilings};

/// Share of suspected sites that must receive an estimate (the metric the
/// issue quotes: 16% before region-aware Opposed on the large disc).
const MIN_RECONSTRUCTED_FRACTION: f64 = 0.95;
/// Share of suspected sites that must end up strictly above their clipped
/// input. Lower than the estimate share because sites within about a pixel
/// of the clip boundary have a truth barely above the clip level, which an
/// unbiased estimate lands on either side of.
const MIN_RAISED_FRACTION: f64 = 0.85;
/// Median |error| in EV the issue asks for on partially clipped sites.
const MAX_MEDIAN_ERROR_EV: f64 = 0.1;

/// Metrics for one Opposed run, restricted to the sites in `mask`.
fn raised_and_errors(
    input: &[f32],
    output: &[f32],
    truth: &[[f32; 3]],
    width: usize,
    mask: &[bool],
) -> (usize, Vec<f64>) {
    let mut raised = 0;
    let mut errors_ev = Vec::new();
    for (index, &selected) in mask.iter().enumerate() {
        if !selected {
            continue;
        }
        let channel = BayerPattern::Rggb
            .color_at(index % width, index / width)
            .channel_index();
        raised += usize::from(output[index] > input[index]);
        let ratio = f64::from(output[index]) / f64::from(truth[index][channel]);
        errors_ev.push(ratio.log2().abs());
    }
    (raised, errors_ev)
}

#[test]
fn soft_disk_is_reconstructed_from_its_border() {
    let spec = scenes::DISK_SOFT;
    for sat in scenes::SAT_LEVELS {
        let scene = generator::generate(&spec, sat);
        let ceilings = shared_ceilings(sat);
        let suspected =
            rohditor_highlight::detect_local_ratios(&scene.mosaic, detection_levels(sat))
                .expect("harness mosaic and levels are valid");
        let mask = suspected.data();
        let total = mask.iter().filter(|&&hit| hit).count();

        let opposed = run(Method::Opposed, sat, ceilings, &scene.mosaic);
        let reconstructed = opposed.recon.expect("opposed reports reconstructed sites");
        let (raised, mut errors_ev) = raised_and_errors(
            scene.mosaic.data(),
            opposed.mosaic.data(),
            &scene.truth,
            spec.width,
            mask,
        );
        errors_ev.sort_by(f64::total_cmp);
        let median_ev = metrics::median_sorted(&errors_ev);
        let off = run(Method::Off, sat, ceilings, &scene.mosaic);
        let (_, mut off_errors_ev) = raised_and_errors(
            scene.mosaic.data(),
            off.mosaic.data(),
            &scene.truth,
            spec.width,
            mask,
        );
        off_errors_ev.sort_by(f64::total_cmp);
        let off_median_ev = metrics::median_sorted(&off_errors_ev);
        println!(
            "disk_soft@{sat}: suspected {total}, reconstructed {reconstructed} ({:.1}%), raised {raised} ({:.1}%), \
             median error {median_ev:.3} EV (unreconstructed {off_median_ev:.3} EV)",
            100.0 * reconstructed as f64 / total as f64,
            100.0 * raised as f64 / total as f64,
        );

        assert!(
            reconstructed as f64 >= MIN_RECONSTRUCTED_FRACTION * total as f64,
            "sat {sat}: only {reconstructed}/{total} sites reconstructed"
        );
        assert!(
            raised as f64 >= MIN_RAISED_FRACTION * total as f64,
            "sat {sat}: only {raised}/{total} sites raised above the clip level"
        );
        assert!(
            median_ev < MAX_MEDIAN_ERROR_EV,
            "sat {sat}: median error {median_ev} EV"
        );
        assert!(
            median_ev < off_median_ev,
            "sat {sat}: reconstruction must beat the clipped input"
        );
    }
}

#[test]
fn adjacent_objects_keep_their_own_hue() {
    let spec = scenes::WARM_COOL_ADJACENT;
    for sat in scenes::SAT_LEVELS {
        let scene = generator::generate(&spec, sat);
        let ceilings = shared_ceilings(sat);
        let suspected =
            rohditor_highlight::detect_local_ratios(&scene.mosaic, detection_levels(sat))
                .expect("harness mosaic and levels are valid");
        let opposed = run(Method::Opposed, sat, ceilings, &scene.mosaic);
        let clip = run(Method::Clip, sat, ceilings, &scene.mosaic);
        // The warm object clips R, the cool one clips B.
        for (name, channel) in [("warm", 0_usize), ("cool", 2)] {
            let mask: Vec<bool> = suspected
                .data()
                .iter()
                .enumerate()
                .map(|(index, &hit)| {
                    hit && BayerPattern::Rggb
                        .color_at(index % spec.width, index / spec.width)
                        .channel_index()
                        == channel
                })
                .collect();
            let hue_max = |output: &[f32]| {
                let sites = metrics::SiteSet {
                    width: spec.width,
                    truth: &scene.truth,
                    input: scene.mosaic.data(),
                    output,
                    suspected: &mask,
                };
                let mut errors = metrics::hue_errors(&sites);
                errors.sort_by(f64::total_cmp);
                errors.last().copied()
            };
            println!(
                "warm_cool@{sat} {name}: opposed hue_max {:?}, clip hue_max {:?}",
                hue_max(opposed.mosaic.data()),
                hue_max(clip.mosaic.data())
            );
        }
    }
}
