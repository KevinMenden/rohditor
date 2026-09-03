use rohditor_image::Orientation;
use serde::{Deserialize, Serialize};

use crate::EditError;

/// Geometry controls which affect the final developed-image coordinate map.
///
/// `crop` is deliberately late, after all pixel adjustments. Its coordinates
/// are normalized pixel edges in the already oriented developed canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GeometryAdjustments {
    #[serde(default)]
    pub orientation_override: Option<Orientation>,
    #[serde(default)]
    pub crop: Option<NormalizedCropRect>,
}

/// A non-destructive rectangle in the full oriented developed canvas.
///
/// Left/top edges are inclusive and right/bottom edges exclusive after the
/// core resolves the normalized edges to concrete pixel boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormalizedCropRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl NormalizedCropRect {
    /// The complete canvas. Call [`Self::canonicalized`] before storing it in
    /// a recipe so neutral geometry remains represented by `None`.
    pub const FULL_FRAME: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };

    #[must_use]
    pub fn is_full_frame(self) -> bool {
        self == Self::FULL_FRAME
    }

    /// Returns `None` for the neutral full-frame rectangle.
    #[must_use]
    pub fn canonicalized(self) -> Option<Self> {
        (!self.is_full_frame()).then_some(self)
    }

    pub(crate) fn validate(self) -> Result<(), EditError> {
        for (field, value) in [
            ("geometry.crop.left", self.left),
            ("geometry.crop.top", self.top),
            ("geometry.crop.right", self.right),
            ("geometry.crop.bottom", self.bottom),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(EditError {
                    field,
                    reason: "must be finite and within 0.0..=1.0".to_owned(),
                });
            }
        }
        if self.left >= self.right {
            return Err(EditError {
                field: "geometry.crop",
                reason: "left must be less than right".to_owned(),
            });
        }
        if self.top >= self.bottom {
            return Err(EditError {
                field: "geometry.crop",
                reason: "top must be less than bottom".to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::NormalizedCropRect;

    #[test]
    fn full_frame_canonicalizes_to_none() {
        assert_eq!(NormalizedCropRect::FULL_FRAME.canonicalized(), None);
    }

    #[test]
    fn validation_rejects_non_finite_inverted_and_out_of_range_rectangles() {
        for crop in [
            NormalizedCropRect {
                left: f64::NAN,
                ..NormalizedCropRect::FULL_FRAME
            },
            NormalizedCropRect {
                left: 0.8,
                right: 0.2,
                ..NormalizedCropRect::FULL_FRAME
            },
            NormalizedCropRect {
                bottom: 1.1,
                ..NormalizedCropRect::FULL_FRAME
            },
        ] {
            assert!(crop.validate().is_err());
        }
    }
}
