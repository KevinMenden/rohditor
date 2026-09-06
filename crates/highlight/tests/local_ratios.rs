use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::ThreadPoolBuilder;
use rohditor_highlight::{
    ChannelDetectionLevels, HighlightError, LocalRatioOptions, ReconstructionStats,
    detect_local_ratios, reconstruct_local_ratios, reconstruct_local_ratios_cancellable,
};
use rohditor_image::{BayerPattern, CfaColor, MosaicImage};

fn levels(value: f32) -> ChannelDetectionLevels {
    ChannelDetectionLevels {
        red: value,
        green: value,
        blue: value,
    }
}

fn local_ratio_fixture(pattern: BayerPattern) -> MosaicImage<f32> {
    let width = 10;
    let height = 10;
    let row_stride = 13;
    let mut data = vec![91.0; row_stride * height];
    for y in 0..height {
        for x in 0..width {
            let value = match pattern.color_at(x, y) {
                CfaColor::Red => 0.6,
                CfaColor::Green => 0.5,
                CfaColor::Blue => 0.25,
            };
            data[y * row_stride + x] = value;
        }
    }
    // The central cell has the same colour ratios at a higher exposure. Its
    // red site is clipped at the detection level and should be recovered from
    // the surrounding clean cells.
    for y in 4..6 {
        for x in 4..6 {
            data[y * row_stride + x] = match pattern.color_at(x, y) {
                CfaColor::Red => 0.9,
                CfaColor::Green => 0.8,
                CfaColor::Blue => 0.4,
            };
        }
    }
    MosaicImage::new(width, height, row_stride, pattern, data).expect("fixture")
}

fn options() -> LocalRatioOptions {
    LocalRatioOptions {
        detection_levels: levels(0.9),
    }
}

fn channel_index(color: CfaColor) -> usize {
    match color {
        CfaColor::Red => 0,
        CfaColor::Green => 1,
        CfaColor::Blue => 2,
    }
}

fn channel_patch_fixture(
    pattern: BayerPattern,
    target_color: CfaColor,
    both_green_sites: bool,
) -> (MosaicImage<f32>, Vec<(usize, usize)>) {
    let width = 14;
    let height = 14;
    let row_stride = 17;
    let base = match target_color {
        CfaColor::Red => [0.6, 0.5, 0.25],
        CfaColor::Green => [0.4, 0.6, 0.25],
        CfaColor::Blue => [0.4, 0.5, 0.6],
    };
    let scale = 1.6;
    let mut data = vec![73.0; row_stride * height];
    let mut target_sites = Vec::new();
    let mut green_sites = 0;
    for y in 0..height {
        for x in 0..width {
            let color = pattern.color_at(x, y);
            let channel = channel_index(color);
            data[y * row_stride + x] = base[channel];
            if (6..8).contains(&x) && (6..8).contains(&y) {
                let value = if color == target_color {
                    if color == CfaColor::Green {
                        green_sites += 1;
                        if both_green_sites || green_sites == 1 {
                            0.9
                        } else {
                            0.8
                        }
                    } else {
                        0.9
                    }
                } else {
                    base[channel] * scale
                };
                data[y * row_stride + x] = value;
                if color == target_color
                    && (target_color != CfaColor::Green || both_green_sites || value == 0.9)
                {
                    target_sites.push((x, y));
                }
            }
        }
    }
    (
        MosaicImage::new(width, height, row_stride, pattern, data).expect("fixture"),
        target_sites,
    )
}

#[test]
fn flat_patch_recovers_a_clipped_red_site_and_preserves_padding() {
    let mosaic = local_ratio_fixture(BayerPattern::Rggb);
    let result = reconstruct_local_ratios(mosaic.clone(), options()).expect("reconstruction");

    assert_eq!(result.stats.suspected_clipped_sites, 1);
    assert_eq!(result.stats.reconstructed_sites, 1);
    assert_eq!(result.stats.fallback_sites, 0);
    assert_eq!(result.stats.changed_sites, 1);
    assert_eq!(result.stats.suspected_by_channel, [1, 0, 0]);
    assert!((result.mosaic.sample(4, 4) - 0.96).abs() < 1.0e-5);
    for y in 0..mosaic.height() {
        assert_eq!(result.mosaic.data()[y * mosaic.row_stride() + 10], 91.0);
        assert_eq!(result.mosaic.data()[y * mosaic.row_stride() + 11], 91.0);
        assert_eq!(result.mosaic.data()[y * mosaic.row_stride() + 12], 91.0);
    }
}

#[test]
fn all_bayer_layouts_reconstruct_their_red_phase() {
    for pattern in [
        BayerPattern::Rggb,
        BayerPattern::Bggr,
        BayerPattern::Grbg,
        BayerPattern::Gbrg,
    ] {
        let result = reconstruct_local_ratios(local_ratio_fixture(pattern), options())
            .expect("reconstruction");
        let red_site = (4..6)
            .flat_map(|y| (4..6).map(move |x| (x, y)))
            .find(|(x, y)| pattern.color_at(*x, *y) == CfaColor::Red)
            .expect("central red site");
        assert!((result.mosaic.sample(red_site.0, red_site.1) - 0.96).abs() < 1.0e-5);
        assert_eq!(result.stats.suspected_by_channel, [1, 0, 0]);
    }
}

#[test]
fn each_channel_and_both_green_sites_use_the_local_ratio_estimate() {
    for target_color in [CfaColor::Red, CfaColor::Green, CfaColor::Blue] {
        let (mosaic, target_sites) = channel_patch_fixture(BayerPattern::Rggb, target_color, false);
        let result = reconstruct_local_ratios(mosaic, options()).expect("channel reconstruction");
        assert_eq!(target_sites.len(), 1);
        assert_eq!(result.stats.suspected_clipped_sites, 1);
        assert_eq!(result.stats.reconstructed_sites, 1);
        assert_eq!(
            result.stats.suspected_by_channel[channel_index(target_color)],
            1
        );
        let recovered = *result.mosaic.sample(target_sites[0].0, target_sites[0].1);
        assert!(
            (recovered - 0.96).abs() < 1.0e-5,
            "{target_color:?} recovered {recovered} with {:?}",
            result.stats
        );
    }

    let (mosaic, target_sites) = channel_patch_fixture(BayerPattern::Rggb, CfaColor::Green, true);
    let result = reconstruct_local_ratios(mosaic, options()).expect("green reconstruction");
    assert_eq!(target_sites.len(), 2);
    assert_eq!(result.stats.suspected_clipped_sites, 2);
    assert_eq!(result.stats.reconstructed_sites, 2);
    for (x, y) in target_sites {
        assert!((result.mosaic.sample(x, y) - 0.96).abs() < 1.0e-5);
    }
}

#[test]
fn one_support_channel_and_dark_candidates_are_conservative() {
    let (mut mosaic, target_sites) =
        channel_patch_fixture(BayerPattern::Rggb, CfaColor::Red, false);
    let row_stride = mosaic.row_stride();
    let (blue_x, blue_y) = (6..8)
        .flat_map(|y| (6..8).map(move |x| (x, y)))
        .find(|(x, y)| mosaic.pattern().color_at(*x, *y) == CfaColor::Blue)
        .expect("central blue site");
    mosaic.data_mut()[blue_y * row_stride + blue_x] = -0.1;
    let result = reconstruct_local_ratios(mosaic, options()).expect("one-support reconstruction");
    assert_eq!(result.stats.suspected_clipped_sites, 1);
    assert_eq!(result.stats.reconstructed_sites, 1);
    let recovered = *result.mosaic.sample(target_sites[0].0, target_sites[0].1);
    assert!(
        (recovered - 0.96).abs() < 1.0e-5,
        "one-support recovered {recovered} with {:?}",
        result.stats
    );

    let width = 12;
    let height = 12;
    let row_stride = 15;
    let mut data = vec![61.0; row_stride * height];
    for y in 0..height {
        for x in 0..width {
            data[y * row_stride + x] = 0.001;
        }
    }
    data[5 * row_stride + 5] = 0.9;
    data[5 * row_stride + 4] = 0.2;
    data[4 * row_stride + 5] = 0.2;
    let dark = MosaicImage::new(width, height, row_stride, BayerPattern::Rggb, data)
        .expect("dark-denominator fixture");
    let result = reconstruct_local_ratios(dark, options()).expect("dark candidates");
    assert_eq!(result.stats.suspected_clipped_sites, 1);
    assert_eq!(result.stats.reconstructed_sites, 0);
    assert_eq!(result.stats.fallback_sites, 1);
}

#[test]
fn cross_channel_edge_guard_rejects_inconsistent_candidates() {
    let width = 14;
    let height = 14;
    let row_stride = 16;
    let mut data = vec![47.0; row_stride * height];
    for y in 0..height {
        for x in 0..width {
            let cell_x = x / 2;
            let cell_y = y / 2;
            let value = match (cell_x, cell_y, BayerPattern::Rggb.color_at(x, y)) {
                (3, 3, CfaColor::Red) => 0.9,
                (3, 3, CfaColor::Green) => 0.8,
                (3, 3, CfaColor::Blue) => 0.4,
                (_, _, CfaColor::Red) => 0.6,
                (_, _, CfaColor::Green) => 0.5,
                (_, _, CfaColor::Blue) => 0.5,
            };
            data[y * row_stride + x] = value;
        }
    }
    let mosaic = MosaicImage::new(width, height, row_stride, BayerPattern::Rggb, data)
        .expect("edge fixture");
    let result = reconstruct_local_ratios(mosaic, options()).expect("edge reconstruction");
    assert_eq!(result.stats.suspected_clipped_sites, 1);
    assert_eq!(result.stats.reconstructed_sites, 0);
    assert_eq!(result.stats.fallback_sites, 1);
    assert_eq!(*result.mosaic.sample(6, 6), 0.9);
}

#[test]
fn no_suspected_sites_is_bit_identical_and_does_not_need_summaries() {
    let mosaic = MosaicImage::new(
        5,
        3,
        7,
        BayerPattern::Bggr,
        vec![
            0.1, 0.2, 0.3, 0.4, 0.5, 77.0, 78.0, 0.6, 0.7, 0.8, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7,
            0.8, 0.1, 0.2, 0.3,
        ],
    )
    .expect("fixture");
    let original = mosaic.clone();
    let result = reconstruct_local_ratios(mosaic, options()).expect("no-op reconstruction");
    assert_eq!(result.mosaic, original);
    assert_eq!(result.stats, ReconstructionStats::default());
}

#[test]
fn materialized_detection_mask_matches_fused_classification() {
    let mosaic = local_ratio_fixture(BayerPattern::Grbg);
    let mask = detect_local_ratios(&mosaic, levels(0.9)).expect("detection mask");
    let mut suspected = 0;
    for y in 0..mosaic.height() {
        for x in 0..mosaic.width() {
            let expected = *mosaic.sample(x, y) >= 0.9;
            assert_eq!(mask.get(x, y), Some(expected), "at ({x}, {y})");
            suspected += usize::from(expected);
        }
        assert!(
            mask.data()[y * mosaic.row_stride() + mosaic.width()..(y + 1) * mosaic.row_stride()]
                .iter()
                .all(|marked| !marked)
        );
    }
    assert_eq!(suspected, 1);
    assert_eq!(mask.data().len(), mosaic.data().len());
}

#[test]
fn lower_bound_and_recovery_bound_are_conservative() {
    let mut mosaic = local_ratio_fixture(BayerPattern::Rggb);
    let row_stride = mosaic.row_stride();
    mosaic.data_mut()[4 * row_stride + 4] = 1.0;
    let result = reconstruct_local_ratios(
        mosaic,
        LocalRatioOptions {
            detection_levels: levels(0.9),
        },
    )
    .expect("lower-bound reconstruction");
    assert_eq!(result.stats.reconstructed_sites, 1);
    assert_eq!(result.stats.changed_sites, 0);
    assert_eq!(*result.mosaic.sample(4, 4), 1.0);

    let mut pathological = local_ratio_fixture(BayerPattern::Rggb);
    let row_stride = pathological.row_stride();
    pathological.data_mut()[4 * row_stride + 4] = 0.9;
    for y in 0..pathological.height() {
        for x in 0..pathological.width() {
            if (x, y) != (4, 4) && pathological.pattern().color_at(x, y) == CfaColor::Red {
                pathological.data_mut()[y * row_stride + x] = 0.89;
            }
        }
    }
    let result = reconstruct_local_ratios(
        pathological,
        LocalRatioOptions {
            detection_levels: levels(0.9),
        },
    )
    .expect("bounded reconstruction");
    assert!(*result.mosaic.sample(4, 4) <= 3.6);
}

#[test]
fn fully_unsupported_sites_are_reported_without_clip_fallback() {
    let mut data = vec![0.2; 4 * 4];
    data[0] = 1.0;
    data[1] = 1.0;
    data[4] = 1.0;
    data[5] = 1.0;
    let mosaic = MosaicImage::new(4, 4, 4, BayerPattern::Rggb, data).expect("fixture");
    let result = reconstruct_local_ratios(mosaic, options()).expect("unsupported reconstruction");
    assert_eq!(result.stats.suspected_clipped_sites, 4);
    assert_eq!(result.stats.fallback_sites, 4);
    assert_eq!(result.stats.fully_unsupported_sites, 4);
}

#[test]
fn invalid_levels_and_non_finite_visible_samples_are_rejected() {
    let mosaic = local_ratio_fixture(BayerPattern::Rggb);
    let error = reconstruct_local_ratios(
        mosaic.clone(),
        LocalRatioOptions {
            detection_levels: ChannelDetectionLevels {
                red: f32::NAN,
                ..levels(0.9)
            },
        },
    )
    .expect_err("invalid threshold");
    assert!(matches!(
        error,
        HighlightError::InvalidLevel { channel: "red", .. }
    ));

    let mut non_finite = mosaic;
    let row_stride = non_finite.row_stride();
    non_finite.data_mut()[2 * row_stride + 3] = f32::INFINITY;
    let error = reconstruct_local_ratios(non_finite, options()).expect_err("invalid sample");
    assert_eq!(error, HighlightError::NonFiniteSample { x: 3, y: 2 });
}

#[test]
fn reconstruction_is_deterministic_across_thread_counts() {
    let mosaic = local_ratio_fixture(BayerPattern::Gbrg);
    let single = ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("thread pool")
        .install(|| reconstruct_local_ratios(mosaic.clone(), options()))
        .expect("single-thread reconstruction");
    let multiple = ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .expect("thread pool")
        .install(|| reconstruct_local_ratios(mosaic, options()))
        .expect("multi-thread reconstruction");
    assert_eq!(single, multiple);
}

#[test]
fn cancellation_is_checked_before_and_during_processing() {
    let mosaic = local_ratio_fixture(BayerPattern::Rggb);
    let checks = AtomicUsize::new(0);
    let error = reconstruct_local_ratios_cancellable(mosaic, options(), &|| {
        checks.fetch_add(1, Ordering::Relaxed) >= 2
    })
    .expect_err("cancellation should interrupt reconstruction");
    assert_eq!(error, HighlightError::Cancelled);
    assert!(checks.load(Ordering::Relaxed) >= 2);
}
