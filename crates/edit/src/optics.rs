use serde::{Deserialize, Serialize};

/// Lens-profile corrections applied in camera-native linear RGB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpticsAdjustments {
    #[serde(default)]
    pub profile: LensProfileSelection,
    #[serde(default = "default_enabled")]
    pub distortion: bool,
    #[serde(default = "default_enabled")]
    pub vignetting: bool,
    #[serde(default = "default_enabled")]
    pub chromatic_aberration: bool,
}

impl Default for OpticsAdjustments {
    fn default() -> Self {
        Self {
            profile: LensProfileSelection::Off,
            distortion: true,
            vignetting: true,
            chromatic_aberration: true,
        }
    }
}

/// Profile selection stored in an edit recipe. IDs are opaque to `rohditor-edit`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum LensProfileSelection {
    #[default]
    Off,
    Automatic,
    Lensfun {
        profile_id: String,
    },
}

const fn default_enabled() -> bool {
    true
}
