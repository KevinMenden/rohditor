use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::ThreadPoolBuilder;
use rohditor_highlight::{
    ChannelClipLevels, ClipStats, HighlightError, clip, clip_cancellable, detect_clipping,
};
use rohditor_image::{BayerPattern, MosaicImage};

fn levels() -> ChannelClipLevels {
    ChannelClipLevels {
        red: 0.8,
        green: 0.9,
        blue: 1.1,
    }
}

fn sample_mosaic(pattern: BayerPattern) -> MosaicImage<f32> {
    MosaicImage::new(
        5,
        3,
        7,
        pattern,
        vec![
            0.7, 0.8, 0.9, 1.0, -0.2, 77.0, 78.0, 0.9, 1.1, 1.2, 1.3, 0.4, 79.0, 80.0, 0.5, 0.8,
            0.9, 1.1, 1.2, 0.6, 81.0,
        ],
    )
    .expect("valid padded fixture")
}

#[test]
fn all_bayer_layouts_classify_both_green_sites_and_preserve_padding() {
    for pattern in [
        BayerPattern::Rggb,
        BayerPattern::Bggr,
        BayerPattern::Grbg,
        BayerPattern::Gbrg,
    ] {
        let original = sample_mosaic(pattern);
        let result = clip(original.clone(), levels()).expect("valid clip");
        assert_eq!(result.mosaic.width(), 5);
        assert_eq!(result.mosaic.row_stride(), 7);
        for y in 0..result.mosaic.height() {
            assert_eq!(result.mosaic.data()[y * 7 + 5], original.data()[y * 7 + 5]);
            assert_eq!(result.mosaic.data()[y * 7 + 6], original.data()[y * 7 + 6]);
        }

        let mut expected = ClipStats::default();
        for y in 0..original.height() {
            for x in 0..original.width() {
                let color = pattern.color_at(x, y);
                let sample = *original.sample(x, y);
                let limit = levels().for_color(color);
                if sample >= limit {
                    expected.affected_sites += 1;
                    expected.affected_by_channel[color.channel_index()] += 1;
                }
                if sample > limit {
                    expected.changed_sites += 1;
                }
                if sample > 1.0 {
                    expected.nominal_over_white_sites += 1;
                }
            }
        }
        assert_eq!(result.stats, expected);
    }
}

#[test]
fn equal_is_affected_but_only_above_is_changed_and_negative_values_survive() {
    let mosaic = MosaicImage::new(2, 2, 2, BayerPattern::Rggb, vec![0.8, 0.9, -0.1, 1.2])
        .expect("valid fixture");
    let result = clip(
        mosaic,
        ChannelClipLevels {
            red: 0.8,
            green: 0.9,
            blue: 1.0,
        },
    )
    .expect("valid clip");
    assert_eq!(result.mosaic.data(), &[0.8, 0.9, -0.1, 1.0]);
    assert_eq!(
        result.stats,
        ClipStats {
            affected_sites: 3,
            changed_sites: 1,
            nominal_over_white_sites: 1,
            affected_by_channel: [1, 1, 1],
        }
    );
}

#[test]
fn detector_agrees_with_fused_affected_classification() {
    let mosaic = sample_mosaic(BayerPattern::Rggb);
    let result = clip(mosaic.clone(), levels()).expect("valid clip");
    let mask = detect_clipping(&mosaic, levels()).expect("valid mask");
    let affected = (0..mosaic.height())
        .flat_map(|y| (0..mosaic.width()).map(move |x| (x, y)))
        .filter(|(x, y)| mask.get(*x, *y).unwrap_or(false))
        .count();
    assert_eq!(affected, result.stats.affected_sites);
    for y in 0..mosaic.height() {
        assert!(!mask.data()[y * mosaic.row_stride() + 5]);
        assert!(!mask.data()[y * mosaic.row_stride() + 6]);
    }
}

#[test]
fn equal_thresholds_cover_red_green_and_blue_sites() {
    let mosaic = MosaicImage::new(2, 2, 2, BayerPattern::Rggb, vec![0.8, 0.9, 1.1, 1.1])
        .expect("valid fixture");
    let result = clip(mosaic, levels()).expect("valid clip");
    assert_eq!(result.stats.affected_by_channel, [1, 2, 1]);
}

#[test]
fn invalid_levels_are_rejected_before_the_input_is_mutated() {
    let mosaic = sample_mosaic(BayerPattern::Rggb);
    let error = clip(
        mosaic,
        ChannelClipLevels {
            red: f32::NAN,
            ..levels()
        },
    )
    .expect_err("NaN level must fail");
    assert!(matches!(
        error,
        HighlightError::InvalidLevel { channel: "red", .. }
    ));
}

#[test]
fn non_finite_samples_report_coordinates() {
    let mosaic = MosaicImage::new(
        3,
        2,
        4,
        BayerPattern::Bggr,
        vec![0.0, 0.0, 0.0, 91.0, 0.0, f32::INFINITY, 0.0, 92.0],
    )
    .expect("valid fixture");
    let error = clip(mosaic, levels()).expect_err("non-finite sample must fail");
    assert_eq!(error, HighlightError::NonFiniteSample { x: 1, y: 1 });
}

#[test]
fn cancellation_is_checked_before_and_during_rows() {
    let mosaic = sample_mosaic(BayerPattern::Rggb);
    let pre_checks = AtomicUsize::new(0);
    let error = clip_cancellable(mosaic.clone(), levels(), &|| {
        pre_checks.fetch_add(1, Ordering::Relaxed);
        true
    })
    .expect_err("pre-cancelled work must fail");
    assert_eq!(error, HighlightError::Cancelled);
    assert!(pre_checks.load(Ordering::Relaxed) > 0);

    let checks = AtomicUsize::new(0);
    let error = clip_cancellable(mosaic, levels(), &|| {
        checks.fetch_add(1, Ordering::Relaxed) >= 2
    })
    .expect_err("mid-pass cancellation must fail");
    assert_eq!(error, HighlightError::Cancelled);
    assert!(checks.load(Ordering::Relaxed) >= 2);
}

#[test]
fn output_and_statistics_are_deterministic_across_thread_counts() {
    let mosaic = sample_mosaic(BayerPattern::Gbrg);
    let single = ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("thread pool")
        .install(|| clip(mosaic.clone(), levels()))
        .expect("single-thread clip");
    let multiple = ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .expect("thread pool")
        .install(|| clip(mosaic, levels()))
        .expect("multi-thread clip");
    assert_eq!(single, multiple);
}
