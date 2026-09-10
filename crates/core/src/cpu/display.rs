//! Orientation-aware display conversion and deterministic quantization.

pub use super::stages::display::{
    render_display_srgb8, render_display_srgb8_dithered,
    render_display_srgb8_dithered_with_geometry, render_display_srgb8_with_geometry,
    render_display_srgb16, render_display_srgb16_with_geometry,
};
pub(crate) use super::stages::display::{
    render_display_srgb8_cancellable_with_geometry,
    render_display_srgb8_dithered_with_geometry_and_diagnostics,
    render_display_srgb16_with_geometry_and_diagnostics,
};
