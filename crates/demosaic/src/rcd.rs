//! Ratio-corrected directional demosaicing.
//!
//! The implementation follows the tiled RCD formulation used by RawTherapee
//! and darktable. It keeps the algorithm independent of RAW decoding and uses
//! bilinear reconstruction for the pixels where a complete RCD neighborhood
//! is not available.

use super::rcd_geometry::{
    RCD_BORDER as BORDER, RCD_MIN_DIMENSION, RCD_TILE_SIZE as TILE_SIZE, rcd_tiles,
};
use super::{
    CancellationCheck, DemosaicError, WhiteBalanceGains, bilinear, checkpoint,
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

    let mut scratch = if mosaic.width() > RCD_MIN_DIMENSION && mosaic.height() > RCD_MIN_DIMENSION {
        Some(RcdScratch::new()?)
    } else {
        None
    };
    for geometry in rcd_tiles(mosaic.width(), mosaic.height()) {
        checkpoint(cancellation)?;
        let scratch = scratch.as_mut().expect("eligible RCD tile has scratch");
        scratch.clear();
        let tile = RcdTile {
            origin_x: geometry.origin_x,
            origin_y: geometry.origin_y,
            width: geometry.width,
            height: geometry.height,
            pattern: mosaic
                .pattern()
                .shifted(geometry.origin_x, geometry.origin_y),
        };
        populate(scratch, mosaic, tile, cancellation)?;
        find_directions(scratch, tile.width, tile.height, cancellation)?;
        calculate_low_pass(scratch, tile.pattern, tile.width, tile.height, cancellation)?;
        interpolate_green(scratch, tile.pattern, tile.width, tile.height, cancellation)?;
        interpolate_red_blue(scratch, tile.pattern, tile.width, tile.height, cancellation)?;
        write_tile(
            scratch,
            mosaic,
            gains,
            tile,
            row_stride,
            output,
            cancellation,
        )?;
    }
    Ok(())
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

#[cfg(test)]
mod stage_fixtures {
    use super::super::rcd_stages::{
        calculate_diagonal_directions, calculate_diagonal_high_pass, interpolate_at_green,
        interpolate_opposite_color,
    };
    use super::*;

    #[test]
    fn asymmetric_signed_partial_tile_stage_snapshot() {
        let (width, height) = (29, 31);
        let data = (0..width * height)
            .map(|i| {
                let (x, y) = (i % width, i / width);
                match (x + 3 * y) % 11 {
                    0 => -0.0,
                    1 => 1.0e-6,
                    2 => -1.0e-6,
                    3 => 1.5,
                    4 => -0.2,
                    5 => 2.0,
                    _ => ((x * 37 + y * 19) % 101) as f32 / 63.0 - 0.4,
                }
            })
            .collect();
        let mosaic = MosaicImage::new(width, height, width, BayerPattern::Rggb, data)
            .expect("valid signed RCD fixture");
        let geometry = rcd_tiles(width, height)
            .next()
            .expect("eligible partial tile");
        let tile = RcdTile {
            origin_x: geometry.origin_x,
            origin_y: geometry.origin_y,
            width: geometry.width,
            height: geometry.height,
            pattern: mosaic.pattern(),
        };
        let mut scratch = RcdScratch::new().expect("RCD scratch allocation");
        let cancellation = || false;
        let red = 10 * TILE_SIZE + 10;
        let green = 10 * TILE_SIZE + 11;
        populate(&mut scratch, &mosaic, tile, &cancellation).expect("populate");
        assert_eq!(scratch.rgb[0][green], *mosaic.sample(11, 10));
        assert_eq!(scratch.rgb[1][green], *mosaic.sample(11, 10));
        find_directions(&mut scratch, width, height, &cancellation).expect("directions");
        let vh = scratch.vh_direction[red];
        calculate_low_pass(&mut scratch, tile.pattern, width, height, &cancellation)
            .expect("low pass");
        let low = scratch.pq_direction[red / 2];
        interpolate_green(&mut scratch, tile.pattern, width, height, &cancellation)
            .expect("green interpolation");
        let interpolated_green = scratch.rgb[1][red];
        calculate_diagonal_high_pass(&mut scratch, width, height, &cancellation)
            .expect("diagonal high pass");
        let (p, q) = (
            scratch.p_color_difference[green / 2],
            scratch.q_color_difference[green / 2],
        );
        calculate_diagonal_directions(&mut scratch, tile.pattern, width, height, &cancellation)
            .expect("diagonal directions");
        let pq = scratch.pq_direction[red / 2];
        interpolate_opposite_color(&mut scratch, tile.pattern, width, height, &cancellation)
            .expect("opposite color");
        let opposite = scratch.rgb[2][red];
        interpolate_at_green(&mut scratch, tile.pattern, width, height, &cancellation)
            .expect("green-site color");
        let (at_green_r, at_green_b) = (scratch.rgb[0][green], scratch.rgb[2][green]);
        let probes = [
            vh,
            low,
            interpolated_green,
            p,
            q,
            pq,
            opposite,
            at_green_r,
            at_green_b,
        ];
        let frozen = [
            0.51112366, 2.155159, 0.60279745, 1.1650273, 63.11414, 0.20751585, 0.9695636,
            0.94769496, 1.0202518,
        ];
        for (stage, (&actual, &expected)) in probes.iter().zip(frozen.iter()).enumerate() {
            assert!(
                (actual - expected).abs() <= 1e-6 + 1e-6 * expected.abs(),
                "CPU RCD stage probe {stage}: {actual} != {expected}"
            );
        }
    }
}
