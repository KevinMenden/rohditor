//! Lens-profile matching and camera-native correction.
//!
//! Lensfun is deliberately an implementation detail of this crate. Callers
//! exchange owned queries, summaries, immutable correction plans, and
//! provenance rather than database handles or Lensfun model types.

mod catalog;
mod correction;
mod matching;
mod plan;
mod resample;

pub use catalog::OpticsService;
pub use correction::{Cancellation, CorrectionResult};

/// Version of Rohditor's optics plan and sampling contract.
pub const OPTICS_ALGORITHM_VERSION: u32 = 1;

/// Components a profile can request or actually provide for one shot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CorrectionComponents {
    pub distortion: bool,
    pub vignetting: bool,
    pub chromatic_aberration: bool,
}

impl CorrectionComponents {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            distortion: false,
            vignetting: false,
            chromatic_aberration: false,
        }
    }

    #[must_use]
    pub const fn any(self) -> bool {
        self.distortion || self.vignetting || self.chromatic_aberration
    }
}

/// Metadata required to resolve or explain a profile match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataField {
    CameraMake,
    CameraModel,
    LensMake,
    LensModel,
    FocalLength,
    Aperture,
    FocusDistance,
}

impl std::fmt::Display for MetadataField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CameraMake => "camera make",
            Self::CameraModel => "camera model",
            Self::LensMake => "lens make",
            Self::LensModel => "lens model",
            Self::FocalLength => "focal length",
            Self::Aperture => "aperture",
            Self::FocusDistance => "focus distance",
        })
    }
}

/// Decoder-owned camera and lens facts passed to the optics domain.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OpticsQuery {
    pub camera_make: String,
    pub camera_model: String,
    pub camera_clean_make: String,
    pub camera_clean_model: String,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    pub focal_length_mm: Option<f32>,
    pub aperture_f_number: Option<f32>,
    pub focus_distance_m: Option<f32>,
}

/// Owned presentation identity for one camera/lens calibration profile.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LensProfileSummary {
    pub id: String,
    pub camera: String,
    pub lens: String,
    pub mount: String,
    pub available: CorrectionComponents,
}

/// Conservative automatic matching result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ProfileMatch {
    Unique(LensProfileSummary),
    Ambiguous(Vec<LensProfileSummary>),
    MissingMetadata { fields: Vec<MetadataField> },
    CameraNotFound,
    LensNotFound,
}

/// Selection mode used when resolving a correction plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileRequest {
    Automatic,
    Explicit { profile_id: String },
}

/// Stable information about the exact database snapshot used for a plan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatabaseProvenance {
    pub source: String,
    pub snapshot: String,
    pub fingerprint: u64,
}

/// Pixel-producing identity attached to corrected image buffers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OpticsProvenance {
    pub profile: LensProfileSummary,
    pub database: DatabaseProvenance,
    pub requested: CorrectionComponents,
    pub applied: CorrectionComponents,
    pub used_infinity_distance_fallback: bool,
    pub scale: f32,
    pub content_fingerprint: u64,
}

/// An immutable, shot-specific correction plan.
#[derive(Debug, Clone)]
pub struct LensCorrectionPlan {
    pub(crate) profile: LensProfileSummary,
    pub(crate) database: DatabaseProvenance,
    pub(crate) requested: CorrectionComponents,
    pub(crate) applied: CorrectionComponents,
    pub(crate) used_infinity_distance_fallback: bool,
    pub(crate) content_fingerprint: u64,
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) norm_scale: f64,
    pub(crate) norm_unscale: f64,
    pub(crate) center_x: f64,
    pub(crate) center_y: f64,
    pub(crate) scale: f32,
    pub(crate) distortion: Option<plan::DistortionModel>,
    pub(crate) tca: Option<plan::TcaModel>,
    pub(crate) vignetting: Option<plan::VignettingModel>,
}

impl LensCorrectionPlan {
    #[must_use]
    pub fn provenance(&self) -> OpticsProvenance {
        OpticsProvenance {
            profile: self.profile.clone(),
            database: self.database.clone(),
            requested: self.requested,
            applied: self.applied,
            used_infinity_distance_fallback: self.used_infinity_distance_fallback,
            scale: self.scale,
            content_fingerprint: self.content_fingerprint,
        }
    }

    #[must_use]
    pub const fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    #[must_use]
    pub const fn scale(&self) -> f32 {
        self.scale
    }
}

/// Errors at the optics boundary. Core maps these into pipeline errors.
#[derive(Debug, thiserror::Error)]
pub enum OpticsError {
    #[error("could not load the Lensfun database: {reason}")]
    Database { reason: String },
    #[error("missing required optics metadata: {field}")]
    MissingMetadata { field: MetadataField },
    #[error("no Lensfun profile matches the camera metadata")]
    CameraNotFound,
    #[error("no Lensfun profile matches the lens metadata")]
    LensNotFound,
    #[error("automatic optics matching is ambiguous ({count} candidates)")]
    Ambiguous { count: usize },
    #[error("Lensfun profile ID was not found: {profile_id}")]
    ProfileNotFound { profile_id: String },
    #[error("Lensfun profile is incompatible with the camera: {reason}")]
    IncompatibleProfile { reason: String },
    #[error("invalid optics dimensions {width}x{height}: {reason}")]
    InvalidDimensions {
        width: usize,
        height: usize,
        reason: String,
    },
    #[error("invalid optics parameter {field}: {reason}")]
    InvalidParameter { field: &'static str, reason: String },
    #[error("optics correction could not keep its cubic footprint in bounds")]
    InvalidFootprint,
    #[error("optics correction allocation failed for {elements} image elements")]
    Allocation { elements: usize },
    #[error("optics correction received an image outside camera-native linear RGB")]
    WrongImageState,
    #[error("optics correction was cancelled")]
    Cancelled,
    #[error("optics correction failed: {reason}")]
    Correction { reason: String },
}
