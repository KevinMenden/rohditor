//! Pure color transforms shared by the CPU reference and GPU contract.

mod gamut;
mod local_white_balance;
mod matrix;
mod oklab;
mod profile_table;
mod range_reference;
mod transfer;

pub use gamut::{
    CHROMA_COMPRESS_ALGORITHM_VERSION, CHROMA_COMPRESS_EPSILON, CHROMA_COMPRESS_SEARCH_ITERATIONS,
    GamutMapResult, GamutMapStatus, GamutMappingDiagnostics, clip_linear_srgb,
    compress_linear_srgb_chroma,
};
pub use local_white_balance::{REC2020_LUMINANCE, local_white_balance_gains};
pub use matrix::{
    A_WHITE, ColorMathError, D50_WHITE, D65_WHITE, LINEAR_REC2020_TO_XYZ_D65, Matrix3,
    XYZ_D65_TO_LINEAR_REC2020, XYZ_D65_TO_LINEAR_SRGB, adapt_xyz_to_d65,
    chromatic_adaptation_to_d65,
};
pub use oklab::{linear_srgb_to_oklab, oklab_to_linear_srgb};
pub use profile_table::{DeltaTable, apply_delta_table, rec2020_prophoto_matrices};
pub use range_reference::{rec2020_encoded_luminance, rec2020_to_oklab};
pub use transfer::{linear_srgb_to_srgb, srgb_to_linear_srgb};
