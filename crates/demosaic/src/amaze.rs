//! Tiled AMaZE (Aliasing Minimization and Zipper Elimination) demosaicing.
//!
//! The implementation keeps the published AMaZE working-set boundary and
//! directional interpolation stages, but adapts the storage and edge policy
//! to this crate: tiles are processed serially with mirrored CFA samples and
//! the outer 16-pixel image boundary remains the bilinear reference result.
//! This keeps peak memory bounded without making the deterministic CPU path
//! depend on a decoder or an application.
//!
//! AMaZE was authored by Emil Martinec and optimized by Ingo Weyrich, with
//! additional ideas from Luis Sanz Rodríguez and Paul Lee. This Rust
//! adaptation is based on the GPLv3-or-later implementations in darktable and
//! RawTherapee:
//! https://github.com/darktable-org/darktable/blob/master/src/iop/demosaicing/amaze.cc
//! https://github.com/RawTherapee/RawTherapee/blob/dev/rtengine/amaze_demosaic_RT.cc

use super::{
    AMAZE_HALO, CancellationCheck, DemosaicError, WhiteBalanceGains, bilinear, checkpoint,
    require_finite_output,
};
use rohditor_image::{BayerPattern, MosaicImage, allocate_zeroed_f32};

use super::amaze_stages::{calculate_gradients, interpolate_chroma, interpolate_green};

// AMaZE's source implementations use a configurable tile that is a multiple
// of 32. A fixed 256-pixel tile is large enough to amortize the 16-pixel halo
// while keeping the scratch set small and predictable.
pub(super) const TILE_SIZE: usize = 256;
pub(super) const BORDER: usize = AMAZE_HALO.left;
pub(super) const TILE_VALID: usize = TILE_SIZE - 2 * BORDER;
pub(super) const PLANE_ELEMENTS: usize = TILE_SIZE * TILE_SIZE;
pub(super) const EPSILON: f32 = 1.0e-5;
pub(super) const EPSILON_SQUARED: f32 = 1.0e-10;
pub(super) const AR_THRESHOLD: f32 = 0.75;
pub(super) const CLIP_POINT: f32 = 1.0;

pub(super) fn reconstruct(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
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
        let tile = AmazeTile {
            origin_x: 0,
            origin_y,
            pattern: mosaic.pattern().shifted(0, origin_y),
        };
        for tile_x in 0..tile_count(mosaic.width()) {
            checkpoint(cancellation)?;
            let tile = AmazeTile {
                origin_x: tile_x * TILE_VALID,
                ..tile
            };
            scratch.clear();
            populate(&mut scratch, mosaic, tile);
            calculate_gradients(&mut scratch, cancellation)?;
            interpolate_green(&mut scratch, tile.pattern, cancellation)?;
            interpolate_chroma(&mut scratch, tile.pattern, cancellation)?;
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
    length.div_ceil(TILE_VALID)
}

#[derive(Clone, Copy)]
pub(super) struct AmazeTile {
    pub(super) origin_x: usize,
    pub(super) origin_y: usize,
    pub(super) pattern: BayerPattern,
}

pub(super) struct AmazeScratch {
    pub(super) cfa: Vec<f32>,
    pub(super) green: Vec<f32>,
    pub(super) red: Vec<f32>,
    pub(super) blue: Vec<f32>,
    pub(super) vertical_weights: Vec<f32>,
    pub(super) horizontal_weights: Vec<f32>,
    pub(super) vertical_difference: Vec<f32>,
    pub(super) horizontal_difference: Vec<f32>,
    pub(super) vertical_alternative: Vec<f32>,
    pub(super) horizontal_alternative: Vec<f32>,
}

impl AmazeScratch {
    fn new() -> Result<Self, DemosaicError> {
        let plane = || allocate_zeroed_f32(PLANE_ELEMENTS).map_err(DemosaicError::from);
        Ok(Self {
            cfa: plane()?,
            green: plane()?,
            red: plane()?,
            blue: plane()?,
            vertical_weights: plane()?,
            horizontal_weights: plane()?,
            vertical_difference: plane()?,
            horizontal_difference: plane()?,
            vertical_alternative: plane()?,
            horizontal_alternative: plane()?,
        })
    }

    fn clear(&mut self) {
        self.cfa.fill(0.0);
        self.green.fill(0.0);
        self.red.fill(0.0);
        self.blue.fill(0.0);
        self.vertical_weights.fill(0.0);
        self.horizontal_weights.fill(0.0);
        self.vertical_difference.fill(0.0);
        self.horizontal_difference.fill(0.0);
        self.vertical_alternative.fill(0.0);
        self.horizontal_alternative.fill(0.0);
    }
}

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
            match tile.pattern.color_at(col, row).channel_index() {
                0 => scratch.red[index] = value,
                2 => scratch.blue[index] = value,
                _ => {}
            }
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
                scratch.green[index],
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
