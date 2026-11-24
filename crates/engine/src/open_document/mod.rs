//! The document a frontend has open, without any UI toolkit.
//!
//! [`OpenDocument`] bundles what the engine tracks per open photo: the edit
//! session, the decoded RAW frame, the preview runtime state, background-job
//! status and the derived data (camera look, noise estimate). The state
//! transitions that worker results cause live here, as methods that return
//! what the frontend must do next (usually "queue a preview"), so a second UI
//! reuses them instead of rewriting the guards. A frontend wraps this type and
//! adds only its own presentation state (textures, overlays, panel models).

mod camera_look;
mod noise;
mod picks;
mod sections;
mod worker_events;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rohditor_core::{
    CameraCalibration, DatabaseProvenance, LensProfileSummary, ProfileMatch, ResolvedCameraColour,
    WhiteBalanceCoordinates, default_recipe, resolve_camera_colour, with_default_noise_reduction,
};
use rohditor_edit::{
    CameraProfileSelection, EditRecipe, TEMPERATURE_RANGE, TINT_RANGE, WhiteBalance,
};
use rohditor_raw::{RawFileInfo, RawFrame};

pub use camera_look::CameraLookState;
pub use noise::{NoisePurpose, NoiseState, NoiseSuggestion};

use crate::autosave::Autosave;
use crate::document::{EditSession, PreviewIntent, PreviewTicket};
use crate::preview::DocumentPreview;

/// An export in flight for the document.
#[derive(Debug)]
pub struct ExportActivity {
    pub id: u64,
    pub recipe_revision: u64,
    pub detail: String,
}

/// A photo open for editing.
pub struct OpenDocument {
    pub autosave: Autosave,
    pub id: u64,
    pub path: PathBuf,
    pub info: Option<RawFileInfo>,
    pub frame: Option<Arc<RawFrame>>,
    pub edits: EditSession,
    /// Preview tickets, resident GPU frame, histogram, view and diagnostics.
    pub preview: DocumentPreview,
    pub open_status: Option<String>,
    pub export_status: Option<ExportActivity>,
    pub optics_match: Option<ProfileMatch>,
    pub optics_candidates: Vec<LensProfileSummary>,
    pub optics_profile_filter: String,
    pub optics_database: Option<DatabaseProvenance>,
    pub optics_error: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
    pub notice: Option<String>,
    /// The photo has no saved edits, so its default recipe is rebuilt from the
    /// file's own metadata once that arrives.
    pub initialize_white_balance_from_metadata: bool,
    camera_look: CameraLookState,
    noise: NoiseState,
    /// A picker's sample on its way from the worker.
    pending_pick: Option<PreviewTicket>,
}

impl OpenDocument {
    /// A document whose RAW file is still being opened.
    pub fn opening(id: u64, path: PathBuf, recipe: EditRecipe, warning: Option<String>) -> Self {
        Self {
            id,
            autosave: Autosave::default(),
            path,
            info: None,
            frame: None,
            edits: EditSession::from_saved(recipe),
            preview: DocumentPreview::new(id),
            open_status: Some("Opening RAW file".to_owned()),
            export_status: None,
            optics_match: None,
            optics_candidates: Vec::new(),
            optics_profile_filter: String::new(),
            optics_database: None,
            optics_error: None,
            warning,
            error: None,
            notice: None,
            initialize_white_balance_from_metadata: false,
            camera_look: CameraLookState::default(),
            noise: NoiseState::default(),
            pending_pick: None,
        }
    }

    /// Mark a photo that has no saved edits: its default recipe follows the
    /// file's metadata, and `automatic_noise_reduction` lets the estimated noise
    /// reduction join that default.
    pub fn mark_new_photo(&mut self, automatic_noise_reduction: bool) {
        self.initialize_white_balance_from_metadata = true;
        self.noise = NoiseState::for_new_photo(automatic_noise_reduction);
    }

    pub fn file_name(&self) -> String {
        file_name(&self.path)
    }

    pub fn is_dirty(&self) -> bool {
        self.edits.is_dirty()
    }

    pub fn ticket(&self) -> PreviewTicket {
        self.preview.ticket(self.edits.revision())
    }

    pub fn begin_preview_request(&mut self, intent: PreviewIntent) -> PreviewTicket {
        self.preview.begin_request(self.edits.revision(), intent)
    }

    pub fn accepts_preview(&self, ticket: PreviewTicket, intent: PreviewIntent) -> bool {
        self.preview.accepts(self.edits.revision(), ticket, intent)
    }

    /// The camera colour currently selected for the document, resolved with the
    /// core's authoritative As Shot white-balance gains.
    pub fn as_shot_colour(&self) -> Option<ResolvedCameraColour> {
        self.as_shot_colour_for(&self.edits.recipe().color.camera_profile)
    }

    fn as_shot_colour_for(
        &self,
        camera_profile: &CameraProfileSelection,
    ) -> Option<ResolvedCameraColour> {
        let info = self
            .frame
            .as_ref()
            .map(|frame| &frame.info)
            .or(self.info.as_ref())?;
        let calibration = CameraCalibration::from_raw_info(info);
        resolve_camera_colour(&calibration, camera_profile, WhiteBalance::AsShot)
            .or_else(|_| {
                // A RAW can omit As Shot multipliers while still carrying a usable
                // camera matrix. Keep custom Temperature/Tint usable in that case; the
                // As Shot mode itself will continue to report the authoritative
                // metadata error when it is rendered.
                resolve_camera_colour(
                    &calibration,
                    camera_profile,
                    WhiteBalance::TemperatureTint {
                        temperature: TEMPERATURE_RANGE.neutral,
                        tint: TINT_RANGE.neutral,
                    },
                )
            })
            .ok()
    }

    /// The camera-specific coordinates of the decoder-provided As Shot gains.
    /// Coordinates are a display convenience; rendering keeps using the original
    /// gains when projection is not possible for a particular calibration.
    pub fn as_shot_coordinates(&self) -> Option<WhiteBalanceCoordinates> {
        self.as_shot_coordinates_for(&self.edits.recipe().color.camera_profile)
    }

    fn as_shot_coordinates_for(
        &self,
        camera_profile: &CameraProfileSelection,
    ) -> Option<WhiteBalanceCoordinates> {
        self.as_shot_colour_for(camera_profile)
            .and_then(|resolved| resolved.as_shot_coordinates)
    }

    /// The recipe the document is reset to: camera-specific white balance, plus
    /// the noise reduction derived from its own noise estimate when it has one.
    pub fn default_recipe(&self) -> EditRecipe {
        with_default_noise_reduction(
            default_recipe(self.as_shot_coordinates()),
            self.noise.default_profile(),
        )
    }

    /// Whether the recipe differs from `defaults`. The fitted camera look is
    /// derived from the photo rather than chosen, so it does not count.
    pub fn differs_from_defaults(&self, defaults: &EditRecipe) -> bool {
        let mut current = self.edits.recipe().clone();
        current.rendering.camera_tone = None;
        current.rendering.camera_chroma = None;
        current != *defaults
    }

    pub fn has_user_adjustments(&self) -> bool {
        self.differs_from_defaults(&self.default_recipe())
    }

    /// What the Detail panel shows next to the noise Auto button.
    pub fn noise_suggestion(&self) -> NoiseSuggestion {
        self.noise.suggestion()
    }
}

/// The last path component, for titles and lists.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("RAW file")
        .to_owned()
}
