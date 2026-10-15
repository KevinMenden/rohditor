use std::error::Error;

use rayon::ThreadPoolBuilder;
use rohditor_demosaic::{
    AMAZE_HALO, DemosaicAlgorithm, DemosaicError, RCD_HALO, WhiteBalanceGains, demosaic,
};
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
fn rcd_reconstructs_constants_for_every_bayer_layout() -> Result<(), Box<dyn Error>> {
    for pattern in PATTERNS {
        let mosaic = mosaic_from_rgb(40, 40, pattern, |_, _| [0.2, 0.4, 0.8])?;
        let image = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::Rcd,
        )?;
        for pixel in image.data().as_chunks::<3>().0 {
            assert_close(pixel, &[0.2, 0.4, 0.8], 1.0e-5);
        }
    }
    Ok(())
}

#[test]
fn amaze_reconstructs_constants_for_every_bayer_layout() -> Result<(), Box<dyn Error>> {
    for pattern in PATTERNS {
        let mosaic = mosaic_from_rgb(40, 40, pattern, |_, _| [0.2, 0.4, 0.8])?;
        let image = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::Amaze,
        )?;
        for pixel in image.data().as_chunks::<3>().0 {
            assert_close(pixel, &[0.2, 0.4, 0.8], 1.0e-5);
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
fn mhc_ignores_balance_and_preserves_observed_sites_for_all_layouts() -> Result<(), Box<dyn Error>>
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
                let expected = *mosaic.get(x, y).expect("source sample");
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
fn rcd_preserves_observed_sites_under_channel_balance_for_all_layouts() -> Result<(), Box<dyn Error>>
{
    let gains = WhiteBalanceGains {
        red: 1.7,
        green: 0.8,
        blue: 2.3,
    };
    for pattern in PATTERNS {
        let mosaic = mosaic_from_rgb(40, 40, pattern, |x, y| {
            let seed = (y * 40 + x) as f32 / 1_000.0;
            [0.1 + seed, 0.2 + seed, 0.3 + seed]
        })?;
        let image = demosaic(&mosaic, gains, DemosaicAlgorithm::Rcd)?;
        for y in 0..mosaic.height() {
            for x in 0..mosaic.width() {
                let color = pattern.color_at(x, y);
                let channel = channel_index(color);
                let expected = *mosaic.get(x, y).expect("source sample");
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
fn amaze_preserves_observed_sites_under_channel_balance_for_all_layouts()
-> Result<(), Box<dyn Error>> {
    let gains = WhiteBalanceGains {
        red: 1.7,
        green: 0.8,
        blue: 2.3,
    };
    for pattern in PATTERNS {
        let mosaic = mosaic_from_rgb(41, 37, pattern, |x, y| {
            let seed = (y * 41 + x) as f32 / 1_000.0;
            [0.1 + seed, 0.2 + seed, 0.3 + seed]
        })?;
        let image = demosaic(&mosaic, gains, DemosaicAlgorithm::Amaze)?;
        for y in 0..mosaic.height() {
            for x in 0..mosaic.width() {
                let color = pattern.color_at(x, y);
                let channel = channel_index(color);
                let expected = *mosaic.get(x, y).expect("source sample");
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
fn rcd_uses_bilinear_when_an_image_cannot_support_the_directional_halo()
-> Result<(), Box<dyn Error>> {
    for (width, height) in [(2, 2), (7, 31), (31, 7), (24, 24)] {
        let mosaic = mosaic_from_rgb(width, height, BayerPattern::Gbrg, |x, y| {
            let value = (x + y * width) as f32;
            [value * 0.1, value * 0.2 + 0.1, value * 0.3 + 0.2]
        })?;
        let bilinear = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::Bilinear,
        )?;
        let rcd = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::Rcd,
        )?;
        for y in 0..height {
            for x in 0..width {
                assert_eq!(rcd.pixel(x, y), bilinear.pixel(x, y));
            }
        }
    }
    Ok(())
}

#[test]
fn rcd_runs_directional_stages_and_keeps_the_outer_halo_bilinear() -> Result<(), Box<dyn Error>> {
    let width = 41;
    let height = 37;
    let mosaic = mosaic_from_rgb(width, height, BayerPattern::Rggb, |x, y| {
        let seed = ((x * 17 + y * 31) % 97) as f32 / 96.0;
        [0.1 + seed * 0.7, 0.2 + (1.0 - seed) * 0.5, 0.3 + seed * 0.4]
    })?;
    let bilinear = demosaic(
        &mosaic,
        WhiteBalanceGains::identity(),
        DemosaicAlgorithm::Bilinear,
    )?;
    let rcd = demosaic(
        &mosaic,
        WhiteBalanceGains::identity(),
        DemosaicAlgorithm::Rcd,
    )?;

    let mut directional_difference = false;
    for y in 0..height {
        for x in 0..width {
            let actual = rcd.pixel(x, y).expect("RCD output pixel");
            assert!(actual.iter().all(|value| value.is_finite()));
            let outer_halo = x < RCD_HALO.left
                || y < RCD_HALO.top
                || x >= width - RCD_HALO.right
                || y >= height - RCD_HALO.bottom;
            if outer_halo {
                assert_eq!(Some(actual), bilinear.pixel(x, y));
            } else if actual
                .iter()
                .zip(bilinear.pixel(x, y).expect("bilinear output pixel"))
                .any(|(rcd, bilinear)| (rcd - bilinear).abs() > 1.0e-4)
            {
                directional_difference = true;
            }
        }
    }
    assert!(directional_difference, "RCD interior was not reconstructed");
    Ok(())
}

#[test]
fn amaze_runs_directional_stages_and_keeps_the_outer_halo_bilinear() -> Result<(), Box<dyn Error>> {
    let width = 41;
    let height = 37;
    let mosaic = mosaic_from_rgb(width, height, BayerPattern::Rggb, |x, y| {
        let seed = ((x * 17 + y * 31) % 97) as f32 / 96.0;
        [0.1 + seed * 0.7, 0.2 + (1.0 - seed) * 0.5, 0.3 + seed * 0.4]
    })?;
    let bilinear = demosaic(
        &mosaic,
        WhiteBalanceGains::identity(),
        DemosaicAlgorithm::Bilinear,
    )?;
    let amaze = demosaic(
        &mosaic,
        WhiteBalanceGains::identity(),
        DemosaicAlgorithm::Amaze,
    )?;

    let mut directional_difference = false;
    for y in 0..height {
        for x in 0..width {
            let actual = amaze.pixel(x, y).expect("AMaZE output pixel");
            assert!(actual.iter().all(|value| value.is_finite()));
            let outer_halo = x < AMAZE_HALO.left
                || y < AMAZE_HALO.top
                || x >= width - AMAZE_HALO.right
                || y >= height - AMAZE_HALO.bottom;
            if outer_halo {
                assert_eq!(Some(actual), bilinear.pixel(x, y));
            } else if actual
                .iter()
                .zip(bilinear.pixel(x, y).expect("bilinear output pixel"))
                .any(|(amaze, bilinear)| (amaze - bilinear).abs() > 1.0e-4)
            {
                directional_difference = true;
            }
        }
    }
    assert!(
        directional_difference,
        "AMaZE interior was not reconstructed"
    );
    Ok(())
}

#[test]
fn amaze_uses_bilinear_when_an_image_cannot_support_the_directional_halo()
-> Result<(), Box<dyn Error>> {
    for (width, height) in [(2, 2), (7, 31), (31, 7), (32, 32)] {
        let mosaic = mosaic_from_rgb(width, height, BayerPattern::Gbrg, |x, y| {
            let value = (x + y * width) as f32;
            [value * 0.1, value * 0.2 + 0.1, value * 0.3 + 0.2]
        })?;
        let bilinear = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::Bilinear,
        )?;
        let amaze = demosaic(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::Amaze,
        )?;
        assert_eq!(amaze, bilinear);
    }
    Ok(())
}

#[test]
fn amaze_reconstructs_across_tile_boundaries() -> Result<(), Box<dyn Error>> {
    let width = 300;
    let height = 275;
    let mosaic = mosaic_from_rgb(width, height, BayerPattern::Grbg, |_, _| [0.18, 0.36, 0.72])?;
    let image = demosaic(
        &mosaic,
        WhiteBalanceGains::identity(),
        DemosaicAlgorithm::Amaze,
    )?;
    for pixel in image.data().as_chunks::<3>().0 {
        assert_close(pixel, &[0.18, 0.36, 0.72], 2.0e-5);
    }
    Ok(())
}

#[test]
fn amaze_keeps_signed_and_over_range_samples_finite() -> Result<(), Box<dyn Error>> {
    let mosaic = mosaic_from_rgb(40, 40, BayerPattern::Bggr, |_, _| [-0.25, 1.25, 2.0])?;
    let image = demosaic(
        &mosaic,
        WhiteBalanceGains::identity(),
        DemosaicAlgorithm::Amaze,
    )?;
    assert!(image.data().iter().all(|value| value.is_finite()));
    assert!(image.data().iter().any(|value| *value < 0.0));
    assert!(image.data().iter().any(|value| *value > 1.0));
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
        assert_close(pixel, &[-0.25, 1.25, 2.0], 1.0e-6);
    }
    Ok(())
}

#[test]
fn rcd_retains_signed_and_over_range_constant_values() -> Result<(), Box<dyn Error>> {
    let mosaic = mosaic_from_rgb(40, 40, BayerPattern::Bggr, |_, _| [-0.25, 1.25, 2.0])?;
    let image = demosaic(
        &mosaic,
        WhiteBalanceGains::identity(),
        DemosaicAlgorithm::Rcd,
    )?;
    for pixel in image.data().as_chunks::<3>().0 {
        assert_close(pixel, &[-0.25, 1.25, 2.0], 1.0e-5);
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

#[test]
fn amaze_is_identical_with_one_and_multiple_rayon_threads() -> Result<(), Box<dyn Error>> {
    // Several tile rows and columns, so bands run on different threads.
    let mosaic = mosaic_from_rgb(520, 470, BayerPattern::Grbg, |x, y| {
        let (xf, yf) = (x as f32, y as f32);
        [
            (xf * 0.071 + yf * 0.013).sin() * 0.3 + 0.4,
            (xf * 0.029 - yf * 0.053).cos() * 0.3 + 0.45,
            ((x * 17 + y * 31) % 101) as f32 / 150.0 + 0.1,
        ]
    })?;
    let run = |threads| -> Result<_, Box<dyn Error>> {
        let pool = ThreadPoolBuilder::new().num_threads(threads).build()?;
        Ok(pool.install(|| demosaic(&mosaic, UNBALANCED, DemosaicAlgorithm::Amaze))?)
    };
    assert_eq!(run(1)?, run(4)?);
    Ok(())
}

/// Soft colour edge, ~5 degrees off horizontal, in an unbalanced raw-like
/// colour space.
fn soft_slanted_edge(x: usize, y: usize) -> [f32; 3] {
    let distance = (x as f32 - 128.0) * 0.087 + (y as f32 - 96.0) * 0.996;
    let t = ((distance + 1.5) / 3.0).clamp(0.0, 1.0);
    let t = t * t * (3.0 - 2.0 * t);
    [0.15 + 0.65 * t, 0.2 + 0.35 * t, 0.7 - 0.6 * t]
}

fn interior_psnr(
    mosaic_truth: impl Fn(usize, usize) -> [f32; 3],
    image: &rohditor_image::LinearRgbImage<f32>,
) -> f64 {
    let mut squared = 0.0_f64;
    let mut count = 0_usize;
    for y in 16..image.height() - 16 {
        for x in 16..image.width() - 16 {
            let expected = mosaic_truth(x, y);
            let actual = image.pixel(x, y).expect("interior pixel");
            for channel in 0..3 {
                let error = f64::from(actual[channel] - expected[channel]);
                squared += error * error;
                count += 1;
            }
        }
    }
    -10.0 * (squared / count as f64).log10()
}

/// Guards against regressing to a simplified AMaZE. The port reaches 55.3 dB
/// here against 42.9 dB for bilinear; the former simplified adaptation was
/// only a few dB above bilinear on comparable edges.
#[test]
fn amaze_reconstructs_a_soft_colour_edge_far_better_than_bilinear() -> Result<(), Box<dyn Error>> {
    let mosaic = mosaic_from_rgb(256, 192, BayerPattern::Rggb, soft_slanted_edge)?;
    let amaze = demosaic(&mosaic, UNBALANCED, DemosaicAlgorithm::Amaze)?;
    let bilinear = demosaic(&mosaic, UNBALANCED, DemosaicAlgorithm::Bilinear)?;
    let amaze_psnr = interior_psnr(soft_slanted_edge, &amaze);
    let bilinear_psnr = interior_psnr(soft_slanted_edge, &bilinear);
    assert!(amaze_psnr > 54.0, "AMaZE reached only {amaze_psnr:.2} dB");
    assert!(
        amaze_psnr > bilinear_psnr + 10.0,
        "AMaZE {amaze_psnr:.2} dB vs bilinear {bilinear_psnr:.2} dB"
    );
    Ok(())
}

const UNBALANCED: WhiteBalanceGains = WhiteBalanceGains {
    red: 2.1,
    green: 1.0,
    blue: 2.6,
};

/// A textured colour image with dim red and blue, like raw sensor data.
fn textured_unbalanced(x: usize, y: usize) -> [f32; 3] {
    let (fx, fy) = (x as f32, y as f32);
    let detail = 0.04 * (0.9 * fx).sin() * (0.7 * fy).cos();
    let edge = if x * 3 + y > 60 { 0.25 } else { 0.0 };
    [
        0.10 + 0.5 * edge + 0.4 * detail,
        0.30 + edge + detail,
        0.08 + 0.4 * edge + 0.3 * detail,
    ]
}

#[test]
fn balance_leaves_flat_images_camera_native_for_every_algorithm() -> Result<(), Box<dyn Error>> {
    for algorithm in [
        DemosaicAlgorithm::Bilinear,
        DemosaicAlgorithm::MalvarHeCutler,
        DemosaicAlgorithm::Rcd,
        DemosaicAlgorithm::Amaze,
    ] {
        for pattern in PATTERNS {
            let mosaic = mosaic_from_rgb(48, 40, pattern, |_, _| [0.15, 0.3, 0.12])?;
            let image = demosaic(&mosaic, UNBALANCED, algorithm)?;
            for pixel in image.data().as_chunks::<3>().0 {
                // The algorithms' EPSILON terms act on the dimmer balanced values.
                assert_close(pixel, &[0.15, 0.3, 0.12], 1.0e-4);
            }
        }
    }
    Ok(())
}

#[test]
fn linear_algorithms_ignore_balance_bit_for_bit() -> Result<(), Box<dyn Error>> {
    let mosaic = mosaic_from_rgb(40, 40, BayerPattern::Rggb, textured_unbalanced)?;
    for algorithm in [
        DemosaicAlgorithm::Bilinear,
        DemosaicAlgorithm::MalvarHeCutler,
    ] {
        let balanced = demosaic(&mosaic, UNBALANCED, algorithm)?;
        let plain = demosaic(&mosaic, WhiteBalanceGains::identity(), algorithm)?;
        assert_eq!(balanced.data(), plain.data(), "{algorithm:?}");
    }
    Ok(())
}

#[test]
fn directional_algorithms_use_balance_and_only_its_channel_ratios() -> Result<(), Box<dyn Error>> {
    let mosaic = mosaic_from_rgb(64, 64, BayerPattern::Grbg, textured_unbalanced)?;
    let scaled_gains = WhiteBalanceGains {
        red: UNBALANCED.red * 3.0,
        green: UNBALANCED.green * 3.0,
        blue: UNBALANCED.blue * 3.0,
    };
    for algorithm in [DemosaicAlgorithm::Rcd, DemosaicAlgorithm::Amaze] {
        let plain = demosaic(&mosaic, WhiteBalanceGains::identity(), algorithm)?;
        let balanced = demosaic(&mosaic, UNBALANCED, algorithm)?;
        let rescaled = demosaic(&mosaic, scaled_gains, algorithm)?;
        let difference = |a: &[f32], b: &[f32]| {
            a.iter()
                .zip(b)
                .map(|(x, y)| (x - y).abs())
                .fold(0.0_f32, f32::max)
        };
        assert!(
            difference(plain.data(), balanced.data()) > 1.0e-4,
            "{algorithm:?} ignored the balance"
        );
        assert!(
            difference(balanced.data(), rescaled.data()) < 1.0e-5,
            "{algorithm:?} depends on the overall gain scale"
        );
    }
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
