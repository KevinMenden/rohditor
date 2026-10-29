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
    AMAZE_BORDER as BORDER, AMAZE_TILE_SIZE as TILE_SIZE, AMAZE_TILE_VALID as TILE_VALID,
    AmazeTileGeometry, CancellationCheck, DemosaicError, WhiteBalanceGains, amaze_tile_count,
    bilinear, checkpoint, mirror_coordinate, require_finite_output,
};
use scratch::AmazeScratch;

// Upstream uses a configurable tile that is a multiple of 32. A fixed
// 256-pixel tile amortizes the 16-pixel halo while keeping the scratch set
// small; the geometry is shared with the GPU executor (`amaze_geometry.rs`).

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
    let tile_columns = amaze_tile_count(width);
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
        // A tile whose core lies entirely in the border ring writes nothing.
        jobs.extend(
            columns
                .into_iter()
                .enumerate()
                .filter_map(|(tile_x, rows)| {
                    let geometry = AmazeTileGeometry {
                        origin_x: tile_x * TILE_VALID,
                        origin_y: tile_y * TILE_VALID,
                    };
                    (!geometry.write_window(width, mosaic.height()).is_empty()).then_some(TileJob {
                        tile_x,
                        tile_y,
                        rows,
                    })
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
    let geometry = AmazeTileGeometry {
        origin_x: tile.origin_x,
        origin_y: tile.origin_y,
    };
    let window = geometry.write_window(mosaic.width(), mosaic.height());
    if window.is_empty() {
        return Ok(());
    }
    let (x_start, x_end) = (window.x.start, window.x.end);

    let width = x_end - x_start;
    for global_y in window.y {
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

#[cfg(test)]
mod stage_fixtures {
    use super::*;

    /// A small asymmetric mosaic with signed values, spikes and exact ties,
    /// dense enough in texture to take the Nyquist branches.
    pub(super) fn fixture() -> MosaicImage<f32> {
        let (width, height) = (53, 47);
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
                    _ => ((x * 37 + y * 19 + x * y) % 101) as f32 / 63.0 - 0.4,
                }
            })
            .collect();
        MosaicImage::new(width, height, width, BayerPattern::Rggb, data).expect("fixture mosaic")
    }

    pub(super) fn fixture_scratch() -> AmazeScratch {
        let mosaic = fixture();
        let mut scratch = AmazeScratch::new().expect("scratch");
        let tile = AmazeTile {
            origin_x: 0,
            origin_y: 0,
            pattern: mosaic.pattern(),
        };
        scratch.clear();
        populate(&mut scratch, &mosaic, tile);
        reconstruct_tile(&mut scratch, tile.pattern, 1.0);
        scratch
    }

    /// Scratch plane by name; `half` planes are indexed by pixel pair.
    pub(super) fn probe(s: &AmazeScratch, name: &str, row: usize, col: usize) -> f32 {
        let full = row * 256 + col;
        let half = row * 128 + col / 2;
        match name {
            "cfa" => s.cfa[full],
            "green" => s.green[full],
            "delhvsqsum" => s.delhvsqsum[full],
            "dirwts0" => s.dirwts0[full],
            "dirwts1" => s.dirwts1[full],
            "vcd" => s.vcd[full],
            "hcd" => s.hcd[full],
            "vcdalt" => s.vcdalt[full],
            "hcdalt" => s.hcdalt[full],
            "cddiffsq" => s.cddiffsq[full],
            "dgintv" => s.dgintv[full],
            "dginth" => s.dginth[full],
            "hvwt" => s.hvwt[half],
            "dgrb0" => s.dgrb[0][half],
            "dgrb1" => s.dgrb[1][half],
            "dgrb2_h" => s.dgrb2_h[half],
            "dgrb2_v" => s.dgrb2_v[half],
            "delp" => s.delp[half],
            "delm" => s.delm[half],
            "sq1m" => s.dgrbsq1m[half],
            "sq1p" => s.dgrbsq1p[half],
            "nyqutest" => s.nyqutest[half],
            "pmwt" => s.pmwt[half],
            "rbm" => s.rbm[half],
            "rbp" => s.rbp[half],
            "rbint" => s.rbint[half],
            "nyquist" => f32::from(s.nyquist[half]),
            "nyquist2" => f32::from(s.nyquist2[half]),
            other => panic!("unknown plane {other}"),
        }
    }

    /// (plane, tile row, tile column, value) of the fixture after every stage.
    /// Sites: (25, 57) and (26, 56) are voted Nyquist sites, (32, 32) is flagged
    /// but outvoted, (31, 33) and (33, 35) are plain. The GPU executor's tests
    /// pin the same values; regenerate both when the algorithm changes.
    const PROBES: &[(&str, usize, usize, f32)] = &[
        ("nyqutest", 25, 57, 0.08931726),
        ("nyquist", 25, 57, 1.0),
        ("nyquist2", 25, 57, 1.0),
        ("hvwt", 25, 57, 0.6249956),
        ("dgrb2_h", 25, 57, 0.087723866),
        ("dgrb2_v", 25, 57, 0.13902828),
        ("dgrb0", 25, 57, 0.7487581),
        ("dgrb1", 25, 57, 1.1797017),
        ("green", 25, 57, 1.1797007),
        ("pmwt", 25, 57, 0.59881574),
        ("rbint", 25, 57, -0.018249098),
        ("nyquist2", 26, 56, 1.0),
        ("hvwt", 26, 56, 0.77032536),
        ("dgrb0", 26, 56, 0.043839097),
        ("dgrb1", 26, 56, 0.51848173),
        ("green", 26, 56, -0.1561609),
        ("nyqutest", 32, 32, 0.051671088),
        ("nyquist", 32, 32, 1.0),
        ("nyquist2", 32, 32, 0.0),
        ("hvwt", 32, 32, 0.22237617),
        ("dgrb2_h", 32, 32, 0.0),
        ("green", 32, 32, 0.7930795),
        ("cfa", 31, 33, 0.5206349),
        ("vcd", 31, 33, 0.1482284),
        ("hcd", 31, 33, -0.8253968),
        ("vcdalt", 31, 33, 0.1482284),
        ("hcdalt", 31, 33, -0.8630527),
        ("cddiffsq", 31, 33, 0.947946),
        ("dgintv", 31, 33, 1.6694309),
        ("dginth", 31, 33, 0.58292013),
        ("delhvsqsum", 31, 33, 1.671696),
        ("dirwts0", 31, 33, 2.3333435),
        ("dirwts1", 31, 33, 1.5746133),
        ("delp", 31, 33, 1.7492063),
        ("delm", 31, 33, 1.5),
        ("sq1m", 31, 33, 2.0440006),
        ("sq1p", 31, 33, 1.7466767),
        ("hvwt", 31, 33, 0.16582216),
        ("pmwt", 31, 33, 0.5514841),
        ("rbm", 31, 33, 0.69720316),
        ("rbp", 31, 33, 0.85232896),
        ("rbint", 31, 33, 0.6516937),
        ("dgrb0", 31, 33, -0.929778),
        ("dgrb1", 31, 33, -0.6639482),
        ("green", 31, 33, -0.14331329),
        ("vcd", 33, 35, 0.4021777),
        ("hcd", 33, 35, 1.7952101),
        ("cddiffsq", 33, 35, 1.9405394),
        ("delp", 33, 35, 0.098411694),
        ("delm", 33, 35, 0.04444444),
        ("hvwt", 33, 35, 0.1975245),
        ("pmwt", 33, 35, 0.60838723),
        ("rbint", 33, 35, -0.12558362),
        ("dgrb0", 33, 35, 1.3346572),
        ("dgrb1", 33, 35, 1.5200521),
        ("green", 33, 35, 1.320052),
    ];

    #[test]
    fn asymmetric_signed_stage_snapshot() {
        let scratch = fixture_scratch();
        let nyquist_sites: u32 = scratch.nyquist.iter().map(|&v| u32::from(v)).sum();
        let voted_sites: u32 = scratch.nyquist2.iter().map(|&v| u32::from(v)).sum();
        assert_eq!((nyquist_sites, voted_sites), (1428, 643));
        for &(name, row, col, expected) in PROBES {
            let actual = probe(&scratch, name, row, col);
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "CPU AMaZE {name} at ({row}, {col}): {actual} != {expected}"
            );
        }
    }
}
