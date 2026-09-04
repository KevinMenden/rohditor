//! Ratio-corrected directional demosaicing.
//!
//! The implementation follows the tiled RCD formulation used by RawTherapee
//! and darktable. It keeps the algorithm independent of RAW decoding and uses
//! bilinear reconstruction for the pixels where a complete RCD neighborhood
//! is not available.

use super::{
    CancellationCheck, DemosaicError, RCD_HALO, WhiteBalanceGains, bilinear, checkpoint,
    require_finite_output,
};
use rohditor_image::{BayerPattern, MosaicImage, allocate_zeroed_f32};

use super::rcd_stages::{
    calculate_low_pass, find_directions, interpolate_green, interpolate_red_blue,
};

// RCD is derived from the GPL implementations maintained by RawTherapee and
// darktable. The directional formulas originate with Luis Sanz Rodríguez;
// the tiled implementation was developed with Ingo Weyrich and Hanno Schwalm.
// See https://github.com/darktable-org/darktable/blob/master/src/iop/demosaicing/rcd.c
// and https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/rcd_demosaic.cc.

pub(super) const BORDER: usize = RCD_HALO.left;
pub(super) const TILE_SIZE: usize = 194;
pub(super) const TILE_VALID: usize = TILE_SIZE - 2 * BORDER;
pub(super) const PLANE_ELEMENTS: usize = TILE_SIZE * TILE_SIZE;
pub(super) const HALF_PLANE_ELEMENTS: usize = PLANE_ELEMENTS / 2;
pub(super) const EPSILON: f32 = 1.0e-5;
pub(super) const EPSILON_SQUARED: f32 = 1.0e-10;

pub(super) fn reconstruct(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    cancellation: &dyn CancellationCheck,
    row_stride: usize,
    output: &mut [f32],
) -> Result<(), DemosaicError> {
    // This gives RCD a deterministic edge policy and means narrow images do
    // not need special-case indexing in the directional stages.
    bilinear::reconstruct(mosaic, gains, cancellation, row_stride, output)?;

    if mosaic.width() <= 2 * BORDER + 4 || mosaic.height() <= 2 * BORDER + 4 {
        return Ok(());
    }

    let horizontal_tiles = tile_count(mosaic.width());
    let vertical_tiles = tile_count(mosaic.height());
    let mut scratch = RcdScratch::new()?;

    for tile_y in 0..vertical_tiles {
        let origin_y = tile_y * TILE_VALID;
        let tile_height = TILE_SIZE.min(mosaic.height() - origin_y);
        for tile_x in 0..horizontal_tiles {
            checkpoint(cancellation)?;
            let origin_x = tile_x * TILE_VALID;
            let tile_width = TILE_SIZE.min(mosaic.width() - origin_x);

            // A small last tile is still covered by the bilinear base image.
            // Keeping it out of the RCD stages avoids partial-tile state being
            // mistaken for a complete directional neighborhood.
            if tile_width <= 2 * BORDER + 4 || tile_height <= 2 * BORDER + 4 {
                continue;
            }

            scratch.clear();
            let tile = RcdTile {
                origin_x,
                origin_y,
                width: tile_width,
                height: tile_height,
                pattern: mosaic.pattern().shifted(origin_x, origin_y),
            };
            populate(&mut scratch, mosaic, tile, cancellation)?;
            find_directions(&mut scratch, tile.width, tile.height, cancellation)?;
            calculate_low_pass(
                &mut scratch,
                tile.pattern,
                tile.width,
                tile.height,
                cancellation,
            )?;
            interpolate_green(
                &mut scratch,
                tile.pattern,
                tile.width,
                tile.height,
                cancellation,
            )?;
            interpolate_red_blue(
                &mut scratch,
                tile.pattern,
                tile.width,
                tile.height,
                cancellation,
            )?;
            write_tile(
                &scratch,
                mosaic,
                gains,
                tile,
                row_stride,
                output,
                cancellation,
            )?;
        }
    }
    Ok(())
}

fn tile_count(length: usize) -> usize {
    1 + (length - 2 * BORDER - 1) / TILE_VALID
}

#[derive(Clone, Copy)]
struct RcdTile {
    origin_x: usize,
    origin_y: usize,
    width: usize,
    height: usize,
    pattern: BayerPattern,
}

pub(super) struct RcdScratch {
    pub(super) cfa: Vec<f32>,
    pub(super) rgb: [Vec<f32>; 3],
    pub(super) vh_direction: Vec<f32>,
    pub(super) pq_direction: Vec<f32>,
    pub(super) p_color_difference: Vec<f32>,
    pub(super) q_color_difference: Vec<f32>,
    pub(super) vertical_buffer: [Vec<f32>; 3],
    pub(super) horizontal_buffer: Vec<f32>,
}

impl RcdScratch {
    fn new() -> Result<Self, DemosaicError> {
        let plane = || allocate_zeroed_f32(PLANE_ELEMENTS).map_err(DemosaicError::from);
        let half_plane = || allocate_zeroed_f32(HALF_PLANE_ELEMENTS).map_err(DemosaicError::from);
        let line = || allocate_zeroed_f32(TILE_SIZE).map_err(DemosaicError::from);

        Ok(Self {
            cfa: plane()?,
            rgb: [plane()?, plane()?, plane()?],
            vh_direction: plane()?,
            pq_direction: half_plane()?,
            p_color_difference: half_plane()?,
            q_color_difference: half_plane()?,
            vertical_buffer: [line()?, line()?, line()?],
            horizontal_buffer: line()?,
        })
    }

    fn clear(&mut self) {
        self.cfa.fill(0.0);
        for channel in &mut self.rgb {
            channel.fill(0.0);
        }
        self.vh_direction.fill(0.0);
        self.pq_direction.fill(0.0);
        self.p_color_difference.fill(0.0);
        self.q_color_difference.fill(0.0);
        for line in &mut self.vertical_buffer {
            line.fill(0.0);
        }
        self.horizontal_buffer.fill(0.0);
    }
}

fn populate(
    scratch: &mut RcdScratch,
    mosaic: &MosaicImage<f32>,
    tile: RcdTile,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for row in 0..tile.height {
        checkpoint(cancellation)?;
        let first_color = tile.pattern.color_at(0, row).channel_index();
        let second_color = tile.pattern.color_at(1, row).channel_index();
        for col in 0..tile.width {
            let value = *mosaic.sample(tile.origin_x + col, tile.origin_y + row);
            let index = row * TILE_SIZE + col;

            // The original implementations use a non-negative working CFA
            // image. Keep the crate's measured sample unchanged for final
            // output, while retaining signed samples in the working planes so
            // the CPU pipeline does not silently clip its input.
            scratch.cfa[index] = value;
            scratch.rgb[first_color][index] = value;
            scratch.rgb[second_color][index] = value;
        }
    }
    Ok(())
}

fn write_tile(
    scratch: &RcdScratch,
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    tile: RcdTile,
    row_stride: usize,
    output: &mut [f32],
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for local_y in BORDER..tile.height - BORDER {
        checkpoint(cancellation)?;
        for local_x in BORDER..tile.width - BORDER {
            let index = local_y * TILE_SIZE + local_x;
            let global_x = tile.origin_x + local_x;
            let global_y = tile.origin_y + local_y;
            let measured_channel = mosaic
                .pattern()
                .color_at(global_x, global_y)
                .channel_index();
            let mut rgb = [
                scratch.rgb[0][index],
                scratch.rgb[1][index],
                scratch.rgb[2][index],
            ];

            // RCD's internal working planes are populated at every CFA site,
            // but preserving the original measured value here maintains the
            // crate-wide observed-sample and no-clipping contracts.
            rgb[measured_channel] = *mosaic.sample(global_x, global_y);
            gains.apply(&mut rgb);
            require_finite_output(&rgb, global_x, global_y)?;
            let output_index = global_y * row_stride + global_x * 3;
            output[output_index..output_index + 3].copy_from_slice(&rgb);
        }
    }
    Ok(())
}
