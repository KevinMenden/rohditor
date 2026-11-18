//! Fixed AMaZE tile geometry shared by the CPU reference and GPU executor.
//!
//! Tiles are `AMAZE_TILE_SIZE` pixels wide with an `AMAZE_BORDER` halo, so each
//! tile's valid core is `AMAZE_TILE_VALID` pixels. Tile origins are relative to
//! the complete mosaic and are always even, so a tile's local Bayer phase equals
//! the image's phase. Cores tile the image exactly: reconstruction reaches the
//! image edges, with the halo beyond an image edge filled by mirroring
//! ([`mirror_coordinate`]), which preserves coordinate parity. Images no larger
//! than twice the border have no tiles and stay entirely bilinear.

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
    /// The pixels this tile reconstructs: its core, clipped only to the image.
    #[must_use]
    pub fn write_window(self, width: usize, height: usize) -> AmazeWriteWindow {
        AmazeWriteWindow {
            x: core_range(self.origin_x, width),
            y: core_range(self.origin_y, height),
        }
    }
}

/// Core range along one axis, clipped only to the image.
fn core_range(origin: usize, length: usize) -> Range<usize> {
    origin..(origin + AMAZE_TILE_VALID).min(length)
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

/// Tiles in row-major order; their cores tile the image exactly. Images no
/// larger than twice the border yield none; they are entirely bilinear.
pub fn amaze_tiles(width: usize, height: usize) -> impl Iterator<Item = AmazeTileGeometry> {
    let (columns, rows) = if width <= 2 * AMAZE_BORDER || height <= 2 * AMAZE_BORDER {
        (0, 0)
    } else {
        (amaze_tile_count(width), amaze_tile_count(height))
    };
    (0..rows).flat_map(move |tile_y| {
        (0..columns).map(move |tile_x| AmazeTileGeometry {
            origin_x: tile_x * AMAZE_TILE_VALID,
            origin_y: tile_y * AMAZE_TILE_VALID,
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
    fn tiles_reach_the_image_edges() {
        // The second column starts at 224 and its core runs to the edge.
        assert_eq!(tiles(240, 33), [(0, 0), (224, 0)]);
        assert_eq!(tiles(241, 33), [(0, 0), (224, 0)]);
        let tile = amaze_tiles(500, 33).nth(2).expect("last tile");
        assert_eq!(tile.write_window(500, 33).x, 448..500);
        assert_eq!(tile.write_window(500, 33).y, 0..33);
        assert_eq!(tiles(500, 500).len(), 9);
        // 450 = 2 * 224 + 2: the third column is a two-pixel sliver at the edge.
        assert_eq!(tiles(450, 33).len(), 3);
        let third = amaze_tiles(450, 33).nth(2).expect("third tile");
        assert_eq!(third.write_window(450, 33).x, 448..450);
    }

    #[test]
    fn every_image_pixel_is_written_exactly_once() {
        for (width, height) in [
            (33, 33),
            (224, 224),
            (225, 225),
            (195, 195),
            (240, 225),
            (33, 257),
            (257, 33),
            (450, 460),
            (513, 389),
        ] {
            let mut written = vec![0_u8; width * height];
            for tile in amaze_tiles(width, height) {
                let window = tile.write_window(width, height);
                assert!(!window.is_empty(), "empty core at {width}x{height}");
                for y in window.y {
                    for x in window.x.clone() {
                        written[y * width + x] += 1;
                    }
                }
            }
            assert!(
                written.iter().all(|&count| count == 1),
                "{width}x{height}: a pixel was not written exactly once"
            );
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

    /// Indices beyond one reflection period (`2 * (length - 1)`) bounce off
    /// the far edge and back; these exact values pin multi-reflection depth.
    /// Output reads reach at most one halo outside the image and the size
    /// gates keep every directional image longer than the halo, so these
    /// depths only occur in unused reaches of the scratch windows — where the
    /// WGSL `mirror` copies this formula verbatim.
    #[test]
    fn mirroring_multi_reflects_beyond_one_period() {
        assert_eq!(mirror_coordinate(-21, 10), 3);
        assert_eq!(mirror_coordinate(35, 10), 1);
        assert_eq!(mirror_coordinate(58, 10), 4);
        assert_eq!(mirror_coordinate(-100, 37), 28);
        // The far window corners the GPU RCD stage snapshot asserts on 29x31.
        assert_eq!(mirror_coordinate(183, 29), 15);
        assert_eq!(mirror_coordinate(183, 31), 3);
        // Even reflection about each edge holds at any bounce depth, and the
        // result always lands inside the image.
        for length in [2_usize, 5, 12, 25, 33, 40] {
            for bounce in 0..4 * length as isize {
                assert_eq!(
                    mirror_coordinate(-bounce, length),
                    mirror_coordinate(bounce, length),
                    "odd extension at length {length}, bounce {bounce}"
                );
                assert_eq!(
                    mirror_coordinate(length as isize - 1 + bounce, length),
                    mirror_coordinate(length as isize - 1 - bounce, length),
                    "asymmetric extension at length {length}, bounce {bounce}"
                );
                assert!(
                    mirror_coordinate(bounce, length) < length,
                    "out-of-image index at length {length}, bounce {bounce}"
                );
            }
        }
    }
}
