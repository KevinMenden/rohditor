//! Deterministic target-gamut mapping for linear sRGB output.

/// Numerical contract for the first chroma-compressing output mapper.
pub const CHROMA_COMPRESS_ALGORITHM_VERSION: u16 = 1;
/// Fixed search depth shared with the GPU implementation.
pub const CHROMA_COMPRESS_SEARCH_ITERATIONS: u32 = 16;
/// Tolerance used for neutral and final gamut-boundary handling.
pub const CHROMA_COMPRESS_EPSILON: f32 = 1.0e-6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamutMapStatus {
    InGamut,
    Compressed,
    Limited,
    ClippedFallback,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GamutMapResult {
    pub linear_srgb: [f32; 3],
    pub status: GamutMapStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GamutMappingDiagnostics {
    pub in_gamut_pixels: u64,
    pub compressed_pixels: u64,
    pub limited_pixels: u64,
    pub clipped_fallback_pixels: u64,
    pub invalid_pixels: u64,
}

impl GamutMappingDiagnostics {
    pub fn record(&mut self, status: GamutMapStatus) {
        match status {
            GamutMapStatus::InGamut => self.in_gamut_pixels += 1,
            GamutMapStatus::Compressed => self.compressed_pixels += 1,
            GamutMapStatus::Limited => self.limited_pixels += 1,
            GamutMapStatus::ClippedFallback => self.clipped_fallback_pixels += 1,
            GamutMapStatus::Invalid => self.invalid_pixels += 1,
        }
    }

    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        Self {
            in_gamut_pixels: self.in_gamut_pixels + other.in_gamut_pixels,
            compressed_pixels: self.compressed_pixels + other.compressed_pixels,
            limited_pixels: self.limited_pixels + other.limited_pixels,
            clipped_fallback_pixels: self.clipped_fallback_pixels + other.clipped_fallback_pixels,
            invalid_pixels: self.invalid_pixels + other.invalid_pixels,
        }
    }

    #[must_use]
    pub const fn total_pixels(self) -> u64 {
        self.in_gamut_pixels
            + self.compressed_pixels
            + self.limited_pixels
            + self.clipped_fallback_pixels
            + self.invalid_pixels
    }
}

/// Hard-clip linear sRGB while giving invalid values deterministic finite output.
#[must_use]
pub fn clip_linear_srgb(rgb: [f32; 3]) -> GamutMapResult {
    let finite = rgb.iter().all(|value| value.is_finite());
    let in_gamut = finite && is_in_gamut(rgb);
    let linear_srgb = rgb.map(sanitize_and_clip);
    let status = if !finite {
        GamutMapStatus::Invalid
    } else if in_gamut {
        GamutMapStatus::InGamut
    } else {
        GamutMapStatus::ClippedFallback
    };
    GamutMapResult {
        linear_srgb,
        status,
    }
}

/// Preserve in-gamut values exactly and reduce OKLCH chroma for out-of-gamut pixels.
#[must_use]
pub fn compress_linear_srgb_chroma(rgb: [f32; 3]) -> GamutMapResult {
    if !rgb.iter().all(|value| value.is_finite()) {
        return invalid_fallback(rgb);
    }
    if is_in_gamut(rgb) {
        return GamutMapResult {
            linear_srgb: rgb,
            status: GamutMapStatus::InGamut,
        };
    }

    let [lightness, a, b] = linear_srgb_to_oklab(rgb);
    if ![lightness, a, b].iter().all(|value| value.is_finite()) {
        return clipped_fallback(rgb);
    }
    let limited_lightness = lightness.clamp(0.0, 1.0);
    let lightness_limited = limited_lightness != lightness;
    let chroma = a.hypot(b);
    if chroma <= CHROMA_COMPRESS_EPSILON {
        return GamutMapResult {
            linear_srgb: oklab_to_linear_srgb([limited_lightness, 0.0, 0.0]).map(sanitize_and_clip),
            status: if lightness_limited {
                GamutMapStatus::Limited
            } else {
                GamutMapStatus::Compressed
            },
        };
    }

    // Scaling the OKLab a/b pair is equivalent to preserving OKLCH hue while
    // searching a single monotonic chroma coordinate.
    let mut lower = 0.0_f32;
    let mut upper = 1.0_f32;
    for _ in 0..CHROMA_COMPRESS_SEARCH_ITERATIONS {
        let scale = (lower + upper) * 0.5;
        let candidate = oklab_to_linear_srgb([limited_lightness, a * scale, b * scale]);
        if candidate.iter().all(|value| value.is_finite()) && is_in_gamut(candidate) {
            lower = scale;
        } else {
            upper = scale;
        }
    }
    let mapped = oklab_to_linear_srgb([limited_lightness, a * lower, b * lower]);
    if !mapped.iter().all(|value| value.is_finite()) {
        return clipped_fallback(rgb);
    }
    GamutMapResult {
        linear_srgb: mapped.map(sanitize_and_clip),
        status: if lightness_limited {
            GamutMapStatus::Limited
        } else {
            GamutMapStatus::Compressed
        },
    }
}

fn invalid_fallback(rgb: [f32; 3]) -> GamutMapResult {
    GamutMapResult {
        linear_srgb: rgb.map(sanitize_and_clip),
        status: GamutMapStatus::Invalid,
    }
}

fn clipped_fallback(rgb: [f32; 3]) -> GamutMapResult {
    GamutMapResult {
        linear_srgb: rgb.map(sanitize_and_clip),
        status: GamutMapStatus::ClippedFallback,
    }
}

fn sanitize_and_clip(value: f32) -> f32 {
    if value.is_nan() || value == f32::NEG_INFINITY {
        0.0
    } else if value == f32::INFINITY {
        1.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

fn is_in_gamut(rgb: [f32; 3]) -> bool {
    rgb.into_iter().all(|value| (0.0..=1.0).contains(&value))
}

fn linear_srgb_to_oklab([red, green, blue]: [f32; 3]) -> [f32; 3] {
    let l = 0.412_221_46 * red + 0.536_332_55 * green + 0.051_445_995 * blue;
    let m = 0.211_903_5 * red + 0.680_699_5 * green + 0.107_396_96 * blue;
    let s = 0.088_302_46 * red + 0.281_718_85 * green + 0.629_978_7 * blue;
    let l_root = l.cbrt();
    let m_root = m.cbrt();
    let s_root = s.cbrt();
    [
        0.210_454_26 * l_root + 0.793_617_8 * m_root - 0.004_072_047 * s_root,
        1.977_998_5 * l_root - 2.428_592_2 * m_root + 0.450_593_7 * s_root,
        0.025_904_037 * l_root + 0.782_771_77 * m_root - 0.808_675_77 * s_root,
    ]
}

fn oklab_to_linear_srgb([lightness, a, b]: [f32; 3]) -> [f32; 3] {
    let l_root = lightness + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_root = lightness - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_root = lightness - 0.089_484_18 * a - 1.291_485_5 * b;
    let l = l_root * l_root * l_root;
    let m = m_root * m_root * m_root;
    let s = s_root * s_root * s_root;
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rgb_close(actual: [f32; 3], expected: [f32; 3], tolerance: f32) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= tolerance,
                "{actual} != {expected}"
            );
        }
    }

    #[test]
    fn in_gamut_values_are_bit_exact() {
        for rgb in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.13, 0.47, 0.91],
            [f32::MIN_POSITIVE, 0.5, 1.0 - f32::EPSILON],
        ] {
            let mapped = compress_linear_srgb_chroma(rgb);
            assert_eq!(mapped.linear_srgb.map(f32::to_bits), rgb.map(f32::to_bits));
            assert_eq!(mapped.status, GamutMapStatus::InGamut);
        }
    }

    #[test]
    fn saturated_values_compress_without_independent_channel_clipping() {
        for rgb in [
            [1.4, 0.2, 0.1],
            [-0.2, 0.7, 1.3],
            [2.0, -0.4, 0.8],
            [0.2, 1.8, -0.3],
        ] {
            let mapped = compress_linear_srgb_chroma(rgb);
            assert!(
                mapped
                    .linear_srgb
                    .into_iter()
                    .all(|value| (0.0..=1.0).contains(&value))
            );
            assert!(matches!(
                mapped.status,
                GamutMapStatus::Compressed | GamutMapStatus::Limited
            ));
            assert_ne!(mapped.linear_srgb, rgb.map(|value| value.clamp(0.0, 1.0)));
        }
    }

    #[test]
    fn oklab_round_trip_handles_asymmetric_signed_values() {
        for rgb in [[0.17, 0.43, 0.81], [-0.08, 0.61, 1.24], [1.7, -0.2, 0.35]] {
            let restored = oklab_to_linear_srgb(linear_srgb_to_oklab(rgb));
            assert_rgb_close(restored, rgb, 2.0e-6);
        }
    }

    #[test]
    fn compression_preserves_oklch_hue_and_lightness_when_feasible() {
        let source = [1.4, 0.2, 0.1];
        let mapped = compress_linear_srgb_chroma(source);
        let source_lab = linear_srgb_to_oklab(source);
        let mapped_lab = linear_srgb_to_oklab(mapped.linear_srgb);
        let source_hue = source_lab[2].atan2(source_lab[1]);
        let mapped_hue = mapped_lab[2].atan2(mapped_lab[1]);
        assert!((source_hue - mapped_hue).abs() < 2.0e-5);
        assert!((source_lab[0] - mapped_lab[0]).abs() < 2.0e-5);
        assert!(mapped_lab[1].hypot(mapped_lab[2]) < source_lab[1].hypot(source_lab[2]));
    }

    #[test]
    fn mapping_is_continuous_across_an_srgb_boundary() {
        let inside = compress_linear_srgb_chroma([1.0, 0.2, 0.1]).linear_srgb;
        let outside = compress_linear_srgb_chroma([1.000_1, 0.2, 0.1]).linear_srgb;
        for (inside, outside) in inside.into_iter().zip(outside) {
            assert!((inside - outside).abs() < 0.002);
        }
    }

    #[test]
    fn gray_axis_and_lightness_limits_remain_neutral() {
        for (rgb, expected, status) in [
            ([-0.25; 3], [0.0; 3], GamutMapStatus::Limited),
            ([1.25; 3], [1.0; 3], GamutMapStatus::Limited),
        ] {
            let mapped = compress_linear_srgb_chroma(rgb);
            assert_rgb_close(mapped.linear_srgb, expected, 2.0e-6);
            assert_eq!(mapped.status, status);
        }
    }

    #[test]
    fn invalid_values_never_escape_as_non_finite() {
        for rgb in [
            [f32::NAN, 0.5, 1.0],
            [f32::INFINITY, 0.5, f32::NEG_INFINITY],
        ] {
            let mapped = compress_linear_srgb_chroma(rgb);
            assert_eq!(mapped.status, GamutMapStatus::Invalid);
            assert!(mapped.linear_srgb.into_iter().all(f32::is_finite));
        }
    }

    #[test]
    fn diagnostics_merge_without_losing_statuses() {
        let mut first = GamutMappingDiagnostics::default();
        first.record(GamutMapStatus::InGamut);
        first.record(GamutMapStatus::Compressed);
        let mut second = GamutMappingDiagnostics::default();
        second.record(GamutMapStatus::Limited);
        second.record(GamutMapStatus::ClippedFallback);
        second.record(GamutMapStatus::Invalid);
        let merged = first.merge(second);
        assert_eq!(merged.total_pixels(), 5);
        assert_eq!(merged.compressed_pixels, 1);
        assert_eq!(merged.invalid_pixels, 1);
    }
}
