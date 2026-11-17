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
        neutral_ceiling: None,
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
            neutralised_sites: 0,
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
            neutral_ceiling: None,
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
            neutralised_sites: 0,
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
            neutral_ceiling: None,
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
            neutral_ceiling: None,
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

fn ceilings(red: f32, green: f32, blue: f32) -> rohditor_highlight::ChannelClipLevels {
    rohditor_highlight::ChannelClipLevels { red, green, blue }
}

fn opposed_options_with_ceiling(
    ceiling: Option<rohditor_highlight::ChannelClipLevels>,
) -> OpposedOptions {
    OpposedOptions {
        detection_levels: levels(0.9),
        neutral_ceiling: ceiling,
    }
}

/// A 2x2 fully blown cell at the top-left corner: no channel keeps clean
/// support, so every suspected site takes the honest fallback branch.
fn blown_cell_fixture() -> MosaicImage<f32> {
    let mut data = vec![0.2; 5 * 5];
    for y in 0..2 {
        for x in 0..2 {
            data[y * 5 + x] = 1.0;
        }
    }
    MosaicImage::new(5, 5, 5, BayerPattern::Rggb, data).expect("fixture")
}

#[test]
fn neutral_ceiling_none_reproduces_legacy_output_bit_exactly() {
    let mosaic = blown_cell_fixture();
    let original = mosaic.clone();
    let result = reconstruct_opposed(mosaic, options()).expect("legacy reconstruction");
    assert_eq!(result.mosaic.data(), original.data());
    assert_eq!(result.stats.neutralised_sites, 0);
    assert_eq!(result.stats.fallback_sites, 4);
}

#[test]
fn fallback_sites_above_ceiling_land_exactly_on_it() {
    let mosaic = blown_cell_fixture();
    let result = reconstruct_opposed(
        mosaic,
        opposed_options_with_ceiling(Some(ceilings(0.6, 0.65, 0.7))),
    )
    .expect("neutralised reconstruction");
    assert_eq!(result.stats.fallback_sites, 4);
    assert_eq!(result.stats.neutralised_sites, 4);
    assert_eq!(result.stats.changed_sites, 0);
    // Per-channel landing points (RGGB phase): R (0,0), G (1,0)+(0,1), B (1,1).
    assert_eq!(*result.mosaic.sample(0, 0), 0.6);
    assert_eq!(*result.mosaic.sample(1, 0), 0.65);
    assert_eq!(*result.mosaic.sample(0, 1), 0.65);
    assert_eq!(*result.mosaic.sample(1, 1), 0.7);
}

#[test]
fn suspected_sites_at_or_below_ceiling_stay_untouched() {
    let mosaic = blown_cell_fixture();
    let original = mosaic.clone();
    let result = reconstruct_opposed(
        mosaic,
        opposed_options_with_ceiling(Some(ceilings(1.0, 1.0, 1.0))),
    )
    .expect("ceiling == input");
    assert_eq!(result.mosaic.data(), original.data());
    assert_eq!(result.stats.neutralised_sites, 0);
}

#[test]
fn raised_sites_are_not_lowered_to_the_ceiling() {
    // The double-clipped fixture reconstructs all three suspected sites
    // above 0.9. A ceiling below the estimates must NOT touch them: v1
    // neutralises only sites reconstruction did not raise.
    let result = reconstruct_opposed(
        double_clipped_fixture(BayerPattern::Rggb),
        opposed_options_with_ceiling(Some(ceilings(0.85, 0.85, 0.85))),
    )
    .expect("raised-not-lowered");
    assert_eq!(result.stats.changed_sites, 3);
    assert_eq!(result.stats.neutralised_sites, 0);
    assert!(*result.mosaic.sample(6, 6) > 0.85);
    assert!(*result.mosaic.sample(7, 6) > 0.85);
    assert!(*result.mosaic.sample(6, 7) > 0.85);
}

#[test]
fn unsuspected_sites_above_ceiling_are_never_touched() {
    let mut mosaic = single_clipped_fixture();
    let row_stride = mosaic.row_stride();
    // A far background site strictly above the ceiling but below the
    // detection level must stay untouched.
    mosaic.data_mut()[13 * row_stride + 12] = 0.6;
    let result = reconstruct_opposed(
        mosaic.clone(),
        opposed_options_with_ceiling(Some(ceilings(0.5, 0.5, 0.5))),
    )
    .expect("unsuspected-above-ceiling");
    assert_eq!(*result.mosaic.sample(12, 13), *mosaic.sample(12, 13));
    assert_eq!(result.stats.neutralised_sites, 0);
}

#[test]
fn reconstructed_but_not_applied_sites_are_neutralised() {
    // Same fixture as `one_channel_recovery...` but with the R site at 4.0:
    // the bounded estimate (max 4 * 0.9 = 3.6) can never raise it, so the
    // site keeps its raw value — the subtle fallback-for-neutralisation set.
    let mut mosaic = single_clipped_fixture();
    let row_stride = mosaic.row_stride();
    mosaic.data_mut()[6 * row_stride + 6] = 4.0;
    let result = reconstruct_opposed(
        mosaic,
        OpposedOptions {
            detection_levels: ChannelDetectionLevels {
                red: 0.9,
                green: 1.0,
                blue: 1.0,
            },
            neutral_ceiling: Some(ceilings(0.95, 1.0, 1.0)),
        },
    )
    .expect("not-applied neutralisation");
    assert_eq!(result.stats.reconstructed_sites, 1);
    assert_eq!(result.stats.changed_sites, 0);
    assert_eq!(result.stats.neutralised_sites, 1);
    assert_eq!(*result.mosaic.sample(6, 6), 0.95);
}

#[test]
fn neutral_ceiling_never_feeds_estimation() {
    // Same mosaic and detection levels, two ceilings: the estimator
    // bookkeeping must be identical; only the neutralised count moves.
    let with_low = reconstruct_opposed(
        blown_cell_fixture(),
        opposed_options_with_ceiling(Some(ceilings(0.6, 0.65, 0.7))),
    )
    .expect("low ceiling");
    let with_high = reconstruct_opposed(
        blown_cell_fixture(),
        opposed_options_with_ceiling(Some(ceilings(1.0, 1.0, 1.0))),
    )
    .expect("high ceiling");
    assert_eq!(
        with_low.stats.suspected_clipped_sites,
        with_high.stats.suspected_clipped_sites
    );
    assert_eq!(
        with_low.stats.reconstructed_sites,
        with_high.stats.reconstructed_sites
    );
    assert_eq!(
        with_low.stats.fallback_sites,
        with_high.stats.fallback_sites
    );
    assert_eq!(with_low.stats.neutralised_sites, 4);
    assert_eq!(with_high.stats.neutralised_sites, 0);
}

#[test]
fn neutralisation_is_deterministic_on_odd_dimensions_across_threads() {
    let width = 11;
    let height = 7;
    let mut data = vec![0.2; width * height];
    for y in 0..4 {
        for x in 0..4 {
            data[y * width + x] = 1.0;
        }
    }
    let mosaic = MosaicImage::new(width, height, width, BayerPattern::Rggb, data).expect("fixture");
    let single = ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("thread pool")
        .install(|| {
            reconstruct_opposed(
                mosaic.clone(),
                opposed_options_with_ceiling(Some(ceilings(0.5, 0.5, 0.5))),
            )
        })
        .expect("single-thread neutralisation");
    let multiple = ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .expect("thread pool")
        .install(|| {
            reconstruct_opposed(
                mosaic,
                opposed_options_with_ceiling(Some(ceilings(0.5, 0.5, 0.5))),
            )
        })
        .expect("multi-thread neutralisation");
    assert_eq!(single, multiple);
    assert_eq!(single.stats.neutralised_sites, 16);
}

#[test]
fn neutralised_sites_aggregate_across_rayon_row_chunks() {
    // 60 rows x a grid of fully blown cells: every row carries fallback
    // sites, so the per-chunk counters must sum exactly across threads.
    let width = 64;
    let height = 60;
    let mut data = vec![0.2; width * height];
    let mut blown_cells = 0;
    for cell_y in (0..height / 2).step_by(3) {
        for cell_x in (0..width / 2).step_by(3) {
            for dy in 0..2 {
                for dx in 0..2 {
                    data[(cell_y * 2 + dy) * width + cell_x * 2 + dx] = 1.0;
                }
            }
            blown_cells += 1;
        }
    }
    let mosaic =
        MosaicImage::new(width, height, width, BayerPattern::Rggb, data).expect("grid fixture");
    let options = opposed_options_with_ceiling(Some(ceilings(0.8, 0.8, 0.8)));
    let single = ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("thread pool")
        .install(|| reconstruct_opposed(mosaic.clone(), options))
        .expect("single-thread grid neutralisation");
    let parallel = ThreadPoolBuilder::new()
        .num_threads(8)
        .build()
        .expect("thread pool")
        .install(|| reconstruct_opposed(mosaic, options))
        .expect("parallel grid neutralisation");
    assert_eq!(single, parallel);
    assert_eq!(single.stats.suspected_clipped_sites, blown_cells * 4);
    assert_eq!(single.stats.neutralised_sites, blown_cells * 4);
}

#[test]
fn invalid_neutral_ceilings_are_rejected_before_any_write() {
    for red in [f32::NAN, 0.0] {
        let error = reconstruct_opposed(
            blown_cell_fixture(),
            opposed_options_with_ceiling(Some(ceilings(red, 0.65, 0.7))),
        )
        .expect_err("invalid ceiling");
        assert!(matches!(
            error,
            HighlightError::InvalidLevel { channel: "red", .. }
        ));
    }
    let error = reconstruct_opposed(
        blown_cell_fixture(),
        opposed_options_with_ceiling(Some(ceilings(0.6, 0.65, f32::NAN))),
    )
    .expect_err("invalid blue ceiling");
    assert!(matches!(
        error,
        HighlightError::InvalidLevel {
            channel: "blue",
            value,
        } if value.is_nan()
    ));
}
