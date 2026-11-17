//! Which of a recipe's two renditions the adjustment stage develops.
//!
//! The HDR rendition is a second render of the same recipe, not an inverse of
//! the SDR output. Only the stages whose output is bounded by 1.0 differ:
//!
//! - the base rendering uses [`crate::BaseRenderingLut::with_headroom`];
//! - Light tone (global and local) continues above 1.0 through
//!   `LightToneLut::sample_extended` instead of jumping back to the identity.
//!
//! Everything else is shared. Range masks in particular keep measuring the SDR
//! reference, so a mask selects the same pixels in both renditions. The tone
//! curve, saturation, HSL and colour grading already leave values above 1.0
//! unclipped, and camera colourfulness holds its gain at 1.0 for brighter
//! pixels.
//!
//! Presence (texture, clarity, dehaze) runs on the rendition's own values. Its
//! blurs see the brighter HDR highlights and dehaze estimates its airlight from
//! them, so with presence active the renditions can also differ slightly below
//! the knee: next to highlights for texture and clarity, anywhere for dehaze.

/// Output range of one adjustment-stage run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Rendition {
    /// The base rendering compresses everything into `[0, 1]`.
    Sdr,
    /// Highlights above diffuse white (1.0) are kept up to `headroom`, a
    /// validated linear ceiling of at least 1.0.
    Extended { headroom: f32 },
}
