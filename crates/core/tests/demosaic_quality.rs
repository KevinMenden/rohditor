use std::error::Error;

use rohditor_core::{
    BayerPattern, CfaColor, DemosaicAlgorithm, LinearRgbImage, MosaicImage, WhiteBalanceGains,
    demosaic,
};

const WIDTH: usize = 96;
const HEIGHT: usize = 80;
const PATTERNS: [BayerPattern; 4] = [
    BayerPattern::Rggb,
    BayerPattern::Bggr,
    BayerPattern::Grbg,
    BayerPattern::Gbrg,
];
const FIXTURES: [Fixture; 6] = [
    Fixture::SlantedEdge,
    Fixture::ZonePlate,
    Fixture::OnePixelLines,
    Fixture::SaturatedBoundary,
    Fixture::SmoothGradient,
    Fixture::Noise,
];

#[test]
#[ignore = "Phase 9 acceptance gate; run explicitly while MHC remains non-default"]
fn generated_ground_truth_suite_records_demosaic_quality() -> Result<(), Box<dyn Error>> {
    let mut bilinear_error = 0.0_f64;
    let mut mhc_error = 0.0_f64;
    let mut sample_count = 0_usize;
    let mut worst_regression = ("", f64::NEG_INFINITY);

    for fixture in FIXTURES {
        let ground_truth = fixture.generate();
        let mut fixture_bilinear_error = 0.0_f64;
        let mut fixture_mhc_error = 0.0_f64;
        let mut fixture_samples = 0_usize;
        let mut bilinear_channel_error = [0.0_f64; 3];
        let mut mhc_channel_error = [0.0_f64; 3];

        for pattern in PATTERNS {
            let mosaic = mosaic_ground_truth(&ground_truth, pattern)?;
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
            accumulate_squared_error(
                &ground_truth,
                &bilinear,
                &mut fixture_bilinear_error,
                &mut bilinear_channel_error,
            );
            accumulate_squared_error(
                &ground_truth,
                &mhc,
                &mut fixture_mhc_error,
                &mut mhc_channel_error,
            );
            fixture_samples += WIDTH * HEIGHT * 3;
        }

        let bilinear_psnr = psnr(fixture_bilinear_error, fixture_samples);
        let mhc_psnr = psnr(fixture_mhc_error, fixture_samples);
        let channel_samples = fixture_samples / 3;
        println!(
            "{:<18} bilinear {:>7.3} dB RMSE {:?}; mhc {:>7.3} dB RMSE {:?}; delta {:+.3} dB",
            fixture.name(),
            bilinear_psnr,
            channel_rmse(bilinear_channel_error, channel_samples),
            mhc_psnr,
            channel_rmse(mhc_channel_error, channel_samples),
            mhc_psnr - bilinear_psnr,
        );

        let regression = bilinear_psnr - mhc_psnr;
        if regression > worst_regression.1 {
            worst_regression = (fixture.name(), regression);
        }
        bilinear_error += fixture_bilinear_error;
        mhc_error += fixture_mhc_error;
        sample_count += fixture_samples;
    }

    let bilinear_psnr = psnr(bilinear_error, sample_count);
    let mhc_psnr = psnr(mhc_error, sample_count);
    println!(
        "aggregate          bilinear {bilinear_psnr:>7.3} dB; mhc {mhc_psnr:>7.3} dB; delta {:+.3} dB",
        mhc_psnr - bilinear_psnr
    );
    // This report is also the executable acceptance gate. It remains ignored
    // while MHC is non-default so ordinary workspace checks stay green, but an
    // explicit Phase 9 gate run must reject an undocumented fixture regression.
    assert!(
        worst_regression.1 <= 0.25,
        "{} regressed by {:.3} dB",
        worst_regression.0,
        worst_regression.1
    );
    assert!(
        mhc_psnr - bilinear_psnr >= 3.0,
        "aggregate MHC improvement was only {:.3} dB",
        mhc_psnr - bilinear_psnr
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum Fixture {
    SlantedEdge,
    ZonePlate,
    OnePixelLines,
    SaturatedBoundary,
    SmoothGradient,
    Noise,
}

impl Fixture {
    const fn name(self) -> &'static str {
        match self {
            Self::SlantedEdge => "slanted-edge",
            Self::ZonePlate => "zone-plate",
            Self::OnePixelLines => "one-pixel-lines",
            Self::SaturatedBoundary => "saturated-boundary",
            Self::SmoothGradient => "smooth-gradient",
            Self::Noise => "noise",
        }
    }

    fn generate(self) -> Vec<[f32; 3]> {
        let mut pixels = Vec::with_capacity(WIDTH * HEIGHT);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                pixels.push(self.rgb_at(x, y));
            }
        }
        pixels
    }

    fn rgb_at(self, x: usize, y: usize) -> [f32; 3] {
        let xf = x as f32;
        let yf = y as f32;
        match self {
            Self::SlantedEdge => {
                let blend = smooth_step((xf - 0.37 * yf - 31.0) / 1.5);
                mix([0.08, 0.11, 0.14], [0.82, 0.76, 0.68], blend)
            }
            Self::ZonePlate => {
                let nx = (xf - WIDTH as f32 * 0.5) / WIDTH as f32;
                let ny = (yf - HEIGHT as f32 * 0.5) / HEIGHT as f32;
                let wave = (700.0 * (nx * nx + ny * ny)).cos();
                let luminance = 0.5 + 0.38 * wave;
                [luminance * 0.96 + 0.015, luminance, luminance * 1.03 - 0.01]
            }
            Self::OnePixelLines => {
                let line = x % 17 == 3 || y % 19 == 5 || (x + y * 2) % 29 == 7;
                let luminance = if line { 0.88 } else { 0.09 };
                [luminance, luminance * 0.98, luminance * 0.94 + 0.01]
            }
            Self::SaturatedBoundary => {
                let blend = smooth_step((xf + 0.28 * yf - 55.0) / 1.25);
                mix([0.92, 0.08, 0.04], [0.03, 0.16, 0.94], blend)
            }
            Self::SmoothGradient => [
                0.08 + 0.72 * xf / (WIDTH - 1) as f32,
                0.12 + 0.66 * yf / (HEIGHT - 1) as f32,
                0.18 + 0.55 * (xf + yf) / (WIDTH + HEIGHT - 2) as f32,
            ],
            Self::Noise => {
                let common = hash_noise(x, y, 0) - 0.5;
                let chroma = hash_noise(x, y, 1) - 0.5;
                [
                    0.42 + common * 0.18 + chroma * 0.015,
                    0.44 + common * 0.17,
                    0.47 + common * 0.16 - chroma * 0.015,
                ]
            }
        }
    }
}

fn mosaic_ground_truth(
    ground_truth: &[[f32; 3]],
    pattern: BayerPattern,
) -> Result<MosaicImage<f32>, Box<dyn Error>> {
    let data = ground_truth
        .iter()
        .enumerate()
        .map(|(index, rgb)| {
            let x = index % WIDTH;
            let y = index / WIDTH;
            rgb[channel_index(pattern.color_at(x, y))]
        })
        .collect();
    Ok(MosaicImage::new(WIDTH, HEIGHT, WIDTH, pattern, data)?)
}

fn accumulate_squared_error(
    expected: &[[f32; 3]],
    actual: &LinearRgbImage<f32>,
    total: &mut f64,
    channels: &mut [f64; 3],
) {
    for (expected, actual) in expected.iter().zip(actual.data().chunks_exact(3)) {
        for channel in 0..3 {
            let error = f64::from(expected[channel] - actual[channel]);
            let squared = error * error;
            *total += squared;
            channels[channel] += squared;
        }
    }
}

fn psnr(squared_error: f64, samples: usize) -> f64 {
    -10.0 * (squared_error / samples as f64).log10()
}

fn channel_rmse(squared_error: [f64; 3], samples: usize) -> [f64; 3] {
    squared_error.map(|error| (error / samples as f64).sqrt())
}

fn smooth_step(value: f32) -> f32 {
    let value = (value * 0.5 + 0.5).clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn mix(left: [f32; 3], right: [f32; 3], amount: f32) -> [f32; 3] {
    [
        left[0] + amount * (right[0] - left[0]),
        left[1] + amount * (right[1] - left[1]),
        left[2] + amount * (right[2] - left[2]),
    ]
}

fn hash_noise(x: usize, y: usize, stream: u32) -> f32 {
    let mut value = (x as u32).wrapping_mul(0x9e37_79b9)
        ^ (y as u32).wrapping_mul(0x85eb_ca6b)
        ^ stream.wrapping_mul(0xc2b2_ae35);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^= value >> 16;
    value as f32 / u32::MAX as f32
}

const fn channel_index(color: CfaColor) -> usize {
    match color {
        CfaColor::Red => 0,
        CfaColor::Green => 1,
        CfaColor::Blue => 2,
    }
}
