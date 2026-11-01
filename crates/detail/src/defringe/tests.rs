use super::*;
use crate::NeverCancelled;
use rohditor_image::LinearRgbSpace;

const BALANCE: [f32; 3] = [2.0, 1.0, 1.5];
const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
const WIDTH: usize = 40;
const HEIGHT: usize = 7;
const STRIDE: usize = WIDTH * 3 + 5;
/// Dark (x < EDGE) meets bright (x >= EDGE); purple fringes the three dark
/// pixels before the edge, green the three bright ones after it.
const EDGE: usize = 20;

fn params(purple: f32, green: f32) -> FringeRemoval {
    FringeRemoval {
        purple: FringeColour {
            amount: purple,
            hue: [270.0, 330.0],
        },
        green: FringeColour {
            amount: green,
            hue: [75.0, 150.0],
        },
        balance: BALANCE,
        to_rec2020: IDENTITY,
        ceilings: [f32::INFINITY; 3],
        noise: None,
    }
}

/// Build an image from balanced values (so neutral grey has no chroma).
fn image(f: impl Fn(usize, usize) -> [f32; 3]) -> LinearRgbImage<f32> {
    let mut data = vec![0.0_f32; STRIDE * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let balanced = f(x, y);
            for c in 0..3 {
                data[y * STRIDE + x * 3 + c] = balanced[c] / BALANCE[c];
            }
        }
    }
    LinearRgbImage::new(WIDTH, HEIGHT, STRIDE, LinearRgbSpace::CameraNative, data).unwrap()
}

fn fringed_edge() -> LinearRgbImage<f32> {
    image(|x, _| match x {
        17..=19 => [0.28, 0.01, 0.28],
        20..=22 => [0.78, 0.9, 0.78],
        _ if x < EDGE => [0.03; 3],
        _ => [0.9; 3],
    })
}

fn balanced(image: &LinearRgbImage<f32>, x: usize, y: usize) -> [f32; 3] {
    let px = image.pixel(x, y).unwrap();
    std::array::from_fn(|c| px[c] * BALANCE[c])
}

fn chroma(image: &LinearRgbImage<f32>, x: usize, y: usize) -> f32 {
    let [r, g, b] = balanced(image, x, y);
    (r - g).abs().max((b - g).abs())
}

fn luma(image: &LinearRgbImage<f32>, x: usize, y: usize) -> f32 {
    let [r, g, b] = balanced(image, x, y);
    (r + 2.0 * g + b) * 0.25
}

#[test]
fn zero_amounts_leave_the_image_bit_identical() {
    let mut fringed = fringed_edge();
    let before = fringed.clone();
    remove_fringes(&mut fringed, params(0.0, 0.0), &NeverCancelled).unwrap();
    assert_eq!(fringed, before);
}

#[test]
fn full_amount_removes_at_least_80_percent_of_the_fringe_chroma() {
    let before = fringed_edge();
    let mut fixed = before.clone();
    remove_fringes(&mut fixed, params(1.0, 1.0), &NeverCancelled).unwrap();
    for y in 0..HEIGHT {
        for x in 17..=22 {
            assert!(
                chroma(&fixed, x, y) <= 0.2 * chroma(&before, x, y),
                "({x},{y}): {} of {}",
                chroma(&fixed, x, y),
                chroma(&before, x, y)
            );
            // Luma is kept, so edge sharpness and brightness are too.
            assert!(
                (luma(&fixed, x, y) - luma(&before, x, y)).abs() < 1e-5,
                "({x},{y})"
            );
        }
        // Everything outside the fringes is untouched.
        for x in (0..17).chain(23..WIDTH) {
            assert_eq!(fixed.pixel(x, y), before.pixel(x, y), "({x},{y})");
        }
    }
}

#[test]
fn half_amount_halves_the_chroma() {
    let before = fringed_edge();
    let mut fixed = before.clone();
    remove_fringes(&mut fixed, params(0.5, 0.0), &NeverCancelled).unwrap();
    // x = 17 has a fully neutral neighbour in its window: replacement is 0.
    let expected = 0.5 * chroma(&before, 17, 3);
    assert!((chroma(&fixed, 17, 3) - expected).abs() < 1e-5);
}

#[test]
fn each_colour_is_corrected_on_its_own() {
    let before = fringed_edge();
    let mut purple_only = before.clone();
    remove_fringes(&mut purple_only, params(1.0, 0.0), &NeverCancelled).unwrap();
    assert!(chroma(&purple_only, 18, 2) < 0.05);
    for x in 20..=22 {
        assert_eq!(purple_only.pixel(x, 2), before.pixel(x, 2));
    }
    let mut green_only = before.clone();
    remove_fringes(&mut green_only, params(0.0, 1.0), &NeverCancelled).unwrap();
    assert!(chroma(&green_only, 21, 2) < 0.025);
    for x in 17..=19 {
        assert_eq!(green_only.pixel(x, 2), before.pixel(x, 2));
    }
}

#[test]
fn a_purple_patch_without_an_edge_is_untouched() {
    let mut flat = image(|_, _| [0.3, 0.05, 0.3]);
    let before = flat.clone();
    remove_fringes(&mut flat, params(1.0, 1.0), &NeverCancelled).unwrap();
    assert_eq!(flat, before);
}

#[test]
fn blown_and_non_finite_pixels_are_copied_through() {
    let mut fringed = fringed_edge();
    let blown = 3 * STRIDE + 18 * 3;
    fringed.data_mut()[blown] += 0.5;
    let non_finite = 4 * STRIDE + 21 * 3;
    fringed.data_mut()[non_finite] = f32::NAN;
    let before = fringed.clone();
    let mut p = params(1.0, 1.0);
    p.ceilings = [fringed.data()[blown], f32::INFINITY, f32::INFINITY];
    remove_fringes(&mut fringed, p, &NeverCancelled).unwrap();
    assert_eq!(fringed.pixel(18, 3), before.pixel(18, 3));
    let (a, b) = (fringed.pixel(21, 4).unwrap(), before.pixel(21, 4).unwrap());
    assert!(a[0].is_nan() && a[1..] == b[1..]);
    assert_eq!(fringed.data().iter().filter(|v| v.is_nan()).count(), 1);
    // The neighbours were still corrected.
    assert!(chroma(&fringed, 19, 3) < 0.05);
}

#[test]
fn the_hue_matrix_decides_what_counts_as_purple() {
    // Swapping red and green in the matrix turns purple fringes into
    // yellow-green ones, so the purple amount no longer fires.
    let swap = [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    let before = fringed_edge();
    let mut fixed = before.clone();
    let mut p = params(1.0, 0.0);
    p.to_rec2020 = swap;
    remove_fringes(&mut fixed, p, &NeverCancelled).unwrap();
    assert_eq!(fixed, before);
}

#[test]
fn rejects_invalid_parameters_and_honours_cancellation() {
    let mut fringed = fringed_edge();
    let mut bad = params(1.0, 0.0);
    bad.balance[1] = 0.0;
    assert!(remove_fringes(&mut fringed, bad, &NeverCancelled).is_err());
    let mut bad = params(1.5, 0.0);
    assert!(remove_fringes(&mut fringed, bad, &NeverCancelled).is_err());
    bad = params(1.0, 0.0);
    bad.purple.hue = [330.0, 270.0];
    assert!(remove_fringes(&mut fringed, bad, &NeverCancelled).is_err());
    bad = params(1.0, 0.0);
    bad.to_rec2020[0][0] = f32::NAN;
    assert!(remove_fringes(&mut fringed, bad, &NeverCancelled).is_err());
    assert_eq!(
        remove_fringes(&mut fringed, params(1.0, 1.0), &|| true),
        Err(DetailError::Cancelled)
    );
}

#[test]
fn scratch_accounting_is_checked() {
    assert_eq!(defringe_scratch_bytes(10, 5), Some(10 * 5 * 3 * 4));
    assert_eq!(defringe_scratch_bytes(usize::MAX, 2), None);
}
