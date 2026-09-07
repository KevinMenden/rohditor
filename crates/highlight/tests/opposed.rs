use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::ThreadPoolBuilder;
use rohditor_highlight::{
    ChannelDetectionLevels, HighlightError, OpposedOptions, OpposedStats, ReconstructionStats,
    detect_opposed, reconstruct_local_ratios, reconstruct_opposed, reconstruct_opposed_cancellable,
};
use rohditor_image::{BayerPattern, CfaColor, MosaicImage};

fn levels(value: f32) -> ChannelDetectionLevels {
    ChannelDetectionLevels {
        red: value,
        green: value,
        blue: value,
    }
}

fn options() -> OpposedOptions {
    OpposedOptions {
        detection_levels: levels(0.9),
    }
}

fn double_clipped_fixture(pattern: BayerPattern) -> MosaicImage<f32> {
    let width = 14;
    let height = 14;
    let row_stride = 17;
    let mut data = vec![0.01; row_stride * height];
    for y in 0..height {
        for x in 0..width {
            if (x / 2, y / 2) == (3, 3) {
                data[y * row_stride + x] = match pattern.color_at(x, y) {
                    CfaColor::Red | CfaColor::Green => 0.9,
                    CfaColor::Blue => 0.8,
                };
            } else if [(2, 2), (3, 2), (4, 2)].contains(&(x / 2, y / 2)) {
                data[y * row_stride + x] = match pattern.color_at(x, y) {
                    CfaColor::Red => 0.8,
                    CfaColor::Green => 0.7,
                    CfaColor::Blue => 0.4,
                };
            }
        }
    }
    MosaicImage::new(width, height, row_stride, pattern, data).expect("fixture")
}

fn single_clipped_fixture() -> MosaicImage<f32> {
    let width = 14;
    let height = 14;
    let row_stride = 17;
    let mut data = vec![0.01; row_stride * height];
    for y in 0..height {
        for x in 0..width {
            let cell = (x / 2, y / 2);
            data[y * row_stride + x] = if cell == (3, 3) {
                match BayerPattern::Rggb.color_at(x, y) {
                    CfaColor::Red => 0.9,
                    CfaColor::Green => 0.88,
                    CfaColor::Blue => 0.44,
                }
            } else if [(2, 2), (3, 2), (4, 2)].contains(&cell) {
                match BayerPattern::Rggb.color_at(x, y) {
                    CfaColor::Red => 0.75,
                    CfaColor::Green => 0.5,
                    CfaColor::Blue => 0.25,
                }
            } else {
                0.01
            };
        }
    }
    MosaicImage::new(width, height, row_stride, BayerPattern::Rggb, data).expect("fixture")
}

#[test]
fn opposed_recovers_multiple_clipped_channels_where_local_ratios_is_conservative() {
    let mosaic = double_clipped_fixture(BayerPattern::Rggb);
    let opposed = reconstruct_opposed(mosaic.clone(), options()).expect("opposed reconstruction");
    assert_eq!(
        opposed.stats,
        OpposedStats {
            suspected_clipped_sites: 3,
            reconstructed_sites: 3,
            changed_sites: 3,
            fallback_sites: 0,
            fully_unsupported_sites: 0,
            suspected_by_channel: [1, 2, 0],
        }
    );
    assert!(*opposed.mosaic.sample(6, 6) > 0.9);
    assert!(*opposed.mosaic.sample(7, 6) > 0.9);
    assert!(*opposed.mosaic.sample(6, 7) > 0.9);

    let local = reconstruct_local_ratios(
        mosaic,
        rohditor_highlight::LocalRatioOptions {
            detection_levels: levels(0.9),
        },
    )
    .expect("local-ratio reconstruction");
    assert_eq!(local.stats.reconstructed_sites, 0);
    assert_eq!(local.stats.fallback_sites, 3);
    assert_eq!(
        local.stats,
        ReconstructionStats {
            suspected_clipped_sites: 3,
            reconstructed_sites: 0,
            changed_sites: 0,
            fallback_sites: 3,
            fully_unsupported_sites: 0,
            suspected_by_channel: [1, 2, 0],
        }
    );
}

#[test]
fn all_bayer_phases_recover_their_center_channels_and_preserve_padding() {
    for pattern in [
        BayerPattern::Rggb,
        BayerPattern::Bggr,
        BayerPattern::Grbg,
        BayerPattern::Gbrg,
    ] {
        let mosaic = double_clipped_fixture(pattern);
        let result = reconstruct_opposed(mosaic.clone(), options()).expect("reconstruction");
        assert_eq!(result.stats.suspected_clipped_sites, 3);
        assert_eq!(result.stats.reconstructed_sites, 3);
        for y in 0..mosaic.height() {
            assert_eq!(
                &result.mosaic.data()
                    [y * mosaic.row_stride() + mosaic.width()..(y + 1) * mosaic.row_stride()],
                &mosaic.data()
                    [y * mosaic.row_stride() + mosaic.width()..(y + 1) * mosaic.row_stride()]
            );
        }
    }
}

#[test]
fn one_channel_recovery_preserves_a_colored_highlight_and_is_bounded() {
    let result = reconstruct_opposed(
        single_clipped_fixture(),
        OpposedOptions {
            detection_levels: ChannelDetectionLevels {
                red: 0.9,
                green: 1.0,
                blue: 1.0,
            },
        },
    )
    .expect("reconstruction");
    assert_eq!(result.stats.suspected_clipped_sites, 1);
    assert_eq!(result.stats.reconstructed_sites, 1);
    assert_eq!(result.stats.changed_sites, 1);
    assert!(result.mosaic.sample(6, 6).is_finite());
    assert!(*result.mosaic.sample(6, 6) > 0.9);
    assert!(result.mosaic.data().iter().all(|value| value.is_finite()));
    assert!(result.mosaic.data().iter().all(|value| *value <= 3.6));
}

#[test]
fn opposing_ratio_guard_rejects_a_red_green_edge_candidate() {
    let mut mosaic = single_clipped_fixture();
    let row_stride = mosaic.row_stride();
    for y in 4..6 {
        for x in 4..6 {
            if mosaic.pattern().color_at(x, y) == CfaColor::Red {
                mosaic.data_mut()[y * row_stride + x] = 0.9;
            }
        }
    }
    // The only bright candidate cells have an incompatible G/B ratio. This
    // should remain a counted fallback instead of bleeding the edge color.
    let pattern = mosaic.pattern();
    for (cell_x, cell_y) in [(2, 2), (3, 2), (4, 2)] {
        for y in cell_y * 2..cell_y * 2 + 2 {
            for x in cell_x * 2..cell_x * 2 + 2 {
                mosaic.data_mut()[y * row_stride + x] = match pattern.color_at(x, y) {
                    CfaColor::Red => 0.8,
                    CfaColor::Green => 0.2,
                    CfaColor::Blue => 0.6,
                };
            }
        }
    }
    let result = reconstruct_opposed(mosaic.clone(), options()).expect("reconstruction");
    assert_eq!(result.stats.suspected_clipped_sites, 1);
    assert_eq!(result.stats.reconstructed_sites, 0);
    assert_eq!(result.stats.fallback_sites, 1);
    assert_eq!(*result.mosaic.sample(6, 6), *mosaic.sample(6, 6));
}

#[test]
fn fully_unsupported_clipped_cells_are_reported_without_clip_fallback() {
    let mut data = vec![0.2; 5 * 5];
    for y in 0..2 {
        for x in 0..2 {
            data[y * 5 + x] = 1.0;
        }
    }
    let mosaic = MosaicImage::new(5, 5, 5, BayerPattern::Rggb, data).expect("fixture");
    let result = reconstruct_opposed(mosaic, options()).expect("reconstruction");
    assert_eq!(result.stats.suspected_clipped_sites, 4);
    assert_eq!(result.stats.reconstructed_sites, 0);
    assert_eq!(result.stats.fallback_sites, 4);
    assert_eq!(result.stats.fully_unsupported_sites, 4);
}

#[test]
fn no_suspected_sites_is_bit_identical_and_padding_is_not_classified() {
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
    let result = reconstruct_opposed(mosaic, options()).expect("no-op reconstruction");
    assert_eq!(result.mosaic, original);
    assert_eq!(result.stats, OpposedStats::default());

    let mask = detect_opposed(&original, levels(0.9)).expect("detection mask");
    assert!(mask.data().iter().all(|marked| !marked));
    assert_eq!(mask.data().len(), original.data().len());
}

#[test]
fn invalid_samples_and_cancellation_are_rejected() {
    let error = reconstruct_opposed(
        double_clipped_fixture(BayerPattern::Rggb),
        OpposedOptions {
            detection_levels: ChannelDetectionLevels {
                red: f32::NAN,
                ..levels(0.9)
            },
        },
    )
    .expect_err("invalid level");
    assert!(matches!(
        error,
        HighlightError::InvalidLevel {
            channel: "red",
            value,
        } if value.is_nan()
    ));

    let mut invalid = double_clipped_fixture(BayerPattern::Rggb);
    let row_stride = invalid.row_stride();
    invalid.data_mut()[2 * row_stride + 3] = f32::NAN;
    let error = reconstruct_opposed(invalid, options()).expect_err("invalid sample");
    assert_eq!(error, HighlightError::NonFiniteSample { x: 3, y: 2 });

    let checks = AtomicUsize::new(0);
    let error = reconstruct_opposed_cancellable(
        double_clipped_fixture(BayerPattern::Gbrg),
        options(),
        &|| checks.fetch_add(1, Ordering::Relaxed) >= 2,
    )
    .expect_err("cancellation");
    assert_eq!(error, HighlightError::Cancelled);
    assert!(checks.load(Ordering::Relaxed) >= 2);
}

#[test]
fn reconstruction_is_deterministic_across_thread_counts() {
    let mosaic = double_clipped_fixture(BayerPattern::Grbg);
    let single = ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("thread pool")
        .install(|| reconstruct_opposed(mosaic.clone(), options()))
        .expect("single-thread reconstruction");
    let multiple = ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .expect("thread pool")
        .install(|| reconstruct_opposed(mosaic, options()))
        .expect("multi-thread reconstruction");
    assert_eq!(single, multiple);
}
