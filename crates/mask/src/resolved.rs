//! Components resolved to pixel space for a renderer that evaluates the maths itself.
//!
//! [`PreparedMask`](crate::PreparedMask) evaluates a layer on the CPU. The GPU colour pass runs
//! the same formulas in a shader, so it takes the same resolved numbers from here instead of
//! deriving them a second time. A brush is the exception: strokes stay on the CPU and reach the
//! shader as a coverage plane, rasterized with [`BrushCoverage`].

use rohditor_edit::{BrushMask, MaskComponent, MaskMode, MaskShape};

use crate::brush::PreparedBrush;
use crate::color_range::{LIGHTNESS_WEIGHT, PreparedColor};
use crate::linear::PreparedLinear;
use crate::luminance::PreparedLuminance;
use crate::radial::PreparedRadial;
use crate::{MaskError, MaskFrame};

/// One component's combine mode, inversion and resolved shape.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedComponent {
    pub mode: MaskMode,
    pub invert: bool,
    pub shape: ResolvedShape,
}

/// The parameters of each shape, with the meaning documented on its CPU evaluator.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedShape {
    Linear {
        start: [f32; 2],
        direction: [f32; 2],
    },
    Radial {
        center: [f32; 2],
        cos: f32,
        sin: f32,
        inv_radius_x: f32,
        inv_radius_y: f32,
        feather: f32,
    },
    /// Read from a coverage plane instead of being evaluated from parameters.
    Brush,
    Luminance {
        low: f32,
        high: f32,
        feather_low: f32,
        feather_high: f32,
    },
    Color {
        samples: Vec<[f32; 3]>,
        lightness_weight: f32,
        inner: f32,
        outer: f32,
    },
}

/// Resolves one component for `frame`. Brush strokes are not touched, so this stays cheap enough
/// to run for every render; [`BrushCoverage::prepare`] reports a degenerate brush.
///
/// # Errors
/// [`MaskError`] for values that would produce NaN alpha.
pub fn resolve_component(
    component: &MaskComponent,
    frame: MaskFrame,
) -> Result<ResolvedComponent, MaskError> {
    let shape = match &component.shape {
        MaskShape::Linear(gradient) => {
            let linear = PreparedLinear::prepare(gradient, frame)?;
            ResolvedShape::Linear {
                start: linear.start,
                direction: linear.direction,
            }
        }
        MaskShape::Radial(radial) => {
            let radial = PreparedRadial::prepare(radial, frame)?;
            ResolvedShape::Radial {
                center: radial.center,
                cos: radial.cos,
                sin: radial.sin,
                inv_radius_x: radial.inv_radius_x,
                inv_radius_y: radial.inv_radius_y,
                feather: radial.feather,
            }
        }
        MaskShape::Brush(_) => ResolvedShape::Brush,
        MaskShape::Luminance(range) => {
            let range = PreparedLuminance::prepare(range)?;
            ResolvedShape::Luminance {
                low: range.low,
                high: range.high,
                feather_low: range.feather_low,
                feather_high: range.feather_high,
            }
        }
        MaskShape::Color(range) => {
            let range = PreparedColor::prepare(range)?;
            ResolvedShape::Color {
                lightness_weight: LIGHTNESS_WEIGHT,
                inner: range.inner,
                outer: range.outer,
                samples: range.samples,
            }
        }
    };
    Ok(ResolvedComponent {
        mode: component.mode,
        invert: component.invert,
        shape,
    })
}

/// One brush component resolved to pixel space, rasterized a row span at a time.
#[derive(Debug, Clone)]
pub struct BrushCoverage(PreparedBrush);

impl BrushCoverage {
    /// Places the dabs of every stroke of `brush` for `frame`.
    ///
    /// # Errors
    /// [`MaskError`] for a degenerate stroke, or a brush too dense for this frame.
    pub fn prepare(brush: &BrushMask, frame: MaskFrame) -> Result<Self, MaskError> {
        PreparedBrush::prepare(brush, frame).map(Self)
    }

    /// Writes the brush's alpha (before the component's `invert`) for the `out.len()` pixels of
    /// row `y` starting at column `x_start`. `keep` is scratch a caller can reuse across rows.
    pub fn evaluate_row(&self, y: usize, x_start: usize, out: &mut [f32], keep: &mut Vec<f32>) {
        self.0.evaluate_row(y, x_start, out, keep);
    }

    /// Bytes held by the placed dabs.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.0.bytes()
    }
}
