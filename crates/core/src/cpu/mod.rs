//! Deterministic CPU processing stages.
//!
//! The implementation is kept private so callers depend on the stable stage
//! facade rather than on the layout of the processing code.

mod adjustments;
mod display;
mod normalize;
mod stages;
mod white_balance;

pub use adjustments::{
    HSL_CHANNEL_CENTERS, HSL_HUE_SHIFT_PER_FULL_VALUE, apply_adjustments, evaluate_tone_curve,
    hsl_channel_weights, hsl_channel_weights_from_display_rgb,
};
pub use display::{
    render_display_srgb8, render_display_srgb8_dithered,
    render_display_srgb8_dithered_with_geometry, render_display_srgb8_with_geometry,
    render_display_srgb16, render_display_srgb16_with_geometry,
};
pub use normalize::{normalize_raw, normalize_raw_preview};
pub use white_balance::{white_balance_gains, white_balance_gains_from_calibration};

pub(crate) use adjustments::apply_adjustments_cancellable;
pub(crate) use display::{
    render_display_srgb8_cancellable_with_geometry,
    render_display_srgb8_dithered_with_geometry_and_diagnostics,
    render_display_srgb16_with_geometry_and_diagnostics,
};
pub(crate) use normalize::{normalize_raw_cancellable, preview_dimensions};
pub(crate) use stages::raw_crop_dimensions;
pub(crate) use white_balance::{
    apply_camera_color_transform_cancellable, apply_white_balance_cancellable,
};
