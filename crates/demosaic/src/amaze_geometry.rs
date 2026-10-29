//! Fixed AMaZE tile geometry shared by the CPU reference and GPU executor.
//!
//! Tiles are `AMAZE_TILE_SIZE` pixels wide with an `AMAZE_BORDER` halo, so each
//! tile's valid core is `AMAZE_TILE_VALID` pixels. Tile origins are relative to
//! the complete mosaic and are always even, so a tile's local Bayer phase equals
//! the image's phase. The outer `AMAZE_BORDER` ring of the image keeps the
//! bilinear reference result; the halo beyond the image edge is filled by
//! mirroring ([`mirror_coordinate`]), which preserves coordinate parity.

use std::ops::Range;

pub const AMAZE_TILE_SIZE: usize = 256;
pub const AMAZE_BORDER: usize = 16;
pub const AMAZE_TILE_VALID: usize = AMAZE_TILE_SIZE - 2 * AMAZE_BORDER;

/// One tile of the fixed grid. `origin_*` is the first pixel of the tile's
/// valid core; the tile's samples start `AMAZE_BORDER` pixels before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AmazeTileGeometry {
    pub origin_x: usize,
    pub origin_y: usize,
}

/// Image extent of the output pixels a tile writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmazeWriteWindow {
    pub x: Range<usize>,
    pub y: Range<usize>,
}

impl AmazeTileGeometry {
    /// The pixels this tile reconstructs: its core, minus the bilinear border
    /// ring of the image (see [`core_range`] for the one exception). Empty when
    /// the ring leaves nothing of the core.
    #[must_use]
    pub fn write_window(self, width: usize, height: usize) -> AmazeWriteWindow {
        AmazeWriteWindow {
            x: core_range(self.origin_x, width),
            y: core_range(self.origin_y, height),
        }
    }
}

/// Core range along one axis, clipped to the image and its border ring.
///
/// Only the tile that reaches the image edge gives up the ring. When the last
/// tile lies entirely inside the ring (`length % AMAZE_TILE_VALID` in `1..=16`),
/// it writes nothing and the tile before it keeps its full core, which reaches
/// up to 15 pixels into the ring. This is the long-standing CPU behaviour and
/// the GPU executor reproduces it.
fn core_range(origin: usize, length: usize) -> Range<usize> {
    let start = origin.max(AMAZE_BORDER);
    let mut end = (origin + AMAZE_TILE_VALID).min(length);
    if end == length {
        end = end.saturating_sub(AMAZE_BORDER);
    }
    start..end.max(start)
}

impl AmazeWriteWindow {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.x.is_empty() || self.y.is_empty()
    }
}

/// Number of tile columns (or rows) needed to cover `length` pixels.
#[must_use]
pub fn amaze_tile_count(length: usize) -> usize {
    length.div_ceil(AMAZE_TILE_VALID)
}

/// Tiles in row-major order that write at least one pixel. Images no larger
/// than twice the border yield none; they are entirely bilinear.
pub fn amaze_tiles(width: usize, height: usize) -> impl Iterator<Item = AmazeTileGeometry> {
    let (columns, rows) = if width <= 2 * AMAZE_BORDER || height <= 2 * AMAZE_BORDER {
        (0, 0)
    } else {
        (amaze_tile_count(width), amaze_tile_count(height))
    };
    (0..rows).flat_map(move |tile_y| {
        (0..columns).filter_map(move |tile_x| {
            let tile = AmazeTileGeometry {
                origin_x: tile_x * AMAZE_TILE_VALID,
                origin_y: tile_y * AMAZE_TILE_VALID,
            };
            (!tile.write_window(width, height).is_empty()).then_some(tile)
        })
    })
}

/// Reflect `index` into `0..length` about the first and last sample. Parity of
/// the index is preserved, so the mirrored sample has the same Bayer colour.
#[must_use]
pub fn mirror_coordinate(index: isize, length: usize) -> usize {
    if length <= 1 {
        return 0;
    }
    let period = 2 * (length - 1);
    let wrapped = index.rem_euclid(period as isize) as usize;
    wrapped.min(period - wrapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiles(width: usize, height: usize) -> Vec<(usize, usize)> {
        amaze_tiles(width, height)
            .map(|tile| (tile.origin_x, tile.origin_y))
            .collect()
    }

    #[test]
    fn images_inside_the_border_ring_have_no_tiles() {
        assert!(tiles(32, 400).is_empty());
        assert!(tiles(400, 32).is_empty());
        assert_eq!(tiles(33, 33), [(0, 0)]);
    }

    #[test]
    fn tiles_stop_where_the_border_ring_leaves_nothing_to_write() {
        // The second column would start at 224, inside the ring (224..240).
        assert_eq!(tiles(240, 33), [(0, 0)]);
        assert_eq!(tiles(241, 33), [(0, 0), (224, 0)]);
        let tile = amaze_tiles(500, 33).nth(2).expect("last tile");
        assert_eq!(tile.write_window(500, 33).x, 448..484);
        assert_eq!(tile.write_window(500, 33).y, 16..17);
        assert_eq!(tiles(500, 500).len(), 9);
    }

    #[test]
    fn a_last_tile_inside_the_ring_leaves_the_previous_core_unclipped() {
        // 450 = 2 * 224 + 2: the third column (448..450) is all ring, so the
        // second keeps its whole core (224..448), 2 pixels short of the edge.
        let second = amaze_tiles(450, 33).nth(1).expect("second tile");
        assert_eq!(second.write_window(450, 33).x, 224..448);
        assert_eq!(tiles(450, 33).len(), 2);
    }

    #[test]
    fn every_interior_pixel_is_written_exactly_once() {
        let (width, height) = (700, 470);
        let mut written = vec![0_u8; width * height];
        for tile in amaze_tiles(width, height) {
            let window = tile.write_window(width, height);
            for y in window.y {
                for x in window.x.clone() {
                    written[y * width + x] += 1;
                }
            }
        }
        for y in 0..height {
            for x in 0..width {
                let interior = (AMAZE_BORDER..width - AMAZE_BORDER).contains(&x)
                    && (AMAZE_BORDER..height - AMAZE_BORDER).contains(&y);
                assert_eq!(written[y * width + x], u8::from(interior), "({x}, {y})");
            }
        }
    }

    #[test]
    fn mirroring_reflects_without_repeating_the_edge_and_keeps_parity() {
        assert_eq!(mirror_coordinate(-1, 10), 1);
        assert_eq!(mirror_coordinate(-16, 40), 16);
        assert_eq!(mirror_coordinate(10, 10), 8);
        assert_eq!(mirror_coordinate(25, 10), 7);
        for index in -300..300 {
            assert_eq!(
                mirror_coordinate(index, 37) & 1,
                index.rem_euclid(2) as usize
            );
        }
        assert_eq!(mirror_coordinate(5, 1), 0);
    }
}
