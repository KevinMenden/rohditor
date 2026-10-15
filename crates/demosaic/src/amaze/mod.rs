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
//! zero.
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

pub(super) fn reconstruct(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    clip_point: f32,
    cancellation: &dyn CancellationCheck,
    row_stride: usize,
    output: &mut [f32],
) -> Result<(), DemosaicError> {
    // Keep the crate-wide edge behavior consistent with the other higher
    // quality algorithms. AMaZE stages only overwrite pixels with a complete
    // 16-pixel neighborhood.
    bilinear::reconstruct(mosaic, gains, cancellation, row_stride, output)?;
    if mosaic.width() <= 2 * BORDER || mosaic.height() <= 2 * BORDER {
        return Ok(());
    }

    let mut scratch = AmazeScratch::new()?;
    for tile_y in 0..tile_count(mosaic.height()) {
        checkpoint(cancellation)?;
        let origin_y = tile_y * TILE_VALID;
        for tile_x in 0..tile_count(mosaic.width()) {
            checkpoint(cancellation)?;
            let tile = AmazeTile {
                origin_x: tile_x * TILE_VALID,
                origin_y,
                pattern: mosaic.pattern().shifted(0, origin_y),
            };
            scratch.clear();
            populate(&mut scratch, mosaic, tile);
            reconstruct_tile(&mut scratch, tile.pattern, clip_point);
            write_tile(&scratch, mosaic, gains, tile, row_stride, output, cancellation)?;
        }
    }
    Ok(())
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
    for row in 0..TILE_SIZE {
        let source_y = mirror_coordinate(
            tile.origin_y as isize + row as isize - BORDER as isize,
            mosaic.height(),
        );
        for col in 0..TILE_SIZE {
            let source_x = mirror_coordinate(
                tile.origin_x as isize + col as isize - BORDER as isize,
                mosaic.width(),
            );
            let value = *mosaic.sample(source_x, source_y);
            let index = row * TILE_SIZE + col;
            scratch.cfa[index] = value;
            scratch.green[index] = value;
        }
    }
}

fn write_tile(
    scratch: &AmazeScratch,
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    tile: AmazeTile,
    row_stride: usize,
    output: &mut [f32],
    cancellation: &dyn CancellationCheck,
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

    for global_y in y_start..y_end {
        checkpoint(cancellation)?;
        let local_y = global_y - tile.origin_y + BORDER;
        for global_x in x_start..x_end {
            let local_x = global_x - tile.origin_x + BORDER;
            let index = local_y * TILE_SIZE + local_x;
            let measured_channel = mosaic
                .pattern()
                .color_at(global_x, global_y)
                .channel_index();
            let mut rgb = [
                scratch.red[index],
                scratch.green[index].max(0.0),
                scratch.blue[index],
            ];
            rgb[measured_channel] = *mosaic.sample(global_x, global_y);
            gains.apply(&mut rgb);
            require_finite_output(&rgb, global_x, global_y)?;
            let output_index = global_y * row_stride + global_x * 3;
            output[output_index..output_index + 3].copy_from_slice(&rgb);
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
