//! Recipe sections: the unit in which edits are reset, copied and blended.
//!
//! A section is a group of recipe fields that is edited together (the Tone
//! sliders, the HSL mixer, the lens profile, ...). Resetting a section,
//! pasting settings onto another photo and applying a preset at some strength
//! all work on sections, so the mapping from section to fields lives here, in
//! one toolkit-free place, rather than in each frontend.
//!
//! Every recipe field except `schema_version` belongs to exactly one section;
//! a test fails when a field is added without being assigned.

mod blend;
#[cfg(test)]
mod tests;

pub use blend::{blend_sections, copy_sections};

/// One group of recipe fields. See the module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipeSection {
    /// Exposure, contrast, highlights, shadows, whites and blacks.
    Tone,
    /// The four-region tone curve.
    ToneCurve,
    /// Texture, clarity and dehaze.
    Presence,
    WhiteBalance,
    /// Saturation and vibrance.
    Saturation,
    /// The HSL mixer.
    ColorMixer,
    ColorGrading,
    /// The camera colour profile and the base rendering choice. The fitted
    /// look of that rendering is [`Self::CameraLook`].
    ColorProfile,
    /// The per-photo fitted tone and colour of the Matched rendering. Derived
    /// from the photo's embedded JPEG, so it is only meaningful for that photo.
    CameraLook,
    HighlightReconstruction,
    NoiseReduction,
    CaptureSharpening,
    FalseColor,
    Defringe,
    /// Lens profile and the corrections it drives.
    Optics,
    /// Crop and orientation override.
    Geometry,
    /// All mask layers with their adjustments.
    LocalMasks,
}

impl RecipeSection {
    /// Every section, in the order the Edit panel lists them.
    pub const ALL: [Self; 17] = [
        Self::Tone,
        Self::ToneCurve,
        Self::Presence,
        Self::WhiteBalance,
        Self::Saturation,
        Self::ColorMixer,
        Self::ColorGrading,
        Self::ColorProfile,
        Self::CameraLook,
        Self::HighlightReconstruction,
        Self::NoiseReduction,
        Self::CaptureSharpening,
        Self::FalseColor,
        Self::Defringe,
        Self::Optics,
        Self::Geometry,
        Self::LocalMasks,
    ];

    /// Whether the section describes one photo (its light, camera, lens or
    /// framing) rather than a taste. Copying these between photos is usually
    /// wrong, so copy/paste UIs should leave them off by default.
    #[must_use]
    pub const fn is_photo_specific(self) -> bool {
        matches!(
            self,
            Self::WhiteBalance
                | Self::ColorProfile
                | Self::CameraLook
                | Self::Optics
                | Self::Geometry
                | Self::LocalMasks
        )
    }
}
