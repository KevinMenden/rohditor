use rayon::prelude::*;

use super::{
    CancellationCheck, DemosaicError, MALVAR_HE_CUTLER_HALO, WhiteBalanceGains, bilinear,
    checkpoint, require_finite_output,
};
use rohditor_image::{CfaColor, MosaicImage};

/// MHC uses a two-pixel neighborhood. The initial deterministic border policy
/// reconstructs this outer band with the bilinear reference implementation.
const HALO: usize = MALVAR_HE_CUTLER_HALO.left;

// Figure 2 of Malvar, He, and Cutler (ICASSP 2004), represented as exact
// integer multiples of 1/16. Symmetry and transposition produce all eight
// interpolation cases from these three kernels.
const GREEN_AT_RED_OR_BLUE_X16: [[i8; 5]; 5] = [
    [0, 0, -2, 0, 0],
    [0, 0, 4, 0, 0],
    [-2, 4, 8, 4, -2],
    [0, 0, 4, 0, 0],
    [0, 0, -2, 0, 0],
];

const COLOR_AT_GREEN_SAME_ROW_X16: [[i8; 5]; 5] = [
    [0, 0, 1, 0, 0],
    [0, -2, 0, -2, 0],
    [-2, 8, 10, 8, -2],
    [0, -2, 0, -2, 0],
    [0, 0, 1, 0, 0],
];

const RED_AT_BLUE_OR_BLUE_AT_RED_X16: [[i8; 5]; 5] = [
    [0, 0, -3, 0, 0],
    [0, 4, 0, 4, 0],
    [-3, 0, 12, 0, -3],
    [0, 4, 0, 4, 0],
    [0, 0, -3, 0, 0],
];

pub(super) fn reconstruct(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    cancellation: &dyn CancellationCheck,
    row_stride: usize,
    output: &mut [f32],
) -> Result<(), DemosaicError> {
    output.par_chunks_mut(row_stride).enumerate().try_for_each(
        |(y, output_row)| -> Result<(), DemosaicError> {
            checkpoint(cancellation)?;
            for (x, pixel) in output_row.chunks_exact_mut(3).enumerate() {
                let mut rgb = if is_border(mosaic, x, y) {
                    bilinear::reconstruct_pixel(mosaic, x, y)
                } else {
                    reconstruct_interior_pixel(mosaic, x, y)
                };
                gains.apply(&mut rgb);
                require_finite_output(&rgb, x, y)?;
                pixel.copy_from_slice(&rgb);
            }
            Ok(())
        },
    )
}

fn is_border(mosaic: &MosaicImage<f32>, x: usize, y: usize) -> bool {
    x < HALO
        || y < HALO
        || x >= mosaic.width().saturating_sub(HALO)
        || y >= mosaic.height().saturating_sub(HALO)
}

fn reconstruct_interior_pixel(mosaic: &MosaicImage<f32>, x: usize, y: usize) -> [f32; 3] {
    let measured = *mosaic.sample(x, y);
    match mosaic.pattern().color_at(x, y) {
        CfaColor::Red => [
            measured,
            apply_kernel(mosaic, x, y, &GREEN_AT_RED_OR_BLUE_X16, false),
            apply_kernel(mosaic, x, y, &RED_AT_BLUE_OR_BLUE_AT_RED_X16, false),
        ],
        CfaColor::Blue => [
            apply_kernel(mosaic, x, y, &RED_AT_BLUE_OR_BLUE_AT_RED_X16, false),
            apply_kernel(mosaic, x, y, &GREEN_AT_RED_OR_BLUE_X16, false),
            measured,
        ],
        CfaColor::Green => {
            let red_horizontal = mosaic.pattern().color_at(x + 1, y) == CfaColor::Red;
            [
                apply_kernel(mosaic, x, y, &COLOR_AT_GREEN_SAME_ROW_X16, !red_horizontal),
                measured,
                apply_kernel(mosaic, x, y, &COLOR_AT_GREEN_SAME_ROW_X16, red_horizontal),
            ]
        }
    }
}

fn apply_kernel(
    mosaic: &MosaicImage<f32>,
    x: usize,
    y: usize,
    coefficients_x16: &[[i8; 5]; 5],
    transpose: bool,
) -> f32 {
    let mut sum = 0.0;
    for (kernel_y, row) in coefficients_x16.iter().enumerate() {
        for (kernel_x, &direct_coefficient) in row.iter().enumerate() {
            let coefficient = if transpose {
                coefficients_x16[kernel_x][kernel_y]
            } else {
                direct_coefficient
            };
            if coefficient != 0 {
                let sample_x = x + kernel_x - HALO;
                let sample_y = y + kernel_y - HALO;
                sum += f32::from(coefficient) * mosaic.sample(sample_x, sample_y);
            }
        }
    }
    sum * (1.0 / 16.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_image::BayerPattern;

    #[test]
    fn published_kernel_impulses_have_exact_coefficients() {
        let cases = [
            (&GREEN_AT_RED_OR_BLUE_X16, false, (2, 2), 0.5),
            (&GREEN_AT_RED_OR_BLUE_X16, false, (2, 0), -0.125),
            (&GREEN_AT_RED_OR_BLUE_X16, false, (2, 1), 0.25),
            (&COLOR_AT_GREEN_SAME_ROW_X16, false, (2, 2), 0.625),
            (&COLOR_AT_GREEN_SAME_ROW_X16, false, (1, 2), 0.5),
            (&COLOR_AT_GREEN_SAME_ROW_X16, true, (2, 1), 0.5),
            (&RED_AT_BLUE_OR_BLUE_AT_RED_X16, false, (2, 2), 0.75),
            (&RED_AT_BLUE_OR_BLUE_AT_RED_X16, false, (1, 1), 0.25),
            (&RED_AT_BLUE_OR_BLUE_AT_RED_X16, false, (2, 0), -0.1875),
        ];

        for (kernel, transpose, (impulse_x, impulse_y), expected) in cases {
            let mut data = vec![0.0; 25];
            data[impulse_y * 5 + impulse_x] = 1.0;
            let mosaic =
                MosaicImage::new(5, 5, 5, BayerPattern::Rggb, data).expect("valid impulse mosaic");
            assert_eq!(apply_kernel(&mosaic, 2, 2, kernel, transpose), expected);
        }
    }
}
