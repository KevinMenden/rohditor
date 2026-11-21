//! Where every tile comes from and where its output goes.
//!
//! The network needs an RGGB mosaic whose sides are multiples of
//! [`SIZE_MULTIPLE`], and context around each tile. Both come from one virtual
//! *canvas* in full-resolution coordinates:
//!
//! ```text
//!   | halo | phase | image (W) | pad to a multiple of 16 | halo |
//! ```
//!
//! `phase` is 0 or 1 so that canvas `(0, 0)` is a red photosite. Canvas samples
//! outside the image mirror back into it (whole-sample reflection, `-k -> k`),
//! which keeps the CFA phase, as the model's own bilinear branch does. The
//! *body* (everything between the halos) is cut into tiles; each tile is run
//! with its halo and only its core is kept. Every tile edge sits on a multiple
//! of 16, so the network's pooling grid is the same in every tile.

use rohditor_image::{BayerPattern, CfaColor};

use crate::NeuralError;

/// Full-resolution sides must be multiples of this (`ml/contract.json`).
pub const SIZE_MULTIPLE: usize = 16;

/// Tile size and halo in full-resolution pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tiling {
    tile: usize,
    halo: usize,
}

impl Tiling {
    /// The contract's 1024 px tiles with a 128 px halo. The halo covers the
    /// network's whole receptive field (about 100 px), so stitched tiles equal
    /// one pass over the image; a 32 px halo left seams of up to 1e-2.
    pub const CONTRACT: Self = Self {
        tile: 1024,
        halo: 128,
    };

    /// Both must be multiples of [`SIZE_MULTIPLE`]; `tile` must be positive.
    pub fn new(tile: usize, halo: usize) -> Result<Self, NeuralError> {
        if tile == 0 || !tile.is_multiple_of(SIZE_MULTIPLE) || !halo.is_multiple_of(SIZE_MULTIPLE) {
            return Err(NeuralError::invalid_input(format!(
                "tile {tile} and halo {halo} must be multiples of {SIZE_MULTIPLE} (tile > 0)"
            )));
        }
        Ok(Self { tile, halo })
    }

    #[must_use]
    pub const fn tile(self) -> usize {
        self.tile
    }

    #[must_use]
    pub const fn halo(self) -> usize {
        self.halo
    }
}

impl Default for Tiling {
    fn default() -> Self {
        Self::CONTRACT
    }
}

/// An axis-aligned window in canvas coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// One tile: the window the network sees and the core it contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    pub input: Window,
    pub core: Window,
}

/// The canvas for one image: its mapping back to image coordinates and its tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Canvas {
    image_width: usize,
    image_height: usize,
    /// Canvas coordinate of image pixel `(0, 0)`.
    offset_x: usize,
    offset_y: usize,
    body_width: usize,
    body_height: usize,
    tiling: Tiling,
}

impl Canvas {
    pub fn new(
        width: usize,
        height: usize,
        pattern: BayerPattern,
        tiling: Tiling,
    ) -> Result<Self, NeuralError> {
        if width < 2 || height < 2 {
            return Err(NeuralError::invalid_input(
                "neural demosaicing requires at least 2x2 samples",
            ));
        }
        let (phase_x, phase_y) = red_phase(pattern);
        let body = |size: usize, phase: usize| {
            size.checked_add(phase)
                .and_then(|n| n.checked_next_multiple_of(SIZE_MULTIPLE))
                .ok_or_else(|| NeuralError::invalid_input("canvas size overflowed"))
        };
        Ok(Self {
            image_width: width,
            image_height: height,
            offset_x: tiling.halo + phase_x,
            offset_y: tiling.halo + phase_y,
            body_width: body(width, phase_x)?,
            body_height: body(height, phase_y)?,
            tiling,
        })
    }

    /// Canvas coordinate of image pixel `(0, 0)`.
    #[must_use]
    pub const fn offset(&self) -> (usize, usize) {
        (self.offset_x, self.offset_y)
    }

    /// Image row/column sampled at canvas coordinate `c` along one axis.
    #[must_use]
    pub fn source_x(&self, c: usize) -> usize {
        mirror(c as isize - self.offset_x as isize, self.image_width)
    }

    #[must_use]
    pub fn source_y(&self, c: usize) -> usize {
        mirror(c as isize - self.offset_y as isize, self.image_height)
    }

    /// Tiles in row-major order, covering the body exactly once.
    #[must_use]
    pub fn tiles(&self) -> Vec<Tile> {
        let Tiling { tile, halo } = self.tiling;
        let spans = |body: usize| {
            (0..body.div_ceil(tile))
                .map(move |i| (halo + i * tile, tile.min(body - i * tile)))
                .collect::<Vec<_>>()
        };
        let columns = spans(self.body_width);
        spans(self.body_height)
            .into_iter()
            .flat_map(|(y, height)| {
                columns.iter().map(move |&(x, width)| Tile {
                    input: Window {
                        x: x - halo,
                        y: y - halo,
                        width: width + 2 * halo,
                        height: height + 2 * halo,
                    },
                    core: Window {
                        x,
                        y,
                        width,
                        height,
                    },
                })
            })
            .collect()
    }
}

/// Position of the red photosite inside the 2x2 pattern.
fn red_phase(pattern: BayerPattern) -> (usize, usize) {
    match (0..4)
        .map(|i| (i & 1, i >> 1))
        .find(|&(x, y)| pattern.color_at(x, y) == CfaColor::Red)
    {
        Some(phase) => phase,
        None => unreachable!("every Bayer pattern has a red photosite"),
    }
}

/// Whole-sample reflection into `[0, n)`: `-k -> k`, `n - 1 + k -> n - 1 - k`.
/// Repeats for offsets larger than the image, so tiny images still work.
/// Both reflections keep the parity, so the CFA colour is preserved.
pub(crate) fn mirror(i: isize, n: usize) -> usize {
    debug_assert!(n >= 2);
    let period = 2 * (n as isize - 1);
    let folded = i.rem_euclid(period);
    if folded < n as isize {
        folded as usize
    } else {
        (period - folded) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_reflects_without_repeating_the_edge_and_keeps_parity() {
        let n = 5;
        let mapped: Vec<_> = (-6..11).map(|i| mirror(i, n)).collect();
        assert_eq!(mapped, [2, 3, 4, 3, 2, 1, 0, 1, 2, 3, 4, 3, 2, 1, 0, 1, 2]);
        for i in -40..40 {
            assert_eq!(mirror(i, n) % 2, i.rem_euclid(2) as usize);
        }
    }

    #[test]
    fn canvas_origin_is_red_for_every_pattern() {
        for pattern in [
            BayerPattern::Rggb,
            BayerPattern::Grbg,
            BayerPattern::Gbrg,
            BayerPattern::Bggr,
        ] {
            let canvas = Canvas::new(37, 22, pattern, Tiling::CONTRACT).expect("canvas");
            for (cx, cy, colour) in [
                (0, 0, CfaColor::Red),
                (1, 0, CfaColor::Green),
                (0, 1, CfaColor::Green),
                (1, 1, CfaColor::Blue),
                (40, 50, CfaColor::Red),
            ] {
                let (x, y) = (canvas.source_x(cx), canvas.source_y(cy));
                assert_eq!(
                    pattern.color_at(x, y),
                    colour,
                    "{pattern:?} at ({cx}, {cy})"
                );
            }
        }
    }

    #[test]
    fn tiles_cover_the_body_once_on_the_pooling_grid() {
        let tiling = Tiling::new(64, 32).expect("tiling");
        // Odd phase (BGGR) and sizes that are not multiples of 16.
        let canvas = Canvas::new(150, 70, BayerPattern::Bggr, tiling).expect("canvas");
        let (body_w, body_h) = (160, 80); // 151 and 71 rounded up to 16
        let tiles = canvas.tiles();
        assert_eq!(tiles.len(), 3 * 2);
        let covered: usize = tiles.iter().map(|t| t.core.width * t.core.height).sum();
        assert_eq!(covered, body_w * body_h);
        for tile in &tiles {
            let Tile { input, core } = tile;
            for value in [
                input.x,
                input.y,
                input.width,
                input.height,
                core.x,
                core.width,
            ] {
                assert_eq!(value % SIZE_MULTIPLE, 0, "{tile:?}");
            }
            assert_eq!(core.x - input.x, 32);
            assert!(input.x + input.width <= body_w + 64);
            assert!(input.y + input.height <= body_h + 64);
        }
        // The image sits inside the body.
        assert_eq!(canvas.offset(), (33, 33));
        assert!(33 + 150 <= 32 + body_w && 33 + 70 <= 32 + body_h);
    }

    #[test]
    fn tiling_must_stay_on_the_size_multiple() {
        assert!(Tiling::new(500, 32).is_err());
        assert!(Tiling::new(512, 30).is_err());
        assert!(Tiling::new(0, 32).is_err());
        assert!(Tiling::new(512, 0).is_ok());
    }
}
