//! Region-aware Opposed (issue #66): clipped regions larger than the local
//! search, the image-wide fallback, isolation between touching objects,
//! determinism and cancellation across the new passes.

use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::ThreadPoolBuilder;
use rohditor_highlight::{
    ChannelDetectionLevels, HighlightError, OpposedOptions, local_ratio_scratch_bytes,
    opposed_scratch_bytes, reconstruct_opposed, reconstruct_opposed_cancellable,
};
use rohditor_image::{BayerPattern, MosaicImage};

const LEVEL: f32 = 0.9;
const BACKGROUND: [f32; 3] = [0.02; 3];

fn options() -> OpposedOptions {
    OpposedOptions {
        detection_levels: ChannelDetectionLevels {
            red: LEVEL,
            green: LEVEL,
            blue: LEVEL,
        },
        neutral_ceiling: None,
    }
}

/// RGGB mosaic whose 2x2 cell `(cx, cy)` holds the colour `colour(cx, cy)`
/// (both greens equal).
fn mosaic_from_cells(
    cells_wide: usize,
    cells_high: usize,
    colour: impl Fn(usize, usize) -> [f32; 3],
) -> MosaicImage<f32> {
    let (width, height) = (cells_wide * 2, cells_high * 2);
    let mut data = vec![0.0; width * height];
    for y in 0..height {
        for x in 0..width {
            let channel = BayerPattern::Rggb.color_at(x, y).channel_index();
            data[y * width + x] = colour(x / 2, y / 2)[channel];
        }
    }
    MosaicImage::new(width, height, width, BayerPattern::Rggb, data).expect("fixture")
}

/// Cube-root mean of two channel values, the Opposed reference.
fn reference(a: f32, b: f32) -> f32 {
    let root = (a.cbrt() + b.cbrt()) / 2.0;
    root * root * root
}

// A large blown-green square (cells 10..30) with a one-cell ring of
// near-clip shoulder around it.
const INTERIOR: [f32; 3] = [0.7, 0.95, 0.6];
const SHOULDER: [f32; 3] = [0.5, 0.8, 0.4];

fn large_region_colour(cx: usize, cy: usize) -> [f32; 3] {
    let inside = |value: usize| (10..30).contains(&value);
    let near = |value: usize| (9..31).contains(&value);
    if inside(cx) && inside(cy) {
        INTERIOR
    } else if near(cx) && near(cy) {
        SHOULDER
    } else {
        BACKGROUND
    }
}

/// `reference(target support) + border correction` for the fixtures above.
fn expected_green() -> f32 {
    reference(INTERIOR[0], INTERIOR[2]) + (SHOULDER[1] - reference(SHOULDER[0], SHOULDER[2]))
}

#[test]
fn deep_interior_of_a_large_region_is_estimated_from_its_border() {
    let mosaic = mosaic_from_cells(40, 40, large_region_colour);
    let output = reconstruct_opposed(mosaic.clone(), options()).expect("reconstruction");
    // 20x20 cells x 2 green sites.
    assert_eq!(output.stats.suspected_clipped_sites, 800);
    assert_eq!(output.stats.reconstructed_sites, 800);
    assert_eq!(output.stats.fallback_sites, 0);
    assert_eq!(output.stats.changed_sites, 800);

    // The centre is 10 cells from the border; local search reaches 3.
    let expected = expected_green();
    assert!(expected > INTERIOR[1], "fixture must need a raise");
    for (x, y) in [(40, 41), (41, 40), (20, 21), (59, 58)] {
        assert!(
            (output.mosaic.sample(x, y) - expected).abs() < 1e-5,
            "({x}, {y}) = {} != {expected}",
            output.mosaic.sample(x, y)
        );
    }
}

#[test]
fn region_without_border_evidence_uses_the_image_wide_pool() {
    // A second, single-cell blown region far from the first, ringed only by
    // darkness (no usable border). It must borrow the pooled correction.
    let mosaic = mosaic_from_cells(60, 40, |cx, cy| {
        if (cx, cy) == (50, 20) {
            INTERIOR
        } else {
            large_region_colour(cx, cy)
        }
    });
    let output = reconstruct_opposed(mosaic, options()).expect("reconstruction");
    let expected = expected_green();
    // Green sites of cell (50, 20): (100, 41) and (101, 40).
    for (x, y) in [(100, 41), (101, 40)] {
        assert!(
            (output.mosaic.sample(x, y) - expected).abs() < 1e-5,
            "({x}, {y}) = {} != {expected}",
            output.mosaic.sample(x, y)
        );
    }
    assert_eq!(output.stats.fallback_sites, 0);
}

#[test]
fn isolated_region_without_any_border_evidence_falls_back() {
    // Only the lone blown cell exists: no border anywhere, so nothing to
    // pool and the sites stay counted fallbacks.
    let mosaic = mosaic_from_cells(20, 20, |cx, cy| {
        if (cx, cy) == (10, 10) {
            INTERIOR
        } else {
            BACKGROUND
        }
    });
    let output = reconstruct_opposed(mosaic.clone(), options()).expect("reconstruction");
    assert_eq!(output.stats.suspected_clipped_sites, 2);
    assert_eq!(output.stats.reconstructed_sites, 0);
    assert_eq!(output.stats.fallback_sites, 2);
    assert_eq!(output.mosaic, mosaic);
}

// Two touching objects of opposite colour. Their dilated masks merge into a
// single region, yet each must be reconstructed as if the other were absent.
const WARM: [f32; 3] = [0.95, 0.8, 0.6];
const WARM_SHOULDER: [f32; 3] = [0.8, 0.5, 0.25];
const COOL: [f32; 3] = [0.6, 0.8, 0.95];
const COOL_SHOULDER: [f32; 3] = [0.25, 0.5, 0.8];

fn two_objects(with_cool: bool) -> MosaicImage<f32> {
    mosaic_from_cells(40, 20, move |cx, cy| {
        let rows = (6..15).contains(&cy);
        let warm = rows && (6..13).contains(&cx);
        let warm_ring = (5..16).contains(&cy) && (5..14).contains(&cx);
        let cool = rows && (15..22).contains(&cx);
        let cool_ring = (5..16).contains(&cy) && (14..23).contains(&cx);
        if warm {
            WARM
        } else if with_cool && cool {
            COOL
        } else if warm_ring {
            WARM_SHOULDER
        } else if with_cool && cool_ring {
            COOL_SHOULDER
        } else {
            BACKGROUND
        }
    })
}

#[test]
fn touching_objects_do_not_borrow_each_others_colour() {
    let both = reconstruct_opposed(two_objects(true), options()).expect("both objects");
    let warm_only = reconstruct_opposed(two_objects(false), options()).expect("warm object");
    // The warm object really is reconstructed ...
    let probe = (2 * 9, 2 * 10);
    assert!(*both.mosaic.sample(probe.0, probe.1) > WARM[0]);
    // ... and identically with or without the cool neighbour: every sample
    // left of the cool object matches bit for bit.
    for y in 0..40 {
        for x in 0..2 * 14 {
            assert_eq!(
                both.mosaic.sample(x, y),
                warm_only.mosaic.sample(x, y),
                "warm object changed at ({x}, {y})"
            );
        }
    }
    // The cool object is reconstructed as a blue-clipped highlight.
    let cool_probe = (2 * 18 + 1, 2 * 10 + 1);
    assert_eq!(
        BayerPattern::Rggb
            .color_at(cool_probe.0, cool_probe.1)
            .channel_index(),
        2
    );
    assert!(*both.mosaic.sample(cool_probe.0, cool_probe.1) > COOL[2]);
}

#[test]
fn region_reconstruction_is_deterministic_across_thread_counts() {
    let mosaic = mosaic_from_cells(60, 40, |cx, cy| {
        if (cx, cy) == (50, 20) {
            INTERIOR
        } else {
            large_region_colour(cx, cy)
        }
    });
    let run = |threads| {
        ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("thread pool")
            .install(|| reconstruct_opposed(mosaic.clone(), options()))
            .expect("reconstruction")
    };
    assert_eq!(run(1), run(4));
}

#[test]
fn every_checkpoint_of_the_region_passes_observes_cancellation() {
    let mosaic = mosaic_from_cells(40, 40, large_region_colour);
    let total = AtomicUsize::new(0);
    reconstruct_opposed_cancellable(mosaic.clone(), options(), &|| {
        total.fetch_add(1, Ordering::Relaxed);
        false
    })
    .expect("uncancelled run");
    let total = total.load(Ordering::Relaxed);
    assert!(total > 0);
    // Cancelling at any single checkpoint must stop the run (checkpoints
    // reached after the cancel point are irrelevant, so use a sticky flag).
    for cancel_at in 0..total {
        let seen = AtomicUsize::new(0);
        let result = reconstruct_opposed_cancellable(mosaic.clone(), options(), &|| {
            seen.fetch_add(1, Ordering::Relaxed) >= cancel_at
        });
        assert_eq!(
            result.expect_err("cancelled"),
            HighlightError::Cancelled,
            "cancel at checkpoint {cancel_at} of {total}"
        );
    }
}

#[test]
fn scratch_bytes_cover_the_region_state_and_fail_checked() {
    let (width, height) = (6_000, 4_000);
    let cells = (width / 2) * (height / 2);
    let local = local_ratio_scratch_bytes(width, height).expect("fits");
    let opposed = opposed_scratch_bytes(width, height).expect("fits");
    // At least one `u32` label per cell on top of the cell summaries.
    assert!(opposed >= local + 4 * cells, "{opposed} vs {local}");
    assert_eq!(opposed_scratch_bytes(usize::MAX, usize::MAX), None);
}
