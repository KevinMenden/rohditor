use serde::{Deserialize, Serialize};

use crate::EditError;

/// Numerical contract used by the first Rohditor Standard rendering.
pub const ROHDITOR_STANDARD_PROCESS_VERSION: u16 = 1;

/// Fixed base rendering applied after camera colour conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "profile", rename_all = "snake_case")]
pub enum RenderingProfileSelection {
    RohditorNeutral,
    RohditorStandard { process_version: u16 },
}

impl RenderingProfileSelection {
    /// Identity base rendering used by migrated recipes and reference tests.
    pub const NEUTRAL: Self = Self::RohditorNeutral;

    /// Current Rohditor Standard rendering.
    pub const STANDARD: Self = Self::RohditorStandard {
        process_version: ROHDITOR_STANDARD_PROCESS_VERSION,
    };

    pub(crate) fn validate(self) -> Result<(), EditError> {
        match self {
            Self::RohditorNeutral => Ok(()),
            Self::RohditorStandard { process_version }
                if process_version == ROHDITOR_STANDARD_PROCESS_VERSION =>
            {
                Ok(())
            }
            Self::RohditorStandard { process_version } => Err(EditError {
                field: "rendering.profile.process_version",
                reason: format!(
                    "Standard process version {process_version} is not supported; expected {ROHDITOR_STANDARD_PROCESS_VERSION}"
                ),
            }),
        }
    }

    /// Stable user-facing name, excluding the serialized process version.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::RohditorNeutral => "Rohditor Neutral",
            Self::RohditorStandard { .. } => "Rohditor Standard",
        }
    }

    /// Process version for diagnostics and cache identity.
    #[must_use]
    pub const fn process_version(self) -> Option<u16> {
        match self {
            Self::RohditorNeutral => None,
            Self::RohditorStandard { process_version } => Some(process_version),
        }
    }
}

impl Default for RenderingProfileSelection {
    fn default() -> Self {
        Self::STANDARD
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderingAdjustments {
    #[serde(default)]
    pub profile: RenderingProfileSelection,
}

impl RenderingAdjustments {
    /// Rendering group whose base transform is an exact identity.
    pub const NEUTRAL: Self = Self {
        profile: RenderingProfileSelection::NEUTRAL,
    };
}
