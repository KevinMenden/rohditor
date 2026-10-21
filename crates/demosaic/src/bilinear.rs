use rayon::prelude::*;

use super::{
    CancellationCheck, DemosaicError, WhiteBalanceGains, checkpoint, require_finite_output,
};
use rohditor_image::{CfaColor, MosaicImage};

const CROSS_OFFSETS: [(isize, isize); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
const DIAGONAL_OFFSETS: [(isize, isize); 4] = [(-1, -1), (1, -1), (-1, 1), (1, 1)];
const HORIZONTAL_OFFSETS: [(isize, isize); 2] = [(-1, 0), (1, 0)];
const VERTICAL_OFFSETS: [(isize, isize); 2] = [(0, -1), (0, 1)];

pub(super) fn reconstruct(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    cancellation: &dyn CancellationCheck,
    row_stride: usize,
    output: &mut [f32],
) -> Result<(), DemosaicError> {
    reconstruct_border(mosaic, gains, 0, cancellation, row_stride, output)
}

/// Reconstruct everything except the interior that starts `skip` pixels from
/// every edge. Higher quality algorithms overwrite that interior and keep
/// this result for the outer ring, so computing it there would be wasted work.
/// `skip = 0` reconstructs the whole image; otherwise the image must be larger
/// than `2 * skip` in both directions.
pub(super) fn reconstruct_border(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    skip: usize,
    cancellation: &dyn CancellationCheck,
    row_stride: usize,
    output: &mut [f32],
) -> Result<(), DemosaicError> {
    let (width, height) = (mosaic.width(), mosaic.height());
    output.par_chunks_mut(row_stride).enumerate().try_for_each(
        |(y, output_row)| -> Result<(), DemosaicError> {
            checkpoint(cancellation)?;
            let pixels = output_row.as_chunks_mut::<3>().0;
            let mut fill = |x: usize| -> Result<(), DemosaicError> {
                let mut rgb = reconstruct_pixel(mosaic, x, y);
                gains.apply(&mut rgb);
                require_finite_output(&rgb, x, y)?;
                pixels[x].copy_from_slice(&rgb);
                Ok(())
            };
            if skip == 0 || y < skip || y >= height - skip {
                (0..width).try_for_each(fill)
            } else {
                (0..skip).chain(width - skip..width).try_for_each(&mut fill)
            }
        },
    )
}

pub(super) fn reconstruct_pixel(mosaic: &MosaicImage<f32>, x: usize, y: usize) -> [f32; 3] {
    match mosaic.pattern().color_at(x, y) {
        CfaColor::Red => [
            *mosaic.sample(x, y),
            average_offsets(mosaic, x, y, &CROSS_OFFSETS),
            average_offsets(mosaic, x, y, &DIAGONAL_OFFSETS),
        ],
        CfaColor::Blue => [
            average_offsets(mosaic, x, y, &DIAGONAL_OFFSETS),
            average_offsets(mosaic, x, y, &CROSS_OFFSETS),
            *mosaic.sample(x, y),
        ],
        CfaColor::Green => {
            let red_horizontal = mosaic.pattern().color_at(x.wrapping_add(1), y) == CfaColor::Red;
            let (red_offsets, blue_offsets) = if red_horizontal {
                (&HORIZONTAL_OFFSETS[..], &VERTICAL_OFFSETS[..])
            } else {
                (&VERTICAL_OFFSETS[..], &HORIZONTAL_OFFSETS[..])
            };
            [
                average_offsets(mosaic, x, y, red_offsets),
                *mosaic.sample(x, y),
                average_offsets(mosaic, x, y, blue_offsets),
            ]
        }
    }
}

fn average_offsets(
    mosaic: &MosaicImage<f32>,
    x: usize,
    y: usize,
    offsets: &[(isize, isize)],
) -> f32 {
    let mut sum = 0.0;
    let mut count = 0_u8;
    for &(offset_x, offset_y) in offsets {
        let Some(neighbor_x) = x.checked_add_signed(offset_x) else {
            continue;
        };
        let Some(neighbor_y) = y.checked_add_signed(offset_y) else {
            continue;
        };
        if neighbor_x < mosaic.width() && neighbor_y < mosaic.height() {
            sum += *mosaic.sample(neighbor_x, neighbor_y);
            count += 1;
        }
    }
    if count == 0 {
        *mosaic.sample(x, y)
    } else {
        sum / f32::from(count)
    }
}
