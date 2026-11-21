//! Contract packing: canvas window -> `[1, 4, h/2, w/2]`, network RGB -> image.
//!
//! Input channels are `[R, G1, G2, B]`: the photosites at `(0, 0)`, `(1, 0)`,
//! `(0, 1)` and `(1, 1)` of each 2x2 block (the sub-pixel order of
//! `PixelShuffle(2)`). Each sample is multiplied by its colour's gain before
//! the network, and the output is divided by it afterwards.

use rohditor_image::MosaicImage;

use crate::NeuralError;
use crate::layout::{Canvas, Tile, Window};
use crate::runner::TileTensor;

/// As-shot white balance normalized to green = 1, as the model was trained.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Balance {
    gains: [f32; 3],
}

impl Balance {
    /// Only the ratios of `gains` (camera R, G, B) matter.
    pub fn new(gains: [f32; 3]) -> Result<Self, NeuralError> {
        if !gains.iter().all(|gain| gain.is_finite() && *gain > 0.0) {
            return Err(NeuralError::invalid_input(
                "white-balance gains must be finite and positive",
            ));
        }
        let green = gains[1];
        Ok(Self {
            gains: gains.map(|gain| gain / green),
        })
    }

    /// `[R, 1, B]`.
    #[must_use]
    pub const fn gains(self) -> [f32; 3] {
        self.gains
    }
}

/// Packed-channel colours: R, G1, G2, B.
const PACKED_COLOUR: [usize; 4] = [0, 1, 1, 2];

/// The balanced, packed network input for `window` of the canvas.
pub(crate) fn pack(
    mosaic: &MosaicImage<f32>,
    canvas: &Canvas,
    window: Window,
    balance: Balance,
) -> Result<TileTensor, NeuralError> {
    let (half_w, half_h) = (window.width / 2, window.height / 2);
    let plane = half_w * half_h;
    let mut data = vec![0.0; 4 * plane];
    let stride = mosaic.row_stride();
    let samples = mosaic.data();
    for py in 0..half_h {
        for (sub_y, channel_pair) in [(0, [0, 1]), (1, [2, 3])] {
            let source_y = canvas.source_y(window.y + 2 * py + sub_y);
            let row = &samples[source_y * stride..];
            for px in 0..half_w {
                for (sub_x, channel) in channel_pair.into_iter().enumerate() {
                    let source_x = canvas.source_x(window.x + 2 * px + sub_x);
                    data[channel * plane + py * half_w + px] =
                        row[source_x] * balance.gains[PACKED_COLOUR[channel]];
                }
            }
        }
    }
    TileTensor::new(4, half_h, half_w, data)
}

/// Write the core of one tile's network output into the camera-native RGB
/// image (row-major, `3 * width` stride), undoing the balance.
pub(crate) fn unpack_core(
    output: &TileTensor,
    canvas: &Canvas,
    tile: Tile,
    balance: Balance,
    image: &mut [f32],
    (width, height): (usize, usize),
) -> Result<(), NeuralError> {
    let expected = [3, tile.input.height, tile.input.width];
    if [output.channels(), output.height(), output.width()] != expected {
        return Err(NeuralError::UnexpectedOutputShape {
            expected,
            actual: output.shape().to_vec(),
        });
    }
    let inverse = balance.gains.map(f32::recip);
    let (offset_x, offset_y) = canvas.offset();
    // Image pixels inside this core.
    let x0 = tile.core.x.saturating_sub(offset_x);
    let x1 = (tile.core.x + tile.core.width)
        .saturating_sub(offset_x)
        .min(width);
    let y0 = tile.core.y.saturating_sub(offset_y);
    let y1 = (tile.core.y + tile.core.height)
        .saturating_sub(offset_y)
        .min(height);
    let planes = [output.plane(0), output.plane(1), output.plane(2)];
    for y in y0..y1 {
        let ty = y + offset_y - tile.input.y;
        let row = &mut image[y * 3 * width..(y + 1) * 3 * width];
        for x in x0..x1 {
            let index = ty * tile.input.width + (x + offset_x - tile.input.x);
            for channel in 0..3 {
                row[3 * x + channel] = planes[channel][index] * inverse[channel];
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Tiling;
    use rohditor_image::{BayerPattern, CfaColor};

    /// Unique values, so any misplaced sample shows up.
    fn ramp_mosaic(width: usize, height: usize, pattern: BayerPattern) -> MosaicImage<f32> {
        let data = (0..width * height).map(|i| i as f32 + 1.0).collect();
        MosaicImage::new(width, height, width, pattern, data).expect("mosaic")
    }

    #[test]
    fn balance_normalizes_to_green() {
        let balance = Balance::new([4.0, 2.0, 3.0]).expect("gains");
        assert_eq!(balance.gains(), [2.0, 1.0, 1.5]);
        assert!(Balance::new([1.0, 0.0, 1.0]).is_err());
        assert!(Balance::new([f32::NAN, 1.0, 1.0]).is_err());
    }

    #[test]
    fn packed_channels_hold_their_colour_and_gain() {
        let (width, height) = (38, 22);
        for pattern in [BayerPattern::Rggb, BayerPattern::Gbrg, BayerPattern::Bggr] {
            let mosaic = ramp_mosaic(width, height, pattern);
            let canvas = Canvas::new(width, height, pattern, Tiling::CONTRACT).expect("canvas");
            let window = canvas.tiles()[0].input;
            let balance = Balance::new([2.0, 1.0, 3.0]).expect("gains");
            let packed = pack(&mosaic, &canvas, window, balance).expect("pack");
            assert_eq!(packed.shape(), [1, 4, window.height / 2, window.width / 2]);
            let half_w = packed.width();
            for (channel, (sub_x, sub_y)) in
                [(0, 0), (1, 0), (0, 1), (1, 1)].into_iter().enumerate()
            {
                for (px, py) in [(0, 0), (20, 13), (half_w - 1, packed.height() - 1)] {
                    let (cx, cy) = (window.x + 2 * px + sub_x, window.y + 2 * py + sub_y);
                    let (x, y) = (canvas.source_x(cx), canvas.source_y(cy));
                    let colour = pattern.color_at(x, y);
                    let expected_colour = [
                        CfaColor::Red,
                        CfaColor::Green,
                        CfaColor::Green,
                        CfaColor::Blue,
                    ][channel];
                    assert_eq!(colour, expected_colour);
                    let gain = [2.0, 1.0, 3.0][colour.channel_index()];
                    let value = packed.plane(channel)[py * half_w + px];
                    assert_eq!(value, *mosaic.sample(x, y) * gain);
                }
            }
        }
    }

    /// Packing each tile and unpacking it again as RGB (every colour plane set
    /// to the balanced photosite value) must reproduce the mosaic exactly, on
    /// a non-square image whose sides are not multiples of 16.
    #[test]
    fn pack_unpack_roundtrip_restores_every_photosite() {
        let (width, height) = (70, 46);
        for pattern in [BayerPattern::Rggb, BayerPattern::Grbg, BayerPattern::Bggr] {
            let mosaic = ramp_mosaic(width, height, pattern);
            let tiling = Tiling::new(32, 16).expect("tiling");
            let canvas = Canvas::new(width, height, pattern, tiling).expect("canvas");
            let balance = Balance::new([1.7, 1.0, 2.3]).expect("gains");
            let mut image = vec![0.0; 3 * width * height];
            for tile in canvas.tiles() {
                let packed = pack(&mosaic, &canvas, tile.input, balance).expect("pack");
                let rgb = unshuffle_to_rgb(&packed);
                unpack_core(&rgb, &canvas, tile, balance, &mut image, (width, height))
                    .expect("unpack");
            }
            for y in 0..height {
                for x in 0..width {
                    let channel = pattern.color_at(x, y).channel_index();
                    let restored = image[3 * (y * width + x) + channel];
                    let original = *mosaic.sample(x, y);
                    assert!(
                        (restored - original).abs() <= original * 1e-6,
                        "{pattern:?} ({x}, {y}): {restored} vs {original}"
                    );
                }
            }
        }
    }

    /// Inverse of the packing: each full-resolution pixel takes its packed
    /// sample in the plane of its colour (other planes stay 0).
    /// The gains stay applied; `unpack_core` divides them out.
    fn unshuffle_to_rgb(packed: &TileTensor) -> TileTensor {
        let (half_w, half_h) = (packed.width(), packed.height());
        let (width, height) = (2 * half_w, 2 * half_h);
        let mut data = vec![0.0; 3 * width * height];
        for y in 0..height {
            for x in 0..width {
                let channel = (y % 2) * 2 + x % 2;
                let colour = PACKED_COLOUR[channel];
                let value = packed.plane(channel)[(y / 2) * half_w + x / 2];
                data[colour * width * height + y * width + x] = value;
            }
        }
        TileTensor::new(3, height, width, data).expect("rgb")
    }
}
