//! The Matched rendering profile's per-photo camera look.
//!
//! Choosing Matched renders at once with the static typical look. In the
//! background the engine fits the photo's own look to its embedded JPEG; when
//! that arrives it is stored in the recipe (so it persists in the sidecar) and
//! the preview refreshes. The fit is derived data: it never enters undo
//! history, and it is redone only when the camera profile changes under it,
//! since colour is fitted against the active profile.

use rohditor_core::CameraLook;
use rohditor_edit::{CameraProfileSelection, RenderingProfileSelection};

use super::OpenDocument;
use crate::coordinator::RenderCoordinator;

/// Where a document stands with fitting its camera look.
#[derive(Debug, Default)]
pub enum CameraLookState {
    /// Nothing requested yet.
    #[default]
    Idle,
    /// A fit is running for this camera profile.
    Pending(CameraProfileSelection),
    /// A fit finished for this camera profile.
    Ready(Box<CameraLook>, CameraProfileSelection),
    /// The fit was refused or failed; the static look stays. Not retried
    /// until the camera profile changes.
    Unavailable(CameraProfileSelection),
}

impl CameraLookState {
    const fn profile(&self) -> Option<&CameraProfileSelection> {
        match self {
            Self::Idle => None,
            Self::Pending(profile) | Self::Ready(_, profile) | Self::Unavailable(profile) => {
                Some(profile)
            }
        }
    }
}

impl OpenDocument {
    /// Bring the recipe and fit state in line, as needed by the current recipe.
    /// Cheap enough to run every frame. Returns whether the recipe changed, so
    /// the caller must queue a preview.
    pub fn sync_camera_look(&mut self, coordinator: &RenderCoordinator) -> bool {
        let recipe = self.edits.recipe();
        if !matches!(
            recipe.rendering.profile,
            RenderingProfileSelection::RohditorMatched { .. }
        ) {
            return false;
        }
        let profile = recipe.color.camera_profile.clone();

        // A fit made against another camera profile no longer matches its colour.
        if self
            .camera_look
            .profile()
            .is_some_and(|fitted_for| *fitted_for != profile)
        {
            self.camera_look = CameraLookState::Idle;
            return self.edits.apply_derived(|recipe| {
                recipe.rendering.camera_tone = None;
                recipe.rendering.camera_chroma = None;
            });
        }
        if self.edits.recipe().rendering.camera_tone.is_some() {
            return false;
        }

        match &self.camera_look {
            CameraLookState::Ready(look, _) => {
                let look = look.clone();
                self.edits
                    .apply_derived(|recipe| look.store_in(&mut recipe.rendering))
            }
            CameraLookState::Idle => {
                let Some(frame) = self.frame.clone() else {
                    return false;
                };
                let request =
                    coordinator.fit_camera_look(self.id, self.path.clone(), frame, profile.clone());
                self.camera_look = match request {
                    Ok(()) => CameraLookState::Pending(profile),
                    Err(message) => {
                        self.warning = Some(message);
                        CameraLookState::Unavailable(profile)
                    }
                };
                false
            }
            CameraLookState::Pending(_) | CameraLookState::Unavailable(_) => false,
        }
    }

    /// Record a finished (or failed) fit. Returns whether the recipe changed, so
    /// the caller must queue a preview.
    pub fn camera_look_ready(
        &mut self,
        coordinator: &RenderCoordinator,
        fitted_for: &CameraProfileSelection,
        result: Result<Box<CameraLook>, String>,
    ) -> bool {
        // Only the result of the fit currently awaited is kept: after a camera
        // profile change a newer request replaces it and this one is stale.
        if !matches!(&self.camera_look, CameraLookState::Pending(profile) if profile == fitted_for)
        {
            return false;
        }
        let profile = fitted_for.clone();
        self.camera_look = match result {
            Ok(look) => {
                if let Err(error) = &look.chroma {
                    tracing::info!(%error, "camera chroma not matched; keeping the tone-only look");
                }
                CameraLookState::Ready(look, profile)
            }
            Err(message) => {
                self.warning = Some(format!(
                    "Matched look: {message}. Using the typical camera look."
                ));
                CameraLookState::Unavailable(profile)
            }
        };
        self.sync_camera_look(coordinator)
    }
}
