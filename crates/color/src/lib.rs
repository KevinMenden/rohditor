//! Pure color transforms shared by the CPU reference and GPU contract.

mod gamut;
mod matrix;
mod transfer;

pub use gamut::{
    CHROMA_COMPRESS_ALGORITHM_VERSION, CHROMA_COMPRESS_EPSILON, CHROMA_COMPRESS_SEARCH_ITERATIONS,
    GamutMapResult, GamutMapStatus, GamutMappingDiagnostics, clip_linear_srgb,
    compress_linear_srgb_chroma,
};
pub use matrix::{
    A_WHITE, ColorMathError, D50_WHITE, D65_WHITE, LINEAR_REC2020_TO_XYZ_D65, Matrix3,
    XYZ_D65_TO_LINEAR_REC2020, XYZ_D65_TO_LINEAR_SRGB, adapt_xyz_to_d65,
    chromatic_adaptation_to_d65,
};
pub use transfer::{linear_srgb_to_srgb, srgb_to_linear_srgb};
