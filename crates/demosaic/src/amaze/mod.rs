//! Tiled AMaZE (Aliasing Minimization and Zipper Elimination) demosaicing.
//!
//! This is a port of RawTherapee's scalar `amaze_demosaic_RT.cc` (5.13). The
//! stages, their order, loop bounds, and the half-resolution plane addressing
//! (`index >> 1`) follow the source so the two can be compared directly;
//! each stage lives in its own file:
//!
//! 1. [`green`]: gradients, colour differences, bounding, direction weights
//! 2. [`nyquist`]: Nyquist texture detection, area interpolation, green
//! 3. [`diagonal`]: diagonal R/B estimates and a second green pass
//! 4. [`chroma`]: fancy chrominance interpolation and the output planes
//!
//! Differences from upstream, all about storage and edges: tiles are 256
//! pixels wide with the usual 16-pixel halo; every plane has its own buffer
//! instead of sharing memory between stages; the halo is filled by mirroring;
//! the outer 16-pixel image boundary keeps the bilinear reference result;
//! samples are 0..1 instead of 0..65535; and the tile's measured sample is
//! written back unchanged. Like upstream, reconstructed values are clamped at
//! zero. Rows of tiles are processed in parallel.
//!
//! AMaZE was authored by Emil Martinec and optimized by Ingo Weyrich, with
//! additional ideas from Luis Sanz Rodríguez and Paul Lee. The upstream code
//! is GPLv3-or-later:
//! https://github.com/RawTherapee/RawTherapee/blob/5.13/rtengine/amaze_demosaic_RT.cc
//! darktable carries a derivative:
//! https://github.com/darktable-org/darktable/blob/master/src/iop/demosaicing/amaze.cc

mod chroma;
mod diagonal;
mod green;
mod math;
mod nyquist;
mod scratch;

use std::sync::{Mutex, PoisonError};

use rayon::prelude::*;
use rohditor_image::{BayerPattern, MosaicImage};

use super::{
    AMAZE_HALO, CancellationCheck, DemosaicError, WhiteBalanceGains, bilinear, checkpoint,
    require_finite_output,
};
use scratch::AmazeScratch;

// Upstream uses a configurable tile that is a multiple of 32. A fixed
// 256-pixel tile amortizes the 16-pixel halo while keeping the scratch set small.
const TILE_SIZE: usize = 256;
const BORDER: usize = AMAZE_HALO.left;
const TILE_VALID: usize = TILE_SIZE - 2 * BORDER;

/// One tile's share of the output: for each row of the tile's valid region, the
/// output samples of that tile's columns.
struct TileJob<'a> {
    tile_x: usize,
    tile_y: usize,
    rows: Vec<&'a mut [f32]>,
}

pub(super) fn reconstruct(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    clip_point: f32,
    cancellation: &dyn CancellationCheck,
    row_stride: usize,
    output: &mut [f32],
) -> Result<(), DemosaicError> {
    // AMaZE stages only produce pixels with a complete 16-pixel neighbourhood;
    // the outer ring keeps the bilinear reference result, consistent with the
    // other higher quality algorithms.
    if mosaic.width() <= 2 * BORDER || mosaic.height() <= 2 * BORDER {
        return bilinear::reconstruct(mosaic, gains, cancellation, row_stride, output);
    }
    bilinear::reconstruct_border(mosaic, gains, BORDER, cancellation, row_stride, output)?;

    // Tiles are independent. Each task owns the output columns of its tile
    // (the rows of every band are split per tile with `split_at_mut`), so
    // tiles run in parallel without sharing any state, and the result does not
    // depend on the thread count.
    let width = mosaic.width();
    let tile_columns = tile_count(width);
    let mut jobs = Vec::new();
    for (tile_y, band) in output.chunks_mut(TILE_VALID * row_stride).enumerate() {
        let mut columns: Vec<Vec<&mut [f32]>> = (0..tile_columns).map(|_| Vec::new()).collect();
        for row in band.chunks_mut(row_stride) {
            let mut rest = &mut row[..width * 3];
            for (tile_x, rows) in columns.iter_mut().enumerate() {
                let samples = TILE_VALID.min(width - tile_x * TILE_VALID) * 3;
                let (head, tail) = rest.split_at_mut(samples);
                rows.push(head);
                rest = tail;
            }
        }
        jobs.extend(
            columns
                .into_iter()
                .enumerate()
                .map(|(tile_x, rows)| TileJob {
                    tile_x,
                    tile_y,
                    rows,
                }),
        );
    }

    // Scratch sets are large (several MB), and allocating them per tile makes
    // threads contend on page faults, so finished tiles return theirs to a
    // pool. At most one set exists per concurrently running tile.
    let pool = Mutex::new(Vec::<AmazeScratch>::new());
    jobs.into_par_iter().try_for_each(|mut job| {
        checkpoint(cancellation)?;
        let pooled = pool.lock().unwrap_or_else(PoisonError::into_inner).pop();
        let mut scratch = match pooled {
            Some(scratch) => scratch,
            None => AmazeScratch::new()?,
        };
        let origin_y = job.tile_y * TILE_VALID;
        let tile = AmazeTile {
            origin_x: job.tile_x * TILE_VALID,
            origin_y,
            pattern: mosaic.pattern().shifted(0, origin_y),
        };
        scratch.clear();
        populate(&mut scratch, mosaic, tile);
        reconstruct_tile(&mut scratch, tile.pattern, clip_point);
        write_tile(&scratch, mosaic, gains, tile, &mut job.rows)?;
        pool.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(scratch);
        Ok(())
    })
}

fn tile_count(length: usize) -> usize {
    length.div_ceil(TILE_VALID)
}

/// Run every AMaZE stage on a populated tile.
fn reconstruct_tile(s: &mut AmazeScratch, pattern: BayerPattern, clip_point: f32) {
    // Highlights are treated as clipped from 80% of the saturation level.
    let clip_pt8 = 0.8 * clip_point;

    green::gradients(s);
    green::colour_differences(s, pattern, clip_pt8);
    green::bound_differences(s, pattern, clip_point);
    green::direction_weights(s, pattern);

    let nyquist = nyquist::detect(s, pattern);
    if let Some(area) = &nyquist {
        nyquist::area_interpolation(s, pattern, area);
    }
    nyquist::populate_green(s, pattern);
    if let Some(area) = &nyquist {
        nyquist::refine_green(s, pattern, area);
    }

    diagonal::diagonal_gradients(s, pattern);
    diagonal::diagonal_estimates(s, pattern, clip_point);
    diagonal::combine_diagonals(s, pattern);
    diagonal::refine_green_with_rb(s, pattern, clip_point);

    chroma::interpolate_chroma(s, pattern);
    chroma::finish_planes(s, pattern);
}

#[derive(Clone, Copy)]
struct AmazeTile {
    origin_x: usize,
    origin_y: usize,
    pattern: BayerPattern,
}

/// Fill the tile's CFA plane, mirroring samples beyond the image edge.
fn populate(scratch: &mut AmazeScratch, mosaic: &MosaicImage<f32>, tile: AmazeTile) {
    // The source column of each tile column, computed once for all rows.
    let first_x = tile.origin_x as isize - BORDER as isize;
    let inside = first_x >= 0 && first_x as usize + TILE_SIZE <= mosaic.width();
    let source_xs: [usize; TILE_SIZE] =
        std::array::from_fn(|col| mirror_coordinate(first_x + col as isize, mosaic.width()));

    for row in 0..TILE_SIZE {
        let source_y = mirror_coordinate(
            tile.origin_y as isize + row as isize - BORDER as isize,
            mosaic.height(),
        );
        let source = &mosaic.data()[source_y * mosaic.row_stride()..][..mosaic.width()];
        let cfa = &mut scratch.cfa[row * TILE_SIZE..][..TILE_SIZE];
        if inside {
            cfa.copy_from_slice(&source[first_x as usize..][..TILE_SIZE]);
        } else {
            for (value, &x) in cfa.iter_mut().zip(&source_xs) {
                *value = source[x];
            }
        }
        scratch.green[row * TILE_SIZE..][..TILE_SIZE].copy_from_slice(cfa);
    }
}

fn write_tile(
    scratch: &AmazeScratch,
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    tile: AmazeTile,
    rows: &mut [&mut [f32]],
) -> Result<(), DemosaicError> {
    let x_start = if tile.origin_x == 0 {
        BORDER
    } else {
        tile.origin_x
    };
    let mut x_end = (tile.origin_x + TILE_VALID).min(mosaic.width());
    if x_end == mosaic.width() {
        x_end = x_end.saturating_sub(BORDER);
    }
    let y_start = if tile.origin_y == 0 {
        BORDER
    } else {
        tile.origin_y
    };
    let mut y_end = (tile.origin_y + TILE_VALID).min(mosaic.height());
    if y_end == mosaic.height() {
        y_end = y_end.saturating_sub(BORDER);
    }
    if x_start >= x_end || y_start >= y_end {
        return Ok(());
    }

    let width = x_end - x_start;
    for global_y in y_start..y_end {
        let local_y = global_y - tile.origin_y + BORDER;
        let local_x = x_start - tile.origin_x + BORDER;
        let at = local_y * TILE_SIZE + local_x;
        let (red, green, blue) = (
            &scratch.red[at..at + width],
            &scratch.green[at..at + width],
            &scratch.blue[at..at + width],
        );
        let measured = &mosaic.data()[global_y * mosaic.row_stride() + x_start..][..width];
        // The measured channel alternates with the column parity.
        let channels = [
            mosaic.pattern().color_at(x_start, global_y).channel_index(),
            mosaic
                .pattern()
                .color_at(x_start + 1, global_y)
                .channel_index(),
        ];
        let column = (x_start - tile.origin_x) * 3;
        let output = &mut rows[global_y - tile.origin_y][column..column + width * 3];

        let mut all_finite = true;
        let pixels = output.as_chunks_mut::<3>().0.iter_mut();
        for (k, ((((pixel, &r), &g), &b), &sample)) in pixels
            .zip(red)
            .zip(green)
            .zip(blue)
            .zip(measured)
            .enumerate()
        {
            // The measured sample replaces the reconstructed value of its channel.
            let channel = channels[k & 1];
            let mut rgb = [
                if channel == 0 { sample } else { r },
                if channel == 1 {
                    sample
                } else {
                    math::fmax(g, 0.0)
                },
                if channel == 2 { sample } else { b },
            ];
            gains.apply(&mut rgb);
            all_finite &= rgb[0].is_finite() & rgb[1].is_finite() & rgb[2].is_finite();
            *pixel = rgb;
        }
        if !all_finite {
            // Rare: locate the first offending pixel for the error report.
            let (x, rgb) = output
                .as_chunks::<3>()
                .0
                .iter()
                .enumerate()
                .find(|(_, rgb)| rgb.iter().any(|value| !value.is_finite()))
                .map(|(k, rgb)| (x_start + k, *rgb))
                .unwrap_or((x_start, [0.0; 3]));
            require_finite_output(&rgb, x, global_y)?;
        }
    }
    Ok(())
}

fn mirror_coordinate(index: isize, length: usize) -> usize {
    if length <= 1 {
        return 0;
    }
    let period = 2 * (length - 1);
    let wrapped = index.rem_euclid(period as isize) as usize;
    wrapped.min(period - wrapped)
}
