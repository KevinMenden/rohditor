//! Per-pixel output transform from linear Rec.2020 into a display gamut.

use rohditor_color::{
    GamutMapResult, GamutMapStatus, LINEAR_REC2020_TO_XYZ_D65, Matrix3, OutputGamut,
    XYZ_D65_TO_LINEAR_DISPLAY_P3, XYZ_D65_TO_LINEAR_SRGB, clip_linear_rgb, compress_chroma,
    linear_srgb_to_srgb,
};
use rohditor_image::RgbPrimaries;

use crate::pipeline::OutputPolicy;

/// Rec.2020 → display-primaries matrix plus the matching gamut mapper.
///
/// Both SDR encoding (`encode`, ceiling 1.0, sRGB transfer) and the
/// extended-range rendition (`map` with a headroom ceiling) share this, so the
/// gamut handling cannot drift between the SDR base and the HDR image.
/// `OutputPolicy::ClipToSrgb` / `ChromaCompressToSrgb` select clipping or
/// chroma compression into whichever gamut is targeted; the names predate P3.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DisplayOutputTransform {
    matrix: Matrix3,
    gamut: OutputGamut,
}

impl DisplayOutputTransform {
    pub(crate) fn new(primaries: RgbPrimaries) -> Self {
        let (to_display, gamut) = match primaries {
            RgbPrimaries::Srgb => (XYZ_D65_TO_LINEAR_SRGB, OutputGamut::Srgb),
            RgbPrimaries::DisplayP3 => (XYZ_D65_TO_LINEAR_DISPLAY_P3, OutputGamut::DisplayP3),
        };
        Self {
            matrix: LINEAR_REC2020_TO_XYZ_D65.then(to_display),
            gamut,
        }
    }

    /// Linear display RGB confined to `0..=ceiling`.
    pub(crate) fn map(
        &self,
        source: [f32; 3],
        output_policy: OutputPolicy,
        ceiling: f32,
    ) -> GamutMapResult {
        let linear = self.matrix.transform(source);
        match output_policy {
            OutputPolicy::ClipToSrgb => clip_linear_rgb(linear, ceiling),
            OutputPolicy::ChromaCompressToSrgb => compress_chroma(linear, self.gamut, ceiling),
        }
    }

    /// SDR pixel with the sRGB transfer applied, ready to quantize.
    pub(crate) fn encode(
        &self,
        source: [f32; 3],
        output_policy: OutputPolicy,
    ) -> ([f32; 3], GamutMapStatus) {
        let mapped = self.map(source, output_policy, 1.0);
        (mapped.linear_rgb.map(linear_srgb_to_srgb), mapped.status)
    }
}

#[cfg(test)]
mod tests {
    use rohditor_color::{LINEAR_DISPLAY_P3_TO_XYZ_D65, XYZ_D65_TO_LINEAR_REC2020};

    use super::*;

    #[test]
    fn p3_encoding_round_trips_a_p3_colour_that_srgb_must_compress() {
        // Saturated green inside P3 but outside sRGB, expressed in Rec.2020.
        let p3_linear = [0.05, 0.8, 0.12];
        let source = LINEAR_DISPLAY_P3_TO_XYZ_D65
            .then(XYZ_D65_TO_LINEAR_REC2020)
            .transform(p3_linear);

        let (p3, p3_status) = DisplayOutputTransform::new(RgbPrimaries::DisplayP3)
            .encode(source, OutputPolicy::ChromaCompressToSrgb);
        assert_eq!(p3_status, GamutMapStatus::InGamut);
        for (actual, linear) in p3.into_iter().zip(p3_linear) {
            assert!((actual - linear_srgb_to_srgb(linear)).abs() < 1.0e-4);
        }

        let (_, srgb_status) = DisplayOutputTransform::new(RgbPrimaries::Srgb)
            .encode(source, OutputPolicy::ChromaCompressToSrgb);
        assert_eq!(srgb_status, GamutMapStatus::Compressed);
    }

    #[test]
    fn extended_ceiling_passes_highlights_that_the_sdr_ceiling_limits() {
        let transform = DisplayOutputTransform::new(RgbPrimaries::DisplayP3);
        // Neutral two stops over diffuse white.
        let source = [3.0, 3.0, 3.0];
        let sdr = transform.map(source, OutputPolicy::ChromaCompressToSrgb, 1.0);
        let hdr = transform.map(source, OutputPolicy::ChromaCompressToSrgb, 4.0);
        assert_eq!(sdr.status, GamutMapStatus::Limited);
        assert_eq!(hdr.status, GamutMapStatus::InGamut);
        assert!(
            (hdr.linear_rgb[1] - 3.0).abs() < 1.0e-3,
            "{:?}",
            hdr.linear_rgb
        );
    }
}
