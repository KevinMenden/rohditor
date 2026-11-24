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

/// One of the corrections a lens profile drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LensCorrection {
    Distortion,
    Vignetting,
    ChromaticAberration,
}

impl LensCorrection {
    pub const ALL: [Self; 3] = [
        Self::Distortion,
        Self::Vignetting,
        Self::ChromaticAberration,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Distortion => "Distortion",
            Self::Vignetting => "Vignetting",
            Self::ChromaticAberration => "Chromatic aberration",
        }
    }

    /// Whether it is wanted, as far as the profile has it.
    #[must_use]
    pub const fn is_on(self, optics: &OpticsAdjustments) -> bool {
        match self {
            Self::Distortion => optics.distortion,
            Self::Vignetting => optics.vignetting,
            Self::ChromaticAberration => optics.chromatic_aberration,
        }
    }

    #[must_use]
    pub fn with(self, optics: &OpticsAdjustments, on: bool) -> OpticsAdjustments {
        let mut next = optics.clone();
        match self {
            Self::Distortion => next.distortion = on,
            Self::Vignetting => next.vignetting = on,
            Self::ChromaticAberration => next.chromatic_aberration = on,
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_correction_is_switched_alone() {
        let on = OpticsAdjustments::default();
        for correction in LensCorrection::ALL {
            let off = correction.with(&on, false);
            for other in LensCorrection::ALL {
                assert_eq!(other.is_on(&off), other != correction);
            }
            assert_eq!(correction.with(&off, true), on);
        }
    }
}
