//! Pins AMaZE's exact output on a synthetic mosaic.
//!
//! AMaZE is a stage-by-stage port of RawTherapee's `amaze_demosaic_RT.cc`, and
//! performance work on it is only safe if the arithmetic stays identical. The
//! input is built from integer arithmetic only (no libm), so the hashes are
//! the same on every platform. It has fine texture, noise, hard edges and
//! clipped patches, so it reaches the Nyquist, saturation and highlight
//! branches, and spans several tiles.
//!
//! If a change to the algorithm is intentional, run this test, check the
//! output against the reference tools (`crates/reference`), and replace the
//! expected hashes below with the ones the failure message prints.

use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains, demosaic};
use rohditor_image::{BayerPattern, MosaicImage};

const WIDTH: usize = 513;
const HEIGHT: usize = 389;

fn synthetic_mosaic(pattern: BayerPattern) -> MosaicImage<f32> {
    let mut state = 12_345_u32;
    let data = (0..WIDTH * HEIGHT)
        .map(|index| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (state >> 8) as f32 / 16_777_216.0;
            let (x, y) = (index % WIDTH, index / WIDTH);
            // Diagonal stripes of period 7 and a slow gradient, plus noise.
            let stripes = ((x * 3 + y * 5) % 7) as f32 / 7.0;
            let gradient = (x + 2 * y) as f32 / (3.0 * WIDTH as f32);
            let value = 0.1 + 0.5 * stripes + 0.3 * gradient + 0.2 * (noise - 0.5);
            if (y / 40 + x / 60) % 5 == 0 {
                1.1 // clipped patch
            } else if (x / 90 + y / 70) % 4 == 1 {
                0.35 + 0.1 * noise // flat, dim region
            } else {
                value.max(-0.05)
            }
        })
        .collect();
    MosaicImage::new(WIDTH, HEIGHT, WIDTH, pattern, data).expect("valid synthetic mosaic")
}

/// FNV-1a over the bit patterns of every sample.
fn hash(values: &[f32]) -> u64 {
    values.iter().fold(0xcbf2_9ce4_8422_2325, |hash, value| {
        (hash ^ u64::from(value.to_bits())).wrapping_mul(0x0100_0000_01b3)
    })
}

#[test]
fn amaze_output_matches_the_pinned_hashes() {
    let balance = WhiteBalanceGains {
        red: 2.1,
        green: 1.0,
        blue: 2.6,
    };
    let mut actual = Vec::new();
    for pattern in [
        BayerPattern::Rggb,
        BayerPattern::Bggr,
        BayerPattern::Grbg,
        BayerPattern::Gbrg,
    ] {
        let mosaic = synthetic_mosaic(pattern);
        for (label, gains) in [
            ("identity", WhiteBalanceGains::identity()),
            ("balanced", balance),
        ] {
            let image = demosaic(&mosaic, gains, DemosaicAlgorithm::Amaze).expect("AMaZE succeeds");
            actual.push(format!("{pattern:?} {label} {:016x}", hash(image.data())));
        }
    }
    let expected = [
        "Rggb identity 82901f8b6efe2140",
        "Rggb balanced 705390860f10ba6e",
        "Bggr identity 76f5f3df21953518",
        "Bggr balanced f10c556f00cf50b5",
        "Grbg identity a353fcb22289c4bb",
        "Grbg balanced 1d34ec0c26a1010e",
        "Gbrg identity 4d11566d39adde2b",
        "Gbrg balanced ce58ea2860d154b7",
    ];
    assert_eq!(
        actual, expected,
        "AMaZE output changed; see the module documentation before updating"
    );
}
