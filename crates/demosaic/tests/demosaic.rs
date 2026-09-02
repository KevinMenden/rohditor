use std::error::Error;

use rayon::ThreadPoolBuilder;
use rohditor_demosaic::{DemosaicAlgorithm, DemosaicError, WhiteBalanceGains, demosaic};
use rohditor_image::{BayerPattern, CfaColor, ImageError, MosaicImage};

const PATTERNS: [BayerPattern; 4] = [
    BayerPattern::Rggb,
    BayerPattern::Bggr,
    BayerPattern::Grbg,
    BayerPattern::Gbrg,
];

#[test]
fn mhc_reconstructs_constants_for_every_bayer_layout() -> Result<(), Box<dyn Error>> {
    for pattern in PATTERNS {
        let mosaic = mosaic_from_rgb(9, 8, pattern, |_, _| [0.2, 0.4, 0.8])?;
        let image = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
        )?;
        for pixel in image.data().as_chunks::<3>().0 {
            assert_close(pixel, &[0.2, 0.4, 0.8], 1.0e-6);
        }
    }
    Ok(())
}

#[test]
fn mhc_reconstructs_affine_fields_at_every_interior_phase() -> Result<(), Box<dyn Error>> {
    for pattern in PATTERNS {
        let mosaic = mosaic_from_rgb(10, 10, pattern, affine_rgb)?;
        let image = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
        )?;
        for y in 2..8 {
            for x in 2..8 {
                let expected = affine_rgb(x, y);
                let actual = image.pixel(x, y).expect("interior output pixel");
                assert_close(actual, &expected, 2.0e-6);
            }
        }
    }
    Ok(())
}

#[test]
fn mhc_preserves_observed_sites_before_white_balance_for_all_layouts() -> Result<(), Box<dyn Error>>
{
    let gains = WhiteBalanceGains {
        red: 1.7,
        green: 0.8,
        blue: 2.3,
    };
    for pattern in PATTERNS {
        let mosaic = mosaic_from_rgb(9, 9, pattern, |x, y| {
            let seed = (y * 9 + x) as f32 / 100.0;
            [0.1 + seed, 0.2 + seed, 0.3 + seed]
        })?;
        let image = demosaic(&mosaic, gains, DemosaicAlgorithm::MalvarHeCutler)?;
        for y in 0..mosaic.height() {
            for x in 0..mosaic.width() {
                let color = pattern.color_at(x, y);
                let channel = channel_index(color);
                let gain = match color {
                    CfaColor::Red => gains.red,
                    CfaColor::Green => gains.green,
                    CfaColor::Blue => gains.blue,
                };
                let expected = mosaic.get(x, y).expect("source sample") * gain;
                let actual = image.pixel(x, y).expect("output pixel")[channel];
                assert!(
                    (actual - expected).abs() <= 1.0e-6,
                    "{pattern:?} ({x}, {y})"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn mhc_uses_bilinear_for_images_and_pixels_inside_the_two_pixel_border()
-> Result<(), Box<dyn Error>> {
    for (width, height) in [(2, 2), (3, 7), (7, 3), (4, 4), (5, 5)] {
        let mosaic = mosaic_from_rgb(width, height, BayerPattern::Gbrg, |x, y| {
            let value = (x + y * width) as f32;
            [value * 0.1, value * 0.2 + 0.1, value * 0.3 + 0.2]
        })?;
        let bilinear = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::Bilinear,
        )?;
        let mhc = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
        )?;
        for y in 0..height {
            for x in 0..width {
                let border = x < 2 || y < 2 || x >= width - 2 || y >= height - 2;
                if border {
                    assert_eq!(mhc.pixel(x, y), bilinear.pixel(x, y));
                }
            }
        }
    }
    Ok(())
}

#[test]
fn mhc_retains_negative_values_and_over_range_highlights() -> Result<(), Box<dyn Error>> {
    let mosaic = mosaic_from_rgb(8, 8, BayerPattern::Rggb, |_, _| [-0.25, 1.25, 2.0])?;
    let image = demosaic(
        &mosaic,
        WhiteBalanceGains {
            red: 2.0,
            green: 1.0,
            blue: 1.5,
        },
        DemosaicAlgorithm::MalvarHeCutler,
    )?;
    for pixel in image.data().as_chunks::<3>().0 {
        assert_close(pixel, &[-0.5, 1.25, 3.0], 1.0e-6);
    }
    Ok(())
}

#[test]
fn demosaic_rejects_non_finite_mosaic_samples() {
    let mut data = vec![0.5; 36];
    data[3 * 6 + 4] = f32::NAN;
    let mosaic = MosaicImage::new(6, 6, 6, BayerPattern::Rggb, data).expect("valid layout");
    let error = demosaic(
        &mosaic,
        WhiteBalanceGains::identity(),
        DemosaicAlgorithm::MalvarHeCutler,
    )
    .expect_err("non-finite normalized input must be rejected");
    assert!(matches!(
        error,
        DemosaicError::NonFiniteImageData {
            stage: "demosaicing",
            x: 4,
            y: 3
        }
    ));
}

#[test]
fn mhc_is_identical_with_one_and_multiple_rayon_threads() -> Result<(), Box<dyn Error>> {
    let mosaic = mosaic_from_rgb(63, 47, BayerPattern::Bggr, |x, y| {
        let xf = x as f32;
        let yf = y as f32;
        [
            (xf * 0.071 + yf * 0.013).sin() * 0.4 + 0.5,
            (xf * 0.029 - yf * 0.053).cos() * 0.3 + 0.45,
            ((x * 17 + y * 31) % 101) as f32 / 100.0,
        ]
    })?;
    let single_pool = ThreadPoolBuilder::new().num_threads(1).build()?;
    let multi_pool = ThreadPoolBuilder::new().num_threads(4).build()?;
    let single = single_pool.install(|| {
        demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
        )
    })?;
    let multiple = multi_pool.install(|| {
        demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
        )
    })?;
    assert_eq!(single, multiple);
    Ok(())
}

fn mosaic_from_rgb(
    width: usize,
    height: usize,
    pattern: BayerPattern,
    rgb_at: impl Fn(usize, usize) -> [f32; 3],
) -> Result<MosaicImage<f32>, ImageError> {
    let mut data = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            data.push(rgb_at(x, y)[channel_index(pattern.color_at(x, y))]);
        }
    }
    MosaicImage::new(width, height, width, pattern, data)
}

fn affine_rgb(x: usize, y: usize) -> [f32; 3] {
    let x = x as f32;
    let y = y as f32;
    [
        0.15 + 0.01 * x + 0.005 * y,
        0.25 - 0.004 * x + 0.008 * y,
        0.4 + 0.003 * x - 0.006 * y,
    ]
}

const fn channel_index(color: CfaColor) -> usize {
    match color {
        CfaColor::Red => 0,
        CfaColor::Green => 1,
        CfaColor::Blue => 2,
    }
}

fn assert_close(actual: &[f32], expected: &[f32], tolerance: f32) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected}, received {actual}"
        );
    }
}
