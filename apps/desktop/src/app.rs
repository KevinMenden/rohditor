use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use rohditor_core::{
    DitherMode, ExportFormat, ExportMetadataPolicy, ExportSettings, Histogram,
    JPEG_QUALITY_DEFAULT, MemoryEstimate, PngBitDepth, PreviewOptions, RenderOptions, StageTimings,
    hsl_channel_weights_from_display_rgb, paths_refer_to_same_file, srgb_to_linear_srgb,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{
    BLACKS_RANGE, COLOR_GRADING_RANGE, CONTRAST_RANGE, EXPOSURE_EV_RANGE, EditRecipe,
    HIGHLIGHTS_RANGE, HSL_CHANNEL_COUNT, HSL_HUE_RANGE, HSL_LUMINANCE_RANGE, HSL_SATURATION_RANGE,
    SATURATION_RANGE, SHADOWS_RANGE, TEMPERATURE_RANGE, TINT_RANGE, TONE_CURVE_RANGE,
    VIBRANCE_RANGE, WHITE_BALANCE_MULTIPLIER_RANGE, WHITES_RANGE, WhiteBalance,
};
use rohditor_raw::{RawFileInfo, RawFrame};
use tracing::{info, warn};

use crate::ProcessorPreference;
use crate::catalog::{CatalogCoordinator, CatalogState, ThumbnailSlot};
use crate::coordinator::{
    PreviewBackend, PreviewResolution, RenderCoordinator, WorkerImage, WorkerPreviewDiagnostics,
};
use crate::document::{EditSession, PreviewIntent, PreviewTicket};
use crate::preview_cache::PreviewCacheHits;
use crate::session;
use crate::ui::adjustment_panel::{
    self, AdjustmentInteraction, AdjustmentRange, AdjustmentRanges, AdjustmentTarget,
    AdjustmentValues, DocumentPanelModel, ExportKind, ExportUiSettings, PngDepth, WhiteBalanceMode,
};
use crate::ui::catalog::{
    self, FilmstripModel, LibraryEntryModel, LibraryEntryState, LibraryModel, LibrarySort,
};
use crate::ui::crop::{CropOverlayModel, CropPanelModel};
use crate::ui::diagnostics::{
    self, CacheModel, CatalogModel, DiagnosticsMessages, DiagnosticsModel, GpuModel, PreviewModel,
    QueueModel, TimingModel,
};
use crate::ui::theme;
use crate::ui::toolbar::{self, FilePanelModel, StatusBarModel, ToolbarModel};
use crate::ui::viewport::{self, PreviewSource, PreviewTexture, ViewState, ViewportModel};
use crate::ui::{PickerMode, ViewMode};
use rohditor_gpu::GpuPreviewUpload;

const GPU_HISTOGRAM_DEBOUNCE: Duration = Duration::from_millis(75);
const AUTO_TONE_EXPOSURE_EPSILON_EV: f32 = 0.01;
/// Longest grid texture edge; thumbnails decode down to this size.
const LIBRARY_TEXTURE_LONG_EDGE: u32 = 256;
/// Maximum resident grid textures before the least recently used are evicted.
const LIBRARY_TEXTURE_CAP: usize = 256;
/// Grid textures decoded per frame, so a folder fill never blocks a frame.
const LIBRARY_DECODE_BUDGET: usize = 8;
/// Extra entries around the visible range kept warm for smooth scrolling.
const LIBRARY_VISIBLE_MARGIN: usize = 24;

#[path = "app/crop.rs"]
pub(crate) mod crop;
#[path = "app/events.rs"]
mod events;
#[path = "app/gpu.rs"]
mod gpu;

use crop::CropToolSession;
use gpu::{
    GpuDocumentPreview, GpuRuntime, PendingGpuHistogram, gpu_output_size, initialize_gpu_runtime,
    register_or_update_gpu_texture,
};

#[derive(Debug, Clone, Copy)]
struct DocumentPreviewDiagnostics {
    worker: WorkerPreviewDiagnostics,
    gpu_upload_preparation: Option<Duration>,
    gpu_submission: Option<Duration>,
    gpu_queue_completion: Option<Duration>,
    gpu_histogram_readback: Option<Duration>,
    gpu_textures_reused: Option<bool>,
    gpu_resident_bytes: usize,
}

impl DocumentPreviewDiagnostics {
    const fn cpu(worker: WorkerPreviewDiagnostics) -> Self {
        Self {
            worker,
            gpu_upload_preparation: None,
            gpu_submission: None,
            gpu_queue_completion: None,
            gpu_histogram_readback: None,
            gpu_textures_reused: None,
            gpu_resident_bytes: 0,
        }
    }
}

#[derive(Debug)]
struct ExportActivity {
    id: u64,
    recipe_revision: u64,
    detail: String,
}

struct Document {
    id: u64,
    path: PathBuf,
    info: Option<RawFileInfo>,
    frame: Option<Arc<RawFrame>>,
    edits: EditSession,
    texture: Option<PreviewTexture>,
    preview_pixels: Option<DisplayPreviewPixels>,
    preview_source: Option<PreviewSource>,
    histogram: Option<Histogram>,
    histogram_revision: Option<u64>,
    pending_gpu_histogram: Option<PendingGpuHistogram>,
    gpu_histogram_due: Option<(PreviewTicket, Instant)>,
    source_scale_requested: bool,
    preview_sequence: u64,
    preview_intent: PreviewIntent,
    gpu_preview: Option<GpuDocumentPreview>,
    view: ViewState,
    open_status: Option<String>,
    preview_status: Option<(u64, String)>,
    export_status: Option<ExportActivity>,
    last_preview_time: Option<Duration>,
    preview_diagnostics: Option<DocumentPreviewDiagnostics>,
    warning: Option<String>,
    error: Option<String>,
    notice: Option<String>,
}

#[derive(Debug, Clone)]
struct DisplayPreviewPixels {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

impl Document {
    fn opening(id: u64, path: PathBuf) -> Self {
        Self {
            id,
            path,
            info: None,
            frame: None,
            edits: EditSession::default(),
            texture: None,
            preview_pixels: None,
            preview_source: None,
            histogram: None,
            histogram_revision: None,
            pending_gpu_histogram: None,
            gpu_histogram_due: None,
            source_scale_requested: false,
            preview_sequence: 0,
            preview_intent: PreviewIntent::CommittedFit,
            gpu_preview: None,
            view: ViewState::default(),
            open_status: Some("Opening RAW file".to_owned()),
            preview_status: None,
            export_status: None,
            last_preview_time: None,
            preview_diagnostics: None,
            warning: None,
            error: None,
            notice: None,
        }
    }

    fn file_name(&self) -> String {
        display_file_name(&self.path)
    }

    fn ticket(&self) -> PreviewTicket {
        PreviewTicket {
            document_id: self.id,
            revision: self.edits.revision(),
            sequence: self.preview_sequence,
        }
    }

    fn begin_preview_request(&mut self, intent: PreviewIntent) -> PreviewTicket {
        self.preview_sequence = self.preview_sequence.saturating_add(1);
        self.preview_intent = intent;
        self.ticket()
    }

    fn accepts_preview(&self, ticket: PreviewTicket, intent: PreviewIntent) -> bool {
        ticket.is_current(self.id, self.edits.revision(), self.preview_sequence)
            && self.preview_intent == intent
    }
}

impl ExportUiSettings {
    fn core(&self) -> ExportSettings {
        ExportSettings {
            format: match self.kind {
                ExportKind::Jpeg => ExportFormat::Jpeg {
                    quality: self.jpeg_quality,
                },
                ExportKind::Png => ExportFormat::Png {
                    bit_depth: match self.png_depth {
                        PngDepth::Eight => PngBitDepth::Eight,
                        PngDepth::Sixteen => PngBitDepth::Sixteen,
                    },
                },
            },
            dithering: if self.dither {
                DitherMode::Ordered8x8
            } else {
                DitherMode::None
            },
            metadata: if self.safe_metadata {
                ExportMetadataPolicy::Safe
            } else {
                ExportMetadataPolicy::None
            },
            overwrite: self.overwrite,
        }
    }

    const fn extension(&self) -> &'static str {
        match self.kind {
            ExportKind::Jpeg => "jpg",
            ExportKind::Png => "png",
        }
    }
}

pub(crate) struct RohditorApp {
    coordinator: RenderCoordinator,
    catalog_coordinator: CatalogCoordinator,
    catalog: CatalogState,
    document: Option<Document>,
    next_document_id: u64,
    next_export_id: u64,
    export_settings: ExportUiSettings,
    render_options: RenderOptions,
    ui_renderer: &'static str,
    processor_preference: ProcessorPreference,
    gpu: Option<GpuRuntime>,
    processor_note: Option<String>,
    startup_error: Option<String>,
    show_diagnostics: bool,
    picker_mode: Option<PickerMode>,
    color_mixer_channel: usize,
    pending_white_balance_pick: Option<PreviewTicket>,
    white_balance_memory: WhiteBalanceModeMemory,
    crop_tool: Option<CropToolSession>,
    view_mode: ViewMode,
    /// Folder of the last applied scan, used to reset library selection when
    /// the catalog switches folders.
    library_folder: Option<PathBuf>,
    library_selection: Option<usize>,
    /// Display order of catalog entries for the current sort mode, mapping
    /// grid positions to catalog entry indices.
    library_order: Vec<usize>,
    library_sort: LibrarySort,
    /// Grid textures keyed by source path with their last-use frame.
    library_textures: HashMap<PathBuf, (egui::TextureHandle, u64)>,
    /// Paths whose thumbnail bytes could not be decoded, so failures are not
    /// retried every frame.
    library_texture_failures: std::collections::HashSet<PathBuf>,
    library_visible: (usize, usize),
    library_columns: usize,
    library_frame: u64,
    library_decode_pending: bool,
    library_scroll_to_selection: bool,
    filmstrip_visible: (usize, usize),
    filmstrip_active: Option<usize>,
}

/// Keep the last values for the two editable WB modes while the user switches
/// through As-shot. This makes the mode selector non-destructive instead of
/// silently resetting a carefully chosen temperature or manual balance.
#[derive(Debug, Clone, Copy)]
struct WhiteBalanceModeMemory {
    temperature: f32,
    tint: f32,
    manual: [f32; 3],
}

impl Default for WhiteBalanceModeMemory {
    fn default() -> Self {
        Self {
            temperature: TEMPERATURE_RANGE.neutral,
            tint: TINT_RANGE.neutral,
            manual: [WHITE_BALANCE_MULTIPLIER_RANGE.neutral; 3],
        }
    }
}

impl WhiteBalanceModeMemory {
    fn remember(&mut self, balance: WhiteBalance) {
        match balance {
            WhiteBalance::TemperatureTint { temperature, tint } => {
                self.temperature = temperature;
                self.tint = tint;
            }
            WhiteBalance::ManualMultipliers { red, green, blue } => {
                self.manual = [red, green, blue];
            }
            WhiteBalance::AsShot => {}
        }
    }

    fn select(&mut self, current: WhiteBalance, mode: WhiteBalanceMode) -> WhiteBalance {
        self.remember(current);
        match mode {
            WhiteBalanceMode::AsShot => WhiteBalance::AsShot,
            WhiteBalanceMode::TemperatureTint => match current {
                WhiteBalance::TemperatureTint { temperature, tint } => {
                    WhiteBalance::TemperatureTint { temperature, tint }
                }
                _ => WhiteBalance::TemperatureTint {
                    temperature: self.temperature,
                    tint: self.tint,
                },
            },
            WhiteBalanceMode::ManualMultipliers => match current {
                WhiteBalance::ManualMultipliers { red, green, blue } => {
                    WhiteBalance::ManualMultipliers { red, green, blue }
                }
                _ => WhiteBalance::ManualMultipliers {
                    red: self.manual[0],
                    green: self.manual[1],
                    blue: self.manual[2],
                },
            },
        }
    }
}

impl RohditorApp {
    pub(crate) fn new(
        context: &eframe::CreationContext<'_>,
        initial_path: Option<PathBuf>,
        processor_preference: ProcessorPreference,
        demosaic: DemosaicAlgorithm,
        show_diagnostics: bool,
    ) -> std::io::Result<Self> {
        theme::apply(&context.egui_ctx);
        let ui_renderer = if context.wgpu_render_state.is_some() {
            "wgpu"
        } else if context.gl.is_some() {
            "glow"
        } else {
            "unknown"
        };
        let (gpu, processor_note, startup_error) =
            match initialize_gpu_runtime(context, processor_preference) {
                Ok((gpu, processor_note)) => (gpu, processor_note, None),
                Err(error) => (
                    None,
                    None,
                    Some(format!("GPU preview is required but unavailable: {error}")),
                ),
            };
        info!(
            ui_renderer,
            processor = if gpu.is_some() { "GPU" } else { "CPU" },
            processor_preference = ?processor_preference,
            "desktop application started"
        );
        let render_options = RenderOptions {
            demosaic,
            ..RenderOptions::default()
        };
        let coordinator = RenderCoordinator::new(
            context.egui_ctx.clone(),
            PreviewOptions {
                render: render_options,
                ..PreviewOptions::default()
            },
        )
        .map_err(std::io::Error::other)?;
        let catalog_coordinator =
            CatalogCoordinator::new(context.egui_ctx.clone()).map_err(std::io::Error::other)?;
        // Restore the last browsed folder when the app was not given a file.
        let mut view_mode = ViewMode::default();
        if initial_path.is_none()
            && let Some(folder) = session::load_last_folder()
            && catalog_coordinator.scan_folder(folder).is_ok()
        {
            view_mode = ViewMode::Library;
        }
        let mut application = Self {
            coordinator,
            catalog_coordinator,
            catalog: CatalogState::default(),
            document: None,
            next_document_id: 1,
            next_export_id: 1,
            export_settings: ExportUiSettings::with_jpeg_quality(JPEG_QUALITY_DEFAULT),
            render_options,
            ui_renderer,
            processor_preference,
            gpu,
            processor_note,
            startup_error,
            show_diagnostics,
            picker_mode: None,
            color_mixer_channel: 0,
            pending_white_balance_pick: None,
            white_balance_memory: WhiteBalanceModeMemory::default(),
            crop_tool: None,
            view_mode,
            library_folder: None,
            library_selection: None,
            library_order: Vec::new(),
            library_sort: LibrarySort::default(),
            library_textures: HashMap::new(),
            library_texture_failures: std::collections::HashSet::new(),
            library_visible: (0, LIBRARY_VISIBLE_MARGIN),
            library_columns: 1,
            library_frame: 0,
            library_decode_pending: false,
            library_scroll_to_selection: false,
            filmstrip_visible: (0, 0),
            filmstrip_active: None,
        };
        if let Some(path) = initial_path {
            application.open_path(&context.egui_ctx, path);
        }
        Ok(application)
    }

    fn open_dialog(&mut self, context: &egui::Context) {
        let selected = rfd::FileDialog::new()
            .set_title("Open Sony RAW")
            .add_filter("Sony RAW", &["arw"])
            .pick_file();
        if let Some(path) = selected {
            self.open_path(context, path);
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
        session::store_last_folder(&folder);
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
            document.open_status.is_some()
                || document.preview_status.is_some()
                || document.export_status.is_some()
        }) || self.pending_white_balance_pick.is_some()
    }

    fn open_path(&mut self, context: &egui::Context, path: PathBuf) {
        self.close_document(context);
        let document_id = self.next_document_id;
        self.next_document_id = self.next_document_id.saturating_add(1);
        self.document = Some(Document::opening(document_id, path.clone()));
        if self.gpu_required_but_unavailable() {
            if let Some(document) = self.document.as_mut() {
                document.open_status = None;
                document.error = self.startup_error.clone();
            }
            self.update_window_title(context);
            return;
        }
        if let Err(error) = self.coordinator.open(document_id, path)
            && let Some(document) = self.document.as_mut()
        {
            document.open_status = None;
            document.error = Some(error);
        }
        self.update_window_title(context);
    }

    fn close_document(&mut self, context: &egui::Context) {
        self.picker_mode = None;
        self.color_mixer_channel = 0;
        self.pending_white_balance_pick = None;
        self.white_balance_memory = WhiteBalanceModeMemory::default();
        self.crop_tool = None;
        if let Some(mut document) = self.document.take() {
            self.release_gpu_preview(&mut document);
            self.coordinator.abandon(document.id);
        }
        self.update_window_title(context);
    }

    fn update_window_title(&self, context: &egui::Context) {
        let title = self.document.as_ref().map_or_else(
            || "Rohditor".to_owned(),
            |document| format!("{} — Rohditor", document.file_name()),
        );
        context.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    fn queue_preview(&mut self, context: &egui::Context, document_id: u64) {
        if self.gpu_required_but_unavailable() {
            if let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id) {
                document.preview_status = None;
                document.error = self.startup_error.clone();
            }
            return;
        }
        let request = self
            .document
            .as_mut()
            .filter(|document| document.id == document_id)
            .and_then(|document| {
                let frame = document.frame.as_ref().map(Arc::clone)?;
                let source_scale_requested = document.source_scale_requested;
                let intent = if source_scale_requested {
                    PreviewIntent::SourceScale
                } else {
                    PreviewIntent::CommittedFit
                };
                Some((
                    document.begin_preview_request(intent),
                    frame,
                    document.edits.recipe().clone(),
                    source_scale_requested,
                ))
            });
        let Some((ticket, frame, recipe, source_scale_requested)) = request else {
            return;
        };
        if let Some(document) = self
            .document
            .as_mut()
            .filter(|document| document.id == document_id)
        {
            // A histogram is only a valid Auto Tone input for the recipe that
            // produced it. Keep the old graph visible, but never apply it to
            // a newer revision.
            document.histogram_revision = None;
            // Do not describe the previous backend as active while the new
            // recipe is still being rendered. The status bar can therefore
            // distinguish an old visible frame from the current work.
            document.preview_diagnostics = None;
        }
        if source_scale_requested {
            if self.gpu.is_some() {
                self.release_document_gpu_preview(document_id);
            }
            if let Some(document) = self.document.as_mut() {
                document.preview_status = Some((
                    ticket.revision,
                    "Queued full-resolution 1:1 inspection".to_owned(),
                ));
            }
            if let Err(error) = self.coordinator.source_scale_preview(ticket, frame, recipe)
                && let Some(document) = self.document.as_mut()
            {
                document.preview_status = None;
                document.error = Some(error);
            }
            return;
        }
        if self.gpu.is_some() && gpu_supports_recipe(&recipe) {
            let gpu_base_is_current = self
                .document
                .as_ref()
                .and_then(|document| document.gpu_preview.as_ref())
                .is_some_and(|preview| {
                    preview.source.supports_dynamic_white_balance()
                        || preview.source.white_balance() == recipe.color.white_balance
                });
            if gpu_base_is_current {
                self.coordinator.cancel_preview(document_id);
                self.render_gpu_preview(context, document_id);
                return;
            }
            if let Some(document) = self.document.as_mut() {
                document.preview_status = Some((
                    ticket.revision,
                    "Preparing linear base for GPU preview".to_owned(),
                ));
            }
            if let Err(error) = self.coordinator.prepare_gpu_base(ticket, frame, recipe)
                && let Some(document) = self.document.as_mut()
            {
                document.preview_status = None;
                document.error = Some(error);
            }
            return;
        }
        // Keep the last GPU frame installed while the first CPU-only color
        // preview is rendered. Releasing it here leaves the viewport without
        // a texture for one or more frames and produces a visible black flash.
        // The worker-event handoff releases it immediately before installing
        // the completed CPU texture in the same UI update.
        if let Some(document) = self.document.as_mut() {
            document.preview_status = Some((
                ticket.revision,
                "Queued CPU preview for the selected color tools".to_owned(),
            ));
        }
        match self.coordinator.preview(ticket, frame, recipe) {
            Ok(()) => {
                info!(
                    document_id,
                    revision = ticket.revision,
                    processor_preference = ?self.processor_preference,
                    "CPU preview queued"
                );
            }
            Err(error) => {
                if let Some(document) = self.document.as_mut() {
                    document.preview_status = None;
                    document.error = Some(error);
                }
            }
        }
    }

    fn enter_crop_tool(&mut self, context: &egui::Context) {
        if self.crop_tool.is_some() {
            return;
        }
        let request = self.document.as_mut().and_then(|document| {
            let frame = document.frame.as_ref().map(Arc::clone)?;
            document.source_scale_requested = false;
            document.view.fit(context.input(|input| input.time));
            let (width, height) = document
                .texture
                .as_ref()
                .map(PreviewTexture::dimensions)
                .unwrap_or((frame.info.width, frame.info.height));
            let session =
                CropToolSession::new(document.edits.recipe().geometry.crop, width, height);
            let ticket = document.begin_preview_request(PreviewIntent::CropToolFullFrame);
            let recipe = document.edits.recipe().clone();
            document.preview_status =
                Some((ticket.revision, "Preparing full image for crop".to_owned()));
            Some((document.id, ticket, frame, recipe, session))
        });
        let Some((document_id, ticket, frame, recipe, session)) = request else {
            return;
        };
        self.picker_mode = None;
        self.pending_white_balance_pick = None;
        self.crop_tool = Some(session);
        if let Err(error) = self.coordinator.crop_tool_preview(ticket, frame, recipe) {
            self.crop_tool = None;
            if let Some(document) = self
                .document
                .as_mut()
                .filter(|document| document.id == document_id)
            {
                document.preview_status = None;
                document.error = Some(error);
            }
        }
    }

    fn finish_crop_tool(&mut self, context: &egui::Context, apply: bool) {
        let Some(session) = self.crop_tool.take() else {
            return;
        };
        let mut changed_document = None;
        if apply
            && session.full_frame_ready()
            && let Some(document) = self.document.as_mut()
        {
            let mut next = document.edits.recipe().clone();
            next.geometry.crop = session.committed_crop();
            if session.is_modified() && document.edits.set_discrete(next) {
                document.notice = None;
                changed_document = Some(document.id);
            }
        }
        let document_id =
            changed_document.or_else(|| self.document.as_ref().map(|document| document.id));
        if let Some(document_id) = document_id {
            self.queue_preview(context, document_id);
        }
    }

    fn show_crop_tool(&mut self, context: &egui::Context) {
        let panel = self.crop_tool.as_ref().map(|session| CropPanelModel {
            aspect: session.aspect(),
            locked: session.locked(),
            portrait: session.portrait(),
            dimensions: session.output_dimensions(),
            ready: session.full_frame_ready(),
        });
        let Some(panel) = panel else {
            return;
        };
        let output = crate::ui::crop::show_panel(context, &panel);
        if let Some(session) = self.crop_tool.as_mut() {
            if let Some(aspect) = output.aspect {
                session.set_aspect(aspect);
            }
            if let Some(locked) = output.locked {
                session.set_locked(locked);
            }
            if output.toggle_orientation {
                session.toggle_orientation();
            }
            if output.reset {
                session.reset();
            }
        }
        let keyboard_apply = context.input(|input| input.key_pressed(egui::Key::Enter));
        let keyboard_cancel = context.input(|input| input.key_pressed(egui::Key::Escape));
        if output.cancel || keyboard_cancel {
            self.finish_crop_tool(context, false);
        } else if output.apply || keyboard_apply {
            self.finish_crop_tool(context, true);
        }
    }

    fn install_gpu_upload(
        &mut self,
        context: &egui::Context,
        ticket: PreviewTicket,
        upload: GpuPreviewUpload,
        diagnostics: WorkerPreviewDiagnostics,
        upload_preparation: Duration,
    ) {
        let Some(document) = self
            .document
            .as_ref()
            .filter(|document| gpu_upload_matches_document(document, ticket))
        else {
            return;
        };
        let recipe = document.edits.recipe().clone();
        let previous = self.document.as_mut().and_then(|document| {
            document.pending_gpu_histogram = None;
            document.gpu_histogram_due = None;
            document.gpu_preview.take()
        });
        let previous_texture_id = previous.as_ref().map(|preview| preview.texture_id);
        let reusable_frame = previous.map(|preview| preview.frame);

        let result = self
            .gpu
            .as_ref()
            .ok_or_else(|| {
                "the GPU processor is no longer active while its preview base was prepared"
                    .to_owned()
            })
            .and_then(|runtime| {
                let source = runtime
                    .processor
                    .upload_prepared(upload)
                    .map_err(|error| error.to_string())?;
                let frame = runtime
                    .processor
                    .render(&source, &recipe, reusable_frame)
                    .map_err(|error| error.to_string())?;
                let texture_id =
                    register_or_update_gpu_texture(runtime, previous_texture_id, &frame);
                Ok::<_, String>((source, frame, texture_id))
            });

        match result {
            Ok((source, frame, texture_id)) => {
                let output_size = gpu_output_size(frame.output_dimensions());
                let elapsed =
                    diagnostics.timings.total + upload_preparation + frame.submission_time();
                let gpu_resident_bytes = source
                    .estimated_bytes()
                    .saturating_add(frame.estimated_bytes());
                let preview_diagnostics = DocumentPreviewDiagnostics {
                    worker: diagnostics,
                    gpu_upload_preparation: Some(upload_preparation),
                    gpu_submission: Some(frame.submission_time()),
                    gpu_queue_completion: frame.queue_completion_time(),
                    gpu_histogram_readback: None,
                    gpu_textures_reused: Some(frame.textures_reused()),
                    gpu_resident_bytes,
                };
                info!(
                    document_id = ticket.document_id,
                    revision = ticket.revision,
                    width = frame.output_dimensions().0,
                    height = frame.output_dimensions().1,
                    base_ms = diagnostics.timings.total.as_millis(),
                    upload_prepare_ms = upload_preparation.as_millis(),
                    submission_us = frame.submission_time().as_micros(),
                    "GPU preview complete"
                );
                if let Some(document) = self.document.as_mut().filter(|document| {
                    document.id == ticket.document_id && document.ticket() == ticket
                }) {
                    document.gpu_preview = Some(GpuDocumentPreview {
                        ticket,
                        source,
                        frame,
                        texture_id,
                    });
                    document.gpu_histogram_due =
                        Some((ticket, Instant::now() + GPU_HISTOGRAM_DEBOUNCE));
                    document.texture = Some(PreviewTexture::Gpu {
                        id: texture_id,
                        size: output_size,
                    });
                    document.preview_pixels = None;
                    document.preview_source =
                        Some(PreviewSource::developed(diagnostics.algorithm, true));
                    document.preview_status = None;
                    document.last_preview_time = Some(elapsed);
                    document.preview_diagnostics = Some(preview_diagnostics);
                    document.error = None;
                    document.warning = None;
                }
                context.request_repaint();
            }
            Err(error) => {
                if let Some(texture_id) = previous_texture_id {
                    self.clear_orphaned_gpu_display(ticket.document_id, texture_id);
                }
                self.handle_gpu_failure(context, ticket.document_id, error);
            }
        }
    }

    fn render_gpu_preview(&mut self, context: &egui::Context, document_id: u64) {
        let Some(document) = self
            .document
            .as_mut()
            .filter(|document| document.id == document_id)
        else {
            return;
        };
        let Some(preview) = document.gpu_preview.take() else {
            return;
        };
        document.pending_gpu_histogram = None;
        document.gpu_histogram_due = None;
        document.histogram_revision = None;
        let texture_id = preview.texture_id;
        let recipe = document.edits.recipe().clone();
        let revision = document.edits.revision();
        let current_ticket = document.ticket();
        let previous_worker_diagnostics = document
            .preview_diagnostics
            .map(|diagnostics| diagnostics.worker);
        document.preview_status = Some((
            revision,
            "Applying edits to resident GPU preview".to_owned(),
        ));

        let result = self
            .gpu
            .as_ref()
            .ok_or_else(|| "the GPU processor is no longer active while applying edits".to_owned())
            .and_then(|runtime| {
                let frame = runtime
                    .processor
                    .render(&preview.source, &recipe, Some(preview.frame))
                    .map_err(|error| error.to_string())?;
                register_or_update_gpu_texture(runtime, Some(texture_id), &frame);
                Ok::<_, String>(frame)
            });

        match result {
            Ok(frame) => {
                let output_size = gpu_output_size(frame.output_dimensions());
                let submission = frame.submission_time();
                let mut worker = previous_worker_diagnostics.unwrap_or(WorkerPreviewDiagnostics {
                    backend: PreviewBackend::GpuBase,
                    resolution: PreviewResolution::Fit,
                    algorithm: self.render_options.demosaic,
                    cache_hits: PreviewCacheHits::default(),
                    timings: StageTimings::default(),
                    memory: MemoryEstimate::default(),
                    cache_resident_bytes: 0,
                    workspace_reused: false,
                });
                worker.backend = PreviewBackend::GpuBase;
                worker.cache_hits = PreviewCacheHits {
                    decoded: true,
                    reconstructed: true,
                    demosaiced: false,
                    adjusted: false,
                };
                worker.timings = StageTimings {
                    adjustments: submission,
                    total: submission,
                    ..StageTimings::default()
                };
                worker.workspace_reused = false;
                let preview_diagnostics = DocumentPreviewDiagnostics {
                    worker,
                    gpu_upload_preparation: None,
                    gpu_submission: Some(submission),
                    gpu_queue_completion: frame.queue_completion_time(),
                    gpu_histogram_readback: None,
                    gpu_textures_reused: Some(frame.textures_reused()),
                    gpu_resident_bytes: preview
                        .source
                        .estimated_bytes()
                        .saturating_add(frame.estimated_bytes()),
                };
                info!(
                    document_id,
                    revision,
                    width = frame.output_dimensions().0,
                    height = frame.output_dimensions().1,
                    submission_us = frame.submission_time().as_micros(),
                    "GPU preview adjustment complete"
                );
                if let Some(document) = self.document.as_mut().filter(|document| {
                    document.ticket() == current_ticket
                        && document.preview_intent == PreviewIntent::CommittedFit
                }) {
                    document.gpu_preview = Some(GpuDocumentPreview {
                        ticket: current_ticket,
                        source: preview.source,
                        frame,
                        texture_id,
                    });
                    document.gpu_histogram_due =
                        Some((current_ticket, Instant::now() + GPU_HISTOGRAM_DEBOUNCE));
                    document.texture = Some(PreviewTexture::Gpu {
                        id: texture_id,
                        size: output_size,
                    });
                    document.preview_pixels = None;
                    document.preview_source =
                        Some(PreviewSource::developed(worker.algorithm, true));
                    document.preview_status = None;
                    document.last_preview_time = Some(submission);
                    document.preview_diagnostics = Some(preview_diagnostics);
                    document.error = None;
                }
                context.request_repaint();
            }
            Err(error) => {
                self.clear_orphaned_gpu_display(document_id, texture_id);
                self.handle_gpu_failure(context, document_id, error);
            }
        }
    }

    fn handle_gpu_failure(&mut self, context: &egui::Context, document_id: u64, error: String) {
        warn!(
            document_id,
            processor_preference = ?self.processor_preference,
            %error,
            "GPU preview failed"
        );
        if self.processor_preference == ProcessorPreference::Auto {
            if let Some(mut document) = self.document.take() {
                if document.id == document_id {
                    self.release_gpu_preview(&mut document);
                    document.warning = Some(format!(
                        "GPU preview failed ({error}); continuing with the CPU processor."
                    ));
                    document.preview_status = None;
                }
                self.document = Some(document);
            }
            self.gpu = None;
            self.processor_note = Some(format!("GPU fallback: {error}"));
            self.queue_preview(context, document_id);
        } else if let Some(document) = self
            .document
            .as_mut()
            .filter(|document| document.id == document_id)
        {
            document.preview_status = None;
            document.error = Some(format!("GPU preview failed: {error}"));
        }
    }

    fn release_gpu_preview(&mut self, document: &mut Document) {
        document.pending_gpu_histogram = None;
        document.gpu_histogram_due = None;
        let texture_id = document
            .gpu_preview
            .take()
            .map(|preview| preview.texture_id);
        if let Some(texture_id) = texture_id {
            self.free_gpu_texture(texture_id);
        }
        if matches!(document.texture, Some(PreviewTexture::Gpu { .. })) {
            document.texture = None;
            document.preview_source = None;
        }
    }

    fn release_document_gpu_preview(&mut self, document_id: u64) {
        let Some(mut document) = self.document.take() else {
            return;
        };
        if document.id == document_id {
            self.release_gpu_preview(&mut document);
        }
        self.document = Some(document);
    }

    fn clear_orphaned_gpu_display(&mut self, document_id: u64, texture_id: egui::TextureId) {
        self.free_gpu_texture(texture_id);
        if let Some(document) = self
            .document
            .as_mut()
            .filter(|document| document.id == document_id)
            && matches!(
                document.texture,
                Some(PreviewTexture::Gpu { id, .. }) if id == texture_id
            )
        {
            document.texture = None;
            document.preview_source = None;
        }
    }

    fn free_gpu_texture(&self, texture_id: egui::TextureId) {
        if let Some(runtime) = self.gpu.as_ref() {
            runtime
                .render_state
                .renderer
                .write()
                .free_texture(&texture_id);
        }
    }

    fn refresh_gpu_queue_completion(&mut self, context: &egui::Context) {
        if let Some(runtime) = self.gpu.as_ref()
            && let Err(error) = runtime.processor.poll()
        {
            warn!(%error, "GPU polling failed while refreshing preview diagnostics");
        }
        let Some(document) = self.document.as_mut() else {
            return;
        };
        let Some(preview) = document.gpu_preview.as_ref() else {
            return;
        };
        let Some(diagnostics) = document.preview_diagnostics.as_mut() else {
            return;
        };
        if diagnostics.gpu_queue_completion.is_some() {
            return;
        }
        if let Some(completion) = preview.frame.queue_completion_time() {
            diagnostics.gpu_queue_completion = Some(completion);
            document.last_preview_time = Some(completion);
        } else {
            context.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn refresh_gpu_histogram(&mut self, context: &egui::Context) {
        if let Some(mut pending) = self
            .document
            .as_mut()
            .and_then(|document| document.pending_gpu_histogram.take())
        {
            let readback_latency = pending.started.elapsed();
            match pending.readback.try_finish() {
                Ok(Some(readback)) => {
                    let histogram = usize::try_from(readback.width)
                        .ok()
                        .and_then(|width| {
                            usize::try_from(readback.height)
                                .ok()
                                .map(|height| (width, height))
                        })
                        .and_then(|(width, height)| {
                            Histogram::from_rgba8(width, height, &readback.rgba)
                        });
                    if let Some(document) = self.document.as_mut().filter(|document| {
                        document.ticket() == pending.ticket
                            && document
                                .gpu_preview
                                .as_ref()
                                .is_some_and(|preview| preview.ticket == pending.ticket)
                    }) && let Some(histogram) = histogram
                    {
                        document.histogram = Some(histogram);
                        document.histogram_revision = Some(pending.ticket.revision);
                        if let Some(diagnostics) = document.preview_diagnostics.as_mut() {
                            diagnostics.gpu_histogram_readback = Some(readback_latency);
                        }
                    }
                    context.request_repaint();
                }
                Ok(None) => {
                    if let Some(document) = self.document.as_mut().filter(|document| {
                        document.ticket() == pending.ticket
                            && document
                                .gpu_preview
                                .as_ref()
                                .is_some_and(|preview| preview.ticket == pending.ticket)
                    }) {
                        document.pending_gpu_histogram = Some(pending);
                        context.request_repaint_after(Duration::from_millis(16));
                    }
                }
                Err(error) => {
                    warn!(ticket = ?pending.ticket, %error, "GPU histogram readback failed");
                }
            }
            return;
        }

        let now = Instant::now();
        let Some((ticket, due)) = self
            .document
            .as_ref()
            .and_then(|document| document.gpu_histogram_due)
        else {
            return;
        };
        if due > now {
            context.request_repaint_after(due.duration_since(now));
            return;
        }

        let readback = self
            .document
            .as_ref()
            .filter(|document| {
                document.ticket() == ticket
                    && document
                        .gpu_preview
                        .as_ref()
                        .is_some_and(|preview| preview.ticket == ticket)
            })
            .and_then(|document| document.gpu_preview.as_ref())
            .and_then(|preview| {
                self.gpu
                    .as_ref()
                    .map(|runtime| runtime.processor.begin_display_readback(&preview.frame))
            });
        if let Some(document) = self.document.as_mut()
            && document
                .gpu_histogram_due
                .is_some_and(|(due_ticket, _)| due_ticket == ticket)
        {
            document.gpu_histogram_due = None;
        }
        match readback {
            Some(Ok(readback)) => {
                if let Some(document) = self.document.as_mut().filter(|document| {
                    document.ticket() == ticket
                        && document
                            .gpu_preview
                            .as_ref()
                            .is_some_and(|preview| preview.ticket == ticket)
                }) {
                    document.pending_gpu_histogram = Some(PendingGpuHistogram {
                        ticket,
                        readback,
                        started: Instant::now(),
                    });
                    context.request_repaint_after(Duration::from_millis(16));
                }
            }
            Some(Err(error)) => {
                warn!(?ticket, %error, "could not start GPU histogram readback");
            }
            None => {}
        }
    }

    fn processor_description(&self) -> String {
        if let Some(runtime) = &self.gpu {
            let capabilities = runtime.processor.capabilities();
            let hardware = format!("{} · {}", capabilities.adapter_name, capabilities.backend);
            match self
                .document
                .as_ref()
                .and_then(|document| document.preview_diagnostics)
                .map(|diagnostics| diagnostics.worker.backend)
            {
                Some(PreviewBackend::Cpu) => format!("CPU fallback · GPU available · {hardware}"),
                Some(PreviewBackend::GpuBase) => format!("GPU active · {hardware}"),
                None => format!("GPU available · {hardware}"),
            }
        } else {
            format!("CPU ({})", self.processor_preference.label())
        }
    }

    fn gpu_required_but_unavailable(&self) -> bool {
        self.processor_preference == ProcessorPreference::Gpu && self.gpu.is_none()
    }

    fn request_export(&mut self) {
        let Some(document) = self.document.as_ref() else {
            return;
        };
        if document.frame.is_none() || document.export_status.is_some() {
            return;
        }
        let extension = self.export_settings.extension();
        let suggested = format!("{}-edited.{extension}", source_stem(&document.path));
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
                document.error = Some(error.to_string());
            }
            return;
        }
        match paths_refer_to_same_file(&document.path, &destination) {
            Ok(true) => {
                if let Some(document) = self.document.as_mut() {
                    document.error = Some(
                        "Refusing to replace the source RAW, including through a symbolic or hard link. Choose a different export destination."
                            .to_owned(),
                    );
                }
                return;
            }
            Ok(false) => {}
            Err(error) => {
                if let Some(document) = self.document.as_mut() {
                    document.error = Some(format!(
                        "Could not validate the export destination {}: {error}",
                        destination.display()
                    ));
                }
                return;
            }
        }

        let export_id = self.next_export_id;
        self.next_export_id = self.next_export_id.saturating_add(1);
        let document_id = document.id;
        let recipe_revision = document.edits.revision();
        let recipe = document.edits.recipe().clone();
        let frame = document.frame.as_ref().map(Arc::clone);
        let Some(frame) = frame else {
            return;
        };
        if let Some(document) = self.document.as_mut() {
            document.export_status = Some(ExportActivity {
                id: export_id,
                recipe_revision,
                detail: "Queued full-resolution CPU export".to_owned(),
            });
            document.error = None;
            document.notice = None;
        }
        if let Err(error) = self.coordinator.export(
            document_id,
            export_id,
            recipe_revision,
            destination,
            frame,
            recipe,
            settings,
            self.render_options,
        ) && let Some(document) = self.document.as_mut()
        {
            document.export_status = None;
            document.error = Some(error);
        }
    }

    fn show_top_bar(&mut self, context: &egui::Context) {
        let model = ToolbarModel {
            mode: self.view_mode,
            document_name: self.document.as_ref().map(Document::file_name),
            can_undo: self
                .document
                .as_ref()
                .is_some_and(|document| document.edits.can_undo()),
            can_redo: self
                .document
                .as_ref()
                .is_some_and(|document| document.edits.can_redo()),
            fit_selected: self
                .document
                .as_ref()
                .is_none_or(|document| document.view.is_fit()),
            source_scale_selected: source_scale_selected(self.document.as_ref()),
            crop_active: self.crop_tool.is_some(),
            zoom_label: self
                .document
                .as_ref()
                .map_or_else(|| "FIT".to_owned(), |document| document.view.zoom_label()),
            diagnostics_open: self.show_diagnostics,
            export_ready: self.view_mode == ViewMode::Develop
                && self.document.as_ref().is_some_and(|document| {
                    document.frame.is_some() && document.export_status.is_none()
                }),
        };
        let actions = toolbar::show_top(context, &model);
        if let Some(mode) = actions.view_mode
            && mode != self.view_mode
        {
            if mode == ViewMode::Library {
                // The crop overlay and pickers belong to the develop viewport.
                self.crop_tool = None;
                self.picker_mode = None;
                self.pending_white_balance_pick = None;
            }
            self.view_mode = mode;
        }
        if actions.toggle_diagnostics {
            self.show_diagnostics = !self.show_diagnostics;
        }
        if actions.open {
            self.open_dialog(context);
        } else if actions.close {
            self.close_document(context);
        }
        if actions.open_folder {
            self.open_folder_dialog();
        }
        if actions.crop {
            self.enter_crop_tool(context);
        }

        let now = context.input(|input| input.time);
        let mut changed_document = None;
        let mut view_changed_document = None;
        if let Some(document) = self.document.as_mut() {
            let mut changed = false;
            if actions.undo {
                changed |= document.edits.undo();
            }
            if actions.redo {
                changed |= document.edits.redo();
            }
            if actions.reset {
                changed |= document.edits.reset();
            }
            if changed {
                document.notice = None;
                changed_document = Some(document.id);
            }
            if actions.fit {
                let changed_mode = document.source_scale_requested;
                document.source_scale_requested = false;
                document.view.fit(now);
                if changed_mode {
                    view_changed_document = Some(document.id);
                }
            }
            if actions.actual_size && self.crop_tool.is_none() {
                let changed_mode = !document.source_scale_requested;
                document.source_scale_requested = true;
                document.view.actual_size(now);
                if changed_mode {
                    view_changed_document = Some(document.id);
                }
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
            self.request_export();
        }
    }

    fn show_file_panel(&mut self, context: &egui::Context) {
        let model = FilePanelModel {
            file_name: self.document.as_ref().map(Document::file_name),
            camera: self.document.as_ref().and_then(|document| {
                document
                    .info
                    .as_ref()
                    .map(|info| format!("{} {}", info.clean_make, info.clean_model))
            }),
            dimensions: self
                .document
                .as_ref()
                .and_then(|document| document.info.as_ref())
                .map(|info| (info.width, info.height)),
            source_state: self
                .document
                .as_ref()
                .and_then(|document| document.preview_source)
                .map(|source| source.short_label().to_owned()),
        };
        let output = toolbar::show_file_panel(context, &model);
        if output.open {
            self.open_dialog(context);
        } else if output.close {
            self.close_document(context);
        }
    }

    fn show_adjustment_panel(&mut self, context: &egui::Context) {
        let model = self.document.as_ref().map(|document| {
            document_panel_model(document, self.picker_mode, self.color_mixer_channel)
        });
        let output = adjustment_panel::show(context, model, &mut self.export_settings);
        let picker_mode = output.picker_mode;
        let mut changed_document = None;
        let mut white_balance_memory = self.white_balance_memory;
        if let Some(document) = self.document.as_mut() {
            if output.dismiss_error {
                document.error = None;
            }
            if output.dismiss_warning {
                document.warning = None;
            }

            let mut changed = false;
            let mut auto_tone_applied = false;
            if let Some(mode) = output.white_balance_mode {
                changed |=
                    set_white_balance_mode(&mut document.edits, mode, &mut white_balance_memory);
            }
            if output.auto_tone
                && document.histogram_revision == Some(document.edits.revision())
                && let Some(histogram) = document.histogram.as_ref()
            {
                let auto_changed = apply_auto_tone(&mut document.edits, histogram);
                changed |= auto_changed;
                auto_tone_applied = auto_changed;
            }
            for interaction in output.interactions {
                changed |= apply_adjustment_interaction(&mut document.edits, interaction);
            }
            if output.reset_all {
                changed |= document.edits.reset();
                // Hidden values are UI state, but reset-all should reset them
                // too; otherwise switching away from As-shot after a reset
                // would unexpectedly resurrect an older manual WB choice.
                white_balance_memory = WhiteBalanceModeMemory::default();
            }
            if changed {
                document.notice = None;
                changed_document = Some(document.id);
            }
            white_balance_memory.remember(document.edits.recipe().color.white_balance);
            if auto_tone_applied {
                document.notice = Some(
                    "Auto tone applied from the current display histogram heuristic".to_owned(),
                );
            }
        }
        self.white_balance_memory = white_balance_memory;
        if let Some(document_id) = changed_document {
            self.queue_preview(context, document_id);
        }
        if let Some(mode) = picker_mode {
            self.picker_mode = mode;
        }
        if let Some(channel) = output.color_mixer_channel {
            self.color_mixer_channel = channel.min(HSL_CHANNEL_COUNT - 1);
        }
        if output.export {
            self.request_export();
        }
    }

    fn show_viewport(&mut self, context: &egui::Context) {
        let crop_overlay = self
            .crop_tool
            .as_ref()
            .filter(|session| session.full_frame_ready())
            .map(|session| CropOverlayModel {
                crop: session.draft(),
                active_handle: session.active_handle(),
            });
        let output = if let Some(document) = self.document.as_mut() {
            let preparing = document.open_status.is_some() || document.preview_status.is_some();
            viewport::show(
                context,
                ViewportModel {
                    has_document: true,
                    preparing,
                    texture: document.texture.as_ref(),
                    source: document.preview_source,
                    picker_mode: self.picker_mode,
                    crop: crop_overlay,
                },
                &mut document.view,
            )
        } else {
            let mut empty_view = ViewState::default();
            viewport::show(
                context,
                ViewportModel {
                    has_document: false,
                    preparing: false,
                    texture: None,
                    source: None,
                    picker_mode: None,
                    crop: None,
                },
                &mut empty_view,
            )
        };
        if output.open {
            self.open_dialog(context);
        }
        if let Some((mode, normalized)) = output.picker_sample {
            match mode {
                PickerMode::WhiteBalance => self.apply_white_balance_pick(context, normalized),
                PickerMode::ColorMixer => self.apply_color_mixer_pick(normalized),
            }
        }
        if let Some((handle, point)) = output.crop.drag_started
            && let Some(session) = self.crop_tool.as_mut()
        {
            session.begin_drag(handle, point);
        }
        if let Some(point) = output.crop.dragged_to
            && let Some(session) = self.crop_tool.as_mut()
        {
            session.drag_to(point);
        }
        if output.crop.drag_stopped
            && let Some(session) = self.crop_tool.as_mut()
        {
            session.finish_drag();
        }
    }

    fn apply_white_balance_pick(&mut self, context: &egui::Context, normalized: egui::Pos2) {
        self.picker_mode = None;
        if self.pending_white_balance_pick.is_some() {
            return;
        }
        let Some((ticket, frame, recipe)) = self.document.as_ref().and_then(|document| {
            let source_ready = matches!(
                document.preview_source,
                Some(
                    PreviewSource::FastCpu
                        | PreviewSource::FastGpu
                        | PreviewSource::HighQualityCpu
                        | PreviewSource::HighQualityGpu
                )
            );
            if !source_ready || document.source_scale_requested {
                return None;
            }
            document.frame.as_ref().map(|frame| {
                (
                    document.ticket(),
                    Arc::clone(frame),
                    document.edits.recipe().clone(),
                )
            })
        }) else {
            if let Some(document) = self.document.as_mut() {
                document.error = Some(
                    "Fit a developed RAW preview before using the white-balance picker".to_owned(),
                );
            }
            return;
        };
        let options = PreviewOptions {
            render: self.render_options,
            ..PreviewOptions::default()
        };
        match self.coordinator.sample_white_balance(
            ticket,
            frame,
            recipe,
            (normalized.x, normalized.y),
            options,
        ) {
            Ok(()) => {
                self.pending_white_balance_pick = Some(ticket);
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|document| document.ticket() == ticket)
                {
                    document.preview_status = Some((
                        ticket.revision,
                        "Sampling a camera-native white-balance patch".to_owned(),
                    ));
                    document.error = None;
                }
                context.request_repaint();
            }
            Err(error) => {
                if let Some(document) = self.document.as_mut() {
                    document.error = Some(error);
                }
            }
        }
    }

    fn apply_color_mixer_pick(&mut self, normalized: egui::Pos2) {
        self.picker_mode = None;
        let sample = self.sample_display_pixel(normalized);
        let Some(sample) = sample else {
            if let Some(document) = self.document.as_mut() {
                document.error = Some(
                    "A current developed preview is required to pick a Color Mixer band".to_owned(),
                );
            }
            return;
        };
        let Some(weights) = hsl_channel_weights_from_display_rgb(sample) else {
            if let Some(document) = self.document.as_mut() {
                document.error =
                    Some("That sample is too neutral to identify a Color Mixer band".to_owned());
            }
            return;
        };
        let selected = weights
            .into_iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.color_mixer_channel = selected;
        if let Some(document) = self.document.as_mut() {
            document.notice = None;
            document.error = None;
        }
    }

    fn sample_display_pixel(&self, normalized: egui::Pos2) -> Option<[u8; 3]> {
        if !normalized.x.is_finite()
            || !normalized.y.is_finite()
            || !(0.0..=1.0).contains(&normalized.x)
            || !(0.0..=1.0).contains(&normalized.y)
        {
            return None;
        }
        let document = self.document.as_ref()?;
        if !document
            .preview_source
            .is_some_and(PreviewSource::is_developed)
        {
            return None;
        }
        if let Some(preview) = document.preview_pixels.as_ref() {
            return sample_rgb_patch(preview.width, preview.height, 3, &preview.rgb, normalized);
        }
        let gpu_preview = document.gpu_preview.as_ref()?;
        let runtime = self.gpu.as_ref()?;
        let readback = runtime
            .processor
            .readback_display(&gpu_preview.frame)
            .ok()?;
        sample_rgb_patch(
            usize::try_from(readback.width).ok()?,
            usize::try_from(readback.height).ok()?,
            4,
            &readback.rgba,
            normalized,
        )
    }

    fn show_status_bar(&mut self, context: &egui::Context) {
        let mut activities = Vec::new();
        let mut busy = false;
        if let Some(note) = &self.processor_note {
            activities.push(note.clone());
        }
        if let Some(document) = &self.document {
            if let Some(status) = &document.open_status {
                activities.push(status.clone());
                busy = true;
            } else if let Some((revision, status)) = &document.preview_status {
                activities.push(format!("{status} · revision {revision}"));
                busy = true;
            }
            if let Some(export) = &document.export_status {
                activities.push(format!(
                    "{} · recipe revision {}",
                    export.detail, export.recipe_revision
                ));
                busy = true;
            }
        }
        busy |= self.catalog.pending_count() > 0;
        toolbar::show_status(
            context,
            &StatusBarModel {
                processor: self.processor_description(),
                ui_renderer: format!("{} UI", self.ui_renderer),
                activity: (!activities.is_empty()).then(|| activities.join("  ·  ")),
                busy,
                catalog: self.catalog_status_text(),
                preview_dimensions: self
                    .document
                    .as_ref()
                    .and_then(|document| document.texture.as_ref())
                    .map(PreviewTexture::dimensions),
                preview_milliseconds: self
                    .document
                    .as_ref()
                    .and_then(|document| document.last_preview_time)
                    .map(|duration| duration.as_secs_f64() * 1_000.0),
                startup_error: self.startup_error.clone(),
            },
        );
    }

    /// One status-bar sentence describing the catalog, if one is open.
    fn catalog_status_text(&self) -> Option<String> {
        if let Some(failure) = self.catalog.failure() {
            return Some(format!("Catalog: {failure}"));
        }
        let folder = self.catalog.folder_name()?;
        let total = self.catalog.entry_count();
        let pending = self.catalog.pending_count();
        let mut text = if pending > 0 {
            format!(
                "Catalog: {} · {} photos · {}/{} thumbnails",
                folder,
                total,
                self.catalog.displayable_count(),
                total
            )
        } else {
            format!("Catalog: {} · {total} photos", folder)
        };
        if let Some(failures) = self.catalog.failure_summary() {
            text.push_str(" · ");
            text.push_str(&failures);
        }
        Some(text)
    }

    /// Library-mode central panel: lazy textures, model, grid, and actions.
    fn show_library(&mut self, context: &egui::Context) {
        if self.catalog.folder() != self.library_folder.as_deref() {
            self.library_folder = self.catalog.folder().map(Path::to_path_buf);
            self.library_selection = None;
            self.library_textures.clear();
            self.library_texture_failures.clear();
        }
        let count = self.catalog.entry_count();
        self.library_selection = self.library_selection.filter(|index| *index < count);
        self.library_order = self.sorted_catalog_indices();
        let opened_by_keyboard = self.handle_library_keyboard(context);
        self.decode_library_textures_for_range(context, self.library_visible);
        let model = self.library_model();
        let output = catalog::show(context, &model);
        self.library_visible = (output.visible_range.start, output.visible_range.end);
        self.library_columns = output.columns.max(1);
        if let Some(sort) = output.sort_changed {
            self.library_sort = sort;
        }
        if let Some(index) = output.selected_entry {
            self.library_selection = Some(index);
        }
        if let Some(index) = opened_by_keyboard.or(output.open_entry) {
            self.open_catalog_entry(context, index);
        }
        if output.open_folder {
            self.open_folder_dialog();
        }
    }

    /// Open a catalog entry for editing, unless it is already open.
    fn open_catalog_entry(&mut self, context: &egui::Context, grid_index: usize) {
        let Some(&catalog_index) = self.library_order.get(grid_index) else {
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
            .is_some_and(|document| document.path == path)
        {
            return;
        }
        self.view_mode = ViewMode::Develop;
        self.open_path(context, path);
    }

    /// Display order of catalog entries: scan order for filename sort, or
    /// capture-date order with undated entries last.
    fn sorted_catalog_indices(&self) -> Vec<usize> {
        let count = self.catalog.entry_count();
        let mut order: Vec<usize> = (0..count).collect();
        if self.library_sort == LibrarySort::CaptureDate {
            order.sort_by(|&left, &right| {
                match (
                    self.catalog.capture_date(left),
                    self.catalog.capture_date(right),
                ) {
                    (Some(left_date), Some(right_date)) => {
                        left_date.cmp(right_date).then_with(|| {
                            self.catalog
                                .entry_name(left)
                                .cmp(&self.catalog.entry_name(right))
                        })
                    }
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => self
                        .catalog
                        .entry_name(left)
                        .cmp(&self.catalog.entry_name(right)),
                }
            });
        }
        order
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
            self.library_selection = Some(moved_library_selection(
                self.library_selection,
                self.library_columns,
                count,
                key,
            ));
            self.library_scroll_to_selection = true;
            None
        } else if context.input(|input| input.key_pressed(egui::Key::Enter)) {
            self.library_selection
        } else {
            None
        }
    }

    /// Decode textures for grid positions around `range`, with a per-frame
    /// budget so large folders never block a frame.
    fn decode_library_textures_for_range(
        &mut self,
        context: &egui::Context,
        (start, end): (usize, usize),
    ) {
        self.library_decode_pending = false;
        let count = self.library_order.len();
        if count == 0 {
            return;
        }
        let start = start.saturating_sub(LIBRARY_VISIBLE_MARGIN).min(count);
        let end = (end + LIBRARY_VISIBLE_MARGIN).min(count);
        let candidates: Vec<(PathBuf, Vec<u8>)> = (start..end)
            .filter_map(|grid_index| {
                let catalog_index = *self.library_order.get(grid_index)?;
                let path = self.catalog.entry_path(catalog_index)?.to_path_buf();
                match self.catalog.slot(catalog_index) {
                    Some(ThumbnailSlot::Ready(thumbnail)) => {
                        Some((path, thumbnail.bytes().to_vec()))
                    }
                    _ => None,
                }
            })
            .collect();

        let frame = self.library_frame;
        let mut budget = LIBRARY_DECODE_BUDGET;
        for (path, bytes) in candidates {
            if let Some(entry) = self.library_textures.get_mut(&path) {
                entry.1 = frame;
                continue;
            }
            if budget == 0 {
                self.library_decode_pending = true;
                continue;
            }
            if self.library_texture_failures.contains(&path) {
                continue;
            }
            match decode_library_texture(&bytes) {
                Ok(image) => {
                    let texture = context.load_texture(
                        path.to_string_lossy().into_owned(),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.library_textures.insert(path, (texture, frame));
                    budget -= 1;
                }
                Err(error) => {
                    warn!(
                        %error,
                        path = %path.display(),
                        "library grid texture decoding failed"
                    );
                    self.library_texture_failures.insert(path);
                }
            }
        }
        evict_least_recently_used(&mut self.library_textures, LIBRARY_TEXTURE_CAP);
    }

    fn library_model(&mut self) -> LibraryModel {
        let selection = self.library_selection;
        let scroll_to_selection = self.library_scroll_to_selection;
        self.library_scroll_to_selection = false;
        let entries = self
            .library_order
            .iter()
            .filter_map(|&catalog_index| self.library_entry_model(catalog_index))
            .collect();
        LibraryModel {
            folder_name: self.catalog.folder_name(),
            entries,
            selection,
            scroll_to_selection,
            sort: self.library_sort,
            failure: self.catalog.failure().map(str::to_owned),
            loading_thumbnails: self.catalog.pending_count() > 0,
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
            .and_then(|path| self.library_textures.get(path))
            .map(|(texture, _)| texture.clone());
        Some(LibraryEntryModel {
            name,
            state,
            texture,
        })
    }

    /// Develop-mode filmstrip below the viewport; clicking a thumbnail opens
    /// that photo, and PageUp/PageDown walk through the folder.
    fn show_filmstrip(&mut self, context: &egui::Context) {
        if self.catalog.entry_count() == 0 {
            return;
        }
        self.library_order = self.sorted_catalog_indices();
        let active_catalog_index = self
            .document
            .as_ref()
            .and_then(|document| self.catalog.entry_index_for_path(&document.path));
        let active_grid = active_catalog_index.and_then(|catalog_index| {
            self.library_order
                .iter()
                .position(|&index| index == catalog_index)
        });
        let scroll_to_active = active_grid != self.filmstrip_active;
        self.filmstrip_active = active_grid;

        if let Some(catalog_index) = self.navigate_filmstrip(context, active_catalog_index) {
            self.open_catalog_entry(context, catalog_index);
        }

        self.decode_library_textures_for_range(context, self.filmstrip_visible);
        let model = FilmstripModel {
            entries: self
                .library_order
                .iter()
                .filter_map(|&catalog_index| self.library_entry_model(catalog_index))
                .collect(),
            active: active_grid,
            scroll_to_active,
        };
        let output = catalog::show_filmstrip(context, &model);
        self.filmstrip_visible = (output.visible_range.start, output.visible_range.end);
        if let Some(grid_index) = output.open_entry {
            self.open_catalog_entry(context, grid_index);
        }
    }

    /// PageUp/PageDown move to the previous/next photo in display order.
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
        let target =
            adjacent_catalog_index(active_catalog_index, self.catalog.entry_count(), forward)?;
        // Convert the catalog index into the display-order grid index.
        self.library_order.iter().position(|&index| index == target)
    }

    fn background_work_is_active(&self) -> bool {
        self.document.as_ref().is_some_and(|document| {
            document.open_status.is_some()
                || document.preview_status.is_some()
                || document.export_status.is_some()
                || document.pending_gpu_histogram.is_some()
        }) || self.pending_white_balance_pick.is_some()
            || self.catalog.pending_count() > 0
            || self.library_decode_pending
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
        let gpu_device = self.gpu.as_ref().map(|runtime| {
            let capabilities = runtime.processor.capabilities();
            format!(
                "{} · {} · timestamp queries: {}",
                capabilities.adapter_name, capabilities.backend, capabilities.timestamp_queries
            )
        });
        let preview = self
            .document
            .as_ref()
            .and_then(|document| document.preview_diagnostics)
            .map(|preview| PreviewModel {
                backend: preview.worker.backend.label().to_owned(),
                algorithm: preview.worker.algorithm.stable_name().to_owned(),
                source_state: match preview.worker.resolution {
                    PreviewResolution::SourceScale => "1:1",
                    PreviewResolution::CropToolFullFrame => "crop authoring",
                    PreviewResolution::Fit => match preview.worker.algorithm {
                        DemosaicAlgorithm::Bilinear => "fast",
                        DemosaicAlgorithm::MalvarHeCutler => "high-quality",
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
                    demosaic: preview.worker.timings.demosaic,
                    resampling: preview.worker.timings.resampling,
                    color_conversion: preview.worker.timings.color_conversion,
                    adjustments: preview.worker.timings.adjustments,
                    output_conversion: preview.worker.timings.output_conversion,
                    total: preview.worker.timings.total,
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
                processor_note: self.processor_note.clone(),
                startup_error: self.startup_error.clone(),
                warning: self
                    .document
                    .as_ref()
                    .and_then(|document| document.warning.clone()),
                error: self
                    .document
                    .as_ref()
                    .and_then(|document| document.error.clone()),
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
                    document.notice = Some(format!(
                        "Saved support-safe diagnostics report to {}.",
                        destination.display()
                    ));
                }
            }
            Err(error) => {
                if let Some(document) = self.document.as_mut() {
                    document.error = Some(error);
                } else {
                    self.startup_error = Some(error);
                }
            }
        }
    }
}

fn source_scale_selected(document: Option<&Document>) -> bool {
    document.is_some_and(|document| document.source_scale_requested)
}

/// Keyboard navigation steps in the library grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LibraryNavigation {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

/// Move a library selection one grid step; `count` must be at least one entry.
/// Without a selection, any key selects the first entry.
fn moved_library_selection(
    current: Option<usize>,
    columns: usize,
    count: usize,
    key: LibraryNavigation,
) -> usize {
    let columns = columns.max(1);
    let last = count - 1;
    let Some(current) = current.map(|current| current.min(last)) else {
        return 0;
    };
    match key {
        LibraryNavigation::Left => current.saturating_sub(1),
        LibraryNavigation::Right => (current + 1).min(last),
        LibraryNavigation::Up => current.saturating_sub(columns),
        LibraryNavigation::Down => (current + columns).min(last),
        LibraryNavigation::Home => 0,
        LibraryNavigation::End => last,
    }
}

/// Step to the previous or next catalog entry in display order. Returns
/// `None` at the ends or when nothing is active.
fn adjacent_catalog_index(current: Option<usize>, count: usize, forward: bool) -> Option<usize> {
    let current = current?;
    if forward {
        (current + 1 < count).then_some(current + 1)
    } else {
        current.checked_sub(1)
    }
}

/// Decode encoded thumbnail bytes into a bounded grid texture image.
fn decode_library_texture(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        .map_err(|error| format!("thumbnail JPEG decoding failed: {error}"))?
        .to_rgb8();
    let (width, height) = decoded.dimensions();
    let long_edge = width.max(height);
    let resized = if long_edge <= LIBRARY_TEXTURE_LONG_EDGE {
        decoded
    } else {
        let scale = f64::from(LIBRARY_TEXTURE_LONG_EDGE) / f64::from(long_edge);
        let target_width = ((f64::from(width) * scale).round() as u32).max(1);
        let target_height = ((f64::from(height) * scale).round() as u32).max(1);
        image::imageops::resize(
            &decoded,
            target_width,
            target_height,
            image::imageops::FilterType::Triangle,
        )
    };
    let (width, height) = resized.dimensions();
    Ok(egui::ColorImage::from_rgb(
        [width as usize, height as usize],
        &resized.into_raw(),
    ))
}

/// Evict least-recently-used textures until the map fits within `cap`.
fn evict_least_recently_used(
    textures: &mut HashMap<PathBuf, (egui::TextureHandle, u64)>,
    cap: usize,
) {
    if textures.len() <= cap {
        return;
    }
    let mut order: Vec<(u64, PathBuf)> = textures
        .iter()
        .map(|(path, (_, frame))| (*frame, path.clone()))
        .collect();
    order.sort_unstable();
    let evict = order.len() - cap;
    for (_, path) in order.into_iter().take(evict) {
        textures.remove(&path);
    }
}

fn document_panel_model(
    document: &Document,
    picker_mode: Option<PickerMode>,
    color_mixer_channel: usize,
) -> DocumentPanelModel {
    let (
        white_balance_mode,
        white_balance_red,
        white_balance_green,
        white_balance_blue,
        white_balance_temperature,
        white_balance_tint,
    ) = match document.edits.recipe().color.white_balance {
        WhiteBalance::AsShot => (
            WhiteBalanceMode::AsShot,
            WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            TEMPERATURE_RANGE.neutral,
            TINT_RANGE.neutral,
        ),
        WhiteBalance::ManualMultipliers { red, green, blue } => (
            WhiteBalanceMode::ManualMultipliers,
            red,
            green,
            blue,
            TEMPERATURE_RANGE.neutral,
            TINT_RANGE.neutral,
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
    DocumentPanelModel {
        file_name: document.file_name(),
        camera: document
            .info
            .as_ref()
            .map(|info| format!("{} {}", info.clean_make, info.clean_model)),
        sensor_dimensions: document.info.as_ref().map(|info| (info.width, info.height)),
        revision: document.edits.revision(),
        has_adjustments: document.edits.recipe() != &EditRecipe::default(),
        values: AdjustmentValues {
            white_balance_mode,
            white_balance_red,
            white_balance_green,
            white_balance_blue,
            white_balance_temperature,
            white_balance_tint,
            exposure: document.edits.recipe().light.exposure_ev,
            contrast: document.edits.recipe().light.contrast,
            highlights: document.edits.recipe().light.highlights,
            shadows: document.edits.recipe().light.shadows,
            whites: document.edits.recipe().light.whites,
            blacks: document.edits.recipe().light.blacks,
            tone_curve_shadows: document.edits.recipe().light.tone_curve.shadows,
            tone_curve_darks: document.edits.recipe().light.tone_curve.darks,
            tone_curve_lights: document.edits.recipe().light.tone_curve.lights,
            tone_curve_highlights: document.edits.recipe().light.tone_curve.highlights,
            saturation: document.edits.recipe().color.saturation,
            vibrance: document.edits.recipe().color.vibrance,
            hsl: document
                .edits
                .recipe()
                .color
                .hsl
                .channels
                .map(|channel| [channel.hue, channel.saturation, channel.luminance]),
            grading: [
                document.edits.recipe().color.grading.shadows,
                document.edits.recipe().color.grading.midtones,
                document.edits.recipe().color.grading.highlights,
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
                neutral: TEMPERATURE_RANGE.neutral,
            },
            tint: AdjustmentRange {
                minimum: TINT_RANGE.minimum,
                maximum: TINT_RANGE.maximum,
                neutral: TINT_RANGE.neutral,
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
        export_ready: document.frame.is_some(),
        export_in_progress: document.export_status.is_some(),
        error: document.error.clone(),
        warning: document.warning.clone(),
        notice: document.notice.clone(),
        histogram: document.histogram,
        auto_tone_available: document.histogram_revision == Some(document.edits.revision()),
        picker_mode,
        color_mixer_channel,
    }
}

fn sample_rgb_patch(
    width: usize,
    height: usize,
    channels: usize,
    pixels: &[u8],
    normalized: egui::Pos2,
) -> Option<[u8; 3]> {
    if width == 0 || height == 0 || channels < 3 {
        return None;
    }
    let expected = width.checked_mul(height)?.checked_mul(channels)?;
    if pixels.len() < expected {
        return None;
    }
    let center_x = (normalized.x * (width.saturating_sub(1)) as f32).round() as usize;
    let center_y = (normalized.y * (height.saturating_sub(1)) as f32).round() as usize;
    let mut totals = [0_u32; 3];
    let mut count = 0_u32;
    for y in center_y.saturating_sub(2)..=(center_y + 2).min(height - 1) {
        for x in center_x.saturating_sub(2)..=(center_x + 2).min(width - 1) {
            let offset = y
                .checked_mul(width)?
                .checked_add(x)?
                .checked_mul(channels)?;
            for (channel, total) in totals.iter_mut().enumerate() {
                *total += u32::from(*pixels.get(offset + channel)?);
            }
            count += 1;
        }
    }
    (count > 0).then(|| totals.map(|total| ((total + count / 2) / count) as u8))
}

fn white_balance_from_camera_sample(
    sample: [f32; 3],
    as_shot_white_balance: [Option<f32>; 4],
) -> Option<WhiteBalance> {
    if sample
        .iter()
        .any(|value| !value.is_finite() || *value <= 1.0e-5)
    {
        return None;
    }
    let [red, green, blue, _] = as_shot_white_balance;
    let [Some(red), Some(green), Some(blue)] = [red, green, blue]
        .map(|value| value.filter(|number| number.is_finite() && *number > 1.0e-5))
    else {
        return None;
    };
    let as_shot_relative = [red / green, 1.0, blue / green];
    let desired_total = [sample[1] / sample[0], 1.0, sample[1] / sample[2]];
    let manual = [
        desired_total[0] / as_shot_relative[0],
        WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
        desired_total[2] / as_shot_relative[2],
    ];
    if manual
        .iter()
        .any(|value| !WHITE_BALANCE_MULTIPLIER_RANGE.contains(*value))
    {
        return None;
    }
    Some(WhiteBalance::ManualMultipliers {
        red: manual[0],
        green: manual[1],
        blue: manual[2],
    })
}

fn set_white_balance_mode(
    edits: &mut EditSession,
    mode: WhiteBalanceMode,
    memory: &mut WhiteBalanceModeMemory,
) -> bool {
    let mut next = edits.recipe().clone();
    next.color.white_balance = memory.select(next.color.white_balance, mode);
    edits.set_discrete(next)
}

fn apply_auto_tone(edits: &mut EditSession, histogram: &Histogram) -> bool {
    let midpoint = histogram.luminance_percentile(0.5);
    let midpoint_linear = srgb_to_linear_srgb(f32::from(midpoint) / 255.0);
    if midpoint_linear <= 0.0 {
        return false;
    }
    let mut next = edits.recipe().clone();
    // The histogram already includes the current recipe. Apply a correction
    // relative to the current exposure; assigning the correction as an
    // absolute value would make Auto tone move in the wrong direction after a
    // manual exposure edit.
    let exposure_correction = (0.18 / midpoint_linear).log2();
    if exposure_correction.abs() > AUTO_TONE_EXPOSURE_EPSILON_EV {
        next.light.exposure_ev = (edits.recipe().light.exposure_ev + exposure_correction)
            .clamp(EXPOSURE_EV_RANGE.minimum, EXPOSURE_EV_RANGE.maximum);
    }
    let low = histogram.luminance_percentile(0.01);
    let high = histogram.luminance_percentile(0.99);
    next.light.blacks = if low > 24 {
        -0.1
    } else if low == 0 {
        0.1
    } else {
        0.0
    };
    next.light.highlights = if high > 245 {
        -0.1
    } else if high < 220 {
        0.1
    } else {
        0.0
    };
    edits.set_discrete(next)
}

fn gpu_supports_recipe(recipe: &EditRecipe) -> bool {
    rohditor_gpu::GpuPreviewProcessor::supports_recipe(recipe)
}

fn gpu_upload_matches_document(document: &Document, ticket: PreviewTicket) -> bool {
    document.id == ticket.document_id
        && document.ticket() == ticket
        && document.preview_intent == PreviewIntent::CommittedFit
        && !document.source_scale_requested
        && gpu_supports_recipe(document.edits.recipe())
}

fn apply_adjustment_interaction(
    edits: &mut EditSession,
    interaction: AdjustmentInteraction,
) -> bool {
    if interaction.drag_started {
        edits.begin_gesture();
    }

    let mut next = edits.recipe().clone();
    match interaction.target {
        AdjustmentTarget::WhiteBalanceRed
        | AdjustmentTarget::WhiteBalanceGreen
        | AdjustmentTarget::WhiteBalanceBlue => {
            let (mut red, mut green, mut blue) = match next.color.white_balance {
                WhiteBalance::AsShot | WhiteBalance::TemperatureTint { .. } => (
                    WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
                    WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
                    WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
                ),
                WhiteBalance::ManualMultipliers { red, green, blue } => (red, green, blue),
            };
            match interaction.target {
                AdjustmentTarget::WhiteBalanceRed => red = interaction.value,
                AdjustmentTarget::WhiteBalanceGreen => green = interaction.value,
                AdjustmentTarget::WhiteBalanceBlue => blue = interaction.value,
                AdjustmentTarget::WhiteBalanceTemperature
                | AdjustmentTarget::WhiteBalanceTint
                | AdjustmentTarget::ToneCurveShadows
                | AdjustmentTarget::ToneCurveDarks
                | AdjustmentTarget::ToneCurveLights
                | AdjustmentTarget::ToneCurveHighlights
                | AdjustmentTarget::HslHue(_)
                | AdjustmentTarget::HslSaturation(_)
                | AdjustmentTarget::HslLuminance(_)
                | AdjustmentTarget::GradingShadows(_)
                | AdjustmentTarget::GradingMidtones(_)
                | AdjustmentTarget::GradingHighlights(_) => {}
                AdjustmentTarget::Exposure
                | AdjustmentTarget::Contrast
                | AdjustmentTarget::Highlights
                | AdjustmentTarget::Shadows
                | AdjustmentTarget::Whites
                | AdjustmentTarget::Blacks
                | AdjustmentTarget::Saturation
                | AdjustmentTarget::Vibrance => {}
            }
            next.color.white_balance = WhiteBalance::ManualMultipliers { red, green, blue };
        }
        AdjustmentTarget::WhiteBalanceTemperature | AdjustmentTarget::WhiteBalanceTint => {
            let (mut temperature, mut tint) = match next.color.white_balance {
                WhiteBalance::TemperatureTint { temperature, tint } => (temperature, tint),
                _ => (TEMPERATURE_RANGE.neutral, TINT_RANGE.neutral),
            };
            match interaction.target {
                AdjustmentTarget::WhiteBalanceTemperature => temperature = interaction.value,
                AdjustmentTarget::WhiteBalanceTint => tint = interaction.value,
                _ => unreachable!("temperature/tint branch only handles its controls"),
            }
            next.color.white_balance = WhiteBalance::TemperatureTint { temperature, tint };
        }
        AdjustmentTarget::Exposure => next.light.exposure_ev = interaction.value,
        AdjustmentTarget::Contrast => next.light.contrast = interaction.value,
        AdjustmentTarget::Highlights => next.light.highlights = interaction.value,
        AdjustmentTarget::Shadows => next.light.shadows = interaction.value,
        AdjustmentTarget::Whites => next.light.whites = interaction.value,
        AdjustmentTarget::Blacks => next.light.blacks = interaction.value,
        AdjustmentTarget::ToneCurveShadows => next.light.tone_curve.shadows = interaction.value,
        AdjustmentTarget::ToneCurveDarks => next.light.tone_curve.darks = interaction.value,
        AdjustmentTarget::ToneCurveLights => next.light.tone_curve.lights = interaction.value,
        AdjustmentTarget::ToneCurveHighlights => {
            next.light.tone_curve.highlights = interaction.value
        }
        AdjustmentTarget::Saturation => next.color.saturation = interaction.value,
        AdjustmentTarget::Vibrance => next.color.vibrance = interaction.value,
        AdjustmentTarget::HslHue(channel) => {
            next.color.hsl.channels[channel].hue = interaction.value
        }
        AdjustmentTarget::HslSaturation(channel) => {
            next.color.hsl.channels[channel].saturation = interaction.value
        }
        AdjustmentTarget::HslLuminance(channel) => {
            next.color.hsl.channels[channel].luminance = interaction.value
        }
        AdjustmentTarget::GradingShadows(channel) => {
            next.color.grading.shadows[channel] = interaction.value
        }
        AdjustmentTarget::GradingMidtones(channel) => {
            next.color.grading.midtones[channel] = interaction.value
        }
        AdjustmentTarget::GradingHighlights(channel) => {
            next.color.grading.highlights[channel] = interaction.value
        }
    }

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
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_worker_events(context);
        self.update_catalog();
        self.library_frame = self.library_frame.wrapping_add(1);
        self.refresh_gpu_queue_completion(context);
        self.refresh_gpu_histogram(context);
        self.show_top_bar(context);
        self.show_status_bar(context);
        match self.view_mode {
            ViewMode::Develop => {
                self.show_file_panel(context);
                self.show_adjustment_panel(context);
                self.show_crop_tool(context);
                self.show_filmstrip(context);
                self.show_viewport(context);
            }
            ViewMode::Library => self.show_library(context),
        }
        self.show_developer_diagnostics(context);
        // eframe normally wakes the event loop from the worker's
        // `request_repaint` callback. Keep a short polling repaint while work
        // is visible as a fallback for compositor/renderer combinations where
        // that wake-up is delayed (observed with glow on Wayland).
        if self.background_work_is_active() {
            context.request_repaint_after(Duration::from_millis(16));
        }
    }
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
        color,
    } = image;
    document.preview_pixels = Some(DisplayPreviewPixels {
        width,
        height,
        rgb: pixels,
    });
    let texture_name = format!("document-{}-preview", document.id);
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
    document.preview_source = Some(source);
}

fn source_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("developed")
        .to_owned()
}

fn display_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("RAW file")
        .to_owned()
}

#[cfg(test)]
mod tests {

    use image::{Rgb, RgbImage};
    use rohditor_image::{DisplayRgbImage, DisplayTransfer};

    use super::*;

    #[test]
    fn library_selection_moves_by_grid_steps_and_clamps() {
        use LibraryNavigation::{Down, End, Home, Left, Right, Up};

        assert_eq!(moved_library_selection(None, 3, 10, Right), 0);
        assert_eq!(moved_library_selection(None, 3, 10, Left), 0);
        assert_eq!(moved_library_selection(Some(4), 3, 10, Down), 7);
        assert_eq!(moved_library_selection(Some(4), 3, 10, Up), 1);
        assert_eq!(moved_library_selection(Some(4), 3, 10, Left), 3);
        assert_eq!(moved_library_selection(Some(9), 3, 10, Right), 9);
        assert_eq!(moved_library_selection(Some(9), 3, 10, Down), 9);
        assert_eq!(moved_library_selection(Some(5), 3, 10, Home), 0);
        assert_eq!(moved_library_selection(Some(5), 3, 10, End), 9);
        assert_eq!(moved_library_selection(Some(5), 1, 1, End), 0);
    }

    #[test]
    fn filmstrip_navigation_steps_and_stops_at_the_ends() {
        assert_eq!(adjacent_catalog_index(None, 10, true), None);
        assert_eq!(adjacent_catalog_index(Some(0), 10, false), None);
        assert_eq!(adjacent_catalog_index(Some(0), 10, true), Some(1));
        assert_eq!(adjacent_catalog_index(Some(8), 10, true), Some(9));
        assert_eq!(adjacent_catalog_index(Some(9), 10, true), None);
        assert_eq!(adjacent_catalog_index(Some(9), 10, false), Some(8));
        assert_eq!(adjacent_catalog_index(Some(0), 0, true), None);
    }

    #[test]
    fn library_texture_decoding_reduces_long_edges() {
        let image = RgbImage::from_pixel(400, 300, Rgb([200, 10, 10]));
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
            .encode_image(&image)
            .expect("encode grid texture source");
        let decoded = decode_library_texture(&bytes).expect("decode grid texture");
        assert_eq!(decoded.size, [256, 192]);

        let small = RgbImage::from_pixel(64, 64, Rgb([10, 200, 10]));
        let mut small_bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut small_bytes, 85)
            .encode_image(&small)
            .expect("encode small grid texture source");
        let small_decoded =
            decode_library_texture(&small_bytes).expect("decode small grid texture");
        assert_eq!(small_decoded.size, [64, 64]);
    }

    #[test]
    fn library_textures_evict_least_recently_used_beyond_the_cap() {
        let context = egui::Context::default();
        let mut textures = HashMap::new();
        for (frame, path) in [(1_u64, "a"), (2, "b"), (3, "c")] {
            let texture = context.load_texture(
                path,
                egui::ColorImage::from_rgb([1, 1], &[255, 0, 0]),
                egui::TextureOptions::LINEAR,
            );
            textures.insert(PathBuf::from(path), (texture, frame));
        }

        evict_least_recently_used(&mut textures, 2);

        assert_eq!(textures.len(), 2);
        assert!(!textures.contains_key(&PathBuf::from("a")));
        assert!(textures.contains_key(&PathBuf::from("c")));
    }

    #[test]
    fn export_ui_settings_map_to_ui_independent_core_settings() {
        let settings = ExportUiSettings {
            kind: ExportKind::Png,
            jpeg_quality: 42,
            png_depth: PngDepth::Sixteen,
            dither: true,
            safe_metadata: false,
            overwrite: true,
        }
        .core();

        assert_eq!(
            settings.format,
            ExportFormat::Png {
                bit_depth: PngBitDepth::Sixteen
            }
        );
        assert_eq!(settings.dithering, DitherMode::Ordered8x8);
        assert_eq!(settings.metadata, ExportMetadataPolicy::None);
        assert!(settings.overwrite);
    }

    #[test]
    fn export_name_uses_only_the_source_stem() {
        assert_eq!(
            source_stem(Path::new("/private/photos/DSC00001.ARW")),
            "DSC00001"
        );
        assert_eq!(source_stem(Path::new(".hidden")), ".hidden");
    }

    #[test]
    fn presentation_slider_events_preserve_one_undo_step_per_drag() {
        let mut edits = EditSession::default();
        for interaction in [
            AdjustmentInteraction {
                target: AdjustmentTarget::Exposure,
                value: 0.25,
                changed: true,
                drag_started: true,
                dragged: true,
                drag_stopped: false,
                reset: false,
            },
            AdjustmentInteraction {
                target: AdjustmentTarget::Exposure,
                value: 0.75,
                changed: true,
                drag_started: false,
                dragged: true,
                drag_stopped: false,
                reset: false,
            },
            AdjustmentInteraction {
                target: AdjustmentTarget::Exposure,
                value: 0.75,
                changed: false,
                drag_started: false,
                dragged: false,
                drag_stopped: true,
                reset: false,
            },
        ] {
            let _ = apply_adjustment_interaction(&mut edits, interaction);
        }

        assert_eq!(edits.revision(), 2);
        assert_eq!(edits.recipe().light.exposure_ev, 0.75);
        assert!(edits.undo());
        assert_eq!(edits.recipe().light.exposure_ev, EXPOSURE_EV_RANGE.neutral);
        assert!(!edits.undo());
    }

    #[test]
    fn white_balance_picker_uses_camera_native_channel_ratios() {
        let WhiteBalance::ManualMultipliers { red, green, blue } =
            white_balance_from_camera_sample(
                [0.5, 0.5, 0.5],
                [Some(1.0), Some(1.0), Some(1.0), None],
            )
            .expect("neutral sample")
        else {
            panic!("picker should produce manual multipliers");
        };
        assert!((red - 1.0).abs() < 1.0e-6);
        assert!((green - 1.0).abs() < 1.0e-6);
        assert!((blue - 1.0).abs() < 1.0e-6);

        let WhiteBalance::ManualMultipliers { red, blue, .. } = white_balance_from_camera_sample(
            [0.8, 0.5, 0.2],
            [Some(1.0), Some(1.0), Some(1.0), None],
        )
        .expect("colored sample") else {
            panic!("picker should produce manual multipliers");
        };
        assert!(red < 1.0);
        assert!(blue > 1.0);
        assert!(
            white_balance_from_camera_sample(
                [0.0, 0.5, 0.5],
                [Some(1.0), Some(1.0), Some(1.0), None]
            )
            .is_none()
        );
    }

    #[test]
    fn white_balance_picker_accounts_for_as_shot_baseline() {
        let WhiteBalance::ManualMultipliers { red, green, blue } =
            white_balance_from_camera_sample(
                [0.4, 0.5, 0.6],
                [Some(2.0), Some(1.0), Some(1.5), None],
            )
            .expect("sample should fit the manual range")
        else {
            panic!("picker should produce manual multipliers");
        };
        assert!((red - 0.625).abs() < 1.0e-6);
        assert!((green - 1.0).abs() < 1.0e-6);
        assert!((blue - (5.0 / 9.0)).abs() < 1.0e-6);
    }

    #[test]
    fn white_balance_mode_switch_preserves_last_editable_values() {
        let mut edits = EditSession::default();
        let mut memory = WhiteBalanceModeMemory::default();
        let mut recipe = EditRecipe::default();
        recipe.color.white_balance = WhiteBalance::TemperatureTint {
            temperature: 7_400.0,
            tint: -0.35,
        };
        assert!(edits.set_discrete(recipe));

        assert!(set_white_balance_mode(
            &mut edits,
            WhiteBalanceMode::ManualMultipliers,
            &mut memory,
        ));
        let mut recipe = edits.recipe().clone();
        recipe.color.white_balance = WhiteBalance::ManualMultipliers {
            red: 1.6,
            green: 0.8,
            blue: 1.2,
        };
        assert!(edits.set_discrete(recipe));
        assert!(set_white_balance_mode(
            &mut edits,
            WhiteBalanceMode::AsShot,
            &mut memory,
        ));

        assert!(set_white_balance_mode(
            &mut edits,
            WhiteBalanceMode::TemperatureTint,
            &mut memory,
        ));
        assert_eq!(
            edits.recipe().color.white_balance,
            WhiteBalance::TemperatureTint {
                temperature: 7_400.0,
                tint: -0.35,
            }
        );
        assert!(set_white_balance_mode(
            &mut edits,
            WhiteBalanceMode::ManualMultipliers,
            &mut memory,
        ));
        assert_eq!(
            edits.recipe().color.white_balance,
            WhiteBalance::ManualMultipliers {
                red: 1.6,
                green: 0.8,
                blue: 1.2,
            }
        );
    }

    #[test]
    fn color_mixer_picker_averages_a_bounded_display_patch() {
        let pixels = vec![
            255, 0, 0, 0, 255, 0, // first row
            0, 0, 255, 255, 255, 255, // second row
        ];
        assert_eq!(
            sample_rgb_patch(2, 2, 3, &pixels, egui::pos2(0.0, 0.0)),
            Some([128, 128, 128])
        );
        assert_eq!(
            sample_rgb_patch(2, 2, 3, &pixels[..9], egui::pos2(0.0, 0.0)),
            None
        );
    }

    #[test]
    fn installing_a_cpu_preview_preserves_the_existing_view() {
        let context = egui::Context::default();
        let mut document = Document::opening(7, PathBuf::from("fixture.arw"));
        document.view.actual_size(0.0);
        let pixels = vec![255, 0, 0, 0, 255, 0];

        install_texture(
            &context,
            &mut document,
            WorkerImage {
                width: 2,
                height: 1,
                color: egui::ColorImage::from_rgb([2, 1], &pixels),
                pixels,
            },
            PreviewSource::FastCpu,
        );

        assert!(!document.view.is_fit());
        assert_eq!(document.view.zoom_label(), "SOURCE 100%");
    }

    #[test]
    fn gpu_upload_guard_requires_the_current_revision_and_supported_recipe() {
        let mut document = Document::opening(7, PathBuf::from("fixture.arw"));
        let ticket = document.ticket();
        assert!(gpu_upload_matches_document(&document, ticket));

        let mut recipe = document.edits.recipe().clone();
        recipe.color.hsl.channels[0].saturation = 0.2;
        assert!(document.edits.set_discrete(recipe));
        assert!(!gpu_upload_matches_document(&document, ticket));
        assert!(!gpu_upload_matches_document(&document, document.ticket()));

        let current = document.ticket();
        let mut recipe = document.edits.recipe().clone();
        recipe.color.hsl = Default::default();
        assert!(document.edits.set_discrete(recipe));
        assert!(gpu_upload_matches_document(&document, document.ticket()));
        assert!(!gpu_upload_matches_document(&document, current));
    }

    #[test]
    fn source_scale_toolbar_selection_tracks_resolution_request() {
        let mut document = Document::opening(7, PathBuf::from("fixture.arw"));
        document.source_scale_requested = true;
        document.view.fit(0.0);

        assert!(source_scale_selected(Some(&document)));

        document.source_scale_requested = false;
        assert!(!source_scale_selected(Some(&document)));
    }

    #[test]
    fn auto_tone_applies_a_relative_exposure_correction() {
        let image = DisplayRgbImage::new(4, 1, 12, DisplayTransfer::Srgb, vec![128; 12])
            .expect("valid histogram fixture");
        let histogram = Histogram::from_display_rgb8(&image);
        let mut edits = EditSession::default();
        edits.set_discrete({
            let mut recipe = EditRecipe::default();
            recipe.light.exposure_ev = 1.0;
            recipe
        });

        assert!(apply_auto_tone(&mut edits, &histogram));
        let midpoint = srgb_to_linear_srgb(128.0 / 255.0);
        let expected = (1.0 + (0.18 / midpoint).log2())
            .clamp(EXPOSURE_EV_RANGE.minimum, EXPOSURE_EV_RANGE.maximum);
        assert!((edits.recipe().light.exposure_ev - expected).abs() < 1.0e-6);
    }

    #[test]
    fn auto_tone_stays_finite_and_clamped_for_extreme_display_histograms() {
        for value in [0_u8, 8, 32, 240, 255] {
            let image = DisplayRgbImage::new(8, 1, 24, DisplayTransfer::Srgb, vec![value; 24])
                .expect("valid histogram fixture");
            let histogram = Histogram::from_display_rgb8(&image);
            let mut edits = EditSession::default();
            assert!(apply_auto_tone(&mut edits, &histogram) || value == 0);
            assert!(edits.recipe().light.exposure_ev.is_finite());
            assert!(EXPOSURE_EV_RANGE.contains(edits.recipe().light.exposure_ev));
            assert!(HIGHLIGHTS_RANGE.contains(edits.recipe().light.highlights));
            assert!(BLACKS_RANGE.contains(edits.recipe().light.blacks));
        }
    }

    #[test]
    fn repeated_auto_tone_is_stable_when_the_histogram_is_already_neutral() {
        let image = DisplayRgbImage::new(4, 1, 12, DisplayTransfer::Srgb, vec![118; 12])
            .expect("valid histogram fixture");
        let histogram = Histogram::from_display_rgb8(&image);
        let mut edits = EditSession::default();
        assert!(apply_auto_tone(&mut edits, &histogram));
        let first = edits.recipe().clone();
        assert!(!apply_auto_tone(&mut edits, &histogram));
        assert_eq!(edits.recipe(), &first);
    }
}
