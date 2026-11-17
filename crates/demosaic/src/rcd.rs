//! Ratio-corrected directional demosaicing.
//!
//! The implementation follows the tiled RCD formulation used by RawTherapee
//! and darktable. It keeps the algorithm independent of RAW decoding. Tile
//! cores cover the whole image and each tile's halo mirrors into the mosaic,
//! so an eligible image is reconstructed directionally to its edges; images
//! too small for a complete mirrored halo are reconstructed with bilinear
//! interpolation.

use super::rcd_geometry::{
    RCD_BORDER as BORDER, RCD_TILE_SIZE as TILE_SIZE, RcdTileGeometry, rcd_tiles,
};
use super::{
    CancellationCheck, DemosaicError, WhiteBalanceGains, bilinear, checkpoint, mirror_coordinate,
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
    // The tile cores cover the whole image and the sample windows mirror
    // into the mosaic beyond its edges, so an eligible image is fully
    // reconstructed directionally; no bilinear base is laid down. Images at
    // or below the minimum dimension have no tiles and stay bilinear.
    let (width, height) = (mosaic.width(), mosaic.height());
    let tiles: Vec<RcdTileGeometry> = rcd_tiles(width, height).collect();
    if tiles.is_empty() {
        return bilinear::reconstruct(mosaic, gains, cancellation, row_stride, output);
    }

    // Tiles are independent and their cores are disjoint, so each task owns
    // the output rows of its core and the result does not depend on the
    // thread count. Scratch sets are large and zero-filled per tile, so
    // finished tiles return theirs to a pool (one set per running tile).
    let jobs = tile_jobs(&tiles, width, height, row_stride, output);
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
        // The window origin is `origin - BORDER`; the even border keeps the
        // window's Bayer phase equal to the core origin's phase.
        let tile = RcdTile {
            origin_x: geometry.origin_x,
            origin_y: geometry.origin_y,
            pattern: mosaic
                .pattern()
                .shifted(geometry.origin_x, geometry.origin_y),
        };
        populate(&mut scratch, mosaic, tile, cancellation)?;
        find_directions(&mut scratch, TILE_SIZE, TILE_SIZE, cancellation)?;
        calculate_low_pass(
            &mut scratch,
            tile.pattern,
            TILE_SIZE,
            TILE_SIZE,
            cancellation,
        )?;
        interpolate_green(
            &mut scratch,
            tile.pattern,
            TILE_SIZE,
            TILE_SIZE,
            cancellation,
        )?;
        interpolate_red_blue(
            &mut scratch,
            tile.pattern,
            TILE_SIZE,
            TILE_SIZE,
            cancellation,
        )?;
        write_tile(
            &scratch,
            mosaic,
            gains,
            geometry.core(width, height),
            job.rows,
            cancellation,
        )?;
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
/// bands follow each other from the first core row (image row zero) downwards.
/// The core columns of one band exactly tile the image row.
fn tile_jobs<'a>(
    tiles: &[RcdTileGeometry],
    width: usize,
    height: usize,
    row_stride: usize,
    output: &'a mut [f32],
) -> Vec<TileJob<'a>> {
    let mut jobs = Vec::with_capacity(tiles.len());
    let mut remaining = output;
    for band in tiles.chunk_by(|a, b| a.origin_y == b.origin_y) {
        let (_, top, _, bottom) = band[0].core(width, height);
        let band_rows = bottom - top;
        let (rows, rest) = remaining.split_at_mut(band_rows * row_stride);
        remaining = rest;

        let mut columns: Vec<Vec<&mut [f32]>> = band.iter().map(|_| Vec::new()).collect();
        for row in rows.chunks_mut(row_stride) {
            let mut row = &mut row[..];
            for (tile, column) in band.iter().zip(&mut columns) {
                let (left, _, right, _) = tile.core(width, height);
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
    /// Core origin in image coordinates; the sample window starts `BORDER`
    /// pixels before it (in signed arithmetic).
    origin_x: usize,
    origin_y: usize,
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

/// Fill the tile's full `TILE_SIZE`×`TILE_SIZE` sample window, mirroring
/// samples beyond the image edge into the image.
fn populate(
    scratch: &mut RcdScratch,
    mosaic: &MosaicImage<f32>,
    tile: RcdTile,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    let (width, height) = (mosaic.width(), mosaic.height());
    // The window origin is the core origin minus the halo, in signed space;
    // for edge tiles it is negative, and mirroring reflects it back inside.
    let window_x = tile.origin_x as isize - BORDER as isize;
    let window_y = tile.origin_y as isize - BORDER as isize;
    // The source column of each window column, computed once for all rows.
    let source_xs: [usize; TILE_SIZE] =
        std::array::from_fn(|col| mirror_coordinate(window_x + col as isize, width));
    let inside = window_x >= 0
        && window_y >= 0
        && (window_x as usize) + TILE_SIZE <= width
        && (window_y as usize) + TILE_SIZE <= height;

    for row in 0..TILE_SIZE {
        checkpoint(cancellation)?;
        let first_color = tile.pattern.color_at(0, row).channel_index();
        let second_color = tile.pattern.color_at(1, row).channel_index();
        let source_y = mirror_coordinate(window_y + row as isize, height);
        let source = &mosaic.data()[source_y * mosaic.row_stride()..][..width];
        let range = row * TILE_SIZE..row * TILE_SIZE + TILE_SIZE;
        let cfa = &mut scratch.cfa[range.clone()];
        if inside {
            cfa.copy_from_slice(&source[window_x as usize..][..TILE_SIZE]);
        } else {
            for (value, &x) in cfa.iter_mut().zip(&source_xs) {
                *value = source[x];
            }
        }

        // The original implementations use a non-negative working CFA
        // image. Keep the crate's measured sample unchanged for final
        // output, while retaining signed samples in the working planes so
        // the CPU pipeline does not silently clip its input.
        scratch.rgb[first_color][range.clone()].copy_from_slice(cfa);
        scratch.rgb[second_color][range].copy_from_slice(cfa);
    }
    Ok(())
}

/// Write the core of a tile (image-clipped) to `rows`, one slice per core
/// row. Scratch slots sit at `image position - (core origin - BORDER)`.
fn write_tile(
    scratch: &RcdScratch,
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    core: (usize, usize, usize, usize),
    rows: Vec<&mut [f32]>,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    let (core_x, core_y, _, _) = core;
    for (row_offset, row) in rows.into_iter().enumerate() {
        checkpoint(cancellation)?;
        let local_y = row_offset + BORDER;
        for (column, pixel) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let global_x = core_x + column;
            let global_y = core_y + row_offset;
            let index = local_y * TILE_SIZE + column + BORDER;
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

/// The job splitter must hand every output sample to exactly one tile. The
/// row and column `split_at_mut` views are disjoint by construction, so this
/// pins total coverage and exact placement: each job stamps its own global
/// sample index through its slices, and the buffer must come back sorted.
#[cfg(test)]
mod job_split_tests {
    use super::*;

    #[test]
    fn tile_jobs_write_every_output_sample_exactly_once() {
        for (width, height) in [
            (25, 25),
            (25, 40),
            (40, 25),
            (41, 37),
            (195, 195),
            (199, 199),
            (370, 380),
            (400, 230),
            (230, 370),
        ] {
            let row_stride = width * 3;
            let mut output = vec![-1.0_f32; row_stride * height];
            let tiles: Vec<RcdTileGeometry> = rcd_tiles(width, height).collect();
            assert!(!tiles.is_empty(), "{width}x{height} must be eligible");
            let jobs = tile_jobs(&tiles, width, height, row_stride, &mut output);
            assert_eq!(jobs.len(), tiles.len());
            for job in jobs {
                let (x0, y0, _, _) = job.geometry.core(width, height);
                for (row_offset, row) in job.rows.into_iter().enumerate() {
                    let global_y = y0 + row_offset;
                    for (column, pixel) in row.iter_mut().enumerate() {
                        *pixel = (global_y * row_stride + x0 * 3 + column) as f32;
                    }
                }
            }
            for (index, &value) in output.iter().enumerate() {
                assert_eq!(value, index as f32, "{width}x{height} sample {index}");
            }
        }
    }
}

#[cfg(test)]
mod stage_fixtures {
    use super::super::rcd_stages::{
        calculate_diagonal_directions, calculate_diagonal_high_pass, interpolate_at_green,
        interpolate_opposite_color,
    };
    use super::*;

    /// The 29×31 fixture tile's window origin is (−10, −10) and the stages run
    /// over the full 194×194 window, so the probe sites at global (10, 10) and
    /// (11, 10) sit at local (20, 20) and (20, 21). The frozen values are the
    /// original ones: pixels ≥ 10 px from every edge are bit-identical under
    /// mirrored halos (every dependency stays inside the image there).
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
            pattern: mosaic.pattern(),
        };
        let mut scratch = RcdScratch::new().expect("RCD scratch allocation");
        let cancellation = || false;
        let red = 20 * TILE_SIZE + 20;
        let green = 20 * TILE_SIZE + 21;
        populate(&mut scratch, &mosaic, tile, &cancellation).expect("populate");
        assert_eq!(scratch.rgb[0][green], *mosaic.sample(11, 10));
        assert_eq!(scratch.rgb[1][green], *mosaic.sample(11, 10));
        find_directions(&mut scratch, TILE_SIZE, TILE_SIZE, &cancellation).expect("directions");
        let vh = scratch.vh_direction[red];
        calculate_low_pass(
            &mut scratch,
            tile.pattern,
            TILE_SIZE,
            TILE_SIZE,
            &cancellation,
        )
        .expect("low pass");
        let low = scratch.pq_direction[red / 2];
        interpolate_green(
            &mut scratch,
            tile.pattern,
            TILE_SIZE,
            TILE_SIZE,
            &cancellation,
        )
        .expect("green interpolation");
        let interpolated_green = scratch.rgb[1][red];
        calculate_diagonal_high_pass(&mut scratch, TILE_SIZE, TILE_SIZE, &cancellation)
            .expect("diagonal high pass");
        let (p, q) = (
            scratch.p_color_difference[green / 2],
            scratch.q_color_difference[green / 2],
        );
        calculate_diagonal_directions(
            &mut scratch,
            tile.pattern,
            TILE_SIZE,
            TILE_SIZE,
            &cancellation,
        )
        .expect("diagonal directions");
        let pq = scratch.pq_direction[red / 2];
        interpolate_opposite_color(
            &mut scratch,
            tile.pattern,
            TILE_SIZE,
            TILE_SIZE,
            &cancellation,
        )
        .expect("opposite color");
        let opposite = scratch.rgb[2][red];
        interpolate_at_green(
            &mut scratch,
            tile.pattern,
            TILE_SIZE,
            TILE_SIZE,
            &cancellation,
        )
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
