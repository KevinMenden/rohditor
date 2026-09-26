//! Fixed RCD tile geometry shared by the CPU reference and GPU executor.

pub const RCD_TILE_SIZE: usize = 194;
pub const RCD_BORDER: usize = 10;
pub const RCD_TILE_STEP: usize = RCD_TILE_SIZE - 2 * RCD_BORDER;
pub const RCD_MIN_DIMENSION: usize = 2 * RCD_BORDER + 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RcdTileGeometry {
    pub origin_x: usize,
    pub origin_y: usize,
    pub width: usize,
    pub height: usize,
}

impl RcdTileGeometry {
    #[must_use]
    pub const fn core(self) -> (usize, usize, usize, usize) {
        (
            self.origin_x + RCD_BORDER,
            self.origin_y + RCD_BORDER,
            self.origin_x + self.width - RCD_BORDER,
            self.origin_y + self.height - RCD_BORDER,
        )
    }
}

/// Tile origins are always relative to the complete selected sensor mosaic.
/// Small images and final partial tiles without a complete neighborhood have
/// no RCD core; their output remains the bilinear base.
pub fn rcd_tiles(width: usize, height: usize) -> impl Iterator<Item = RcdTileGeometry> {
    let counts = if width <= RCD_MIN_DIMENSION || height <= RCD_MIN_DIMENSION {
        (0, 0)
    } else {
        (
            1 + (width - 2 * RCD_BORDER - 1) / RCD_TILE_STEP,
            1 + (height - 2 * RCD_BORDER - 1) / RCD_TILE_STEP,
        )
    };
    (0..counts.1).flat_map(move |tile_y| {
        (0..counts.0).filter_map(move |tile_x| {
            let origin_x = tile_x * RCD_TILE_STEP;
            let origin_y = tile_y * RCD_TILE_STEP;
            let width = RCD_TILE_SIZE.min(width - origin_x);
            let height = RCD_TILE_SIZE.min(height - origin_y);
            (width > RCD_MIN_DIMENSION && height > RCD_MIN_DIMENSION).then_some(RcdTileGeometry {
                origin_x,
                origin_y,
                width,
                height,
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_small_and_partial_tile_domains() {
        assert_eq!(rcd_tiles(24, 194).count(), 0);
        assert_eq!(
            rcd_tiles(25, 25).next().expect("eligible tile").core(),
            (10, 10, 15, 15)
        );
        assert_eq!(rcd_tiles(198, 194).count(), 1);
        assert_eq!(rcd_tiles(199, 194).count(), 2);
        let tiles: Vec<_> = rcd_tiles(200, 370).collect();
        assert_eq!(tiles.len(), 4);
        assert_eq!(tiles[3].core(), (184, 184, 190, 358));
    }
}
