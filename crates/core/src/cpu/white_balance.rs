//! Camera-native white-balance and calibration application stages.

pub(crate) use super::stages::white_balance::{
    apply_camera_color_transform_cancellable, apply_white_balance_cancellable,
};
pub use super::stages::white_balance::{white_balance_gains, white_balance_gains_from_calibration};
