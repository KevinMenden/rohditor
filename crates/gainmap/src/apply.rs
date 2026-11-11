//! The inverse of [`crate::compute_gain_map`]: base + map -> HDR rendition.
//!
//! Real viewers do this on the GPU; this CPU version exists so tests can check the whole
//! encoding against the HDR rendition it was computed from.

use rayon::prelude::*;

use crate::{GainMapError, GainMapImage, GainMapMetadata, LinearRgbView};

/// Fraction of the full boost to apply for a display with `display_headroom_log2` stops of
/// headroom: 0 at (or below) the base headroom, 1 at (or above) the alternate headroom.
#[must_use]
pub fn boost_weight(metadata: &GainMapMetadata, display_headroom_log2: f32) -> f32 {
    let span = metadata.alternate_hdr_headroom_log2 - metadata.base_hdr_headroom_log2;
    if span <= 0.0 {
        return 0.0;
    }
    ((display_headroom_log2 - metadata.base_hdr_headroom_log2) / span).clamp(0.0, 1.0)
}

/// Applies the gain map to the linear SDR `base` for a display with the given headroom.
///
/// The map is upsampled bilinearly in its encoded domain, as decoders do. Any map size is
/// accepted, since both images are aligned by their edges and no size check is needed.
/// Only single-channel maps are supported (see the crate docs), although [`crate::iso21496`]
/// can already read three-channel metadata; others return
/// [`GainMapError::UnsupportedChannelCount`].
/// Returns interleaved linear RGB with the same size as `base`. `is_cancelled` is polled once
/// per row, as in [`crate::compute_gain_map`].
pub fn apply_gain_map(
    base: &LinearRgbView<'_>,
    map: &GainMapImage,
    metadata: &GainMapMetadata,
    display_headroom_log2: f32,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<f32>, GainMapError> {
    metadata.validate()?;
    if !display_headroom_log2.is_finite() {
        // `clamp` would pass a NaN through and turn the whole image into NaN.
        return Err(GainMapError::InvalidParams {
            name: "display_headroom_log2",
            reason: "must be finite",
        });
    }
    if map.channels() != 1 {
        return Err(GainMapError::UnsupportedChannelCount(map.channels()));
    }
    if metadata.channels.len() != 1 {
        return Err(GainMapError::UnsupportedChannelCount(
            metadata.channels.len(),
        ));
    }
    let channel = metadata.channels[0];
    let weight = boost_weight(metadata, display_headroom_log2);
    let width = base.width();
    // Cannot overflow: `LinearRgbView::new` checked `width * height * 3` already.
    let stride = width * 3;
    // The interpolation taps depend only on the column or row, so build them once.
    let x_taps = axis_taps(width, map.width());
    let y_taps = axis_taps(base.height(), map.height());
    let mut out = vec![0.0_f32; base.pixels().len()];

    out.par_chunks_mut(stride)
        .enumerate()
        .try_for_each(|(y, out_row)| {
            if is_cancelled() {
                return Err(GainMapError::Cancelled);
            }
            let base_row = base.row(y);
            for (x, x_tap) in x_taps.iter().enumerate() {
                let encoded = sample_bilinear(map, x_tap, &y_taps[y]) / 255.0;
                let recovery = encoded.powf(1.0 / channel.gamma);
                let boost = channel.min_log2 + (channel.max_log2 - channel.min_log2) * recovery;
                let gain = (boost * weight).exp2();
                for c in 0..3 {
                    let rgb = base_row[x * 3 + c];
                    out_row[x * 3 + c] =
                        ((rgb + channel.offset_sdr) * gain - channel.offset_hdr).max(0.0);
                }
            }
            Ok(())
        })?;
    Ok(out)
}

/// Bilinear sample of the encoded map for one column tap and one row tap.
fn sample_bilinear(map: &GainMapImage, x: &Tap, y: &Tap) -> f32 {
    let at = |mx: usize, my: usize| f32::from(map.data()[my * map.width() + mx]);
    let top = at(x.lower, y.lower) * (1.0 - x.fraction) + at(x.upper, y.lower) * x.fraction;
    let bottom = at(x.lower, y.upper) * (1.0 - x.fraction) + at(x.upper, y.upper) * x.fraction;
    top * (1.0 - y.fraction) + bottom * y.fraction
}

/// Where one base column (or row) samples the map along one axis.
struct Tap {
    lower: usize,
    upper: usize,
    /// Weight of `upper`; `lower` gets the rest.
    fraction: f32,
}

/// Taps for every position of an axis.
///
/// Map pixel `i` is centred at `(i + 0.5) * base_size / map_size` in base pixels, so both
/// images are aligned by their edges whatever the (possibly odd) sizes.
fn axis_taps(base_size: usize, map_size: usize) -> Vec<Tap> {
    let last = (map_size - 1) as f32;
    (0..base_size)
        .map(|pos| {
            let coord = (pos as f32 + 0.5) * map_size as f32 / base_size as f32 - 0.5;
            let coord = coord.clamp(0.0, last);
            let lower = coord.floor();
            // Index conversions are exact: both are small non-negative integers.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Tap {
                lower: lower as usize,
                upper: (lower + 1.0).min(last) as usize,
                fraction: coord - lower,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChannelMetadata;

    fn metadata() -> GainMapMetadata {
        GainMapMetadata::single_channel(ChannelMetadata::new(0.0, 3.0), 3.0)
    }

    #[test]
    fn weight_follows_display_headroom() {
        let m = metadata();
        assert_eq!(boost_weight(&m, -1.0), 0.0);
        assert_eq!(boost_weight(&m, 0.0), 0.0);
        assert_eq!(boost_weight(&m, 1.5), 0.5);
        assert_eq!(boost_weight(&m, 8.0), 1.0);
    }

    #[test]
    fn zero_weight_returns_the_base() {
        let pixels = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let base = LinearRgbView::new(2, 1, &pixels).unwrap();
        let map = GainMapImage::new(1, 1, 1, vec![255]).unwrap();
        let out = apply_gain_map(&base, &map, &metadata(), 0.0, &|| false).unwrap();
        for (a, b) in out.iter().zip(&pixels) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn full_map_applies_the_full_boost() {
        // Grey pixel with equal offsets: (v + o) * 8 - o.
        let pixels = [0.25, 0.25, 0.25];
        let base = LinearRgbView::new(1, 1, &pixels).unwrap();
        let map = GainMapImage::new(1, 1, 1, vec![255]).unwrap();
        let out = apply_gain_map(&base, &map, &metadata(), 3.0, &|| false).unwrap();
        let expected = (0.25 + 1.0 / 64.0) * 8.0 - 1.0 / 64.0;
        assert!(out.iter().all(|v| (v - expected).abs() < 1e-5));
    }

    #[test]
    fn decodes_with_gamma_and_negative_minimum() {
        // Encoded 128 with gamma 2 over [-1, 2] stops: recovery = (128/255)^(1/2).
        let mut meta = metadata();
        meta.channels[0].gamma = 2.0;
        meta.channels[0].min_log2 = -1.0;
        meta.channels[0].max_log2 = 2.0;
        meta.alternate_hdr_headroom_log2 = 2.0;
        let pixels = [0.25, 0.5, 0.75];
        let base = LinearRgbView::new(1, 1, &pixels).unwrap();
        let map = GainMapImage::new(1, 1, 1, vec![128]).unwrap();
        let out = apply_gain_map(&base, &map, &meta, 2.0, &|| false).unwrap();
        let recovery = (128.0_f32 / 255.0).sqrt();
        let gain = (-1.0 + 3.0 * recovery).exp2();
        let o = 1.0 / 64.0;
        for (got, rgb) in out.iter().zip(pixels) {
            let want = (rgb + o) * gain - o;
            assert!((got - want).abs() < 1e-5, "{got} vs {want}");
        }
    }

    #[test]
    fn stops_when_cancelled() {
        let pixels = [0.0; 6];
        let base = LinearRgbView::new(2, 1, &pixels).unwrap();
        let map = GainMapImage::new(1, 1, 1, vec![0]).unwrap();
        assert_eq!(
            apply_gain_map(&base, &map, &metadata(), 3.0, &|| true),
            Err(GainMapError::Cancelled)
        );
    }

    /// A non-square 2x3 map with a distinct value in every cell, so a swapped x/y tap changes
    /// the result.
    fn asymmetric_map() -> GainMapImage {
        GainMapImage::new(2, 3, 1, vec![0, 100, 50, 150, 200, 250]).unwrap()
    }

    #[test]
    fn upsampling_matches_known_bilinear_samples() {
        let map = asymmetric_map();
        let x_taps = axis_taps(4, 2);
        let y_taps = axis_taps(6, 3);
        let at = |x: usize, y: usize| sample_bilinear(&map, &x_taps[x], &y_taps[y]);
        // Base column 1 is 0.25 map pixels right of map column 0; row 1 is 0.25 below row 0.
        assert_eq!(at(0, 0), 0.0); // clamped to the corner
        assert_eq!(at(1, 0), 25.0); // horizontal only: 0 -> 100
        assert_eq!(at(0, 1), 12.5); // vertical only: 0 -> 50
        assert_eq!(at(1, 1), 37.5); // both: 25 -> 75
        assert_eq!(at(3, 5), 250.0); // opposite corner
        assert_eq!(at(3, 3), 175.0); // row 3 = 1.25 -> 0.25 between map rows 1 and 2
    }

    #[test]
    fn apply_uses_the_upsampled_map_for_every_pixel() {
        // Black base: output = offset * (2^(3 * encoded) - 1), so it exposes the sampled
        // value of each pixel directly.
        let pixels = [0.0; 4 * 6 * 3];
        let base = LinearRgbView::new(4, 6, &pixels).unwrap();
        let map = asymmetric_map();
        let out = apply_gain_map(&base, &map, &metadata(), 3.0, &|| false).unwrap();
        let (x_taps, y_taps) = (axis_taps(4, 2), axis_taps(6, 3));
        let o = 1.0 / 64.0;
        for y in 0..6 {
            for x in 0..4 {
                let encoded = sample_bilinear(&map, &x_taps[x], &y_taps[y]) / 255.0;
                let want = o * (3.0 * encoded).exp2() - o;
                let got = out[(y * 4 + x) * 3];
                assert!((got - want).abs() < 1e-5, "({x}, {y}): {got} vs {want}");
            }
        }
        // Spot check against the literal sample above: pixel (1, 1) samples 37.5 / 255.
        let want = o * (3.0 * 37.5_f32 / 255.0).exp2() - o;
        assert!((out[(4 + 1) * 3] - want).abs() < 1e-5);
    }

    #[test]
    fn rejects_non_finite_display_headroom() {
        let pixels = [0.1; 3];
        let base = LinearRgbView::new(1, 1, &pixels).unwrap();
        let map = GainMapImage::new(1, 1, 1, vec![255]).unwrap();
        for headroom in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(matches!(
                apply_gain_map(&base, &map, &metadata(), headroom, &|| false),
                Err(GainMapError::InvalidParams { .. })
            ));
        }
    }

    #[test]
    fn rejects_three_channel_maps() {
        let pixels = [0.0; 3];
        let base = LinearRgbView::new(1, 1, &pixels).unwrap();
        let map = GainMapImage::new(1, 1, 3, vec![0; 3]).unwrap();
        assert_eq!(
            apply_gain_map(&base, &map, &metadata(), 3.0, &|| false),
            Err(GainMapError::UnsupportedChannelCount(3))
        );
    }
}
