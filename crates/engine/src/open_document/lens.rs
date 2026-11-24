//! The lens profiles for the open photo, and choosing one.

use std::sync::Arc;

use rohditor_core::{DatabaseProvenance, LensProfileSummary, MetadataField, ProfileMatch};
use rohditor_edit::{LensCorrection, LensProfileSelection};

use super::OpenDocument;

/// What the profile database has for the photo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LensProfiles {
    /// What the photo's metadata matches, once looked up.
    pub matched: Option<ProfileMatch>,
    /// All profiles that can be chosen by hand.
    pub choices: Vec<LensProfileSummary>,
    pub database: Option<DatabaseProvenance>,
    pub error: Option<String>,
}

impl LensProfiles {
    /// The profile that is used without a choice by hand.
    #[must_use]
    pub const fn detected(&self) -> Option<&LensProfileSummary> {
        match &self.matched {
            Some(ProfileMatch::Unique(profile)) => Some(profile),
            _ => None,
        }
    }

    /// The profiles that match the photo equally well, if there are several.
    #[must_use]
    pub fn ambiguous(&self) -> &[LensProfileSummary] {
        match &self.matched {
            Some(ProfileMatch::Ambiguous(profiles)) => profiles,
            _ => &[],
        }
    }

    /// The profile of `selection`, if there is one.
    #[must_use]
    pub fn profile(&self, selection: &LensProfileSelection) -> Option<&LensProfileSummary> {
        match selection {
            LensProfileSelection::Off => None,
            LensProfileSelection::Automatic => self.detected(),
            LensProfileSelection::Lensfun { profile_id } => self
                .choices
                .iter()
                .find(|profile| profile.id == *profile_id),
        }
    }

    /// Whether the profile of `selection` has `correction`. `None` without a
    /// profile.
    #[must_use]
    pub fn offers(
        &self,
        selection: &LensProfileSelection,
        correction: LensCorrection,
    ) -> Option<bool> {
        let available = self.profile(selection)?.available;
        Some(match correction {
            LensCorrection::Distortion => available.distortion,
            LensCorrection::Vignetting => available.vignetting,
            LensCorrection::ChromaticAberration => available.chromatic_aberration,
        })
    }

    /// Why no profile was detected.
    #[must_use]
    pub fn problem(&self) -> Option<String> {
        if let Some(error) = &self.error {
            return Some(error.clone());
        }
        Some(match self.matched.as_ref()? {
            ProfileMatch::Unique(_) => return None,
            ProfileMatch::Ambiguous(profiles) => {
                format!("{} profiles match the lens: choose one", profiles.len())
            }
            ProfileMatch::MissingMetadata { fields } => {
                let fields: Vec<_> = fields.iter().map(MetadataField::to_string).collect();
                format!("The photo does not name its {}", fields.join(", "))
            }
            ProfileMatch::CameraNotFound => "There are no profiles for this camera".to_owned(),
            ProfileMatch::LensNotFound => "There is no profile for this lens".to_owned(),
        })
    }
}

impl OpenDocument {
    pub fn lens_profiles_ready(&mut self, profiles: LensProfiles) {
        self.lens_profiles = Arc::new(profiles);
    }

    /// Choose the lens profile. Returns whether the recipe changed. Says so if
    /// no profile was detected for the automatic choice, which could not be
    /// rendered.
    pub fn select_lens_profile(&mut self, selection: LensProfileSelection) -> bool {
        let profiles = &self.lens_profiles;
        if selection == LensProfileSelection::Automatic && profiles.detected().is_none() {
            let problem = profiles.problem();
            self.notice = Some(
                problem.unwrap_or_else(|| "The lens profile is still being looked up".to_owned()),
            );
            return false;
        }
        let mut next = self.edits.recipe().clone();
        next.optics.profile = selection;
        self.edits.set_discrete(next)
    }

    pub fn set_lens_correction(&mut self, correction: LensCorrection, on: bool) -> bool {
        let mut next = self.edits.recipe().clone();
        next.optics = correction.with(&next.optics, on);
        self.edits.set_discrete(next)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rohditor_core::{CorrectionComponents, default_recipe};

    use super::*;

    fn summary(id: &str) -> LensProfileSummary {
        LensProfileSummary {
            id: id.to_owned(),
            camera: "Camera".to_owned(),
            lens: "Lens".to_owned(),
            mount: "Mount".to_owned(),
            available: CorrectionComponents {
                distortion: true,
                vignetting: false,
                chromatic_aberration: true,
            },
        }
    }

    fn profiles(matched: ProfileMatch) -> LensProfiles {
        LensProfiles {
            matched: Some(matched),
            choices: vec![summary("a"), summary("b")],
            ..LensProfiles::default()
        }
    }

    fn by_hand(id: &str) -> LensProfileSelection {
        LensProfileSelection::Lensfun {
            profile_id: id.to_owned(),
        }
    }

    #[test]
    fn a_selection_has_the_corrections_of_its_profile() {
        let profiles = profiles(ProfileMatch::Unique(summary("a")));
        let vignetting = LensCorrection::Vignetting;
        let offers = |selection, correction| profiles.offers(&selection, correction);
        assert_eq!(offers(LensProfileSelection::Off, vignetting), None);
        assert_eq!(offers(by_hand("unknown"), vignetting), None);
        for selection in [LensProfileSelection::Automatic, by_hand("b")] {
            assert_eq!(offers(selection.clone(), vignetting), Some(false));
            assert_eq!(offers(selection, LensCorrection::Distortion), Some(true));
        }
    }

    #[test]
    fn only_a_detected_lens_is_chosen_automatically() {
        let mut document =
            OpenDocument::opening(1, PathBuf::from("fixture.arw"), default_recipe(None), None);
        let automatic = LensProfileSelection::Automatic;
        assert!(!document.select_lens_profile(automatic.clone()));
        assert!(document.notice.take().is_some());
        document.lens_profiles_ready(profiles(ProfileMatch::LensNotFound));
        assert!(!document.select_lens_profile(automatic.clone()));
        assert_eq!(document.notice.take(), document.lens_profiles.problem());
        // One by hand still works.
        assert!(document.select_lens_profile(by_hand("a")));
        document.lens_profiles_ready(profiles(ProfileMatch::Unique(summary("a"))));
        assert_eq!(document.lens_profiles.problem(), None);
        assert!(document.select_lens_profile(automatic));
        assert!(document.set_lens_correction(LensCorrection::Distortion, false));
        assert!(!document.edits.recipe().optics.distortion);
        assert_eq!(document.notice, None);
    }
}
