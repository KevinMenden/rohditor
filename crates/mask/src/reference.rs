//! The per-pixel reference that range components select on.
//!
//! A gradient or a brush only needs a pixel's position. A luminance or colour range needs what
//! the pixel *looks like*, which only the pipeline knows. The pipeline measures it once per row
//! (see `rohditor_core`) and hands it to [`PreparedMask::evaluate_row`](crate::PreparedMask) as
//! a [`ReferenceRow`]; this crate never sees an image. Masks without range components ignore it.

/// Which reference values a mask reads, so the pipeline only measures those.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReferenceNeeds {
    /// sRGB-encoded luminance (cheap).
    pub luminance: bool,
    /// OkLab colour (three cube roots per pixel).
    pub oklab: bool,
}

impl ReferenceNeeds {
    /// Whether any reference value is read at all.
    #[must_use]
    pub const fn any(self) -> bool {
        self.luminance || self.oklab
    }

    /// Both sets of needs.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            luminance: self.luminance || other.luminance,
            oklab: self.oklab || other.oklab,
        }
    }
}

/// Reference values for the pixels of one row span, aligned with the `x_range` and `out` slices
/// given to `evaluate_row`. A plane the mask does not read may be left empty.
#[derive(Debug, Clone, Copy)]
pub struct ReferenceRow<'a> {
    /// sRGB-encoded luminance, `0..=1`.
    pub luminance: &'a [f32],
    /// OkLab `[L, a, b]`.
    pub oklab: &'a [[f32; 3]],
}

impl ReferenceRow<'_> {
    /// For masks that need no reference.
    pub const NONE: ReferenceRow<'static> = ReferenceRow {
        luminance: &[],
        oklab: &[],
    };
}
