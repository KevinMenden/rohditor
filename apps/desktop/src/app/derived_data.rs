//! Glue between the engine's derived-data state machines (the Matched camera
//! look and the noise estimate, see `rohditor_engine::open_document`) and the
//! app: it supplies the worker and settings, and queues a preview when the
//! recipe changed.

use eframe::egui;

use rohditor_core::{CameraLook, NoiseProfile};
use rohditor_edit::CameraProfileSelection;

use super::RohditorApp;

impl RohditorApp {
    /// Bring the open document's recipe and camera-look fit in line. Cheap
    /// enough to run every frame.
    pub(super) fn sync_camera_look(&mut self, context: &egui::Context) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if document.core.sync_camera_look(&self.coordinator) {
            let document_id = document.core.id;
            self.queue_preview(context, document_id);
        }
    }

    /// Record a finished (or failed) camera-look fit for the open document.
    pub(super) fn camera_look_ready(
        &mut self,
        context: &egui::Context,
        document_id: u64,
        fitted_for: &CameraProfileSelection,
        result: Result<Box<CameraLook>, String>,
    ) {
        let Some(document) = self
            .document
            .as_mut()
            .filter(|doc| doc.core.id == document_id)
        else {
            return;
        };
        if document
            .core
            .camera_look_ready(&self.coordinator, fitted_for, result)
        {
            self.queue_preview(context, document_id);
        }
    }

    /// The RAW frame arrived: start the default noise estimate for a new photo.
    pub(super) fn start_default_noise_estimate(&mut self, document_id: u64) {
        let render_options = self.settings.render_options();
        if let Some(document) = self
            .document
            .as_mut()
            .filter(|doc| doc.core.id == document_id)
        {
            document
                .core
                .start_default_noise_estimate(&self.coordinator, render_options);
        }
    }

    pub(super) fn noise_profile_ready(
        &mut self,
        context: &egui::Context,
        document_id: u64,
        result: Result<Option<NoiseProfile>, String>,
    ) {
        let Some(document) = self
            .document
            .as_mut()
            .filter(|doc| doc.core.id == document_id)
        else {
            return;
        };
        if document.core.noise_profile_ready(result) {
            self.queue_preview(context, document_id);
        }
    }

    /// The Detail panel's Auto button.
    pub(super) fn apply_noise_suggestion(&mut self, context: &egui::Context) {
        let render_options = self.settings.render_options();
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let document_id = document.core.id;
        if document
            .core
            .apply_noise_suggestion(&self.coordinator, render_options)
        {
            self.queue_preview(context, document_id);
        }
    }
}
