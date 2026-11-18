//! Fixed RCD tile geometry shared by the CPU reference and GPU executor.
//!
//! Tiles are `RCD_TILE_SIZE` pixels wide with an `RCD_BORDER` halo, so each
//! tile's core is at most `RCD_TILE_STEP` pixels. `origin_*` is the first
//! pixel of the tile's core; the sample window starts `RCD_BORDER` pixels
//! before it, computed in signed arithmetic. Tile origins are relative to the
//! complete mosaic and are always even, so a tile's local Bayer phase equals
//! the image's phase (the halo is even, too, so the window shares that phase).
//! Cores tile the image exactly: an eligible image is reconstructed
//! directionally to its edges, with the halo beyond an image edge filled by
//! mirroring (see `mirror_coordinate`, which preserves coordinate parity).
//! Images at or below the size gate have no tiles and stay entirely bilinear.

pub const RCD_TILE_SIZE: usize = 194;
pub const RCD_BORDER: usize = 10;
pub const RCD_TILE_STEP: usize = RCD_TILE_SIZE - 2 * RCD_BORDER;
pub const RCD_MIN_DIMENSION: usize = 2 * RCD_BORDER + 4;

/// One tile of the fixed grid. `origin_*` is the first pixel of the tile's
/// core (image coordinates); `width`/`height` are the full sample-window
/// extents, which begin `RCD_BORDER` pixels before the core origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcdTileGeometry {
    pub origin_x: usize,
    pub origin_y: usize,
    pub width: usize,
    pub height: usize,
}

impl RcdTileGeometry {
    /// The pixels this tile writes: its core, clipped only to the image, as
    /// `(x0, y0, x1, y1)`. The cores of all tiles cover the image exactly once.
    #[must_use]
    pub const fn core(self, width: usize, height: usize) -> (usize, usize, usize, usize) {
        let x_end = self.origin_x + RCD_TILE_STEP;
        let y_end = self.origin_y + RCD_TILE_STEP;
        (
            self.origin_x,
            self.origin_y,
            if x_end < width { x_end } else { width },
            if y_end < height { y_end } else { height },
        )
    }
}

/// Tile origins are always relative to the complete selected sensor mosaic.
/// Every tile writes a non-empty core and no partial tile is dropped; the
/// windows of edge and last tiles mirror into the image instead of clipping.
/// Images no larger than the minimum dimension yield no tiles at all and keep
/// the bilinear reconstruction.
pub fn rcd_tiles(width: usize, height: usize) -> impl Iterator<Item = RcdTileGeometry> {
    let counts = if width <= RCD_MIN_DIMENSION || height <= RCD_MIN_DIMENSION {
        (0, 0)
    } else {
        (
            width.div_ceil(RCD_TILE_STEP),
            height.div_ceil(RCD_TILE_STEP),
        )
    };
    (0..counts.1).flat_map(move |tile_y| {
        (0..counts.0).map(move |tile_x| RcdTileGeometry {
            origin_x: tile_x * RCD_TILE_STEP,
            origin_y: tile_y * RCD_TILE_STEP,
            width: RCD_TILE_SIZE,
            height: RCD_TILE_SIZE,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_the_size_gate_and_tile_counts() {
        assert_eq!(rcd_tiles(24, 194).count(), 0);
        assert_eq!(rcd_tiles(194, 24).count(), 0);
        assert_eq!(rcd_tiles(25, 25).count(), 1);
        // 198 and 199 need a second tile column whose core reaches the edge.
        assert_eq!(rcd_tiles(198, 194).count(), 4);
        assert_eq!(rcd_tiles(199, 194).count(), 4);
        assert_eq!(rcd_tiles(200, 370).count(), 6);
    }

    #[test]
    fn cores_tile_the_image_exactly_once() {
        for (width, height) in [
            (25, 25),
            (25, 40),
            (40, 25),
            (33, 33),
            (41, 37),
            (194, 194),
            (195, 195),
            (199, 199),
            (348, 348),
            (349, 175),
            (370, 380),
            (400, 230),
            (230, 370),
        ] {
            let mut written = vec![0_u8; width * height];
            for tile in rcd_tiles(width, height) {
                let (x0, y0, x1, y1) = tile.core(width, height);
                assert!(x0 < x1 && y0 < y1, "empty core at {width}x{height}");
                for y in y0..y1 {
                    for x in x0..x1 {
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
}
