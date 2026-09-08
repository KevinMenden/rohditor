//! The deliberately small, matrix-only camera-profile boundary.
//!
//! Rohditor does not try to implement the complete DCP specification here.
//! This crate accepts the subset that can be represented by a pair of camera
//! matrices and rejects pixel-producing features it cannot evaluate.

use std::fmt;
use std::path::Path;

use serde::de::{Error as _, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod dcp;

pub use dcp::{parse_dcp_bytes, parse_dcp_file};

/// Maximum size of an imported profile. This bounds both parsing work and the
/// amount of data the desktop registry can retain from an untrusted file.
pub const DCP_MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
/// Current serialized profile payload version.
pub const CAMERA_PROFILE_FORMAT_VERSION: u8 = 1;
/// Version of the matrix evaluator used in cache identities.
pub const CAMERA_PROFILE_EVALUATOR_VERSION: u8 = 1;
/// Maximum number of illuminant-specific matrix pairs supported by the MVP.
pub const MAX_MATRIX_CALIBRATIONS: usize = 2;

/// Illuminant references supported by the first matrix-only implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationIlluminant {
    D65,
    D50,
    StandardLightA,
}

impl CalibrationIlluminant {
    #[must_use]
    pub const fn priority(self) -> u8 {
        match self {
            Self::D65 => 0,
            Self::D50 => 1,
            Self::StandardLightA => 2,
        }
    }

    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::StandardLightA => 17,
            Self::D50 => 21,
            Self::D65 => 23,
        }
    }
}

impl fmt::Display for CalibrationIlluminant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::D65 => "D65",
            Self::D50 => "D50",
            Self::StandardLightA => "Standard Light A",
        })
    }
}

/// A validated matrix-only camera profile embedded in an edit recipe or
/// installed in the desktop registry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatrixCameraProfile {
    pub format_version: u8,
    /// Lowercase SHA-256 of the original DCP bytes.
    pub source_sha256: String,
    pub name: String,
    pub camera_model: String,
    #[serde(default)]
    pub copyright: Option<String>,
    pub calibrations: Vec<MatrixCalibration>,
}

impl<'de> Deserialize<'de> for MatrixCameraProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Fields {
            format_version: u8,
            source_sha256: BoundedText<64>,
            name: BoundedText<256>,
            camera_model: BoundedText<256>,
            #[serde(default)]
            copyright: Option<BoundedText<1024>>,
            #[serde(deserialize_with = "deserialize_calibrations")]
            calibrations: Vec<MatrixCalibration>,
        }

        let fields = Fields::deserialize(deserializer)?;
        let profile = Self {
            format_version: fields.format_version,
            source_sha256: fields.source_sha256.0,
            name: fields.name.0,
            camera_model: fields.camera_model.0,
            copyright: fields.copyright.map(|value| value.0),
            calibrations: fields.calibrations,
        };
        profile.validate().map_err(D::Error::custom)?;
        Ok(profile)
    }
}

struct BoundedText<const MAXIMUM_BYTES: usize>(String);

impl<'de, const MAXIMUM_BYTES: usize> Deserialize<'de> for BoundedText<MAXIMUM_BYTES> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TextVisitor<const MAXIMUM_BYTES: usize>;

        impl<'de, const MAXIMUM_BYTES: usize> Visitor<'de> for TextVisitor<MAXIMUM_BYTES> {
            type Value = BoundedText<MAXIMUM_BYTES>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "text no longer than {MAXIMUM_BYTES} bytes")
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(value)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value.len() > MAXIMUM_BYTES {
                    return Err(E::custom(format!(
                        "text exceeds the {MAXIMUM_BYTES}-byte limit"
                    )));
                }
                Ok(BoundedText(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value.len() > MAXIMUM_BYTES {
                    return Err(E::custom(format!(
                        "text exceeds the {MAXIMUM_BYTES}-byte limit"
                    )));
                }
                Ok(BoundedText(value))
            }
        }

        deserializer.deserialize_string(TextVisitor)
    }
}

fn deserialize_calibrations<'de, D>(deserializer: D) -> Result<Vec<MatrixCalibration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct CalibrationVisitor;

    impl<'de> Visitor<'de> for CalibrationVisitor {
        type Value = Vec<MatrixCalibration>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("one or two matrix calibrations")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut calibrations = Vec::with_capacity(MAX_MATRIX_CALIBRATIONS);
            while let Some(calibration) = sequence.next_element()? {
                if calibrations.len() >= MAX_MATRIX_CALIBRATIONS {
                    return Err(A::Error::custom(format!(
                        "more than {MAX_MATRIX_CALIBRATIONS} calibrations are not supported"
                    )));
                }
                calibrations.push(calibration);
            }
            Ok(calibrations)
        }
    }

    deserializer.deserialize_seq(CalibrationVisitor)
}

/// One illuminant-specific matrix pair from a matrix camera profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatrixCalibration {
    pub illuminant: CalibrationIlluminant,
    /// DCP ColorMatrix: XYZ to camera, in row-major order.
    pub xyz_to_camera: [[f32; 3]; 3],
    /// Optional DCP ForwardMatrix: camera to XYZ D50, in row-major order.
    #[serde(default)]
    pub forward_camera_to_xyz_d50: Option<[[f32; 3]; 3]>,
}

impl MatrixCameraProfile {
    /// Validate serialized profile data before it crosses a recipe or UI
    /// boundary.
    pub fn validate(&self) -> Result<(), CameraProfileError> {
        if self.format_version != CAMERA_PROFILE_FORMAT_VERSION {
            return Err(CameraProfileError::InvalidProfile {
                field: "format_version",
                reason: format!(
                    "version {} is not supported; expected {}",
                    self.format_version, CAMERA_PROFILE_FORMAT_VERSION
                ),
            });
        }
        validate_sha256(&self.source_sha256)?;
        validate_text("name", &self.name, 256)?;
        validate_text("camera_model", &self.camera_model, 256)?;
        if let Some(copyright) = &self.copyright {
            validate_text("copyright", copyright, 1024)?;
        }
        if self.calibrations.is_empty() || self.calibrations.len() > MAX_MATRIX_CALIBRATIONS {
            return Err(CameraProfileError::InvalidProfile {
                field: "calibrations",
                reason: format!(
                    "expected one or two calibrations, got {}",
                    self.calibrations.len()
                ),
            });
        }
        for (index, calibration) in self.calibrations.iter().enumerate() {
            calibration.validate(index)?;
        }
        if self
            .calibrations
            .windows(2)
            .any(|pair| pair[0].illuminant == pair[1].illuminant)
        {
            return Err(CameraProfileError::InvalidProfile {
                field: "calibrations.illuminant",
                reason: "each supported illuminant may occur only once".to_owned(),
            });
        }
        Ok(())
    }

    /// Select the supported calibration with the documented D65, D50, A
    /// preference. The parser and recipe validator guarantee this is present.
    #[must_use]
    pub fn preferred_calibration(&self) -> Option<&MatrixCalibration> {
        self.calibrations
            .iter()
            .min_by_key(|calibration| calibration.illuminant.priority())
    }

    /// Whether this profile was authored for the supplied decoder camera
    /// identity. Clean names are preferred, with make included as a fallback
    /// for profiles that use a manufacturer-qualified model string.
    #[must_use]
    pub fn matches_camera(
        &self,
        make: &str,
        model: &str,
        clean_make: &str,
        clean_model: &str,
    ) -> bool {
        let profile = normalize_camera_text(&self.camera_model);
        [model, clean_model]
            .into_iter()
            .filter(|candidate| !candidate.trim().is_empty())
            .map(normalize_camera_text)
            .any(|candidate| candidate == profile)
            || [
                format!("{} {}", make.trim(), model.trim()),
                format!("{} {}", clean_make.trim(), clean_model.trim()),
            ]
            .into_iter()
            .map(|candidate| normalize_camera_text(&candidate))
            .any(|candidate| !candidate.trim().is_empty() && candidate == profile)
    }
}

impl MatrixCalibration {
    fn validate(&self, index: usize) -> Result<(), CameraProfileError> {
        validate_matrix(
            &self.xyz_to_camera,
            &format!("calibrations[{index}].xyz_to_camera"),
        )?;
        if let Some(forward) = self.forward_camera_to_xyz_d50 {
            validate_matrix(
                &forward,
                &format!("calibrations[{index}].forward_camera_to_xyz_d50"),
            )?;
        }
        Ok(())
    }
}

/// Errors produced while reading or validating an imported camera profile.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CameraProfileError {
    #[error("DCP is {actual} bytes; the maximum is {maximum} bytes")]
    FileTooLarge { actual: usize, maximum: usize },
    #[error("could not read DCP {path}: {reason}")]
    Io { path: String, reason: String },
    #[error("invalid DCP TIFF: {reason}")]
    InvalidTiff { reason: String },
    #[error("unsupported DCP tag 0x{tag:04x}: {reason}")]
    UnsupportedTag { tag: u16, reason: String },
    #[error("unsupported DCP component {component}: {reason}")]
    UnsupportedComponent {
        component: &'static str,
        reason: String,
    },
    #[error("missing required DCP tag {tag}")]
    MissingTag { tag: &'static str },
    #[error("invalid profile field {field}: {reason}")]
    InvalidProfile { field: &'static str, reason: String },
}

fn validate_sha256(value: &str) -> Result<(), CameraProfileError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(CameraProfileError::InvalidProfile {
            field: "source_sha256",
            reason: "expected exactly 64 hexadecimal characters".to_owned(),
        });
    }
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(CameraProfileError::InvalidProfile {
            field: "source_sha256",
            reason: "the digest must use lowercase hexadecimal".to_owned(),
        });
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum_bytes: usize,
) -> Result<(), CameraProfileError> {
    if value.trim().is_empty() {
        return Err(CameraProfileError::InvalidProfile {
            field,
            reason: "must not be empty".to_owned(),
        });
    }
    if value.len() > maximum_bytes || value.chars().any(char::is_control) {
        return Err(CameraProfileError::InvalidProfile {
            field,
            reason: format!("must be printable text no longer than {maximum_bytes} bytes"),
        });
    }
    Ok(())
}

fn validate_matrix(matrix: &[[f32; 3]; 3], field: &str) -> Result<(), CameraProfileError> {
    if matrix.iter().flatten().any(|value| !value.is_finite()) {
        return Err(CameraProfileError::InvalidProfile {
            field: "calibrations",
            reason: format!("{field} contains a non-finite value"),
        });
    }
    let determinant = matrix_determinant(*matrix);
    if !determinant.is_finite() || determinant.abs() < 1.0e-8 {
        return Err(CameraProfileError::InvalidProfile {
            field: "calibrations",
            reason: format!("{field} is singular"),
        });
    }
    Ok(())
}

fn matrix_determinant(matrix: [[f32; 3]; 3]) -> f32 {
    matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0])
}

fn normalize_camera_text(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse an already-loaded DCP. This alias makes call sites explicit about
/// the fact that the parser does not perform installation or persistence.
pub fn parse_dcp(bytes: &[u8]) -> Result<MatrixCameraProfile, CameraProfileError> {
    parse_dcp_bytes(bytes)
}

/// Parse a DCP path without retaining the path in the resulting profile.
pub fn parse_profile_file(path: &Path) -> Result<MatrixCameraProfile, CameraProfileError> {
    parse_dcp_file(path)
}
