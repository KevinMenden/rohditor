use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::{CameraChroma, CameraTone, EditError};

/// Numerical contract used by the first Rohditor Standard rendering.
pub const ROHDITOR_STANDARD_PROCESS_VERSION: u16 = 1;

/// Numerical contract of the camera-matched rendering (fitted tone curve).
pub const ROHDITOR_MATCHED_PROCESS_VERSION: u16 = 1;

/// Fixed base rendering applied after camera colour conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "profile", rename_all = "snake_case")]
pub enum RenderingProfileSelection {
    RohditorNeutral,
    RohditorStandard {
        process_version: u16,
    },
    /// Base tone fitted per photo to the camera's embedded JPEG. The fitted
    /// look lives in [`RenderingAdjustments`]; without one a static typical
    /// camera look is used.
    RohditorMatched {
        process_version: u16,
    },
}

impl RenderingProfileSelection {
    /// Identity base rendering used by migrated recipes and reference tests.
    pub const NEUTRAL: Self = Self::RohditorNeutral;

    /// Current Rohditor Standard rendering.
    pub const STANDARD: Self = Self::RohditorStandard {
        process_version: ROHDITOR_STANDARD_PROCESS_VERSION,
    };

    /// Current camera-matched rendering.
    pub const MATCHED: Self = Self::RohditorMatched {
        process_version: ROHDITOR_MATCHED_PROCESS_VERSION,
    };

    pub(crate) fn validate(self) -> Result<(), EditError> {
        let (expected, process_version) = match self {
            Self::RohditorNeutral => return Ok(()),
            Self::RohditorStandard { process_version } => {
                (ROHDITOR_STANDARD_PROCESS_VERSION, process_version)
            }
            Self::RohditorMatched { process_version } => {
                (ROHDITOR_MATCHED_PROCESS_VERSION, process_version)
            }
        };
        if process_version == expected {
            return Ok(());
        }
        Err(EditError {
            field: "rendering.profile.process_version",
            reason: format!(
                "{} process version {process_version} is not supported; expected {expected}",
                self.display_name()
            ),
        })
    }

    /// Stable user-facing name, excluding the serialized process version.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::RohditorNeutral => "Rohditor Neutral",
            Self::RohditorStandard { .. } => "Rohditor Standard",
            Self::RohditorMatched { .. } => "Rohditor Matched",
        }
    }

    /// Process version for diagnostics and cache identity.
    #[must_use]
    pub const fn process_version(self) -> Option<u16> {
        match self {
            Self::RohditorNeutral => None,
            Self::RohditorStandard { process_version }
            | Self::RohditorMatched { process_version } => Some(process_version),
        }
    }
}

impl Default for RenderingProfileSelection {
    fn default() -> Self {
        Self::STANDARD
    }
}

/// Tone and colour resolved for the Matched profile.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchedLook<'a> {
    pub tone: Cow<'a, CameraTone>,
    pub chroma: Option<CameraChroma>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderingAdjustments {
    #[serde(default)]
    pub profile: RenderingProfileSelection,
    /// Fitted camera tone, consumed only by the Matched profile. It is stored
    /// in the recipe so rendering is deterministic and never re-fits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_tone: Option<CameraTone>,
    /// Fitted camera colourfulness curve, applied right after the base tone
    /// by the Matched profile only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_chroma: Option<CameraChroma>,
}

impl RenderingAdjustments {
    /// Rendering group whose base transform is an exact identity.
    pub const NEUTRAL: Self = Self {
        profile: RenderingProfileSelection::NEUTRAL,
        camera_tone: None,
        camera_chroma: None,
    };

    pub(crate) fn validate(&self) -> Result<(), EditError> {
        self.profile.validate()?;
        if let Some(tone) = &self.camera_tone {
            tone.validate()?;
        }
        if let Some(chroma) = &self.camera_chroma {
            chroma.validate()?;
        }
        Ok(())
    }

    /// The look the Matched profile renders with, or `None` for the other
    /// profiles, which ignore stored fits.
    ///
    /// A fitted tone brings its own colour (possibly none, if the chroma fit
    /// was refused). Without a fitted tone the static fallback look applies as
    /// a pair, so a failed fit never leaves colour without matching tone.
    #[must_use]
    pub fn matched_look(&self) -> Option<MatchedLook<'_>> {
        if !matches!(
            self.profile,
            RenderingProfileSelection::RohditorMatched { .. }
        ) {
            return None;
        }
        Some(match &self.camera_tone {
            Some(tone) => MatchedLook {
                tone: Cow::Borrowed(tone),
                chroma: self.camera_chroma,
            },
            None => MatchedLook {
                tone: Cow::Owned(CameraTone::static_fallback()),
                chroma: Some(CameraChroma::STATIC_FALLBACK),
            },
        })
    }
}
