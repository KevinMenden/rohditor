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

/// A user-level change to the optics section of a recipe.
#[derive(Debug, Clone)]
pub enum OpticsAction {
    SelectProfile(LensProfileSelection),
    SetDistortion(bool),
    SetVignetting(bool),
    SetChromaticAberration(bool),
    Reset,
}

impl OpticsAction {
    /// The optics settings after this action.
    #[must_use]
    pub fn apply(self, optics: &OpticsAdjustments) -> OpticsAdjustments {
        let mut next = optics.clone();
        match self {
            Self::SelectProfile(profile) => next.profile = profile,
            Self::SetDistortion(value) => next.distortion = value,
            Self::SetVignetting(value) => next.vignetting = value,
            Self::SetChromaticAberration(value) => next.chromatic_aberration = value,
            Self::Reset => next = OpticsAdjustments::default(),
        }
        next
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;

    #[test]
    fn actions_change_only_their_own_setting_and_reset_restores_defaults() {
        let base = OpticsAdjustments::default();
        let profile = LensProfileSelection::Lensfun {
            profile_id: "test".to_owned(),
        };
        let selected = OpticsAction::SelectProfile(profile.clone()).apply(&base);
        assert_eq!(selected.profile, profile);
        assert_eq!(selected.distortion, base.distortion);

        let toggled = OpticsAction::SetVignetting(!base.vignetting).apply(&selected);
        assert_eq!(toggled.vignetting, !base.vignetting);
        assert_eq!(toggled.profile, profile);

        assert_eq!(OpticsAction::Reset.apply(&toggled), base);
    }
}
