//! Rohditor's presentation-only desktop design system.
//!
//! These modules may depend on egui, but deliberately do not know about RAW
//! decoding, edit recipes, worker jobs, or GPU processors. `app.rs` translates
//! between these view models and application commands.

/// Viewport sampling modes are mutually exclusive and purpose-specific. This
/// prevents one tool's click from being interpreted as another tool's sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PickerMode {
    WhiteBalance,
    ColorMixer,
}

pub(crate) mod adjustment_panel;
pub(crate) mod diagnostics;
pub(crate) mod icons;
pub(crate) mod theme;
pub(crate) mod toolbar;
pub(crate) mod viewport;
pub(crate) mod widgets;
