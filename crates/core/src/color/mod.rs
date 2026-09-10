//! Core's metadata-aware color facade.
//!
//! Pure matrix, transfer, and gamut operations live in `rohditor-color`.
//! This module adapts RAW metadata and recipe selections to that contract.

mod calibration;
mod metadata;
mod output;
mod transforms;

pub use calibration::{
    CameraCalibration, CameraColorTransform, CameraProfileKey, CameraProfileProvenance,
    ResolvedCameraColour, camera_color_transform, camera_profile_key, resolve_camera_colour,
};
pub use output::{clip_linear_srgb_for_output, convert_rec2020_to_display_srgb};
pub use rohditor_color::{linear_srgb_to_srgb, srgb_to_linear_srgb};
pub use transforms::{
    LINEAR_REC2020_TO_XYZ_D65, Matrix3, XYZ_D65_TO_LINEAR_REC2020, XYZ_D65_TO_LINEAR_SRGB,
    adapt_xyz_to_d65,
};

pub(crate) use metadata::encode_rec2020_for_srgb_output;
