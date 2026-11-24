// Top-level desktop composition; focused helpers live beside this module.
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use rohditor_core::NEURAL_FALLBACK_DEMOSAIC;
use rohditor_core::{
    CameraCalibration, HighlightDiagnostics, JPEG_QUALITY_DEFAULT, OutputPolicy, PreviewOptions,
    WhiteBalanceCoordinates, WhiteBalanceMode, WhiteBalanceModeMemory, auto_tone,
    camera_color_transform, paths_refer_to_same_file, resolve_camera_colour,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{
    BLACKS_RANGE, COLOR_GRADING_RANGE, CONTRAST_RANGE, EXPOSURE_EV_RANGE, EditRecipe,
    HIGHLIGHT_THRESHOLD_RANGE, HIGHLIGHTS_RANGE, HSL_CHANNEL_COUNT, HSL_HUE_RANGE,
    HSL_LUMINANCE_RANGE, HSL_SATURATION_RANGE, HighlightMethod, Parameter, RecipeSection,
    SATURATION_RANGE, SHADOWS_RANGE, TEMPERATURE_RANGE, TINT_RANGE, TONE_CURVE_RANGE,
    VIBRANCE_RANGE, WHITE_BALANCE_MULTIPLIER_RANGE, WHITES_RANGE, WhiteBalance,
};
use rohditor_raw::RawFileInfo;
use tracing::{info, warn};

#[cfg(test)]
use rohditor_edit::HighlightAdjustments;

use crate::ProcessorPreference;
use crate::ui::ViewMode;
use crate::ui::adjustment_panel::{
    self, AdjustmentInteraction, AdjustmentRange, AdjustmentRanges, AdjustmentTarget,
    AdjustmentValues, CameraProfileChoice, DocumentPanelModel, PresetsPanel,
};
use crate::ui::catalog::{
    self, FilmstripModel, LibraryEntryModel, LibraryEntryState, LibraryModel,
};
use crate::ui::crop::CropOverlayModel;
use crate::ui::diagnostics::{
    self, CacheModel, CatalogModel, DiagnosticsMessages, DiagnosticsModel, GpuModel,
    HighlightStatsModel, PreviewModel, QueueModel, TimingModel,
};
use crate::ui::export_dialog::{self, ExportDialogModel};
use crate::ui::optics::OpticsPanelModel;
use crate::ui::presets::PresetsPanelState;
use crate::ui::settings as settings_ui;
use crate::ui::theme;
use crate::ui::tool_panel;
use crate::ui::toolbar::{self, FilePanelModel, StatusBarModel, ToolbarModel};
use crate::ui::viewport::{self, PreviewTexture, ViewportModel};
use rohditor_engine::camera_profiles::CameraProfileRegistry;
use rohditor_engine::catalog::library::{LibraryNavigation, LibraryState};
use rohditor_engine::catalog::{CatalogCoordinator, CatalogState, ThumbnailSlot};
use rohditor_engine::coordinator::{
    PreparedGpuSource, PreviewBackend, PreviewResolution, RenderCoordinator, WorkerImage,
    WorkerPreviewDiagnostics,
};
use rohditor_engine::document::{EditSession, PreviewIntent, PreviewTicket};
use rohditor_engine::export_options::{ExportKind, ExportOptions};
use rohditor_engine::open_document::{ExportActivity, OpenDocument};
use rohditor_engine::presets::PresetStore;
use rohditor_engine::preview::{
    DocumentPreview, GpuUpload, PreviewOutcome, PreviewRequest, PreviewRuntime, PreviewSource,
};
use rohditor_engine::session;
use rohditor_engine::settings::{self, AppSettings};
use rohditor_engine::tools::{ActiveTool, Picker, ToolKind};
use rohditor_engine::view::ViewState;
use rohditor_engine::wake::WakeFn;

const RECIPE_SAVE_NOTICE_DURATION: Duration = Duration::from_secs(3);
mod clipping;
mod derived_data;
#[path = "events.rs"]
mod events;
#[path = "gpu.rs"]
mod gpu;
mod info;
#[path = "lifecycle.rs"]
mod lifecycle;
mod masks;
mod neural_model;
mod presets;
mod screenshot;
mod tools;
#[cfg(test)]
mod ui_tests;

#[derive(Debug)]
enum RecipeSaveStatus {
    Pending(String),
    Notice { message: String, until: Instant },
    Error(String),
}

impl RecipeSaveStatus {
    fn message(&self) -> &str {
        match self {
            Self::Pending(message) | Self::Error(message) => message,
            Self::Notice { message, .. } => message,
        }
    }

    fn is_pending(&self) -> bool {
        matches!(self, Self::Pending(_))
    }

    fn notice_deadline(&self) -> Option<Instant> {
        match self {
            Self::Notice { until, .. } => Some(*until),
            Self::Pending(_) | Self::Error(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SettingsDialog {
    draft: AppSettings,
}

/// The engine's open document plus the egui state that presents it.
struct Document {
    /// Toolkit-free document state: edits, RAW frame, preview runtime, job status.
    core: OpenDocument,
    texture: Option<PreviewTexture>,
    /// The Masks tool's red tint of the selected mask.
    mask_overlay: Option<masks::MaskTint>,
    /// The (recipe revision, layer) whose range overlay was asked of the worker. It stays set
    /// after a failure, so a failing request is not repeated every frame.
    range_overlay_requested: Option<(u64, u32)>,
    preview_pixels: Option<DisplayPreviewPixels>,
    /// egui's id for the engine's GPU display texture.
    gpu_texture_id: Option<egui::TextureId>,
    /// The `preview` display generation `texture` was last synced to.
    synced_display_generation: u64,
    clipping: clipping::ClippingState,
}

#[derive(Debug, Clone)]
struct DisplayPreviewPixels {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

impl Document {
    fn opening(id: u64, path: PathBuf, recipe: EditRecipe, warning: Option<String>) -> Self {
        Self {
            core: OpenDocument::opening(id, path, recipe, warning),
            texture: None,
            mask_overlay: None,
            range_overlay_requested: None,
            preview_pixels: None,
            gpu_texture_id: None,
            synced_display_generation: 0,
            clipping: clipping::ClippingState::default(),
        }
    }
}

pub(crate) struct RohditorApp {
    recipe_saves: rohditor_engine::persistence::SaveWorker,
    coordinator: RenderCoordinator,
    catalog_coordinator: CatalogCoordinator,
    catalog: CatalogState,
    document: Option<Document>,
    next_document_id: u64,
    next_export_id: u64,
    export_settings: ExportOptions,
    /// The export dialog, opened from the toolbar.
    export_dialog_open: bool,
    settings: AppSettings,
    settings_warning: Option<String>,
    /// Why the neural demosaic cannot be selected; `None` when its model is loaded.
    neural_unavailable: Option<String>,
    recipe_save_status: Option<RecipeSaveStatus>,
    recipe_save_error: Option<String>,
    queued_recipe_snapshots: HashMap<PathBuf, (u64, EditRecipe)>,
    camera_profiles: CameraProfileRegistry,
    presets: PresetStore,
    presets_ui: PresetsPanelState,
    settings_dialog: Option<SettingsDialog>,
    /// Where settings, session and camera profiles persist. Injected so tests
    /// can use a scratch directory instead of the user's real one.
    config_dir: Option<PathBuf>,
    /// Set by `--screenshot`: save one frame and quit.
    screenshot: Option<screenshot::ScreenshotRequest>,
    ui_renderer: &'static str,
    /// Processor choice and the engine's GPU preview runtime.
    preview_runtime: PreviewRuntime,
    /// egui's wgpu renderer, when the UI renders with wgpu.
    egui_gpu: Option<gpu::EguiGpu>,
    startup_error: Option<String>,
    show_diagnostics: bool,
    /// The viewport's clipping overlay (`J`). A view aid: not persisted.
    show_clipping: bool,
    /// The active tool and its transient state (pickers, crop draft).
    tool: ActiveTool,
    color_mixer_channel: usize,
    /// A click of the color-range picker waiting for its sample from the worker.
    pending_color_pick: Option<PreviewTicket>,
    white_balance_memory: WhiteBalanceModeMemory,
    view_mode: ViewMode,
    /// Library selection, display order and resident grid textures.
    library: LibraryState<egui::TextureHandle>,
    filmstrip_visible: (usize, usize),
    filmstrip_active: Option<usize>,
}

fn white_balance_preview_request(
    interaction: AdjustmentInteraction,
    changed: bool,
) -> (bool, bool) {
    let is_white_balance = matches!(
        interaction.target,
        AdjustmentTarget::Parameter(Parameter::Temperature | Parameter::Tint)
            | AdjustmentTarget::WhiteBalanceRed
            | AdjustmentTarget::WhiteBalanceGreen
            | AdjustmentTarget::WhiteBalanceBlue
    );
    (
        is_white_balance && changed,
        is_white_balance
            && (interaction.drag_stopped
                || (changed && !interaction.drag_started && !interaction.dragged)),
    )
}

impl RohditorApp {
    /// Arm the `--screenshot` capture: after `delay`, save a frame and exit.
    pub(crate) fn with_screenshot(mut self, path: PathBuf, delay: Duration) -> Self {
        self.screenshot = Some(screenshot::ScreenshotRequest::new(path, delay));
        self
    }

    pub(crate) fn new(
        context: &eframe::CreationContext<'_>,
        initial_path: Option<PathBuf>,
        processor_preference: ProcessorPreference,
        settings: AppSettings,
        settings_warning: Option<String>,
        show_diagnostics: bool,
        directories: crate::config::AppDirectories,
    ) -> std::io::Result<Self> {
        let crate::config::AppDirectories {
            config: config_dir,
            data: data_dir,
        } = directories;
        theme::apply(&context.egui_ctx);
        let ui_renderer = if context.wgpu_render_state.is_some() {
            "wgpu"
        } else if context.gl.is_some() {
            "glow"
        } else {
            "unknown"
        };
        let egui_gpu = gpu::EguiGpu::new(context);
        let preview_runtime = PreviewRuntime::new(
            processor_preference,
            egui_gpu.as_ref().map(gpu::EguiGpu::wgpu_context),
        );
        let startup_error = preview_runtime.startup_error().map(str::to_owned);
        info!(
            ui_renderer,
            processor = if preview_runtime.gpu().is_some() { "GPU" } else { "CPU" },
            processor_preference = ?processor_preference,
            "desktop application started"
        );
        let neural = neural_model::load(
            data_dir.as_deref(),
            processor_preference == ProcessorPreference::Cpu,
        );
        let neural_unavailable = neural.as_ref().err().cloned();
        let (settings, settings_warning) = match &neural_unavailable {
            Some(reason) if settings.demosaic() == DemosaicAlgorithm::Neural => {
                let mut settings = settings;
                settings.set_demosaic(NEURAL_FALLBACK_DEMOSAIC);
                (
                    settings,
                    Some(format!(
                        "The neural demosaic is unavailable ({reason}); using {} instead.",
                        NEURAL_FALLBACK_DEMOSAIC.stable_name()
                    )),
                )
            }
            _ => (settings, settings_warning),
        };
        let coordinator = RenderCoordinator::with_neural(wake_from(&context.egui_ctx), neural.ok())
            .map_err(std::io::Error::other)?;
        if let Some(runtime) = preview_runtime.gpu() {
            let wgpu = runtime.context();
            coordinator
                .configure_capture_device(
                    wgpu.adapter.clone(),
                    wgpu.device.clone(),
                    wgpu.queue.clone(),
                    wgpu.target_format,
                )
                .map_err(std::io::Error::other)?;
        }
        let catalog_coordinator =
            CatalogCoordinator::new(wake_from(&context.egui_ctx)).map_err(std::io::Error::other)?;
        // Restore the last browsed folder when the app was not given a file.
        let mut view_mode = ViewMode::default();
        if initial_path.is_none()
            && let Some(directory) = &config_dir
            && let Some(folder) = session::load_last_folder(directory)
            && catalog_coordinator.scan_folder(folder).is_ok()
        {
            view_mode = ViewMode::Library;
        }
        let mut application = Self {
            recipe_saves: rohditor_engine::persistence::SaveWorker::new()?,
            coordinator,
            catalog_coordinator,
            catalog: CatalogState::default(),
            document: None,
            next_document_id: 1,
            next_export_id: 1,
            export_settings: ExportOptions::with_jpeg_quality(JPEG_QUALITY_DEFAULT),
            export_dialog_open: false,
            settings,
            settings_warning,
            neural_unavailable,
            recipe_save_status: None,
            recipe_save_error: None,
            queued_recipe_snapshots: HashMap::new(),
            camera_profiles: CameraProfileRegistry::load(config_dir.as_deref()),
            presets: PresetStore::load(config_dir.as_deref()),
            presets_ui: PresetsPanelState::default(),
            settings_dialog: None,
            config_dir,
            screenshot: None,
            ui_renderer,
            preview_runtime,
            egui_gpu,
            startup_error,
            show_diagnostics,
            show_clipping: false,
            tool: ActiveTool::default(),
            color_mixer_channel: 0,
            pending_color_pick: None,
            white_balance_memory: WhiteBalanceModeMemory::default(),
            view_mode,
            library: LibraryState::default(),
            filmstrip_visible: (0, 0),
            filmstrip_active: None,
        };
        if let Some(path) = initial_path {
            application.open_path(&context.egui_ctx, path);
        }
        Ok(application)
    }

    fn open_file_dialog(&mut self, context: &egui::Context) {
        let selected = rfd::FileDialog::new()
            .set_title("Open Sony ARW photo")
            .add_filter("Sony ARW photos", &["arw"])
            .pick_file();
        if let Some(path) = selected {
            self.view_mode = ViewMode::Develop;
            self.open_path(context, path);
        }
    }

    fn import_camera_profile(&mut self, context: &egui::Context) {
        let Some(info) = self
            .document
            .as_ref()
            .and_then(|document| document.core.info.clone())
        else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import matrix camera profile")
            .add_filter("DNG Camera Profiles", &["dcp"])
            .pick_file()
        else {
            return;
        };
        let result = self.camera_profiles.import(&path, &info);
        match result {
            Ok(profile) => {
                let changed_document = self.document.as_mut().and_then(|document| {
                    let changed = document.core.select_camera_profile(
                        rohditor_edit::CameraProfileSelection::Matrix(profile),
                    );
                    document.core.error = None;
                    if self.camera_profiles.warning().is_some() {
                        document.core.warning = self.camera_profiles.warning().map(str::to_owned);
                    }
                    changed.then_some(document.core.id)
                });
                if let Some(document_id) = changed_document {
                    self.queue_preview(context, document_id);
                }
            }
            Err(error) => {
                if let Some(document) = self.document.as_mut() {
                    document.core.error = Some(error);
                }
            }
        }
    }

    fn open_folder_dialog(&mut self) {
        let Some(folder) = rfd::FileDialog::new()
            .set_title("Open photo folder")
            .pick_folder()
        else {
            return;
        };
        if let Err(error) = self.catalog_coordinator.scan_folder(folder.clone()) {
            warn!(%error, "could not start the catalog scan");
            return;
        }
        if let Some(directory) = &self.config_dir {
            session::store_last_folder(directory, &folder);
        }
        self.view_mode = ViewMode::Library;
    }

    /// Apply pending catalog events and keep the catalog worker paused
    /// exactly while document work would contend with it.
    fn update_catalog(&mut self) {
        for event in self.catalog_coordinator.try_events() {
            self.catalog.apply_event(event);
        }
        self.catalog_coordinator
            .set_paused(self.document_work_in_flight());
    }

    fn document_work_in_flight(&self) -> bool {
        self.document.as_ref().is_some_and(|document| {
            document.core.open_status.is_some()
                || document.core.preview.status.is_some()
                || document.core.export_status.is_some()
        }) || self.is_picking()
    }

    fn preview_options(&self) -> PreviewOptions {
        PreviewOptions {
            render: self.settings.render_options(),
            ..PreviewOptions::default()
        }
    }

    /// The document inputs of a preview operation, owned so the engine can
    /// borrow the document's preview state mutably at the same time.
    fn preview_request(&self, context: &egui::Context, document: &Document) -> PreviewRequest {
        PreviewRequest {
            frame: document.core.frame.clone(),
            recipe: document.core.edits.recipe().clone(),
            revision: document.core.edits.revision(),
            options: self.preview_options(),
            histogram_always: self.show_clipping,
            now: context.input(|input| input.time),
        }
    }

    /// Run an engine preview operation on the document `document_id` (when it
    /// is the open one) and apply its outcome to the UI state.
    fn with_document_preview<R>(
        &mut self,
        context: &egui::Context,
        document_id: u64,
        operation: impl FnOnce(
            &mut PreviewRuntime,
            &RenderCoordinator,
            &mut DocumentPreview,
            &PreviewRequest,
        ) -> (R, PreviewOutcome),
    ) -> Option<R> {
        let request = self.preview_request(
            context,
            self.document
                .as_ref()
                .filter(|document| document.core.id == document_id)?,
        );
        let document = self.document.as_mut()?;
        let (result, outcome) = operation(
            &mut self.preview_runtime,
            &self.coordinator,
            &mut document.core.preview,
            &request,
        );
        outcome.error.apply(&mut document.core.error);
        outcome.warning.apply(&mut document.core.warning);
        if let Some(gpu) = &self.egui_gpu {
            gpu.sync_display(document);
        }
        if outcome.repaint {
            context.request_repaint();
        }
        Some(result)
    }

    fn queue_preview(&mut self, context: &egui::Context, document_id: u64) {
        self.with_document_preview(
            context,
            document_id,
            |runtime, coordinator, preview, request| {
                ((), runtime.queue_preview(coordinator, preview, request))
            },
        );
    }

    /// Rebuild the presentation currently requested by the viewport. A
    /// settings change is not an edit, so this advances only the presentation
    /// sequence and deliberately leaves the recipe history untouched.
    fn refresh_current_presentation(&mut self, context: &egui::Context) {
        let Some(document_id) = self.document.as_ref().map(|document| document.core.id) else {
            return;
        };
        if self.tool.crop().is_none() {
            self.queue_preview(context, document_id);
            return;
        }

        let options = self.preview_options();
        let request = self.document.as_mut().and_then(|document| {
            let frame = document.core.frame.as_ref().map(Arc::clone)?;
            let ticket = document
                .core
                .begin_preview_request(PreviewIntent::CropToolFullFrame);
            let recipe = document.core.edits.recipe().clone();
            document.core.preview.histogram_revision = None;
            document.core.preview.diagnostics = None;
            document.core.preview.status = Some((
                ticket.revision,
                "Rebuilding full image for crop with the selected demosaic algorithm".to_owned(),
            ));
            Some((ticket, frame, recipe))
        });
        let Some((ticket, frame, recipe)) = request else {
            return;
        };
        self.cancel_pick();
        if let Err(error) = self
            .coordinator
            .crop_tool_preview(ticket, frame, recipe, options)
            && let Some(document) = self.document.as_mut()
        {
            document.core.preview.status = None;
            document.core.error = Some(error);
        }
    }

    fn open_settings(&mut self) {
        if self.settings_dialog.is_none() {
            self.settings_dialog = Some(SettingsDialog {
                draft: self.settings,
            });
        }
    }

    fn show_application_settings(&mut self, context: &egui::Context) {
        let Some(dialog) = self.settings_dialog else {
            return;
        };
        let model = settings_ui::SettingsWindowModel {
            active_demosaic: self.settings.demosaic(),
            draft_demosaic: dialog.draft.demosaic(),
            active_output_policy: self.settings.output_policy(),
            draft_output_policy: dialog.draft.output_policy(),
            active_cpu_only: self.settings.cpu_only(),
            draft_cpu_only: dialog.draft.cpu_only(),
            active_auto_noise_reduction: self.settings.auto_noise_reduction(),
            draft_auto_noise_reduction: dialog.draft.auto_noise_reduction(),
            warning: self.settings_warning.as_deref(),
            neural_unavailable: self.neural_unavailable.as_deref(),
        };
        let mut open = true;
        let output = settings_ui::show(context, &mut open, &model);
        if output.cancel || !open {
            self.settings_dialog = None;
            return;
        }
        if let Some(demosaic) = output.selected_demosaic
            && let Some(dialog) = self.settings_dialog.as_mut()
        {
            dialog.draft.set_demosaic(demosaic);
        }
        if let Some(output_policy) = output.selected_output_policy
            && let Some(dialog) = self.settings_dialog.as_mut()
        {
            dialog.draft.set_output_policy(output_policy);
        }
        if let Some(cpu_only) = output.selected_cpu_only
            && let Some(dialog) = self.settings_dialog.as_mut()
        {
            dialog.draft.set_cpu_only(cpu_only);
        }
        if let Some(enabled) = output.selected_auto_noise_reduction
            && let Some(dialog) = self.settings_dialog.as_mut()
        {
            dialog.draft.set_auto_noise_reduction(enabled);
        }
        if output.apply {
            self.apply_settings(context);
        }
    }

    fn apply_settings(&mut self, context: &egui::Context) {
        let Some(next) = self.settings_dialog.map(|dialog| dialog.draft) else {
            return;
        };
        if next == self.settings {
            return;
        }

        // The processor is chosen at startup, so a CPU-only change alone does
        // not alter the current image.
        let changes_pixels = next.demosaic() != self.settings.demosaic()
            || next.output_policy() != self.settings.output_policy();
        self.settings = next;
        if changes_pixels {
            self.cancel_pick();
            self.refresh_current_presentation(context);
        }
        match settings::save(self.config_dir.as_deref(), next) {
            Ok(()) => {
                self.settings_warning = None;
                self.settings_dialog = None;
            }
            Err(error) => {
                let message = format!(
                    "The new setting is active for this run, but could not be saved and will not survive restart: {error}"
                );
                warn!(%error, "desktop settings could not be saved");
                self.settings_warning = Some(message);
                if let Some(dialog) = self.settings_dialog.as_mut() {
                    dialog.draft = next;
                }
            }
        }
    }

    fn install_gpu_upload(
        &mut self,
        context: &egui::Context,
        ticket: PreviewTicket,
        prepared: PreparedGpuSource,
        diagnostics: WorkerPreviewDiagnostics,
        upload_preparation: Duration,
    ) {
        self.with_document_preview(
            context,
            ticket.document_id,
            |runtime, coordinator, preview, request| {
                let upload = GpuUpload {
                    ticket,
                    prepared,
                    diagnostics,
                    upload_preparation,
                };
                let outcome = runtime.install_gpu_upload(coordinator, preview, request, upload);
                ((), outcome)
            },
        );
    }

    /// Apply a white-balance edit to the resident GPU frame at once. `false`
    /// means the caller must queue an exact preview instead.
    fn render_gpu_white_balance_draft(
        &mut self,
        context: &egui::Context,
        document_id: u64,
    ) -> bool {
        self.with_document_preview(
            context,
            document_id,
            |runtime, coordinator, preview, request| {
                runtime.render_white_balance_draft(coordinator, preview, request)
            },
        )
        .unwrap_or(false)
    }

    fn handle_gpu_failure(&mut self, context: &egui::Context, document_id: u64, error: String) {
        self.with_document_preview(
            context,
            document_id,
            |runtime, coordinator, preview, request| {
                (
                    (),
                    runtime.handle_gpu_failure(coordinator, preview, request, error),
                )
            },
        );
    }

    /// Drop the document's resident GPU frame and free its egui texture.
    fn release_gpu_preview(&self, document: &mut Document) {
        document.core.preview.release_gpu();
        if let Some(gpu) = &self.egui_gpu {
            gpu.sync_display(document);
        }
    }

    fn release_document_gpu_preview(&mut self, document_id: u64) {
        let Some(mut document) = self.document.take() else {
            return;
        };
        if document.core.id == document_id {
            self.release_gpu_preview(&mut document);
        }
        self.document = Some(document);
    }

    fn refresh_gpu_queue_completion(&mut self, context: &egui::Context) {
        let Some(document) = self.document.as_mut() else {
            self.preview_runtime.poll_device();
            return;
        };
        if let Some(after) = self
            .preview_runtime
            .poll_completion(&mut document.core.preview)
        {
            context.request_repaint_after(after);
        }
    }

    fn refresh_gpu_histogram(&mut self, context: &egui::Context) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let poll = self.preview_runtime.poll_histogram(
            &mut document.core.preview,
            document.core.edits.revision(),
            self.show_clipping,
        );
        if poll.repaint {
            context.request_repaint();
        }
        if let Some(after) = poll.repaint_after {
            context.request_repaint_after(after);
        }
        if let Some(pixels) = poll.pixels {
            clipping::install_gpu_readback(
                context,
                document,
                pixels.width,
                pixels.height,
                &pixels.rgba,
            );
        }
    }

    fn processor_description(&self) -> String {
        if let Some(runtime) = self.preview_runtime.gpu() {
            let capabilities = runtime.processor().capabilities();
            let hardware = format!("{} · {}", capabilities.adapter_name, capabilities.backend);
            if self
                .document
                .as_ref()
                .and_then(|document| document.core.preview.diagnostics)
                .is_some_and(|diagnostics| diagnostics.worker.capture_cpu_recovery)
            {
                return format!("CPU sensor recovery · GPU spatial/color · {hardware}");
            }
            match self
                .document
                .as_ref()
                .and_then(|document| document.core.preview.diagnostics)
                .map(|diagnostics| diagnostics.worker.backend)
            {
                Some(PreviewBackend::Cpu) => format!("CPU fallback · GPU available · {hardware}"),
                Some(PreviewBackend::GpuBase) => {
                    let sensor_gpu = self
                        .document
                        .as_ref()
                        .and_then(|d| d.core.preview.diagnostics)
                        .is_some_and(|d| d.worker.sensor_gpu);
                    if sensor_gpu {
                        format!("GPU sensor/spatial/color · {hardware}")
                    } else {
                        format!("CPU sensor · GPU spatial/color · {hardware}")
                    }
                }
                None => format!("GPU available · {hardware}"),
            }
        } else {
            format!("CPU ({})", self.preview_runtime.preference().label())
        }
    }

    fn gpu_required_but_unavailable(&self) -> bool {
        self.preview_runtime.gpu_required_but_unavailable()
    }

    fn request_export(&mut self) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        if document.core.frame.is_none() || document.core.export_status.is_some() {
            return;
        }
        let extension = self.export_settings.extension();
        let suggested = self
            .export_settings
            .suggested_file_name(&document.core.path);
        let filter_name = match self.export_settings.kind {
            ExportKind::Jpeg => "JPEG image",
            ExportKind::Png => "PNG image",
        };
        let selected = rfd::FileDialog::new()
            .set_title("Export developed image")
            .set_file_name(suggested)
            .add_filter(filter_name, &[extension])
            .save_file();
        let Some(mut destination) = selected else {
            return;
        };
        if destination.extension().is_none() {
            destination.set_extension(extension);
        }

        let settings = self.export_settings.core();
        if let Err(error) = settings.validate_destination(&destination) {
            if let Some(document) = self.document.as_mut() {
                document.core.error = Some(error.to_string());
            }
            return;
        }
        match paths_refer_to_same_file(&document.core.path, &destination) {
            Ok(true) => {
                if let Some(document) = self.document.as_mut() {
                    document.core.error = Some(
                        "Refusing to replace the source RAW, including through a symbolic or hard link. Choose a different export destination."
                            .to_owned(),
                    );
                }
                return;
            }
            Ok(false) => {}
            Err(error) => {
                if let Some(document) = self.document.as_mut() {
                    document.core.error = Some(format!(
                        "Could not validate the export destination {}: {error}",
                        destination.display()
                    ));
                }
                return;
            }
        }

        let export_id = self.next_export_id;
        self.next_export_id = self.next_export_id.saturating_add(1);
        let document_id = document.core.id;
        let recipe_revision = document.core.edits.revision();
        let recipe = document.core.edits.recipe().clone();
        let frame = document.core.frame.as_ref().map(Arc::clone);
        let Some(frame) = frame else {
            return;
        };
        if let Some(document) = self.document.as_mut() {
            document.core.export_status = Some(ExportActivity {
                id: export_id,
                recipe_revision,
                detail: "Queued full-resolution export".to_owned(),
            });
            document.core.error = None;
            document.core.notice = None;
        }
        if let Err(error) = self.coordinator.export(
            document_id,
            export_id,
            recipe_revision,
            destination,
            frame,
            recipe,
            settings,
            self.settings.render_options(),
            self.preview_runtime.preference() != ProcessorPreference::Cpu,
        ) && let Some(document) = self.document.as_mut()
        {
            document.core.export_status = None;
            document.core.error = Some(error);
        }
    }

    fn show_top_bar(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        let model = ToolbarModel {
            mode: self.view_mode,
            document_name: self
                .document
                .as_ref()
                .map(|document| document.core.file_name()),
            can_undo: self
                .document
                .as_ref()
                .is_some_and(|document| document.core.edits.can_undo()),
            can_redo: self
                .document
                .as_ref()
                .is_some_and(|document| document.core.edits.can_redo()),
            fit_selected: self
                .document
                .as_ref()
                .is_none_or(|document| document.core.preview.view.is_fit()),
            source_scale_selected: source_scale_selected(self.document.as_ref()),
            zoom_label: self.document.as_ref().map_or_else(
                || "FIT".to_owned(),
                |document| document.core.preview.view.zoom_label(),
            ),
            diagnostics_open: self.show_diagnostics,
            clipping_selected: self.show_clipping,
            dirty: self
                .document
                .as_ref()
                .is_some_and(|document| document.core.is_dirty()),
            export_ready: self.view_mode == ViewMode::Develop
                && self.document.as_ref().is_some_and(|document| {
                    document.core.frame.is_some() && document.core.export_status.is_none()
                }),
        };
        let actions = toolbar::show_top(root, &model);
        if let Some(mode) = actions.view_mode
            && mode != self.view_mode
        {
            // The mode switch swaps the entire central UI, unmounting any
            // dragged slider, so it commits an open gesture first.
            if let Some(document) = self.document.as_mut() {
                document.core.edits.finish_gesture();
            }
            if mode == ViewMode::Library {
                // The crop overlay and pickers belong to the develop viewport.
                self.tool = ActiveTool::default();
                self.cancel_pick();
            }
            self.view_mode = mode;
        }
        if actions.toggle_diagnostics {
            self.show_diagnostics = !self.show_diagnostics;
        }
        if actions.toggle_clipping {
            self.toggle_clipping_overlay();
        }
        if actions.settings {
            self.open_settings();
        }
        if actions.save {
            let _ = self.save_document();
        }
        if actions.open_file {
            self.open_file_dialog(context);
        } else if actions.close {
            self.request_close_document(context);
        }
        if actions.open_folder {
            self.open_folder_dialog();
        }
        if actions.undo || actions.redo || actions.reset {
            // History moves under the draft, so the draft is discarded first.
            self.leave_tool(context, false);
        }

        let now = context.input(|input| input.time);
        let mut changed_document = None;
        let mut view_changed_document = None;
        if let Some(document) = self.document.as_mut() {
            let mut changed = false;
            if actions.undo {
                changed |= document.core.edits.undo();
            }
            if actions.redo {
                changed |= document.core.edits.redo();
            }
            if actions.reset {
                let recipe = document.core.default_recipe();
                changed |= document.core.edits.reset_to(recipe);
            }
            if changed {
                document.core.notice = None;
                changed_document = Some(document.core.id);
            }
            if actions.fit && document.core.preview.request_fit(now) {
                view_changed_document = Some(document.core.id);
            }
            if actions.actual_size
                && self.tool.crop().is_none()
                && document.core.preview.request_actual_size(now)
            {
                view_changed_document = Some(document.core.id);
            }
        }
        if actions.reset {
            self.white_balance_memory = WhiteBalanceModeMemory::default();
        }
        if let Some(document_id) = changed_document {
            self.queue_preview(context, document_id);
        } else if let Some(document_id) = view_changed_document {
            self.queue_preview(context, document_id);
        }
        if actions.export {
            self.export_dialog_open = true;
        }
    }

    /// The dialog edits the export settings; its button starts the export.
    fn show_export_dialog(&mut self, context: &egui::Context) {
        if !self.export_dialog_open {
            return;
        }
        let model = ExportDialogModel {
            ready: self
                .document
                .as_ref()
                .is_some_and(|document| document.core.frame.is_some()),
            in_progress: self
                .document
                .as_ref()
                .is_some_and(|document| document.core.export_status.is_some()),
        };
        let mut open = true;
        let export_requested =
            export_dialog::show(context, &mut open, &model, &mut self.export_settings);
        self.export_dialog_open = open && !export_requested;
        if export_requested {
            self.request_export();
        }
    }

    fn show_file_panel(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        let model = FilePanelModel {
            file_name: self
                .document
                .as_ref()
                .map(|document| document.core.file_name()),
            camera: self.document.as_ref().and_then(|document| {
                document
                    .core
                    .info
                    .as_ref()
                    .map(|info| format!("{} {}", info.clean_make, info.clean_model))
            }),
            dimensions: self
                .document
                .as_ref()
                .and_then(|document| document.core.info.as_ref())
                .map(|info| (info.width, info.height)),
            source_state: self
                .document
                .as_ref()
                .and_then(|document| document.core.preview.source)
                .map(|source| source.short_label().to_owned()),
        };
        let output = toolbar::show_file_panel(root, &model);
        if output.close {
            self.request_close_document(context);
        }
    }

    fn show_adjustment_panel(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        let model = self.document.as_ref().map(|document| {
            document_panel_model(
                document,
                self.tool.picker(),
                self.color_mixer_channel,
                &self.camera_profiles,
            )
        });
        let panel = tools::tool_panel_model(self.document.as_ref(), ToolKind::Edit);
        let presets = PresetsPanel {
            state: &mut self.presets_ui,
            presets: self.presets.presets(),
        };
        let output = tool_panel::show(root, &panel, |ui| {
            adjustment_panel::show_body(ui, model, presets)
        });
        let picker_mode = output.picker_mode;
        let mut changed_document = None;
        let mut white_balance_draft = None;
        let mut white_balance_exact = None;
        let mut white_balance_memory = self.white_balance_memory;
        if let Some(document) = self.document.as_mut() {
            let mut changed = false;
            let mut auto_tone_applied = false;
            if let Some(selection) = output.camera_profile {
                changed |= document.core.select_camera_profile(selection);
            }
            if let Some(selection) = output.rendering_profile {
                let mut next = document.core.edits.recipe().clone();
                next.rendering.profile = selection;
                changed |= document.core.edits.set_discrete(next);
            }
            if let Some(enabled) = output.capture_enabled {
                let mut next = document.core.edits.recipe().clone();
                next.capture_sharpening.enabled = enabled;
                changed |= document.core.edits.set_discrete(next);
            }
            if let Some(method) = output.highlight_method {
                changed |= set_highlight_method(&mut document.core.edits, method);
            }
            // Temperature is camera-calibrated and tint is relative to the
            // As Shot locus offset. Resolve the current profile after applying
            // a profile selection so mode switches and slider edits start from
            // this document's actual As Shot white point.
            let as_shot_coordinates = document.core.as_shot_coordinates();
            if let Some(mode) = output.white_balance_mode {
                let mut next = document.core.edits.recipe().clone();
                next.color.white_balance = white_balance_memory.select(
                    next.color.white_balance,
                    mode,
                    as_shot_coordinates,
                );
                let mode_changed = document.core.edits.set_discrete(next);
                changed |= mode_changed;
                if mode_changed {
                    white_balance_draft = Some(document.core.id);
                    white_balance_exact = Some(document.core.id);
                }
            }
            if let Some(action) = output.optics_action {
                let mut next = document.core.edits.recipe().clone();
                next.optics = action.apply(&next.optics);
                changed |= document.core.edits.set_discrete(next);
            }
            if let Some(filter) = output.optics_filter {
                document.core.optics_profile_filter = filter;
            }
            if output.auto_tone
                && document.core.preview.histogram_revision == Some(document.core.edits.revision())
                && let Some(histogram) = document.core.preview.histogram.as_ref()
            {
                let auto_changed = auto_tone(document.core.edits.recipe(), histogram)
                    .is_some_and(|next| document.core.edits.set_discrete(next));
                changed |= auto_changed;
                auto_tone_applied = auto_changed;
            }
            let defaults = document.core.default_recipe();
            for interaction in output.interactions {
                let interaction_changed =
                    apply_adjustment_interaction(&mut document.core.edits, interaction, &defaults);
                changed |= interaction_changed;
                let (request_draft, request_exact) =
                    white_balance_preview_request(interaction, interaction_changed);
                if request_draft {
                    white_balance_draft = Some(document.core.id);
                }
                if request_exact {
                    white_balance_exact = Some(document.core.id);
                }
            }
            if output.reset_all {
                let defaults = document.core.default_recipe();
                changed |= document.core.edits.reset_to(defaults);
                // Hidden values are UI state, but reset-all should reset them
                // too; otherwise switching away from As-shot after a reset
                // would unexpectedly resurrect an older manual WB choice.
                white_balance_memory = WhiteBalanceModeMemory::default();
            }
            if let Some(section) = output.reset_section {
                changed |= document.core.reset_section(section);
                if section == RecipeSection::WhiteBalance {
                    // Same reason as reset-all: forget the hidden manual choice.
                    white_balance_memory = WhiteBalanceModeMemory::default();
                }
            }
            if changed {
                document.core.notice = None;
                changed_document = Some(document.core.id);
            }
            white_balance_memory.remember(document.core.edits.recipe().color.white_balance);
            if auto_tone_applied {
                document.core.notice = Some(
                    "Auto tone applied from the current display histogram heuristic".to_owned(),
                );
            }
        }
        self.white_balance_memory = white_balance_memory;
        let draft_rendered = white_balance_draft
            .is_some_and(|document_id| self.render_gpu_white_balance_draft(context, document_id));
        if let Some(document_id) = white_balance_exact {
            self.queue_preview(context, document_id);
        } else if let Some(document_id) = changed_document
            && !draft_rendered
        {
            self.queue_preview(context, document_id);
        }
        if let Some(action) = output.presets {
            self.handle_presets_action(context, action);
        }
        if output.import_camera_profile {
            self.import_camera_profile(context);
        }
        if output.auto_noise_reduction {
            self.apply_noise_suggestion(context);
        }
        if let Some(picker) = picker_mode {
            self.tool.set_picker(picker);
        }
        if let Some(channel) = output.color_mixer_channel {
            self.color_mixer_channel = channel.min(HSL_CHANNEL_COUNT - 1);
        }
    }

    fn show_viewport(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        self.refresh_clipping_overlay(context);
        let crop_overlay = self
            .tool
            .crop()
            .filter(|session| session.full_frame_ready())
            .map(|session| CropOverlayModel {
                crop: session.draft(),
                active_handle: session.active_handle(),
            });
        let output = if let Some(document) = self.document.as_mut() {
            let preparing =
                document.core.open_status.is_some() || document.core.preview.status.is_some();
            let crop_policy = self.settings.render_options().raw_crop_policy;
            if let Some(session) = self.tool.masks() {
                let options = PreviewOptions {
                    render: self.settings.render_options(),
                    ..PreviewOptions::default()
                };
                masks::refresh_tint(
                    context,
                    document,
                    session,
                    crop_policy,
                    &self.coordinator,
                    options,
                );
            }
            let masks = self.tool.masks().and_then(|session| {
                masks::overlay_model(
                    session,
                    &document.core.edits,
                    document.core.frame.as_deref(),
                    crop_policy,
                    document.mask_overlay.as_ref(),
                )
            });
            viewport::show(
                root,
                ViewportModel {
                    has_document: true,
                    preparing,
                    texture: document.texture.as_ref(),
                    clipping: document
                        .clipping
                        .texture_for(document.texture.as_ref())
                        .filter(|_| self.show_clipping),
                    source: document.core.preview.source,
                    picker_mode: self.tool.picker(),
                    crop: crop_overlay,
                    masks,
                },
                &mut document.core.preview.view,
            )
        } else {
            let mut empty_view = ViewState::default();
            viewport::show(
                root,
                ViewportModel {
                    has_document: false,
                    preparing: false,
                    texture: None,
                    clipping: None,
                    source: None,
                    picker_mode: None,
                    crop: None,
                    masks: None,
                },
                &mut empty_view,
            )
        };
        if let Some((mode, normalized)) = output.picker_sample {
            self.pick(context, mode, normalized);
        }
        if let Some((handle, point)) = output.crop.drag_started
            && let Some(session) = self.tool.crop_mut()
        {
            session.begin_drag(handle, point);
        }
        if let Some(point) = output.crop.dragged_to
            && let Some(session) = self.tool.crop_mut()
        {
            session.drag_to(point);
        }
        if output.crop.drag_stopped
            && let Some(session) = self.tool.crop_mut()
        {
            session.finish_drag();
        }
        self.apply_mask_pointer(context, &output.masks);
    }

    fn pick(&mut self, context: &egui::Context, picker: Picker, position: egui::Pos2) {
        self.tool.set_picker(None);
        let options = self.preview_options();
        if let Some(document) = self.document.as_mut() {
            let position = (f64::from(position.x), f64::from(position.y));
            document
                .core
                .pick(&self.coordinator, picker, position, options);
            context.request_repaint();
        }
    }

    fn is_picking(&self) -> bool {
        self.document
            .as_ref()
            .is_some_and(|document| document.core.is_picking())
    }

    fn cancel_pick(&mut self) {
        if let Some(document) = self.document.as_mut() {
            document.core.cancel_pick();
        }
    }

    fn refresh_recipe_save_status(&mut self, context: &egui::Context) {
        let now = Instant::now();
        let Some(deadline) = self
            .recipe_save_status
            .as_ref()
            .and_then(RecipeSaveStatus::notice_deadline)
        else {
            return;
        };
        if now >= deadline {
            self.recipe_save_status = None;
        } else {
            context.request_repaint_after(deadline.saturating_duration_since(now));
        }
    }

    fn show_status_bar(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        self.refresh_recipe_save_status(context);
        let mut activities = Vec::new();
        let mut busy = false;
        if let Some(note) = self.preview_runtime.note() {
            activities.push(note.to_owned());
        }
        if let Some(document) = &self.document {
            if let Some(error) = &document.core.error {
                activities.push(format!("Error: {error}"));
            }
            if let Some(warning) = &document.core.warning {
                activities.push(format!("Warning: {warning}"));
            }
            if let Some(notice) = &document.core.notice {
                activities.push(notice.clone());
            }
            if let Some(status) = &document.core.open_status {
                activities.push(status.clone());
                busy = true;
            } else if let Some((revision, status)) = &document.core.preview.status {
                activities.push(format!("{status} · revision {revision}"));
                busy = true;
            }
            if let Some(export) = &document.core.export_status {
                activities.push(format!(
                    "{} · recipe revision {}",
                    export.detail, export.recipe_revision
                ));
                busy = true;
            }
        }
        let activity_busy = busy;
        busy |= self.catalog.pending_count() > 0;
        let save_pending = self
            .recipe_save_status
            .as_ref()
            .is_some_and(RecipeSaveStatus::is_pending)
            && self.recipe_save_error.is_none();
        busy |= save_pending;
        let save_status = self
            .recipe_save_error
            .as_ref()
            .map(|error| format!("Save failed: {error}"))
            .or_else(|| {
                self.recipe_save_status
                    .as_ref()
                    .map(|status| status.message().to_owned())
            });
        toolbar::show_status(
            root,
            &StatusBarModel {
                processor: self.processor_description(),
                ui_renderer: format!("{} UI", self.ui_renderer),
                activity: (!activities.is_empty()).then(|| activities.join("  ·  ")),
                activity_busy,
                busy,
                save_status,
                save_pending,
                preview_dimensions: self
                    .document
                    .as_ref()
                    .and_then(|document| document.texture.as_ref())
                    .map(PreviewTexture::dimensions),
                preview_milliseconds: self
                    .document
                    .as_ref()
                    .and_then(|document| document.core.preview.last_time)
                    .map(|duration| duration.as_secs_f64() * 1_000.0),
                startup_error: self.startup_error.clone(),
            },
        );
    }

    /// Library-mode central panel: lazy textures, model, grid, and actions.
    fn show_library(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        self.library.sync(&self.catalog);
        let opened_by_keyboard = self.handle_library_keyboard(context);
        self.fill_library_textures(context, self.library.visible);
        let model = self.library_model();
        let output = catalog::show(root, &model);
        self.library.visible = (output.visible_range.start, output.visible_range.end);
        self.library.columns = output.columns.max(1);
        if let Some(sort) = output.sort_changed {
            self.library.sort = sort;
        }
        if let Some(index) = output.selected_entry {
            self.library.selection = Some(index);
        }
        if let Some(index) = opened_by_keyboard.or(output.open_entry) {
            self.open_catalog_entry(context, index);
        }
    }

    /// Open a catalog entry for editing, unless it is already open.
    fn open_catalog_entry(&mut self, context: &egui::Context, grid_index: usize) {
        let Some(catalog_index) = self.library.catalog_index(grid_index) else {
            return;
        };
        let Some(path) = self
            .catalog
            .entry_path(catalog_index)
            .map(Path::to_path_buf)
        else {
            return;
        };
        if self
            .document
            .as_ref()
            .is_some_and(|document| document.core.path == path)
        {
            return;
        }
        self.view_mode = ViewMode::Develop;
        self.open_path(context, path);
    }

    /// Apply library keyboard navigation; returns the entry to open on Enter.
    fn handle_library_keyboard(&mut self, context: &egui::Context) -> Option<usize> {
        let count = self.catalog.entry_count();
        if count == 0 {
            return None;
        }
        let key = context.input(|input| {
            if input.key_pressed(egui::Key::ArrowLeft) {
                Some(LibraryNavigation::Left)
            } else if input.key_pressed(egui::Key::ArrowRight) {
                Some(LibraryNavigation::Right)
            } else if input.key_pressed(egui::Key::ArrowUp) {
                Some(LibraryNavigation::Up)
            } else if input.key_pressed(egui::Key::ArrowDown) {
                Some(LibraryNavigation::Down)
            } else if input.key_pressed(egui::Key::Home) {
                Some(LibraryNavigation::Home)
            } else if input.key_pressed(egui::Key::End) {
                Some(LibraryNavigation::End)
            } else {
                None
            }
        });
        if let Some(key) = key {
            self.library.navigate(count, key);
            None
        } else if context.input(|input| input.key_pressed(egui::Key::Enter)) {
            self.library.selection
        } else {
            None
        }
    }

    /// Decode grid textures around `range` (the engine's budget and cache
    /// policy) and upload them to egui.
    fn fill_library_textures(&mut self, context: &egui::Context, range: (usize, usize)) {
        self.library
            .fill_textures(&self.catalog, range, |path, decoded| {
                context.load_texture(
                    path.to_string_lossy().into_owned(),
                    egui::ColorImage::from_rgb([decoded.width, decoded.height], &decoded.rgb),
                    egui::TextureOptions::LINEAR,
                )
            });
    }

    fn library_model(&mut self) -> LibraryModel {
        let selection = self.library.selection;
        let scroll_to_selection = std::mem::take(&mut self.library.scroll_to_selection);
        let entries = self
            .library
            .order()
            .iter()
            .filter_map(|&catalog_index| self.library_entry_model(catalog_index))
            .collect();
        LibraryModel {
            folder_name: self.catalog.folder_name(),
            folder_path: self
                .catalog
                .folder()
                .map(|folder| folder.to_string_lossy().into_owned()),
            entries,
            selection,
            scroll_to_selection,
            sort: self.library.sort,
            failure: self.catalog.failure().map(str::to_owned),
            scanning: self.catalog.scanning(),
            thumbnail_progress: self
                .catalog
                .folder()
                .map(|_| (self.catalog.resolved_count(), self.catalog.entry_count())),
        }
    }

    fn library_entry_model(&self, catalog_index: usize) -> Option<LibraryEntryModel> {
        let name = self.catalog.entry_name(catalog_index)?.to_owned();
        let state = match self.catalog.slot(catalog_index) {
            Some(ThumbnailSlot::Ready(_)) => LibraryEntryState::Ready,
            Some(ThumbnailSlot::Placeholder(_)) => LibraryEntryState::Placeholder,
            Some(ThumbnailSlot::Failed(_)) => LibraryEntryState::Failed,
            _ => LibraryEntryState::Pending,
        };
        let texture = self
            .catalog
            .entry_path(catalog_index)
            .and_then(|path| self.library.texture(path))
            .cloned();
        Some(LibraryEntryModel {
            name,
            state,
            texture,
        })
    }

    /// Develop-mode filmstrip below the viewport; clicking a thumbnail opens
    /// that photo, and PageUp/PageDown walk through the folder.
    fn show_filmstrip(&mut self, root: &mut egui::Ui) {
        let context = &root.ctx().clone();
        if self.catalog.entry_count() == 0 {
            return;
        }
        self.library.refresh_order(&self.catalog);
        let active_catalog_index = self
            .document
            .as_ref()
            .and_then(|document| self.catalog.entry_index_for_path(&document.core.path));
        let active_grid =
            active_catalog_index.and_then(|catalog_index| self.library.grid_index(catalog_index));
        let scroll_to_active = active_grid != self.filmstrip_active;
        self.filmstrip_active = active_grid;

        if let Some(catalog_index) = self.navigate_filmstrip(context, active_catalog_index) {
            self.open_catalog_entry(context, catalog_index);
        }

        self.fill_library_textures(context, self.filmstrip_visible);
        let model = FilmstripModel {
            entries: self
                .library
                .order()
                .iter()
                .filter_map(|&catalog_index| self.library_entry_model(catalog_index))
                .collect(),
            active: active_grid,
            scroll_to_active,
        };
        let output = catalog::show_filmstrip(root, &model);
        self.filmstrip_visible = (output.visible_range.start, output.visible_range.end);
        if let Some(grid_index) = output.open_entry {
            self.open_catalog_entry(context, grid_index);
        }
    }

    /// PageUp/PageDown move to the previous/next photo in display order; the
    /// result is a grid position.
    fn navigate_filmstrip(
        &mut self,
        context: &egui::Context,
        active_catalog_index: Option<usize>,
    ) -> Option<usize> {
        let forward = context.input(|input| {
            if input.key_pressed(egui::Key::PageDown) {
                Some(true)
            } else if input.key_pressed(egui::Key::PageUp) {
                Some(false)
            } else {
                None
            }
        })?;
        self.library
            .adjacent_grid_index(active_catalog_index, forward)
    }

    fn background_work_is_active(&self) -> bool {
        self.document.as_ref().is_some_and(|document| {
            document.core.open_status.is_some()
                || document.core.preview.status.is_some()
                || document.core.export_status.is_some()
                || document.core.preview.has_pending_histogram()
        }) || self.is_picking()
            || self.catalog.scanning()
            || self.catalog.pending_count() > 0
            || self.library.decode_pending()
            || (!self.queued_recipe_snapshots.is_empty() && self.recipe_save_error.is_none())
    }

    fn show_developer_diagnostics(&mut self, context: &egui::Context) {
        if !self.show_diagnostics {
            return;
        }
        let model = self.diagnostics_model();
        let mut open = self.show_diagnostics;
        let output = diagnostics::show(context, &mut open, &model);
        self.show_diagnostics = open;
        if output.export_requested {
            self.request_diagnostics_export(&model);
        }
    }

    fn diagnostics_model(&self) -> DiagnosticsModel {
        let queue = self.coordinator.preview_queue_stats();
        let gpu_device = self.preview_runtime.gpu().map(|runtime| {
            let capabilities = runtime.processor().capabilities();
            format!(
                "{} · {} · timestamp queries: {}",
                capabilities.adapter_name, capabilities.backend, capabilities.timestamp_queries
            )
        });
        let preview = self
            .document
            .as_ref()
            .and_then(|document| document.core.preview.diagnostics)
            .and_then(|preview| {
                let document = self.document.as_ref()?;
                Some(PreviewModel {
                    backend: if preview.worker.backend == PreviewBackend::GpuBase {
                        if preview.worker.sensor_gpu {
                            "GPU sensor/spatial/color"
                        } else {
                            "CPU sensor / GPU spatial/color"
                        }
                    } else {
                        preview.worker.backend.label()
                    }
                    .to_owned(),
                    algorithm: preview.worker.algorithm.stable_name().to_owned(),
                    profile: camera_profile_diagnostic(document),
                    capture_sharpening: preview
                        .worker
                        .detail
                        .and_then(|p| p.capture_sharpening.map(|s| (p, s)))
                        .map_or_else(
                            || "Off".to_owned(),
                            |(p, s)| {
                                format!(
                                    "RL v{}, amount {:.2}, sigma {:.2} px, noise protection {:.2}",
                                    p.capture_sharpening_version,
                                    s.amount,
                                    s.radius,
                                    s.noise_protection
                                )
                            },
                        ),
                    false_color: preview
                        .worker
                        .detail
                        .and_then(|p| p.false_color.map(|f| (p, f)))
                        .map_or_else(
                            || "Off".to_owned(),
                            |(p, f)| format!("v{}, {} steps", p.false_color_version, f.steps),
                        ),
                    noise_reduction: preview
                        .worker
                        .detail
                        .and_then(|p| p.noise_reduction.map(|n| (p, n)))
                        .map_or_else(
                            || "Off".to_owned(),
                            |(p, n)| {
                                format!(
                                    "v{}, chroma {:.2}, luminance {:.2} (detail {:.2})",
                                    p.noise_reduction_version,
                                    n.chroma,
                                    n.luminance,
                                    n.luminance_detail
                                )
                            },
                        ),
                    rendering: rendering_profile_diagnostic(
                        document.core.edits.recipe().rendering.profile,
                    ),
                    output_policy: output_policy_diagnostic(
                        self.settings.render_options().output_policy,
                    ),
                    output_gamut_statistics: preview
                        .worker
                        .output_gamut_diagnostics
                        .map(output_gamut_statistics),
                    source_state: match preview.worker.resolution {
                        PreviewResolution::SourceScale => "1:1",
                        PreviewResolution::CropToolFullFrame => "crop authoring",
                        PreviewResolution::Fit => match preview.worker.algorithm {
                            DemosaicAlgorithm::Bilinear => "fast",
                            DemosaicAlgorithm::MalvarHeCutler => "high-quality",
                            DemosaicAlgorithm::Rcd => "high-quality",
                            DemosaicAlgorithm::Amaze => "high-quality",
                            DemosaicAlgorithm::Neural => "neural",
                        },
                    }
                    .to_owned(),
                    cache: CacheModel {
                        decoded: preview.worker.cache_hits.decoded,
                        reconstructed: preview.worker.cache_hits.reconstructed,
                        demosaiced: preview.worker.cache_hits.demosaiced,
                        adjusted: preview.worker.cache_hits.adjusted,
                        workspace_reused: preview.worker.workspace_reused,
                    },
                    timings: TimingModel {
                        metadata: preview.worker.timings.metadata,
                        normalization: preview.worker.timings.normalization,
                        highlight_processing: preview.worker.timings.highlight_processing,
                        demosaic: preview.worker.timings.demosaic,
                        false_color: preview.worker.timings.false_color,
                        chroma_noise_reduction: preview.worker.timings.chroma_noise_reduction,
                        luminance_noise_reduction: preview.worker.timings.luminance_noise_reduction,
                        capture_sharpening: preview.worker.timings.capture_sharpening,
                        optics: preview.worker.timings.optics,
                        defringe: preview.worker.timings.defringe,
                        resampling: preview.worker.timings.resampling,
                        color_conversion: preview.worker.timings.color_conversion,
                        adjustments: preview.worker.timings.adjustments,
                        output_conversion: preview.worker.timings.output_conversion,
                        total: preview.worker.timings.total,
                    },
                    highlight: match preview.worker.highlight_diagnostics {
                        HighlightDiagnostics::Off => HighlightStatsModel::Off,
                        HighlightDiagnostics::Clip(stats) => HighlightStatsModel::Clip {
                            affected_sites: stats.affected_sites,
                            changed_sites: stats.changed_sites,
                            nominal_over_white_sites: stats.nominal_over_white_sites,
                            affected_by_channel: stats.affected_by_channel,
                        },
                        HighlightDiagnostics::Opposed(stats) => HighlightStatsModel::Opposed {
                            suspected_clipped_sites: stats.suspected_clipped_sites,
                            reconstructed_sites: stats.reconstructed_sites,
                            changed_sites: stats.changed_sites,
                            fallback_sites: stats.fallback_sites,
                            fully_unsupported_sites: stats.fully_unsupported_sites,
                            neutralised_sites: stats.neutralised_sites,
                            suspected_by_channel: stats.suspected_by_channel,
                        },
                    },
                    cache_resident_bytes: preview.worker.cache_resident_bytes,
                    estimated_peak_bytes: preview.worker.memory.estimated_peak_bytes,
                    gpu: preview.gpu_submission.map(|submission| GpuModel {
                        upload_preparation: preview.gpu_upload_preparation,
                        submission: Some(submission),
                        queue_completion: preview.gpu_queue_completion,
                        histogram_readback: preview.gpu_histogram_readback,
                        textures_reused: preview.gpu_textures_reused,
                        resident_bytes: preview.gpu_resident_bytes,
                    }),
                })
            });
        let catalog =
            (self.catalog.entry_count() > 0 || self.catalog.failure().is_some()).then(|| {
                CatalogModel {
                    photos: self.catalog.entry_count(),
                    thumbnails_ready: self.catalog.ready_count(),
                    thumbnails_placeholder: self.catalog.placeholder_count(),
                    thumbnails_pending: self.catalog.pending_count(),
                    thumbnails_failed: self.catalog.failure_count(),
                    resident_bytes: self.catalog.resident_thumbnail_bytes(),
                }
            });
        DiagnosticsModel {
            processor: self.processor_description(),
            ui_renderer: self.ui_renderer.to_owned(),
            gpu_device,
            queue: QueueModel {
                requested: queue.requested,
                coalesced: queue.coalesced,
                cancellation_requests: queue.cancellation_requests,
                cancelled: queue.cancelled,
                completed: queue.completed,
                failed: queue.failed,
                active: queue.active,
                pending: queue.pending,
            },
            catalog,
            preview,
            messages: DiagnosticsMessages {
                processor_note: self.preview_runtime.note().map(str::to_owned),
                startup_error: self.startup_error.clone(),
                warning: self
                    .document
                    .as_ref()
                    .and_then(|document| document.core.warning.clone()),
                error: self
                    .document
                    .as_ref()
                    .and_then(|document| document.core.error.clone()),
            },
        }
    }

    fn request_diagnostics_export(&mut self, model: &DiagnosticsModel) {
        let Some(mut destination) = rfd::FileDialog::new()
            .set_title("Save Rohditor diagnostics")
            .set_file_name("rohditor-diagnostics.json")
            .add_filter("JSON report", &["json"])
            .save_file()
        else {
            return;
        };
        if destination.extension().is_none() {
            destination.set_extension("json");
        }

        let report = diagnostics::report(model);
        let result = File::options()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map(BufWriter::new)
            .map_err(|error| {
                format!(
                    "Could not create diagnostics report {}: {error}",
                    destination.display()
                )
            })
            .and_then(|mut writer| {
                serde_json::to_writer_pretty(&mut writer, &report).map_err(|error| {
                    format!(
                        "Could not encode diagnostics report {}: {error}",
                        destination.display()
                    )
                })?;
                writer.flush().map_err(|error| {
                    format!(
                        "Could not finish diagnostics report {}: {error}",
                        destination.display()
                    )
                })
            });
        match result {
            Ok(()) => {
                if let Some(document) = self.document.as_mut() {
                    document.core.notice = Some(format!(
                        "Saved support-safe diagnostics report to {}.",
                        destination.display()
                    ));
                }
            }
            Err(error) => {
                if let Some(document) = self.document.as_mut() {
                    document.core.error = Some(error);
                } else {
                    self.startup_error = Some(error);
                }
            }
        }
    }
}

fn camera_profile_diagnostic(document: &Document) -> String {
    let Some(info) = document.core.info.as_ref() else {
        return "unavailable (RAW metadata pending)".to_owned();
    };
    let calibration = CameraCalibration::from_raw_info(info);
    resolve_camera_colour(
        &calibration,
        &document.core.edits.recipe().color.camera_profile,
        document.core.edits.recipe().color.white_balance,
    )
    .map(|resolved| resolved.provenance.to_string())
    .unwrap_or_else(|error| format!("unavailable ({error})"))
}

fn rendering_profile_diagnostic(profile: rohditor_edit::RenderingProfileSelection) -> String {
    match profile.process_version() {
        Some(version) => format!("{} v{version}", profile.display_name()),
        None => profile.display_name().to_owned(),
    }
}

fn output_policy_diagnostic(policy: OutputPolicy) -> String {
    match policy.algorithm_version() {
        Some(version) => format!("{} v{version}", policy.display_name()),
        None => policy.display_name().to_owned(),
    }
}

fn output_gamut_statistics(diagnostics: rohditor_core::GamutMappingDiagnostics) -> String {
    format!(
        "{} in gamut, {} compressed, {} limited, {} clipped fallback, {} invalid",
        diagnostics.in_gamut_pixels,
        diagnostics.compressed_pixels,
        diagnostics.limited_pixels,
        diagnostics.clipped_fallback_pixels,
        diagnostics.invalid_pixels,
    )
}

fn source_scale_selected(document: Option<&Document>) -> bool {
    document.is_some_and(|document| document.core.preview.source_scale_requested)
}

fn document_panel_model(
    document: &Document,
    picker_mode: Option<Picker>,
    color_mixer_channel: usize,
    camera_profiles: &CameraProfileRegistry,
) -> DocumentPanelModel {
    let camera_profile = document.core.edits.recipe().color.camera_profile.clone();
    let mut camera_profile_choices = vec![CameraProfileChoice {
        selection: rohditor_edit::CameraProfileSelection::Automatic,
        label: "Automatic (RAW / decoder matrix)".to_owned(),
        detail: document.core.info.as_ref().map_or_else(
            || "Decoder matrix selected when RAW metadata is available".to_owned(),
            automatic_profile_detail,
        ),
        tooltip: "Use the existing decoder-derived camera calibration".to_owned(),
    }];
    let profiles = camera_profiles.choices(document.core.info.as_ref(), &camera_profile);
    camera_profile_choices.extend(profiles.into_iter().map(camera_profile_choice));
    let as_shot_coordinates =
        document
            .core
            .as_shot_coordinates()
            .unwrap_or(WhiteBalanceCoordinates {
                temperature: TEMPERATURE_RANGE.neutral,
                tint: TINT_RANGE.neutral,
            });
    let (
        white_balance_mode,
        white_balance_red,
        white_balance_green,
        white_balance_blue,
        white_balance_temperature,
        white_balance_tint,
    ) = match document.core.edits.recipe().color.white_balance {
        WhiteBalance::AsShot => (
            WhiteBalanceMode::AsShot,
            WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            as_shot_coordinates.temperature,
            as_shot_coordinates.tint,
        ),
        WhiteBalance::ManualMultipliers { red, green, blue } => (
            WhiteBalanceMode::ManualMultipliers,
            red,
            green,
            blue,
            as_shot_coordinates.temperature,
            as_shot_coordinates.tint,
        ),
        WhiteBalance::TemperatureTint { temperature, tint } => (
            WhiteBalanceMode::TemperatureTint,
            WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            temperature,
            tint,
        ),
    };
    // Built once per frame and shared by everything that compares to defaults.
    let defaults = document.core.default_recipe();
    DocumentPanelModel {
        file_name: document.core.file_name(),
        camera: document.core.info.as_ref().map(RawFileInfo::camera_label),
        sensor_dimensions: document
            .core
            .info
            .as_ref()
            .map(|info| (info.width, info.height)),
        info: document.core.info.as_ref().map(info::info_panel_model),
        revision: document.core.edits.revision(),
        dirty: document.core.is_dirty(),
        has_adjustments: document.core.differs_from_defaults(&defaults),
        camera_profile,
        camera_profile_choices,
        rendering_profile: document.core.edits.recipe().rendering.profile,
        values: AdjustmentValues {
            white_balance_mode,
            white_balance_red,
            white_balance_green,
            white_balance_blue,
            white_balance_temperature,
            white_balance_tint,
            exposure: document.core.edits.recipe().light.exposure_ev,
            contrast: document.core.edits.recipe().light.contrast,
            highlights: document.core.edits.recipe().light.highlights,
            shadows: document.core.edits.recipe().light.shadows,
            whites: document.core.edits.recipe().light.whites,
            blacks: document.core.edits.recipe().light.blacks,
            highlight_method: document.core.edits.recipe().raw.highlights.method,
            highlight_threshold: match document.core.edits.recipe().raw.highlights.method {
                HighlightMethod::Clip => document.core.edits.recipe().raw.highlights.clip.threshold,
                HighlightMethod::Opposed => {
                    document
                        .core
                        .edits
                        .recipe()
                        .raw
                        .highlights
                        .opposed
                        .detection_threshold
                }
                HighlightMethod::Off => document.core.edits.recipe().raw.highlights.clip.threshold,
            },
            tone_curve_shadows: document.core.edits.recipe().light.tone_curve.shadows,
            tone_curve_darks: document.core.edits.recipe().light.tone_curve.darks,
            tone_curve_lights: document.core.edits.recipe().light.tone_curve.lights,
            tone_curve_highlights: document.core.edits.recipe().light.tone_curve.highlights,
            saturation: document.core.edits.recipe().color.saturation,
            vibrance: document.core.edits.recipe().color.vibrance,
            hsl: document
                .core
                .edits
                .recipe()
                .color
                .hsl
                .channels
                .map(|channel| [channel.hue, channel.saturation, channel.luminance]),
            grading: [
                document.core.edits.recipe().color.grading.shadows,
                document.core.edits.recipe().color.grading.midtones,
                document.core.edits.recipe().color.grading.highlights,
            ],
        },
        ranges: AdjustmentRanges {
            white_balance: AdjustmentRange {
                minimum: WHITE_BALANCE_MULTIPLIER_RANGE.minimum,
                maximum: WHITE_BALANCE_MULTIPLIER_RANGE.maximum,
                neutral: WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            },
            temperature: AdjustmentRange {
                minimum: TEMPERATURE_RANGE.minimum,
                maximum: TEMPERATURE_RANGE.maximum,
                neutral: as_shot_coordinates.temperature,
            },
            tint: AdjustmentRange {
                minimum: TINT_RANGE.minimum,
                maximum: TINT_RANGE.maximum,
                neutral: as_shot_coordinates.tint,
            },
            exposure: AdjustmentRange {
                minimum: EXPOSURE_EV_RANGE.minimum,
                maximum: EXPOSURE_EV_RANGE.maximum,
                neutral: EXPOSURE_EV_RANGE.neutral,
            },
            contrast: AdjustmentRange {
                minimum: CONTRAST_RANGE.minimum,
                maximum: CONTRAST_RANGE.maximum,
                neutral: CONTRAST_RANGE.neutral,
            },
            highlights: AdjustmentRange {
                minimum: HIGHLIGHTS_RANGE.minimum,
                maximum: HIGHLIGHTS_RANGE.maximum,
                neutral: HIGHLIGHTS_RANGE.neutral,
            },
            shadows: AdjustmentRange {
                minimum: SHADOWS_RANGE.minimum,
                maximum: SHADOWS_RANGE.maximum,
                neutral: SHADOWS_RANGE.neutral,
            },
            whites: AdjustmentRange {
                minimum: WHITES_RANGE.minimum,
                maximum: WHITES_RANGE.maximum,
                neutral: WHITES_RANGE.neutral,
            },
            blacks: AdjustmentRange {
                minimum: BLACKS_RANGE.minimum,
                maximum: BLACKS_RANGE.maximum,
                neutral: BLACKS_RANGE.neutral,
            },
            highlight_threshold: AdjustmentRange {
                minimum: HIGHLIGHT_THRESHOLD_RANGE.minimum,
                maximum: HIGHLIGHT_THRESHOLD_RANGE.maximum,
                neutral: HIGHLIGHT_THRESHOLD_RANGE.neutral,
            },
            tone_curve: AdjustmentRange {
                minimum: TONE_CURVE_RANGE.minimum,
                maximum: TONE_CURVE_RANGE.maximum,
                neutral: TONE_CURVE_RANGE.neutral,
            },
            saturation: AdjustmentRange {
                minimum: SATURATION_RANGE.minimum,
                maximum: SATURATION_RANGE.maximum,
                neutral: SATURATION_RANGE.neutral,
            },
            vibrance: AdjustmentRange {
                minimum: VIBRANCE_RANGE.minimum,
                maximum: VIBRANCE_RANGE.maximum,
                neutral: VIBRANCE_RANGE.neutral,
            },
            hsl_hue: AdjustmentRange {
                minimum: HSL_HUE_RANGE.minimum,
                maximum: HSL_HUE_RANGE.maximum,
                neutral: HSL_HUE_RANGE.neutral,
            },
            hsl_saturation: AdjustmentRange {
                minimum: HSL_SATURATION_RANGE.minimum,
                maximum: HSL_SATURATION_RANGE.maximum,
                neutral: HSL_SATURATION_RANGE.neutral,
            },
            hsl_luminance: AdjustmentRange {
                minimum: HSL_LUMINANCE_RANGE.minimum,
                maximum: HSL_LUMINANCE_RANGE.maximum,
                neutral: HSL_LUMINANCE_RANGE.neutral,
            },
            grading: AdjustmentRange {
                minimum: COLOR_GRADING_RANGE.minimum,
                maximum: COLOR_GRADING_RANGE.maximum,
                neutral: COLOR_GRADING_RANGE.neutral,
            },
        },
        auto_tone_available: document.core.preview.histogram_revision
            == Some(document.core.edits.revision()),
        picker_mode,
        color_mixer_channel,
        optics: optics_panel_model(document),
        false_color: document.core.edits.recipe().false_color,
        noise_reduction: document.core.edits.recipe().noise_reduction,
        capture_sharpening: document.core.edits.recipe().capture_sharpening,
        defringe: document.core.edits.recipe().defringe,
        presence: document.core.edits.recipe().presence,
        noise_suggestion: document.core.noise_suggestion(),
        changed_sections: rohditor_edit::changed_sections(document.core.edits.recipe(), &defaults),
    }
}

fn optics_panel_model(document: &Document) -> OpticsPanelModel {
    let (camera, lens, focal_length, aperture, focus_distance) = document
        .core
        .info
        .as_ref()
        .map_or((None, None, None, None, None), |info| {
            (
                Some(info.camera_label()),
                info.capture.lens_label(),
                info.capture.focal_length_label(),
                info.capture.aperture_label(),
                info.capture.focus_distance_label(),
            )
        });
    let (applied, used_infinity_distance_fallback, scale) = document
        .core
        .preview
        .diagnostics
        .map_or((None, false, None), |diagnostics| {
            (
                diagnostics.worker.optics_applied,
                diagnostics.worker.optics_used_infinity_distance_fallback,
                diagnostics.worker.optics_scale,
            )
        });
    OpticsPanelModel {
        profile: document.core.edits.recipe().optics.profile.clone(),
        distortion: document.core.edits.recipe().optics.distortion,
        vignetting: document.core.edits.recipe().optics.vignetting,
        chromatic_aberration: document.core.edits.recipe().optics.chromatic_aberration,
        camera,
        lens,
        focal_length,
        aperture,
        focus_distance,
        match_result: document.core.optics_match.clone(),
        candidates: document.core.optics_candidates.clone(),
        profile_filter: document.core.optics_profile_filter.clone(),
        database: document.core.optics_database.clone(),
        error: document.core.optics_error.clone(),
        applied,
        used_infinity_distance_fallback,
        scale,
    }
}

fn automatic_profile_detail(info: &RawFileInfo) -> String {
    let transform = camera_color_transform(info);
    let origin = info
        .color_matrices
        .iter()
        .filter_map(|matrix| {
            let normalized = matrix
                .illuminant
                .to_ascii_uppercase()
                .replace([' ', '_', '-'], "");
            let priority = match normalized.as_str() {
                "D65" => 0,
                "D50" => 1,
                "A" | "STANDARDLIGHTA" => 2,
                _ => return None,
            };
            Some((priority, matrix.origin))
        })
        .min_by_key(|(priority, _)| *priority)
        .map_or(
            rohditor_raw::CameraMatrixOrigin::LegacyDecoderFallback,
            |(_, origin)| origin,
        );
    match transform {
        Ok(transform) => format!("{origin:?} · {}", transform.source_illuminant),
        Err(error) => format!("{origin:?} · unavailable ({error})"),
    }
}

fn camera_profile_choice(
    profile: rohditor_camera_profile::MatrixCameraProfile,
) -> CameraProfileChoice {
    let illuminant = profile.preferred_calibration().map_or_else(
        || "unknown illuminant".to_owned(),
        |value| value.illuminant.to_string(),
    );
    let digest = profile.source_sha256.chars().take(8).collect::<String>();
    CameraProfileChoice {
        label: profile.name.clone(),
        detail: format!("Matrix DCP · {illuminant}"),
        tooltip: format!(
            "Camera model: {} · {illuminant} · SHA-256 {digest}",
            profile.camera_model
        ),
        selection: rohditor_edit::CameraProfileSelection::Matrix(profile),
    }
}

fn set_highlight_method(edits: &mut EditSession, method: HighlightMethod) -> bool {
    let mut next = edits.recipe().clone();
    next.raw.highlights.method = method;
    edits.set_discrete(next)
}

#[cfg(test)]
fn gpu_base_highlights_match(
    retained: HighlightAdjustments,
    requested: HighlightAdjustments,
) -> bool {
    if retained.method != requested.method {
        return false;
    }
    match requested.method {
        HighlightMethod::Off => true,
        HighlightMethod::Clip => {
            retained.clip.threshold.to_bits() == requested.clip.threshold.to_bits()
        }
        HighlightMethod::Opposed => {
            retained.opposed.detection_threshold.to_bits()
                == requested.opposed.detection_threshold.to_bits()
        }
    }
}

/// `recipe` with one of the manual white-balance multipliers at `value`.
fn with_multiplier(recipe: &EditRecipe, channel: usize, value: f32) -> EditRecipe {
    let mut multipliers = match recipe.color.white_balance {
        WhiteBalance::AsShot | WhiteBalance::TemperatureTint { .. } => {
            [WHITE_BALANCE_MULTIPLIER_RANGE.neutral; 3]
        }
        WhiteBalance::ManualMultipliers { red, green, blue } => [red, green, blue],
    };
    multipliers[channel] = value;
    let [red, green, blue] = multipliers;
    let mut next = recipe.clone();
    next.color.white_balance = WhiteBalance::ManualMultipliers { red, green, blue };
    next
}

fn apply_adjustment_interaction(
    edits: &mut EditSession,
    interaction: AdjustmentInteraction,
    defaults: &EditRecipe,
) -> bool {
    if interaction.drag_started {
        edits.begin_gesture();
    }

    let next = match interaction.target {
        AdjustmentTarget::Parameter(parameter) => {
            parameter.with_value(edits.recipe(), defaults, interaction.value)
        }
        AdjustmentTarget::WhiteBalanceRed => with_multiplier(edits.recipe(), 0, interaction.value),
        AdjustmentTarget::WhiteBalanceGreen => {
            with_multiplier(edits.recipe(), 1, interaction.value)
        }
        AdjustmentTarget::WhiteBalanceBlue => with_multiplier(edits.recipe(), 2, interaction.value),
    };

    let changed = if interaction.reset {
        edits.set_discrete(next)
    } else if interaction.changed {
        if interaction.dragged || interaction.drag_stopped || edits.gesture_active() {
            edits.set_during_gesture(next)
        } else {
            edits.set_discrete(next)
        }
    } else {
        false
    };
    if interaction.drag_stopped {
        edits.finish_gesture();
    }
    changed
}

impl eframe::App for RohditorApp {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let context = &root.ctx().clone();
        self.process_worker_events(context);
        self.sync_camera_look(context);
        self.process_recipe_save_events();
        self.update_catalog();
        self.library.begin_frame();
        self.refresh_gpu_queue_completion(context);
        self.refresh_gpu_histogram(context);
        self.show_top_bar(root);
        self.show_status_bar(root);
        match self.view_mode {
            ViewMode::Develop => {
                self.show_file_panel(root);
                self.show_tool_rail(root);
                self.show_tool_panel(root);
                self.show_filmstrip(root);
                self.show_viewport(root);
            }
            ViewMode::Library => self.show_library(root),
        }
        self.show_developer_diagnostics(context);
        self.show_application_settings(context);
        self.show_export_dialog(context);
        // Ordering requirement: the autosave beat must run after the widget
        // passes above. Its gesture reap decides from `pointer.any_down()`,
        // which only reflects this frame's events once the widgets have
        // consumed them; run before the panels, it would act on the previous
        // frame's pointer state and could commit a drag the panel pass is
        // still continuing, splitting it across undo steps.
        self.update_autosave(context);
        self.update_window_title(context);
        self.update_screenshot(context);
        // eframe normally wakes the event loop from the worker's
        // `request_repaint` callback. Keep a short polling repaint while work
        // is visible as a fallback for compositor/renderer combinations where
        // that wake-up is delayed (observed with glow on Wayland).
        if self.background_work_is_active() {
            context.request_repaint_after(Duration::from_millis(16));
        }
    }
}

/// Adapts egui's repaint request to the toolkit-neutral worker wake callback.
fn wake_from(context: &egui::Context) -> WakeFn {
    let context = context.clone();
    Arc::new(move || context.request_repaint())
}

fn install_texture(
    context: &egui::Context,
    document: &mut Document,
    image: WorkerImage,
    source: PreviewSource,
) {
    let WorkerImage {
        width,
        height,
        pixels,
    } = image;
    let color = egui::ColorImage::from_rgb([width, height], &pixels);
    document.preview_pixels = Some(DisplayPreviewPixels {
        width,
        height,
        rgb: pixels,
    });
    document.clipping.mark_stale();
    let texture_name = format!("document-{}-preview", document.core.id);
    match document.texture.as_mut() {
        Some(PreviewTexture::Cpu(texture)) => {
            texture.set(color, egui::TextureOptions::LINEAR);
        }
        _ => {
            document.texture = Some(PreviewTexture::Cpu(context.load_texture(
                texture_name,
                color,
                egui::TextureOptions::LINEAR,
            )));
        }
    }
    document.core.preview.source = Some(source);
}

#[cfg(test)]
mod tests {
    use rohditor_core::default_recipe;
    use rohditor_engine::preview::gpu_supports_recipe;

    use super::*;

    #[test]
    fn presentation_slider_events_preserve_one_undo_step_per_drag() {
        let mut edits = EditSession::default();
        for interaction in [
            AdjustmentInteraction {
                target: Parameter::Exposure.into(),
                value: 0.25,
                changed: true,
                drag_started: true,
                dragged: true,
                drag_stopped: false,
                reset: false,
            },
            AdjustmentInteraction {
                target: Parameter::Exposure.into(),
                value: 0.75,
                changed: true,
                drag_started: false,
                dragged: true,
                drag_stopped: false,
                reset: false,
            },
            AdjustmentInteraction {
                target: Parameter::Exposure.into(),
                value: 0.75,
                changed: false,
                drag_started: false,
                dragged: false,
                drag_stopped: true,
                reset: false,
            },
        ] {
            let _ = apply_adjustment_interaction(&mut edits, interaction, &EditRecipe::default());
        }

        assert_eq!(edits.revision(), 2);
        assert_eq!(edits.recipe().light.exposure_ev, 0.75);
        assert!(edits.undo());
        assert_eq!(edits.recipe().light.exposure_ev, EXPOSURE_EV_RANGE.neutral);
        assert!(!edits.undo());
    }

    #[test]
    fn hsl_and_grading_interactions_remain_gpu_supported_through_undo_and_reset() {
        for target in [
            Parameter::HslHue(0),
            Parameter::HslSaturation(3),
            Parameter::HslLuminance(7),
            Parameter::GradingShadows(0),
            Parameter::GradingMidtones(1),
            Parameter::GradingHighlights(2),
        ] {
            let mut edits = EditSession::default();
            let original = edits.recipe().clone();
            for (value, start, stop) in
                [(0.3, true, false), (0.6, false, false), (0.6, false, true)]
            {
                apply_adjustment_interaction(
                    &mut edits,
                    AdjustmentInteraction {
                        target: target.into(),
                        value,
                        changed: !stop,
                        drag_started: start,
                        dragged: !stop,
                        drag_stopped: stop,
                        reset: false,
                    },
                    &EditRecipe::default(),
                );
                assert!(gpu_supports_recipe(edits.recipe()));
            }
            assert_ne!(edits.recipe(), &original);
            assert!(edits.undo());
            assert_eq!(edits.recipe(), &original);
            assert!(gpu_supports_recipe(edits.recipe()));
            assert!(edits.redo());
            assert!(gpu_supports_recipe(edits.recipe()));
            apply_adjustment_interaction(
                &mut edits,
                AdjustmentInteraction {
                    target: target.into(),
                    value: 0.0,
                    changed: true,
                    drag_started: false,
                    dragged: false,
                    drag_stopped: false,
                    reset: true,
                },
                &EditRecipe::default(),
            );
            assert_eq!(edits.recipe(), &original);
            assert!(gpu_supports_recipe(edits.recipe()));
        }
    }

    #[test]
    fn capture_slider_uses_undo_redo_and_reset() {
        let mut edits = EditSession::default();
        for (value, start, stop) in [(0.7, true, false), (0.9, false, false), (0.9, false, true)] {
            apply_adjustment_interaction(
                &mut edits,
                AdjustmentInteraction {
                    target: Parameter::CaptureRadius.into(),
                    value,
                    changed: !stop,
                    drag_started: start,
                    dragged: !stop,
                    drag_stopped: stop,
                    reset: false,
                },
                &EditRecipe::default(),
            );
        }
        assert_eq!(edits.recipe().capture_sharpening.radius, 0.9);
        assert!(edits.undo());
        assert_eq!(edits.recipe().capture_sharpening.radius, 0.6);
        assert!(edits.redo());
        assert_eq!(edits.recipe().capture_sharpening.radius, 0.9);
        apply_adjustment_interaction(
            &mut edits,
            AdjustmentInteraction {
                target: Parameter::CaptureRadius.into(),
                value: 0.6,
                changed: true,
                drag_started: false,
                dragged: false,
                drag_stopped: false,
                reset: true,
            },
            &EditRecipe::default(),
        );
        assert_eq!(edits.recipe().capture_sharpening.radius, 0.6);
    }

    #[test]
    fn white_balance_drag_uses_gpu_drafts_and_commits_once_on_release() {
        let dragging = AdjustmentInteraction {
            target: Parameter::Temperature.into(),
            value: 5_100.0,
            changed: true,
            drag_started: false,
            dragged: true,
            drag_stopped: false,
            reset: false,
        };
        assert_eq!(white_balance_preview_request(dragging, true), (true, false));

        let released = AdjustmentInteraction {
            changed: false,
            dragged: false,
            drag_stopped: true,
            ..dragging
        };
        assert_eq!(
            white_balance_preview_request(released, false),
            (false, true)
        );

        let typed = AdjustmentInteraction {
            changed: true,
            dragged: false,
            drag_stopped: false,
            ..dragging
        };
        assert_eq!(white_balance_preview_request(typed, true), (true, true));
    }

    #[test]
    fn installing_a_cpu_preview_preserves_the_existing_view() {
        let context = egui::Context::default();
        let mut document =
            Document::opening(7, PathBuf::from("fixture.arw"), EditRecipe::default(), None);
        document.core.preview.view.actual_size(0.0);
        let pixels = vec![255, 0, 0, 0, 255, 0];

        install_texture(
            &context,
            &mut document,
            WorkerImage {
                width: 2,
                height: 1,
                pixels,
            },
            PreviewSource::FastCpu,
        );

        assert!(!document.core.preview.view.is_fit());
        assert_eq!(document.core.preview.view.zoom_label(), "SOURCE 100%");
    }

    #[test]
    fn a_fitted_camera_look_is_not_a_user_adjustment() {
        let mut document =
            Document::opening(7, PathBuf::from("fixture.arw"), default_recipe(None), None);
        assert!(!document.core.has_user_adjustments());

        document.core.edits.apply_derived(|recipe| {
            recipe.rendering.camera_tone = Some(rohditor_edit::CameraTone::static_fallback());
        });
        assert!(!document.core.has_user_adjustments());
        assert!(
            !document.core.is_dirty(),
            "opening a photo must not create a sidecar"
        );

        let mut recipe = document.core.edits.recipe().clone();
        recipe.light.exposure_ev = 0.5;
        assert!(document.core.edits.set_discrete(recipe));
        assert!(document.core.has_user_adjustments());
    }

    #[test]
    fn gpu_base_reuse_requires_matching_raw_highlight_semantics() {
        let off = HighlightAdjustments {
            method: HighlightMethod::Off,
            ..HighlightAdjustments::default()
        };
        let mut clip = off;
        clip.method = HighlightMethod::Clip;

        assert!(gpu_base_highlights_match(off, off));
        assert!(!gpu_base_highlights_match(off, clip));
        assert!(!gpu_base_highlights_match(clip, off));
        assert!(gpu_base_highlights_match(clip, clip));

        let mut different_threshold = clip;
        different_threshold.clip.threshold = 1.25;
        assert!(!gpu_base_highlights_match(clip, different_threshold));

        let mut opposed = off;
        opposed.method = HighlightMethod::Opposed;
        assert!(!gpu_base_highlights_match(off, opposed));
        assert!(gpu_base_highlights_match(opposed, opposed));
        let mut different_detection = opposed;
        different_detection.opposed.detection_threshold = 1.25;
        assert!(!gpu_base_highlights_match(opposed, different_detection));
    }

    #[test]
    fn source_scale_toolbar_selection_tracks_resolution_request() {
        let mut document =
            Document::opening(7, PathBuf::from("fixture.arw"), EditRecipe::default(), None);
        document.core.preview.source_scale_requested = true;
        document.core.preview.view.fit(0.0);

        assert!(source_scale_selected(Some(&document)));

        document.core.preview.source_scale_requested = false;
        assert!(!source_scale_selected(Some(&document)));
    }
}
