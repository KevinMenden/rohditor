//! Rohditor's presentation-only desktop design system.
//!
//! These modules may depend on egui, but deliberately do not know about RAW
//! decoding, edit recipes, worker jobs, or GPU processors. `app.rs` translates
//! between these view models and application commands.

/// Top-level workspace modes, following the familiar library/develop split.
/// The library browses a folder; the develop mode edits one photo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ViewMode {
    #[default]
    Develop,
    Library,
}

pub(crate) mod adjustment_panel;
pub(crate) mod catalog;
pub(crate) mod crop;
mod detail;
pub(crate) mod diagnostics;
pub(crate) mod icons;
pub(crate) mod optics;
pub(crate) mod settings;
pub(crate) mod theme;
pub(crate) mod tool_panel;
pub(crate) mod tool_rail;
pub(crate) mod toolbar;
pub(crate) mod viewport;
pub(crate) mod widgets;
