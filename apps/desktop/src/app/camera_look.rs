//! The Matched rendering profile's per-photo camera look.
//!
//! Choosing Matched renders at once with the static typical look. In the
//! background the engine fits the photo's own look to its embedded JPEG; when
//! that arrives it is stored in the recipe (so it persists in the sidecar) and
//! the preview refreshes. The fit is derived data: it never enters undo
//! history, and it is redone only when the camera profile changes under it,
//! since colour is fitted against the active profile.

use eframe::egui;

use rohditor_core::CameraLook;
use rohditor_edit::{CameraProfileSelection, RenderingProfileSelection};

use super::RohditorApp;

/// Where a document stands with fitting its camera look.
#[derive(Debug, Default)]
pub(super) enum CameraLookState {
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

impl RohditorApp {
    /// Bring the open document's recipe and fit state in line, as needed by
    /// the current recipe. Cheap enough to run every frame.
    pub(super) fn sync_camera_look(&mut self, context: &egui::Context) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let recipe = document.edits.recipe();
        if !matches!(
            recipe.rendering.profile,
            RenderingProfileSelection::RohditorMatched { .. }
        ) {
            return;
        }
        let profile = recipe.color.camera_profile.clone();

        // A fit made against another camera profile no longer matches its colour.
        if document
            .camera_look
            .profile()
            .is_some_and(|fitted_for| *fitted_for != profile)
        {
            document.camera_look = CameraLookState::Idle;
            let cleared = document.edits.apply_derived(|recipe| {
                recipe.rendering.camera_tone = None;
                recipe.rendering.camera_chroma = None;
            });
            let document_id = document.id;
            if cleared {
                self.queue_preview(context, document_id);
            }
            return;
        }
        if document.edits.recipe().rendering.camera_tone.is_some() {
            return;
        }

        match &document.camera_look {
            CameraLookState::Ready(look, _) => {
                let look = look.clone();
                let applied = document
                    .edits
                    .apply_derived(|recipe| look.store_in(&mut recipe.rendering));
                let document_id = document.id;
                if applied {
                    self.queue_preview(context, document_id);
                }
            }
            CameraLookState::Idle => {
                let Some(frame) = document.frame.clone() else {
                    return;
                };
                let request = self.coordinator.fit_camera_look(
                    document.id,
                    document.path.clone(),
                    frame,
                    profile.clone(),
                );
                document.camera_look = match request {
                    Ok(()) => CameraLookState::Pending(profile),
                    Err(message) => {
                        document.warning = Some(message);
                        CameraLookState::Unavailable(profile)
                    }
                };
            }
            CameraLookState::Pending(_) | CameraLookState::Unavailable(_) => {}
        }
    }

    /// Record a finished (or failed) fit for the open document.
    pub(super) fn camera_look_ready(
        &mut self,
        context: &egui::Context,
        document_id: u64,
        fitted_for: &CameraProfileSelection,
        result: Result<Box<CameraLook>, String>,
    ) {
        let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id) else {
            return;
        };
        // Only the result of the fit currently awaited is kept: after a camera
        // profile change a newer request replaces it and this one is stale.
        if !matches!(&document.camera_look, CameraLookState::Pending(profile) if profile == fitted_for)
        {
            return;
        }
        let profile = fitted_for.clone();
        document.camera_look = CameraLookState::Idle;
        document.camera_look = match result {
            Ok(look) => {
                if let Err(error) = &look.chroma {
                    tracing::info!(%error, "camera chroma not matched; keeping the tone-only look");
                }
                CameraLookState::Ready(look, profile)
            }
            Err(message) => {
                document.warning = Some(format!(
                    "Matched look: {message}. Using the typical camera look."
                ));
                CameraLookState::Unavailable(profile)
            }
        };
        self.sync_camera_look(context);
    }
}
