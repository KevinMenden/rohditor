//! Pins RCD's clean-fixture reconstruction quality to a committed JSON baseline.
//!
//! RCD's output agrees with RawTherapee and darktable to 0.01 dB on real
//! images, so freezing these numbers pins externally validated quality, not
//! self-agreement. The floors in `fixtures/rcd_clean_baseline.json` are the
//! gate; the `measured_*` values there are informational.
//!
//! What carries signal (honest split, do not overclaim):
//! - `slanted-edge`, `curved-edge`, `one-pixel-lines` and `saturated-boundary`
//!   carry the regression signal: RCD's realized margin over bilinear is
//!   >= 5.7 dB there, so any real port or arithmetic regression trips a floor.
//! - `zone-plate` and `noise` are catastrophe gates only. RCD's headroom over
//!   bilinear there is only 1.66 / 2.02 dB, so small f32/libm drift consumes
//!   most of the relative margin; this suite does not meaningfully measure
//!   zone-plate quality.
//! - `smooth-gradient` is a purity/floor guard: both outputs sit at the f32
//!   numerical floor and RCD intentionally loses to bilinear's exactness
//!   there, so its relative gate is 0.0 (skipped) and only its absolute floor
//!   guards against catastrophe.
//!
//! Provenance of every floor: measured with a 16 px interior band excluded
//! (the band covers both RCD's 10 px border ring and AMaZE's 16 px ring, and
//! immunizes these scores against border-reconstruction work in issue #22).
//! Never transplant numbers from the 2 px band scoring of
//! `demosaic_quality.rs` — the border ring costs ~7 dB on slanted-edge.
//!
//! The floors are tolerance floors, not bit-exact hashes: the fixtures use
//! libm (`cos`, `sqrt`), which is not bit-reproducible across architectures,
//! and RCD's direction selection is a discrete step that flips on tiny f32
//! differences. The 0.8 dB absolute / 0.7 dB relative margins absorb ulp-level
//! drift while any real regression (whole dBs) still fails.
//!
//! Re-pin story: an intentional RCD arithmetic change bumps `algorithm_version`
//! in `src/lib.rs`, which fails the contract test below; then run
//! `cargo test -p rohditor-demosaic --test rcd_clean_baseline -- --nocapture`,
//! verify the printed values against the reference tools (`crates/reference`,
//! `level1_clean_fixtures`), and update `measured_*`, the floors and the
//! recorded date in the JSON by hand.

use std::error::Error;

use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains, demosaic};
use rohditor_image::{BayerPattern, CfaColor, LinearRgbImage, MosaicImage};
use serde::Deserialize;

const BASELINE_JSON: &str = include_str!("fixtures/rcd_clean_baseline.json");

const WIDTH: usize = 96;
const HEIGHT: usize = 80;
const PATTERNS: [BayerPattern; 4] = [
    BayerPattern::Rggb,
    BayerPattern::Bggr,
    BayerPattern::Grbg,
    BayerPattern::Gbrg,
];
const FIXTURES: [Fixture; 7] = [
    Fixture::SlantedEdge,
    Fixture::CurvedEdge,
    Fixture::ZonePlate,
    Fixture::OnePixelLines,
    Fixture::SaturatedBoundary,
    Fixture::SmoothGradient,
    Fixture::Noise,
];

#[derive(Debug, Deserialize)]
struct Baseline {
    common: CommonBaseline,
    fixtures: Vec<FixtureBaseline>,
}

#[derive(Debug, Deserialize)]
struct CommonBaseline {
    algorithm: String,
    algorithm_version: u8,
    width: usize,
    height: usize,
    patterns: Vec<String>,
    interior_border: usize,
    balance: String,
}

#[derive(Debug, Deserialize)]
struct FixtureBaseline {
    id: String,
    measured_rcd_psnr_db: f64,
    measured_bilinear_psnr_db: f64,
    min_rcd_psnr_db: f64,
    min_improvement_over_bilinear_db: f64,
}

fn load_baseline() -> Result<Baseline, Box<dyn Error>> {
    Ok(serde_json::from_str(BASELINE_JSON)?)
}

/// Fails with a clear message whenever RCD's algorithm version is bumped for
/// an intentional change, so the committed floors get re-verified on purpose.
#[test]
fn baseline_json_describes_the_current_algorithm_contract() -> Result<(), Box<dyn Error>> {
    let baseline = load_baseline()?;
    assert_eq!(
        baseline.common.algorithm,
        DemosaicAlgorithm::Rcd.stable_name(),
        "baseline JSON pins a different algorithm than RCD"
    );
    assert_eq!(
        baseline.common.algorithm_version,
        DemosaicAlgorithm::Rcd.contract().algorithm_version(),
        "RCD's algorithm_version changed; re-verify and re-pin the JSON floors"
    );
    // Hardening: the JSON must describe exactly this test's measurement
    // geometry and fixture set, so no silent weakening by editing the JSON.
    assert_eq!(
        baseline.common.width, WIDTH,
        "baseline JSON width {} disagrees with the fixture width {WIDTH}",
        baseline.common.width
    );
    assert_eq!(
        baseline.common.height, HEIGHT,
        "baseline JSON height {} disagrees with the fixture height {HEIGHT}",
        baseline.common.height
    );
    let json_patterns = baseline
        .common
        .patterns
        .iter()
        .map(|name| BayerPattern::parse(name, 2, 2))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        json_patterns, PATTERNS,
        "baseline JSON patterns disagree with the four tested Bayer patterns (in this exact order)"
    );
    assert_eq!(
        baseline.common.balance, "identity",
        "baseline is scored at identity white balance; a different balance needs re-measured floors"
    );
    let border = baseline.common.interior_border;
    assert_eq!(
        border, 16,
        "interior_border must stay 16: every floor is band-16-specific (it excludes RCD's 10 px ring and bilinear's border policy); re-measure and re-pin if the band changes"
    );
    assert!(
        2 * border < HEIGHT.min(WIDTH),
        "interior_border {border} leaves no interior rows or columns to score (needs 2 * border < min(width, height))"
    );
    let mut json_ids: Vec<&str> = baseline
        .fixtures
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    let mut fixture_ids: Vec<&str> = FIXTURES.iter().map(|fixture| fixture.name()).collect();
    json_ids.sort_unstable();
    fixture_ids.sort_unstable();
    assert_eq!(
        json_ids, fixture_ids,
        "baseline JSON fixture ids are not exactly the tested fixture set (sorted JSON vs sorted fixtures; deletion, duplication and unknown ids all fail)"
    );
    for entry in &baseline.fixtures {
        assert!(
            entry.min_improvement_over_bilinear_db >= 0.0,
            "{}: negative min_improvement_over_bilinear_db is not a thing; 0.0 is the documented skip",
            entry.id
        );
    }
    Ok(())
}

#[test]
fn rcd_meets_the_committed_clean_fixture_baseline() -> Result<(), Box<dyn Error>> {
    let baseline = load_baseline()?;
    let border = baseline.common.interior_border;

    for entry in &baseline.fixtures {
        let fixture = FIXTURES
            .into_iter()
            .find(|candidate| candidate.name() == entry.id)
            .ok_or_else(|| format!("unknown fixture id {:?} in baseline JSON", entry.id))?;
        let ground_truth = fixture.generate();
        let samples = PATTERNS.len() * (WIDTH - 2 * border) * (HEIGHT - 2 * border) * 3;

        let mut rcd_squared = 0.0_f64;
        let mut bilinear_squared = 0.0_f64;
        for pattern in PATTERNS {
            let mosaic = mosaic_ground_truth(&ground_truth, pattern)?;
            let rcd = demosaic(
                &mosaic,
                WhiteBalanceGains::identity(),
                DemosaicAlgorithm::Rcd,
            )?;
            let bilinear = demosaic(
                &mosaic,
                WhiteBalanceGains::identity(),
                DemosaicAlgorithm::Bilinear,
            )?;
            rcd_squared += interior_squared_error(&ground_truth, &rcd, border);
            bilinear_squared += interior_squared_error(&ground_truth, &bilinear, border);
        }

        let rcd_psnr = psnr(rcd_squared, samples);
        let bilinear_psnr = psnr(bilinear_squared, samples);
        println!(
            "{:<18} rcd {:>9.4} dB (floor {:>5.1}) bilinear {:>9.4} dB delta {:+.2}",
            entry.id,
            rcd_psnr,
            entry.min_rcd_psnr_db,
            bilinear_psnr,
            rcd_psnr - bilinear_psnr,
        );

        assert!(
            rcd_psnr >= entry.min_rcd_psnr_db,
            "{}: RCD {:.4} dB is below the committed floor {:.1} dB (recorded {:.2} dB)",
            entry.id,
            rcd_psnr,
            entry.min_rcd_psnr_db,
            entry.measured_rcd_psnr_db,
        );
        // A 0.0 relative floor skips the comparison (smooth-gradient: both
        // outputs at the f32 floor, RCD intentionally loses to exact bilinear).
        if entry.min_improvement_over_bilinear_db > 0.0 {
            assert!(
                rcd_psnr >= bilinear_psnr + entry.min_improvement_over_bilinear_db,
                "{}: RCD {:.4} dB is less than {:.1} dB above bilinear {:.4} dB \
                 (recorded bilinear {:.2} dB)",
                entry.id,
                rcd_psnr,
                entry.min_improvement_over_bilinear_db,
                bilinear_psnr,
                entry.measured_bilinear_psnr_db,
            );
        }
    }
    Ok(())
}

/// Analytic fixtures identical to demosaic_quality.rs (2026-11-09);
/// change both if you change either.
#[derive(Debug, Clone, Copy)]
enum Fixture {
    SlantedEdge,
    CurvedEdge,
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
            Self::CurvedEdge => "curved-edge",
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
            Self::CurvedEdge => {
                let dx = xf - WIDTH as f32 * 0.48;
                let dy = yf - HEIGHT as f32 * 0.52;
                let radius = (dx * dx + dy * dy).sqrt();
                let blend = smooth_step((radius - 24.0) / 1.5);
                mix([0.72, 0.64, 0.55], [0.10, 0.13, 0.17], blend)
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

/// Sums squared error over pixels deeper than the border band: 16 px excludes
/// RCD's 10 px ring and bilinear's border policy, matching the AMaZE test.
fn interior_squared_error(
    ground_truth: &[[f32; 3]],
    image: &LinearRgbImage<f32>,
    border: usize,
) -> f64 {
    let mut total = 0.0_f64;
    for y in border..HEIGHT - border {
        for x in border..WIDTH - border {
            let expected = ground_truth[y * WIDTH + x];
            let actual = image.pixel(x, y).expect("interior coordinate is valid");
            for channel in 0..3 {
                let error = f64::from(expected[channel] - actual[channel]);
                total += error * error;
            }
        }
    }
    total
}

fn psnr(squared_error: f64, samples: usize) -> f64 {
    -10.0 * (squared_error / samples as f64).log10()
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
