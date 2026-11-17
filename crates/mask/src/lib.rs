//! Mask evaluation: turns one [`MaskLayer`](rohditor_edit::MaskLayer) into per-pixel alpha.
//!
//! The recipe stores masks in normalized, resolution-independent coordinates (see
//! `rohditor_edit::local`). This crate resolves a layer to pixel space once per render with
//! [`PreparedMask::prepare`] and then evaluates it one row at a time with
//! [`PreparedMask::evaluate_row`], so a pipeline never has to hold a full-resolution alpha
//! plane per layer (96 MB at 24 MP). It knows nothing about the pipeline: the CPU stage, a GPU
//! port and the UI overlay can all share the same maths.
//!
//! # Pixel mapping
//!
//! Pixel `(x, y)` has its centre at `((x + 0.5) / width, (y + 0.5) / height)` in recipe
//! coordinates. All maths runs in *pixel space* (recipe point times frame size), so angles stay
//! true on non-square frames. Lengths in the recipe are in long-edge units and are multiplied by
//! `max(width, height)`.
//!
//! # Range components
//!
//! Luminance and colour ranges select by image content. The pipeline measures a per-pixel
//! reference (see [`ReferenceRow`]) and passes it in; this crate stays free of images.

#![cfg_attr(test, allow(clippy::unwrap_used))]

mod brush;
mod color_range;
mod linear;
mod luminance;
mod math;
mod prepared;
mod radial;
mod reference;
mod resolved;
#[cfg(test)]
mod test_support;

pub use prepared::PreparedMask;
pub use reference::{ReferenceNeeds, ReferenceRow};
pub use resolved::{BrushCoverage, ResolvedComponent, ResolvedShape, resolve_component};

use thiserror::Error;

/// Pixel grid of the developed frame being processed (preview-reduced or full size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaskFrame {
    pub width: usize,
    pub height: usize,
}

impl MaskFrame {
    /// Longest side, the unit of the recipe's lengths.
    pub(crate) fn long_edge(self) -> f32 {
        self.width.max(self.height) as f32
    }
}

/// Why a layer could not be prepared.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MaskError {
    #[error("mask frame must be at least 1x1, got {width}x{height}")]
    EmptyFrame { width: usize, height: usize },
    /// A value that would divide by zero or produce NaN alpha. A layer that passed
    /// `LocalAdjustments::validate` never triggers this.
    #[error("degenerate {shape} mask: {reason}")]
    Degenerate {
        shape: &'static str,
        reason: &'static str,
    },
    /// The brush would need more dabs than [`brush::MAX_DAB_ENTRIES`] allows at this frame size.
    #[error("brush needs {entries} dab entries, limit is {limit}")]
    BrushTooDense { entries: usize, limit: usize },
}
