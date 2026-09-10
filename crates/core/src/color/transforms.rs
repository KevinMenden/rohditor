//! Core-facing access to the shared matrix and chromatic-adaptation contract.

use crate::PipelineError;

/// Core compatibility facade for the shared color-math matrix.
///
/// The arithmetic lives in `rohditor-color`; this wrapper preserves the
/// established core error type for callers of `rohditor_core::Matrix3`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix3 {
    inner: rohditor_color::Matrix3,
}

impl Matrix3 {
    #[must_use]
    pub const fn new(values: [[f32; 3]; 3]) -> Self {
        Self {
            inner: rohditor_color::Matrix3::new(values),
        }
    }

    #[must_use]
    pub const fn identity() -> Self {
        Self::new([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]])
    }

    #[must_use]
    pub const fn values(self) -> [[f32; 3]; 3] {
        self.inner.values()
    }

    #[must_use]
    pub fn transform(self, vector: [f32; 3]) -> [f32; 3] {
        self.inner.transform(vector)
    }

    #[must_use]
    pub fn then(self, next: Self) -> Self {
        Self::from_shared(self.inner.then(next.inner))
    }

    pub fn inverse(self) -> Result<Self, PipelineError> {
        self.inner
            .inverse()
            .map(Self::from_shared)
            .map_err(Into::into)
    }

    pub(super) const fn from_shared(inner: rohditor_color::Matrix3) -> Self {
        Self { inner }
    }
}

/// D65 XYZ to linear Rec.2020.
pub const XYZ_D65_TO_LINEAR_REC2020: Matrix3 =
    Matrix3::new(rohditor_color::XYZ_D65_TO_LINEAR_REC2020.values());

/// Linear Rec.2020 to D65 XYZ.
pub const LINEAR_REC2020_TO_XYZ_D65: Matrix3 =
    Matrix3::new(rohditor_color::LINEAR_REC2020_TO_XYZ_D65.values());

/// D65 XYZ to linear sRGB.
pub const XYZ_D65_TO_LINEAR_SRGB: Matrix3 =
    Matrix3::new(rohditor_color::XYZ_D65_TO_LINEAR_SRGB.values());

pub use super::metadata::adapt_xyz_to_d65;
