//! SDR base + HDR rendition -> encoded gain map and its metadata.

use rayon::prelude::*;

use crate::image::sample_count;
use crate::{
    ChannelMetadata, DEFAULT_OFFSET, GainMapError, GainMapImage, GainMapMetadata, LinearRgbView,
    P3_LUMA_WEIGHTS,
};

/// Luminance is limited to this before the ratio so one infinite pixel cannot overflow the
/// division. It is far above any boost the encoding can represent, so the pixel simply lands
/// on the maximum boost.
const MAX_LUMINANCE: f32 = 1.0e6;

/// Choices that shape a gain map. The defaults describe a 3-stop (8x) luminance map at half
/// resolution in Display P3.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GainMapParams {
    /// Weights turning linear RGB into luminance, in the inputs' primaries.
    pub luma_weights: [f32; 3],
    pub offset_sdr: f32,
    pub offset_hdr: f32,
    /// Smallest representable boost in stops. 0 means the map never darkens the base.
    pub min_boost_log2: f32,
    /// Largest representable boost in stops; larger gains are clamped to it.
    pub max_boost_log2: f32,
    pub gamma: f32,
    /// Headroom of the HDR rendition in stops, written to the metadata. Usually equal to
    /// `max_boost_log2`.
    pub hdr_headroom_log2: f32,
    /// Linear reduction factor: the map is `ceil(width / downsample)` wide. 2 is half size.
    pub downsample: usize,
}

impl Default for GainMapParams {
    fn default() -> Self {
        Self {
            luma_weights: P3_LUMA_WEIGHTS,
            offset_sdr: DEFAULT_OFFSET,
            offset_hdr: DEFAULT_OFFSET,
            min_boost_log2: 0.0,
            max_boost_log2: 3.0,
            gamma: 1.0,
            hdr_headroom_log2: 3.0,
            downsample: 2,
        }
    }
}

impl GainMapParams {
    /// Metadata a reader needs for a map computed with these parameters.
    fn metadata(&self) -> GainMapMetadata {
        GainMapMetadata::single_channel(
            ChannelMetadata {
                min_log2: self.min_boost_log2,
                max_log2: self.max_boost_log2,
                gamma: self.gamma,
                offset_sdr: self.offset_sdr,
                offset_hdr: self.offset_hdr,
            },
            self.hdr_headroom_log2,
        )
    }

    fn validate(&self) -> Result<(), GainMapError> {
        if self.downsample == 0 {
            return Err(GainMapError::InvalidParams {
                name: "downsample",
                reason: "must be at least 1",
            });
        }
        if self.luma_weights.iter().any(|w| !w.is_finite() || *w < 0.0)
            || self.luma_weights.iter().sum::<f32>() <= 0.0
        {
            return Err(GainMapError::InvalidParams {
                name: "luma_weights",
                reason: "must be finite, non-negative and not all zero",
            });
        }
        // The remaining fields are exactly the metadata fields; one validator covers them.
        self.metadata().validate()
    }
}

/// Computes the encoded luminance gain map and the metadata that decodes it.
///
/// Both images must share size and primaries (see the crate docs). Non-finite samples do not
/// fail the export: NaN counts as black and infinity as the brightest representable value.
pub fn compute_gain_map(
    sdr: &LinearRgbView<'_>,
    hdr: &LinearRgbView<'_>,
    params: &GainMapParams,
) -> Result<(GainMapImage, GainMapMetadata), GainMapError> {
    if (sdr.width(), sdr.height()) != (hdr.width(), hdr.height()) {
        return Err(GainMapError::SizeMismatch {
            sdr_width: sdr.width(),
            sdr_height: sdr.height(),
            hdr_width: hdr.width(),
            hdr_height: hdr.height(),
        });
    }
    params.validate()?;

    let factor = params.downsample;
    let map_width = sdr.width().div_ceil(factor);
    let map_height = sdr.height().div_ceil(factor);
    let mut data = vec![0_u8; sample_count(map_width, map_height, 1)?];

    // Each map row only reads its own `factor` source rows, so rows are independent and no
    // full-resolution gain plane is ever allocated.
    data.par_chunks_mut(map_width)
        .enumerate()
        .for_each(|(map_y, out_row)| {
            let first = map_y * factor;
            let last = (first + factor).min(sdr.height());
            let mut sums = vec![0.0_f32; map_width];
            for y in first..last {
                accumulate_row(sdr.row(y), hdr.row(y), params, &mut sums);
            }
            let rows = (last - first) as f32;
            for (map_x, (out, sum)) in out_row.iter_mut().zip(&sums).enumerate() {
                let columns = block_columns(map_x, factor, sdr.width()) as f32;
                *out = encode(sum / (rows * columns), params);
            }
        });

    let image = GainMapImage::new(map_width, map_height, 1, data)?;
    Ok((image, params.metadata()))
}

/// Number of source columns covered by map column `map_x` (the last block can be partial).
fn block_columns(map_x: usize, factor: usize, width: usize) -> usize {
    (width - map_x * factor).min(factor)
}

/// Adds the clamped log2 gain of every pixel in a source row to its block's running sum.
fn accumulate_row(sdr: &[f32], hdr: &[f32], params: &GainMapParams, sums: &mut [f32]) {
    let (sdr, hdr) = (sdr.as_chunks::<3>().0, hdr.as_chunks::<3>().0);
    for (x, (s, h)) in sdr.iter().zip(hdr).enumerate() {
        let sdr_luma = luminance(s, &params.luma_weights);
        let hdr_luma = luminance(h, &params.luma_weights);
        let gain = ((hdr_luma + params.offset_hdr) / (sdr_luma + params.offset_sdr)).log2();
        // Zero offsets make black over black 0/0; call that "no boost" rather than NaN.
        let gain = if gain.is_nan() { 0.0 } else { gain };
        // Clamping before averaging keeps an extreme pixel from dragging its whole block.
        sums[x / params.downsample] += gain.clamp(params.min_boost_log2, params.max_boost_log2);
    }
}

fn luminance(rgb: &[f32; 3], weights: &[f32; 3]) -> f32 {
    let luma: f32 = rgb.iter().zip(weights).map(|(v, w)| sanitize(*v) * w).sum();
    luma.min(MAX_LUMINANCE)
}

/// NaN -> 0, negatives -> 0, infinity -> [`MAX_LUMINANCE`].
fn sanitize(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, MAX_LUMINANCE)
    }
}

/// Normalises a mean log2 gain to `[0, 1]`, applies gamma and rounds to 8 bits.
fn encode(mean_gain_log2: f32, params: &GainMapParams) -> u8 {
    let range = params.max_boost_log2 - params.min_boost_log2;
    let normalised = if range > 0.0 {
        ((mean_gain_log2 - params.min_boost_log2) / range).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // Truncation is safe: the value is in [0, 255] after rounding.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = (normalised.powf(params.gamma) * 255.0).round() as u8;
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(width: usize, height: usize, value: f32) -> Vec<f32> {
        vec![value; width * height * 3]
    }

    fn compute(sdr: &[f32], hdr: &[f32], w: usize, h: usize, p: &GainMapParams) -> GainMapImage {
        let sdr = LinearRgbView::new(w, h, sdr).unwrap();
        let hdr = LinearRgbView::new(w, h, hdr).unwrap();
        compute_gain_map(&sdr, &hdr, p).unwrap().0
    }

    #[test]
    fn equal_images_give_zero_map() {
        let img = grey(4, 2, 0.4);
        let map = compute(&img, &img, 4, 2, &GainMapParams::default());
        assert_eq!((map.width(), map.height()), (2, 1));
        assert!(map.data().iter().all(|b| *b == 0));
    }

    #[test]
    fn full_boost_saturates_and_clamps_beyond() {
        // hdr = 8 * sdr with equal offsets gives exactly 3 stops only for offset 0.
        let params = GainMapParams {
            offset_sdr: 0.0,
            offset_hdr: 0.0,
            ..GainMapParams::default()
        };
        let sdr = grey(2, 2, 0.25);
        let at_max = compute(&sdr, &grey(2, 2, 2.0), 2, 2, &params);
        let beyond = compute(&sdr, &grey(2, 2, 40.0), 2, 2, &params);
        assert_eq!(at_max.data(), [255]);
        assert_eq!(beyond.data(), [255]);
    }

    #[test]
    fn one_stop_is_a_third_of_the_range() {
        let params = GainMapParams {
            offset_sdr: 0.0,
            offset_hdr: 0.0,
            ..GainMapParams::default()
        };
        let map = compute(&grey(2, 2, 0.25), &grey(2, 2, 0.5), 2, 2, &params);
        assert_eq!(map.data(), [85]); // 255 / 3
    }

    #[test]
    fn black_pixels_use_the_offset_ratio_only() {
        let black = grey(2, 2, 0.0);
        let map = compute(&black, &black, 2, 2, &GainMapParams::default());
        assert_eq!(map.data(), [0]);
        // Black SDR under a bright HDR pixel is a huge ratio and clamps to the maximum.
        let map = compute(&black, &grey(2, 2, 1.0), 2, 2, &GainMapParams::default());
        assert_eq!(map.data(), [255]);
    }

    #[test]
    fn black_over_black_with_zero_offsets_is_no_boost() {
        let params = GainMapParams {
            offset_sdr: 0.0,
            offset_hdr: 0.0,
            min_boost_log2: -1.0,
            max_boost_log2: 1.0,
            ..GainMapParams::default()
        };
        let black = grey(2, 2, 0.0);
        // 0 stops sits in the middle of [-1, 1].
        assert_eq!(compute(&black, &black, 2, 2, &params).data(), [128]);
    }

    #[test]
    fn non_finite_input_is_sanitised() {
        let params = GainMapParams::default();
        let sdr = grey(2, 2, 0.2);
        let mut hdr = grey(2, 2, 0.2);
        hdr[0] = f32::NAN;
        hdr[4] = f32::INFINITY;
        hdr[7] = f32::NEG_INFINITY;
        let map = compute(&sdr, &hdr, 2, 2, &params);
        // A NaN/-inf pixel counts as black (cannot raise the boost above 0 here) and +inf
        // counts as the maximum, so the block is the mean of 0, 0, 1 and 0 of the range.
        assert_eq!(map.data().len(), 1);
        assert!(map.data()[0] > 0 && map.data()[0] < 255);
        // Non-finite SDR input must not panic or poison the output either.
        let mut bad_sdr = grey(2, 2, 0.2);
        bad_sdr[1] = f32::NAN;
        bad_sdr[5] = f32::INFINITY;
        let _ = compute(&bad_sdr, &sdr, 2, 2, &params);
    }

    #[test]
    fn odd_sizes_average_partial_blocks() {
        // 3x3 source, factor 2 -> 2x2 map: the right column and bottom row of blocks are
        // partial (1 pixel wide or tall), and must be averaged over their real size.
        let (w, h) = (3, 3);
        let params = GainMapParams {
            offset_sdr: 0.0,
            offset_hdr: 0.0,
            ..GainMapParams::default()
        };
        let sdr = grey(w, h, 0.25);
        let mut hdr = grey(w, h, 0.25);
        // Boost only the bottom-right source pixel by the full 3 stops.
        let i = (2 * w + 2) * 3;
        hdr[i..i + 3].fill(2.0);
        let map = compute(&sdr, &hdr, w, h, &params);
        assert_eq!((map.width(), map.height()), (2, 2));
        // The bottom-right block is a 1x1 partial block, so it is not diluted.
        assert_eq!(map.data(), [0, 0, 0, 255]);
    }

    #[test]
    fn asymmetric_boost_lands_in_the_right_block() {
        let (w, h) = (4, 2);
        let params = GainMapParams {
            offset_sdr: 0.0,
            offset_hdr: 0.0,
            ..GainMapParams::default()
        };
        let sdr = grey(w, h, 0.25);
        let mut hdr = grey(w, h, 0.25);
        // One of the four pixels in the right block gets 3 stops: block mean = 3/4 of range.
        hdr[(2 * 3)..(2 * 3 + 3)].fill(2.0);
        let map = compute(&sdr, &hdr, w, h, &params);
        assert_eq!(map.data(), [0, 64]); // round(255 * 0.25)
    }

    #[test]
    fn gamma_is_applied_to_the_normalised_value() {
        let params = GainMapParams {
            offset_sdr: 0.0,
            offset_hdr: 0.0,
            gamma: 2.0,
            ..GainMapParams::default()
        };
        // 1.5 stops = 0.5 normalised -> 0.25 after gamma.
        let sdr = grey(2, 2, 0.25);
        let map = compute(&sdr, &grey(2, 2, 0.25 * 2.0_f32.powf(1.5)), 2, 2, &params);
        assert_eq!(map.data(), [64]);
    }

    #[test]
    fn rejects_mismatched_sizes_and_bad_params() {
        let a = grey(2, 2, 0.1);
        let b = grey(4, 2, 0.1);
        let view_a = LinearRgbView::new(2, 2, &a).unwrap();
        let view_b = LinearRgbView::new(4, 2, &b).unwrap();
        assert!(matches!(
            compute_gain_map(&view_a, &view_b, &GainMapParams::default()),
            Err(GainMapError::SizeMismatch { .. })
        ));
        let params = GainMapParams {
            downsample: 0,
            ..GainMapParams::default()
        };
        assert!(compute_gain_map(&view_a, &view_a, &params).is_err());
        let params = GainMapParams {
            luma_weights: [0.0; 3],
            ..GainMapParams::default()
        };
        assert!(compute_gain_map(&view_a, &view_a, &params).is_err());
    }

    #[test]
    fn rejects_wrong_buffer_length() {
        assert!(LinearRgbView::new(2, 2, &[0.0; 11]).is_err());
        assert!(LinearRgbView::new(0, 2, &[]).is_err());
    }
}
