use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use rohditor_core::{
    BLACKS_RANGE, COLOR_GRADING_RANGE, CONTRAST_RANGE, DemosaicAlgorithm, DitherMode,
    EXPOSURE_EV_RANGE, ExportFormat, ExportMetadataPolicy, ExportSettings, HIGHLIGHTS_RANGE,
    HSL_HUE_RANGE, HSL_LUMINANCE_RANGE, HSL_SATURATION_RANGE, Histogram, JPEG_QUALITY_DEFAULT,
    MemoryEstimate, PngBitDepth, PreviewOptions, RenderOptions, SATURATION_RANGE, SHADOWS_RANGE,
    StageTimings, TEMPERATURE_RANGE, TINT_RANGE, TONE_CURVE_RANGE, VIBRANCE_RANGE,
    WHITE_BALANCE_MULTIPLIER_RANGE, WHITES_RANGE, WhiteBalance, paths_refer_to_same_file,
    srgb_to_linear_srgb,
};
use rohditor_raw::{RawFileInfo, RawFrame};
use tracing::{info, warn};

use crate::ProcessorPreference;
use crate::coordinator::{
    PreviewBackend, PreviewResolution, RenderCoordinator, WorkerImage, WorkerPreviewDiagnostics,
};
use crate::document::{EditSession, PreviewTicket};
use crate::preview_cache::PreviewCacheHits;
use crate::ui::adjustment_panel::{
    self, AdjustmentInteraction, AdjustmentRange, AdjustmentRanges, AdjustmentTarget,
    AdjustmentValues, DocumentPanelModel, ExportKind, ExportUiSettings, PngDepth, WhiteBalanceMode,
};
use crate::ui::diagnostics::{
    self, CacheModel, DiagnosticsMessages, DiagnosticsModel, GpuModel, PreviewModel, QueueModel,
    TimingModel,
};
use crate::ui::theme;
use crate::ui::toolbar::{self, FilePanelModel, StatusBarModel, ToolbarModel};
use crate::ui::viewport::{self, PreviewSource, PreviewTexture, ViewState, ViewportModel};
use rohditor_gpu::GpuPreviewUpload;

#[path = "app/events.rs"]
mod events;
#[path = "app/gpu.rs"]
mod gpu;

use gpu::{
    GpuDocumentPreview, GpuRuntime, gpu_output_size, initialize_gpu_runtime,
    register_or_update_gpu_texture,
};

#[derive(Debug, Clone, Copy)]
struct DocumentPreviewDiagnostics {
    worker: WorkerPreviewDiagnostics,
    gpu_upload_preparation: Option<Duration>,
    gpu_submission: Option<Duration>,
    gpu_queue_completion: Option<Duration>,
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
    preview_pixels: Option<Vec<u8>>,
    preview_source: Option<PreviewSource>,
    histogram: Option<Histogram>,
    source_scale_requested: bool,
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
            source_scale_requested: false,
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
        }
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
    white_balance_picker_active: bool,
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
        let mut application = Self {
            coordinator,
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
            white_balance_picker_active: false,
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
        self.white_balance_picker_active = false;
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
            .as_ref()
            .filter(|document| document.id == document_id)
            .and_then(|document| {
                document.frame.as_ref().map(|frame| {
                    (
                        document.ticket(),
                        Arc::clone(frame),
                        document.edits.recipe().clone(),
                    )
                })
            });
        let Some((ticket, frame, recipe)) = request else {
            return;
        };
        let source_scale_requested = self
            .document
            .as_ref()
            .is_some_and(|document| document.source_scale_requested);
        if source_scale_requested {
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
                    preview.source.white_balance() == recipe.color.white_balance
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
        if self.gpu.is_some() {
            self.release_document_gpu_preview(document_id);
        }
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

    fn install_gpu_upload(
        &mut self,
        context: &egui::Context,
        ticket: PreviewTicket,
        upload: GpuPreviewUpload,
        diagnostics: WorkerPreviewDiagnostics,
        upload_preparation: Duration,
    ) {
        let Some(document) = self.document.as_ref().filter(|document| {
            document.id == ticket.document_id
                && !document.source_scale_requested
                && document.edits.recipe().color.white_balance == upload.white_balance()
        }) else {
            return;
        };
        let recipe = document.edits.recipe().clone();
        let previous = self
            .document
            .as_mut()
            .and_then(|document| document.gpu_preview.take());
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
                let histogram = gpu_histogram(runtime, &frame)?;
                let texture_id =
                    register_or_update_gpu_texture(runtime, previous_texture_id, &frame);
                Ok::<_, String>((source, frame, texture_id, histogram))
            });

        match result {
            Ok((source, frame, texture_id, histogram)) => {
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
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|document| document.id == ticket.document_id)
                {
                    document.gpu_preview = Some(GpuDocumentPreview {
                        source,
                        frame,
                        texture_id,
                    });
                    document.preview_pixels = None;
                    document.texture = Some(PreviewTexture::Gpu {
                        id: texture_id,
                        size: output_size,
                    });
                    document.preview_source =
                        Some(PreviewSource::developed(diagnostics.algorithm, true));
                    document.histogram = Some(histogram);
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
        let texture_id = preview.texture_id;
        let recipe = document.edits.recipe().clone();
        let revision = document.edits.revision();
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
                let histogram = gpu_histogram(runtime, &frame)?;
                register_or_update_gpu_texture(runtime, Some(texture_id), &frame);
                Ok::<_, String>((frame, histogram))
            });

        match result {
            Ok((frame, histogram)) => {
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
                    demosaiced: true,
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
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|document| document.id == document_id)
                {
                    document.gpu_preview = Some(GpuDocumentPreview {
                        source: preview.source,
                        frame,
                        texture_id,
                    });
                    document.preview_pixels = None;
                    document.texture = Some(PreviewTexture::Gpu {
                        id: texture_id,
                        size: output_size,
                    });
                    document.preview_source =
                        Some(PreviewSource::developed(worker.algorithm, true));
                    document.histogram = Some(histogram);
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

    fn processor_description(&self) -> String {
        if let Some(runtime) = &self.gpu {
            let capabilities = runtime.processor.capabilities();
            format!(
                "GPU · {} · {}",
                capabilities.adapter_name, capabilities.backend
            )
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
            actual_size_selected: self
                .document
                .as_ref()
                .is_some_and(|document| document.view.is_actual_size()),
            zoom_label: self
                .document
                .as_ref()
                .map_or_else(|| "FIT".to_owned(), |document| document.view.zoom_label()),
            diagnostics_open: self.show_diagnostics,
            export_ready: self.document.as_ref().is_some_and(|document| {
                document.frame.is_some() && document.export_status.is_none()
            }),
        };
        let actions = toolbar::show_top(context, &model);
        if actions.toggle_diagnostics {
            self.show_diagnostics = !self.show_diagnostics;
        }
        if actions.open {
            self.open_dialog(context);
        } else if actions.close {
            self.close_document(context);
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
            if actions.actual_size {
                let changed_mode = !document.source_scale_requested;
                document.source_scale_requested = true;
                document.view.actual_size(now);
                if changed_mode {
                    view_changed_document = Some(document.id);
                }
            }
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
        let model = self
            .document
            .as_ref()
            .map(|document| document_panel_model(document, self.white_balance_picker_active));
        let output = adjustment_panel::show(context, model, &mut self.export_settings);
        let picker_state = output.white_balance_picker_active;
        let mut changed_document = None;
        if let Some(document) = self.document.as_mut() {
            if output.dismiss_error {
                document.error = None;
            }
            if output.dismiss_warning {
                document.warning = None;
            }

            let mut changed = false;
            if let Some(mode) = output.white_balance_mode {
                changed |= set_white_balance_mode(&mut document.edits, mode);
            }
            if output.auto_tone
                && let Some(histogram) = document.histogram.as_ref()
            {
                changed |= apply_auto_tone(&mut document.edits, histogram);
            }
            for interaction in output.interactions {
                changed |= apply_adjustment_interaction(&mut document.edits, interaction);
            }
            if output.reset_all {
                changed |= document.edits.reset();
            }
            if changed {
                document.notice = None;
                changed_document = Some(document.id);
            }
        }
        if let Some(document_id) = changed_document {
            self.queue_preview(context, document_id);
        }
        if let Some(active) = picker_state {
            self.white_balance_picker_active = active;
        }
        if output.export {
            self.request_export();
        }
    }

    fn show_viewport(&mut self, context: &egui::Context) {
        let output = if let Some(document) = self.document.as_mut() {
            let preparing = document.open_status.is_some() || document.preview_status.is_some();
            viewport::show(
                context,
                ViewportModel {
                    has_document: true,
                    preparing,
                    texture: document.texture.as_ref(),
                    source: document.preview_source,
                    white_balance_picker_active: self.white_balance_picker_active,
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
                    white_balance_picker_active: false,
                },
                &mut empty_view,
            )
        };
        if output.open {
            self.open_dialog(context);
        }
        if let Some(normalized) = output.white_balance_pick {
            self.apply_white_balance_pick(context, normalized);
        }
    }

    fn apply_white_balance_pick(&mut self, context: &egui::Context, normalized: egui::Pos2) {
        self.white_balance_picker_active = false;
        let sample = match self.sample_preview_pixel(normalized) {
            Ok(sample) => sample,
            Err(error) => {
                if let Some(document) = self.document.as_mut() {
                    document.error = Some(error);
                }
                return;
            }
        };
        let Some(balance) = white_balance_from_sample(sample) else {
            if let Some(document) = self.document.as_mut() {
                document.error = Some(
                    "Could not pick white balance from a nearly black preview pixel".to_owned(),
                );
            }
            return;
        };
        let mut changed_document = None;
        if let Some(document) = self.document.as_mut() {
            let mut next = document.edits.recipe().clone();
            next.color.white_balance = balance;
            if document.edits.set_discrete(next) {
                document.notice = Some("White balance picked from the preview".to_owned());
                document.error = None;
                changed_document = Some(document.id);
            }
        }
        if let Some(document_id) = changed_document {
            self.queue_preview(context, document_id);
        }
    }

    fn sample_preview_pixel(&self, normalized: egui::Pos2) -> Result<[u8; 3], String> {
        let Some(document) = self.document.as_ref() else {
            return Err("Open a photo before using the white-balance picker".to_owned());
        };
        if let Some(pixels) = document.preview_pixels.as_ref() {
            let Some(texture) = document.texture.as_ref() else {
                return Err("The preview is not ready for white-balance sampling".to_owned());
            };
            let (width, height) = texture.dimensions();
            let x = ((width.saturating_sub(1)) as f32 * normalized.x).round() as usize;
            let y = ((height.saturating_sub(1)) as f32 * normalized.y).round() as usize;
            let index = y
                .checked_mul(width)
                .and_then(|value| value.checked_add(x))
                .and_then(|value| value.checked_mul(3))
                .ok_or_else(|| "The preview pixel coordinate overflowed".to_owned())?;
            let pixel = pixels
                .get(index..index.saturating_add(3))
                .ok_or_else(|| "The preview pixel was outside the image".to_owned())?;
            return Ok([pixel[0], pixel[1], pixel[2]]);
        }

        let Some(preview) = document.gpu_preview.as_ref() else {
            return Err("The preview is not ready for white-balance sampling".to_owned());
        };
        let Some(runtime) = self.gpu.as_ref() else {
            return Err("The GPU preview is no longer available for sampling".to_owned());
        };
        let readback = runtime
            .processor
            .readback_display(&preview.frame)
            .map_err(|error| format!("Could not sample the GPU preview: {error}"))?;
        let width = usize::try_from(readback.width)
            .map_err(|_| "GPU preview width overflowed this system's usize".to_owned())?;
        let height = usize::try_from(readback.height)
            .map_err(|_| "GPU preview height overflowed this system's usize".to_owned())?;
        let x = ((width.saturating_sub(1)) as f32 * normalized.x).round() as usize;
        let y = ((height.saturating_sub(1)) as f32 * normalized.y).round() as usize;
        let index = y
            .checked_mul(width)
            .and_then(|value| value.checked_add(x))
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(|| "The GPU preview pixel coordinate overflowed".to_owned())?;
        let pixel = readback
            .rgba
            .get(index..index.saturating_add(4))
            .ok_or_else(|| "The GPU preview pixel was outside the image".to_owned())?;
        Ok([pixel[0], pixel[1], pixel[2]])
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
        toolbar::show_status(
            context,
            &StatusBarModel {
                processor: self.processor_description(),
                ui_renderer: format!("{} UI", self.ui_renderer),
                activity: (!activities.is_empty()).then(|| activities.join("  ·  ")),
                busy,
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

    fn background_work_is_active(&self) -> bool {
        self.document.as_ref().is_some_and(|document| {
            document.open_status.is_some()
                || document.preview_status.is_some()
                || document.export_status.is_some()
        })
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
                    textures_reused: preview.gpu_textures_reused,
                    resident_bytes: preview.gpu_resident_bytes,
                }),
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

fn document_panel_model(
    document: &Document,
    white_balance_picker_active: bool,
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
        has_adjustments: document.edits.recipe() != &rohditor_core::EditRecipe::default(),
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
        white_balance_picker_active,
    }
}

fn white_balance_from_sample(sample: [u8; 3]) -> Option<WhiteBalance> {
    let [red, green, blue] = sample.map(|value| srgb_to_linear_srgb(f32::from(value) / 255.0));
    if red <= 1.0e-4 || green <= 1.0e-4 || blue <= 1.0e-4 {
        return None;
    }
    Some(WhiteBalance::ManualMultipliers {
        red: (green / red).clamp(
            WHITE_BALANCE_MULTIPLIER_RANGE.minimum,
            WHITE_BALANCE_MULTIPLIER_RANGE.maximum,
        ),
        green: WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
        blue: (green / blue).clamp(
            WHITE_BALANCE_MULTIPLIER_RANGE.minimum,
            WHITE_BALANCE_MULTIPLIER_RANGE.maximum,
        ),
    })
}

fn set_white_balance_mode(edits: &mut EditSession, mode: WhiteBalanceMode) -> bool {
    let mut next = edits.recipe().clone();
    next.color.white_balance = match mode {
        WhiteBalanceMode::AsShot => WhiteBalance::AsShot,
        WhiteBalanceMode::TemperatureTint => match next.color.white_balance {
            WhiteBalance::TemperatureTint { temperature, tint } => {
                WhiteBalance::TemperatureTint { temperature, tint }
            }
            _ => WhiteBalance::TemperatureTint {
                temperature: TEMPERATURE_RANGE.neutral,
                tint: TINT_RANGE.neutral,
            },
        },
        WhiteBalanceMode::ManualMultipliers => match next.color.white_balance {
            WhiteBalance::ManualMultipliers { red, green, blue } => {
                WhiteBalance::ManualMultipliers { red, green, blue }
            }
            _ => WhiteBalance::ManualMultipliers {
                red: WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
                green: WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
                blue: WHITE_BALANCE_MULTIPLIER_RANGE.neutral,
            },
        },
    };
    edits.set_discrete(next)
}

fn apply_auto_tone(edits: &mut EditSession, histogram: &Histogram) -> bool {
    let midpoint = histogram.luminance_percentile(0.5);
    let midpoint_linear = srgb_to_linear_srgb(f32::from(midpoint) / 255.0);
    if midpoint_linear <= 0.0 {
        return false;
    }
    let mut next = edits.recipe().clone();
    next.light.exposure_ev = (0.18 / midpoint_linear)
        .log2()
        .clamp(EXPOSURE_EV_RANGE.minimum, EXPOSURE_EV_RANGE.maximum);
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

fn gpu_supports_recipe(recipe: &rohditor_core::EditRecipe) -> bool {
    recipe
        .color
        .hsl
        .channels
        .iter()
        .all(|channel| channel.hue == 0.0 && channel.saturation == 0.0 && channel.luminance == 0.0)
        && recipe.color.grading.shadows == [0.0; 3]
        && recipe.color.grading.midtones == [0.0; 3]
        && recipe.color.grading.highlights == [0.0; 3]
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
        self.refresh_gpu_queue_completion(context);
        self.show_top_bar(context);
        self.show_status_bar(context);
        self.show_file_panel(context);
        self.show_adjustment_panel(context);
        self.show_viewport(context);
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
        width: _,
        height: _,
        color,
        pixels,
    } = image;
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
            let now = context.input(|input| input.time);
            document.view.fit(now);
        }
    }
    document.preview_pixels = Some(pixels);
    document.preview_source = Some(source);
}

fn gpu_histogram(
    runtime: &GpuRuntime,
    frame: &rohditor_gpu::GpuPreviewFrame,
) -> Result<Histogram, String> {
    let readback = runtime
        .processor
        .readback_display(frame)
        .map_err(|error| format!("could not analyze GPU preview histogram: {error}"))?;
    Histogram::from_rgba8(
        usize::try_from(readback.width).map_err(|_| "GPU histogram width overflowed")?,
        usize::try_from(readback.height).map_err(|_| "GPU histogram height overflowed")?,
        &readback.rgba,
    )
    .ok_or_else(|| "GPU histogram buffer dimensions did not match its pixels".to_owned())
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
    use super::*;

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
    fn white_balance_picker_uses_linear_channel_ratios() {
        let WhiteBalance::ManualMultipliers { red, green, blue } =
            white_balance_from_sample([128, 128, 128]).expect("neutral sample")
        else {
            panic!("picker should produce manual multipliers");
        };
        assert!((red - 1.0).abs() < 1.0e-6);
        assert!((green - 1.0).abs() < 1.0e-6);
        assert!((blue - 1.0).abs() < 1.0e-6);

        let WhiteBalance::ManualMultipliers { red, blue, .. } =
            white_balance_from_sample([200, 128, 64]).expect("colored sample")
        else {
            panic!("picker should produce manual multipliers");
        };
        assert!(red < 1.0);
        assert!(blue > 1.0);
        assert!(white_balance_from_sample([0, 128, 128]).is_none());
    }
}
