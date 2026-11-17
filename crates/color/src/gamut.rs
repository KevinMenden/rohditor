//! Deterministic target-gamut mapping for linear sRGB and Display P3 output.
//!
//! Every mapper takes a `ceiling`: the largest in-gamut channel value. SDR
//! output uses 1.0; an extended-range rendition passes its headroom so values
//! above diffuse white are kept while negative (out-of-gamut) channels are
//! still removed.

use crate::oklab::{
    linear_display_p3_to_oklab, linear_srgb_to_oklab, oklab_to_linear_display_p3,
    oklab_to_linear_srgb,
};

/// Numerical contract for the first chroma-compressing output mapper.
pub const CHROMA_COMPRESS_ALGORITHM_VERSION: u16 = 1;
/// Fixed search depth shared with the GPU implementation.
pub const CHROMA_COMPRESS_SEARCH_ITERATIONS: u32 = 16;
/// Tolerance used for neutral and final gamut-boundary handling.
pub const CHROMA_COMPRESS_EPSILON: f32 = 1.0e-6;

/// Primaries of the gamut that chroma compression maps into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputGamut {
    Srgb,
    DisplayP3,
}

impl OutputGamut {
    fn rgb_to_oklab(self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            Self::Srgb => linear_srgb_to_oklab(rgb),
            Self::DisplayP3 => linear_display_p3_to_oklab(rgb),
        }
    }

    fn oklab_to_rgb(self, lab: [f32; 3]) -> [f32; 3] {
        match self {
            Self::Srgb => oklab_to_linear_srgb(lab),
            Self::DisplayP3 => oklab_to_linear_display_p3(lab),
        }
    }
}

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
    /// Linear RGB in the target gamut's primaries.
    pub linear_rgb: [f32; 3],
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
    clip_linear_rgb(rgb, 1.0)
}

/// Hard-clip linear RGB of any primaries to `0..=ceiling`.
///
/// Invalid values get deterministic finite output.
#[must_use]
pub fn clip_linear_rgb(rgb: [f32; 3], ceiling: f32) -> GamutMapResult {
    let finite = rgb.iter().all(|value| value.is_finite());
    let in_gamut = finite && is_in_gamut(rgb, ceiling);
    let linear_rgb = rgb.map(|value| sanitize_and_clip(value, ceiling));
    let status = if !finite {
        GamutMapStatus::Invalid
    } else if in_gamut {
        GamutMapStatus::InGamut
    } else {
        GamutMapStatus::ClippedFallback
    };
    GamutMapResult { linear_rgb, status }
}

/// Preserve in-gamut values exactly and reduce OKLCH chroma for out-of-gamut pixels.
#[must_use]
pub fn compress_linear_srgb_chroma(rgb: [f32; 3]) -> GamutMapResult {
    compress_chroma(rgb, OutputGamut::Srgb, 1.0)
}

/// Chroma-compress linear RGB in the primaries of `gamut` into `0..=ceiling`.
///
/// In-gamut values are returned bit-exactly. Out-of-gamut values keep their
/// OKLCH hue and lightness where feasible and lose only chroma.
#[must_use]
pub fn compress_chroma(rgb: [f32; 3], gamut: OutputGamut, ceiling: f32) -> GamutMapResult {
    if !rgb.iter().all(|value| value.is_finite()) {
        return invalid_fallback(rgb, ceiling);
    }
    if is_in_gamut(rgb, ceiling) {
        return GamutMapResult {
            linear_rgb: rgb,
            status: GamutMapStatus::InGamut,
        };
    }

    let [lightness, a, b] = gamut.rgb_to_oklab(rgb);
    if ![lightness, a, b].iter().all(|value| value.is_finite()) {
        return clipped_fallback(rgb, ceiling);
    }
    // A neutral at `ceiling` has OkLab lightness cbrt(ceiling), which is 1.0
    // for the SDR ceiling.
    let limited_lightness = lightness.clamp(0.0, ceiling.cbrt());
    let lightness_limited = limited_lightness != lightness;
    let chroma = a.hypot(b);
    if chroma <= CHROMA_COMPRESS_EPSILON {
        return GamutMapResult {
            linear_rgb: gamut
                .oklab_to_rgb([limited_lightness, 0.0, 0.0])
                .map(|value| sanitize_and_clip(value, ceiling)),
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
        let candidate = gamut.oklab_to_rgb([limited_lightness, a * scale, b * scale]);
        if candidate.iter().all(|value| value.is_finite()) && is_in_gamut(candidate, ceiling) {
            lower = scale;
        } else {
            upper = scale;
        }
    }
    let mapped = gamut.oklab_to_rgb([limited_lightness, a * lower, b * lower]);
    if !mapped.iter().all(|value| value.is_finite()) {
        return clipped_fallback(rgb, ceiling);
    }
    GamutMapResult {
        linear_rgb: mapped.map(|value| sanitize_and_clip(value, ceiling)),
        status: if lightness_limited {
            GamutMapStatus::Limited
        } else {
            GamutMapStatus::Compressed
        },
    }
}

fn invalid_fallback(rgb: [f32; 3], ceiling: f32) -> GamutMapResult {
    GamutMapResult {
        linear_rgb: rgb.map(|value| sanitize_and_clip(value, ceiling)),
        status: GamutMapStatus::Invalid,
    }
}

fn clipped_fallback(rgb: [f32; 3], ceiling: f32) -> GamutMapResult {
    GamutMapResult {
        linear_rgb: rgb.map(|value| sanitize_and_clip(value, ceiling)),
        status: GamutMapStatus::ClippedFallback,
    }
}

fn sanitize_and_clip(value: f32, ceiling: f32) -> f32 {
    if value.is_nan() || value == f32::NEG_INFINITY {
        0.0
    } else if value == f32::INFINITY {
        ceiling
    } else {
        value.clamp(0.0, ceiling)
    }
}

fn is_in_gamut(rgb: [f32; 3], ceiling: f32) -> bool {
    rgb.into_iter()
        .all(|value| (0.0..=ceiling).contains(&value))
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
            assert_eq!(mapped.linear_rgb.map(f32::to_bits), rgb.map(f32::to_bits));
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
                    .linear_rgb
                    .into_iter()
                    .all(|value| (0.0..=1.0).contains(&value))
            );
            assert!(matches!(
                mapped.status,
                GamutMapStatus::Compressed | GamutMapStatus::Limited
            ));
            assert_ne!(mapped.linear_rgb, rgb.map(|value| value.clamp(0.0, 1.0)));
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
        let mapped_lab = linear_srgb_to_oklab(mapped.linear_rgb);
        let source_hue = source_lab[2].atan2(source_lab[1]);
        let mapped_hue = mapped_lab[2].atan2(mapped_lab[1]);
        assert!((source_hue - mapped_hue).abs() < 2.0e-5);
        assert!((source_lab[0] - mapped_lab[0]).abs() < 2.0e-5);
        assert!(mapped_lab[1].hypot(mapped_lab[2]) < source_lab[1].hypot(source_lab[2]));
    }

    #[test]
    fn mapping_is_continuous_across_an_srgb_boundary() {
        let inside = compress_linear_srgb_chroma([1.0, 0.2, 0.1]).linear_rgb;
        let outside = compress_linear_srgb_chroma([1.000_1, 0.2, 0.1]).linear_rgb;
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
            assert_rgb_close(mapped.linear_rgb, expected, 2.0e-6);
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
            assert!(mapped.linear_rgb.into_iter().all(f32::is_finite));
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

    #[test]
    fn p3_values_inside_p3_but_outside_srgb_are_preserved() {
        // Saturated P3 green is outside sRGB but a valid P3 colour.
        let p3_green = [0.0, 0.9, 0.1];
        let mapped = compress_chroma(p3_green, OutputGamut::DisplayP3, 1.0);
        assert_eq!(mapped.status, GamutMapStatus::InGamut);
        assert_eq!(mapped.linear_rgb, p3_green);
    }

    #[test]
    fn p3_compression_keeps_hue_and_lands_in_the_p3_gamut() {
        let source = [-0.12, 1.15, 0.2];
        let mapped = compress_chroma(source, OutputGamut::DisplayP3, 1.0);
        assert!(
            mapped
                .linear_rgb
                .into_iter()
                .all(|v| (0.0..=1.0).contains(&v))
        );
        assert_eq!(mapped.status, GamutMapStatus::Compressed);
        let source_lab = linear_display_p3_to_oklab(source);
        let mapped_lab = linear_display_p3_to_oklab(mapped.linear_rgb);
        let hue = |lab: [f32; 3]| lab[2].atan2(lab[1]);
        assert!((hue(source_lab) - hue(mapped_lab)).abs() < 2.0e-4);
        assert!((source_lab[0] - mapped_lab[0]).abs() < 2.0e-4);
    }

    #[test]
    fn ceiling_keeps_highlights_above_diffuse_white() {
        // Bright but in gamut for a headroom of 4: untouched.
        let bright = [3.2, 2.5, 1.1];
        let mapped = compress_chroma(bright, OutputGamut::DisplayP3, 4.0);
        assert_eq!(mapped.status, GamutMapStatus::InGamut);
        assert_eq!(mapped.linear_rgb, bright);

        // A negative channel is still compressed, without pulling the
        // highlight back down to 1.0.
        let mapped = compress_chroma([3.0, -0.2, 1.0], OutputGamut::DisplayP3, 4.0);
        assert!(
            mapped
                .linear_rgb
                .into_iter()
                .all(|v| (0.0..=4.0).contains(&v))
        );
        assert!(mapped.linear_rgb[0] > 1.5, "{:?}", mapped.linear_rgb);
    }

    #[test]
    fn values_above_the_ceiling_are_limited_and_stay_neutral() {
        let mapped = compress_chroma([9.0; 3], OutputGamut::DisplayP3, 4.0);
        assert_eq!(mapped.status, GamutMapStatus::Limited);
        assert_rgb_close(mapped.linear_rgb, [4.0; 3], 1.0e-4);
    }

    #[test]
    fn clipping_respects_the_ceiling_for_invalid_values() {
        let clipped = clip_linear_rgb([f32::INFINITY, -1.0, 2.5], 4.0);
        assert_eq!(clipped.linear_rgb, [4.0, 0.0, 2.5]);
        assert_eq!(clipped.status, GamutMapStatus::Invalid);
    }
}
