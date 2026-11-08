//! Ratio-corrected directional demosaicing.
//!
//! The implementation follows the tiled RCD formulation used by RawTherapee
//! and darktable. It keeps the algorithm independent of RAW decoding and uses
//! bilinear reconstruction for the pixels where a complete RCD neighborhood
//! is not available.

use super::rcd_geometry::{
    RCD_BORDER as BORDER, RCD_TILE_SIZE as TILE_SIZE, RcdTileGeometry, rcd_tiles,
};
use super::{
    CancellationCheck, DemosaicError, WhiteBalanceGains, bilinear, checkpoint,
    require_finite_output,
};
use rayon::prelude::*;
use rohditor_image::{BayerPattern, MosaicImage, allocate_zeroed_f32};
use std::sync::{Mutex, PoisonError};

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
    // Pixels outside every tile core (the outer ring and any partial tile
    // without a complete neighbourhood) keep the bilinear result. This gives
    // RCD a deterministic edge policy and means narrow images do not need
    // special-case indexing in the directional stages. The cores themselves
    // are overwritten below, so bilinear skips them.
    let tiles: Vec<RcdTileGeometry> = rcd_tiles(mosaic.width(), mosaic.height()).collect();
    let covered = tiles
        .iter()
        .fold((BORDER, BORDER, 0, 0), |(x0, y0, x1, y1), tile| {
            let (_, _, right, bottom) = tile.core();
            (x0, y0, x1.max(right), y1.max(bottom))
        });
    let interior = (!tiles.is_empty()).then_some((covered.0..covered.2, covered.1..covered.3));
    bilinear::reconstruct_outside(mosaic, gains, interior, cancellation, row_stride, output)?;

    // Tiles are independent and their cores are disjoint, so each task owns
    // the output rows of its core and the result does not depend on the
    // thread count. Scratch sets are large and zero-filled per tile, so
    // finished tiles return theirs to a pool (one set per running tile).
    let jobs = tile_jobs(&tiles, row_stride, output);
    let pool = Mutex::new(Vec::<RcdScratch>::new());
    jobs.into_par_iter().try_for_each(|job| {
        checkpoint(cancellation)?;
        let pooled = pool.lock().unwrap_or_else(PoisonError::into_inner).pop();
        let mut scratch = match pooled {
            Some(scratch) => scratch,
            None => RcdScratch::new()?,
        };
        scratch.clear();
        let geometry = job.geometry;
        let tile = RcdTile {
            origin_x: geometry.origin_x,
            origin_y: geometry.origin_y,
            width: geometry.width,
            height: geometry.height,
            pattern: mosaic
                .pattern()
                .shifted(geometry.origin_x, geometry.origin_y),
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
        write_tile(&scratch, mosaic, gains, tile, job.rows, cancellation)?;
        pool.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(scratch);
        Ok(())
    })
}

/// One tile's share of the output: for each row of the tile's core, the
/// output samples of that core's columns.
struct TileJob<'a> {
    geometry: RcdTileGeometry,
    rows: Vec<&'a mut [f32]>,
}

/// Split `output` into one job per tile. `tiles` must be in the row-major
/// order of `rcd_tiles`, so tiles of one band share their core rows and the
/// bands follow each other from the first core row downwards.
fn tile_jobs<'a>(
    tiles: &[RcdTileGeometry],
    row_stride: usize,
    output: &'a mut [f32],
) -> Vec<TileJob<'a>> {
    let mut jobs = Vec::with_capacity(tiles.len());
    let mut remaining = output.get_mut(BORDER * row_stride..).unwrap_or_default();
    for band in tiles.chunk_by(|a, b| a.origin_y == b.origin_y) {
        let (first_x, top, _, bottom) = band[0].core();
        let band_rows = bottom - top;
        let (rows, rest) = remaining.split_at_mut(band_rows * row_stride);
        remaining = rest;

        let mut columns: Vec<Vec<&mut [f32]>> = band.iter().map(|_| Vec::new()).collect();
        for row in rows.chunks_mut(row_stride) {
            let mut row = &mut row[first_x * 3..];
            for (tile, column) in band.iter().zip(&mut columns) {
                let (left, _, right, _) = tile.core();
                let (head, tail) = row.split_at_mut((right - left) * 3);
                column.push(head);
                row = tail;
            }
        }
        jobs.extend(
            band.iter()
                .zip(columns)
                .map(|(&geometry, rows)| TileJob { geometry, rows }),
        );
    }
    jobs
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

/// Write the core of `tile` to `rows`, one slice per core row.
fn write_tile(
    scratch: &RcdScratch,
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    tile: RcdTile,
    rows: Vec<&mut [f32]>,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    for (local_y, row) in (BORDER..tile.height - BORDER).zip(rows) {
        checkpoint(cancellation)?;
        for (local_x, pixel) in (BORDER..tile.width - BORDER).zip(row.as_chunks_mut::<3>().0) {
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
            *pixel = rgb;
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
            0.51112366, 2.155159, 0.56720024, 1.1650273, 63.11414, 0.20751585, 0.9845681,
            0.95685244, 1.0756333,
        ];
        for (stage, (&actual, &expected)) in probes.iter().zip(frozen.iter()).enumerate() {
            assert!(
                (actual - expected).abs() <= 1e-6 + 1e-6 * expected.abs(),
                "CPU RCD stage probe {stage}: {actual} != {expected}"
            );
        }
    }
}
