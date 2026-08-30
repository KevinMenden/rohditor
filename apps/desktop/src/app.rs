use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use rohditor_core::{
    CONTRAST_RANGE, DitherMode, EXPOSURE_EV_RANGE, ExportFormat, ExportMetadataPolicy,
    ExportSettings, JPEG_QUALITY_DEFAULT, PngBitDepth, SATURATION_RANGE,
    WHITE_BALANCE_MULTIPLIER_RANGE, WhiteBalance, paths_refer_to_same_file,
};
use rohditor_gpu::{
    GpuCapabilities, GpuPreviewFrame, GpuPreviewProcessor, GpuPreviewSource, GpuPreviewUpload,
};
use rohditor_raw::{RawFileInfo, RawFrame};
use tracing::{info, warn};

use crate::ProcessorPreference;
use crate::coordinator::{JobKind, RenderCoordinator, WorkerEvent, WorkerImage};
use crate::document::{EditSession, PreviewTicket};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureKind {
    Embedded,
    DevelopedCpu,
    DevelopedGpu,
}

impl TextureKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Embedded => "Embedded preview (developing RAW…)",
            Self::DevelopedCpu => "CPU-developed RAW preview",
            Self::DevelopedGpu => "GPU-developed RAW preview",
        }
    }
}

#[derive(Clone)]
enum DocumentTexture {
    Cpu(egui::TextureHandle),
    Gpu {
        id: egui::TextureId,
        size: egui::Vec2,
    },
}

impl DocumentTexture {
    fn size_vec2(&self) -> egui::Vec2 {
        match self {
            Self::Cpu(texture) => texture.size_vec2(),
            Self::Gpu { size, .. } => *size,
        }
    }

    fn paint(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        match self {
            Self::Cpu(texture) => {
                egui::Image::new(texture)
                    .fit_to_exact_size(rect.size())
                    .texture_options(egui::TextureOptions::LINEAR)
                    .paint_at(ui, rect);
            }
            Self::Gpu { id, size } => {
                egui::Image::from_texture((*id, *size))
                    .fit_to_exact_size(rect.size())
                    .texture_options(egui::TextureOptions::LINEAR)
                    .paint_at(ui, rect);
            }
        }
    }
}

struct GpuDocumentPreview {
    source: GpuPreviewSource,
    frame: GpuPreviewFrame,
    texture_id: egui::TextureId,
}

struct GpuRuntime {
    render_state: eframe::egui_wgpu::RenderState,
    processor: GpuPreviewProcessor,
}

#[derive(Debug)]
struct ViewState {
    fit: bool,
    zoom: f32,
    pan: egui::Vec2,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            fit: true,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
        }
    }
}

impl ViewState {
    fn fit(&mut self) {
        self.fit = true;
        self.pan = egui::Vec2::ZERO;
    }

    fn actual_size(&mut self) {
        self.fit = false;
        self.zoom = 1.0;
        self.pan = egui::Vec2::ZERO;
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
    texture: Option<DocumentTexture>,
    texture_kind: Option<TextureKind>,
    gpu_preview: Option<GpuDocumentPreview>,
    view: ViewState,
    open_status: Option<String>,
    preview_status: Option<(u64, String)>,
    export_status: Option<ExportActivity>,
    last_preview_time: Option<Duration>,
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
            texture_kind: None,
            gpu_preview: None,
            view: ViewState::default(),
            open_status: Some("Opening RAW file".to_owned()),
            preview_status: None,
            export_status: None,
            last_preview_time: None,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportKind {
    Jpeg,
    Png,
}

#[derive(Debug, Clone)]
struct ExportUiSettings {
    kind: ExportKind,
    jpeg_quality: u8,
    png_depth: PngBitDepth,
    dither: bool,
    safe_metadata: bool,
    overwrite: bool,
}

impl Default for ExportUiSettings {
    fn default() -> Self {
        Self {
            kind: ExportKind::Jpeg,
            jpeg_quality: JPEG_QUALITY_DEFAULT,
            png_depth: PngBitDepth::Eight,
            dither: false,
            safe_metadata: true,
            overwrite: false,
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
                    bit_depth: self.png_depth,
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
    ui_renderer: &'static str,
    processor_preference: ProcessorPreference,
    gpu: Option<GpuRuntime>,
    processor_note: Option<String>,
    startup_error: Option<String>,
}

impl RohditorApp {
    pub(crate) fn new(
        context: &eframe::CreationContext<'_>,
        initial_path: Option<PathBuf>,
        processor_preference: ProcessorPreference,
    ) -> std::io::Result<Self> {
        context.egui_ctx.set_visuals(egui::Visuals::dark());
        let ui_renderer = if context.wgpu_render_state.is_some() {
            "wgpu"
        } else if context.gl.is_some() {
            "glow"
        } else {
            "unknown"
        };
        let (gpu, processor_note) = initialize_gpu_runtime(context, processor_preference)?;
        info!(
            ui_renderer,
            processor = if gpu.is_some() { "GPU" } else { "CPU" },
            processor_preference = ?processor_preference,
            "desktop application started"
        );
        let coordinator =
            RenderCoordinator::new(context.egui_ctx.clone()).map_err(std::io::Error::other)?;
        let mut application = Self {
            coordinator,
            document: None,
            next_document_id: 1,
            next_export_id: 1,
            export_settings: ExportUiSettings::default(),
            ui_renderer,
            processor_preference,
            gpu,
            processor_note,
            startup_error: None,
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
        if let Err(error) = self.coordinator.open(document_id, path)
            && let Some(document) = self.document.as_mut()
        {
            document.open_status = None;
            document.error = Some(error);
        }
        self.update_window_title(context);
    }

    fn close_document(&mut self, context: &egui::Context) {
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

    fn process_worker_events(&mut self, context: &egui::Context) {
        let events = self.coordinator.try_events().collect::<Vec<_>>();
        for event in events {
            self.process_worker_event(context, event);
        }
    }

    fn process_worker_event(&mut self, context: &egui::Context, event: WorkerEvent) {
        match event {
            WorkerEvent::Progress {
                document_id,
                job,
                revision,
                export_id,
                detail,
            } => {
                let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id)
                else {
                    return;
                };
                match job {
                    JobKind::Open => document.open_status = Some(detail),
                    JobKind::Preview => {
                        if revision == Some(document.edits.revision()) {
                            document.preview_status = Some((document.edits.revision(), detail));
                        }
                    }
                    JobKind::Export => {
                        if let Some(activity) = document.export_status.as_mut()
                            && export_id == Some(activity.id)
                        {
                            activity.detail = detail;
                        }
                    }
                }
            }
            WorkerEvent::MetadataReady { document_id, info } => {
                if let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id) {
                    document.info = Some(*info);
                }
            }
            WorkerEvent::PlaceholderReady { document_id, image } => {
                if let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id)
                    && !matches!(
                        document.texture_kind,
                        Some(TextureKind::DevelopedCpu | TextureKind::DevelopedGpu)
                    )
                {
                    install_texture(context, document, image, TextureKind::Embedded);
                }
            }
            WorkerEvent::RawReady { document_id, frame } => {
                let should_preview = if let Some(document) =
                    self.document.as_mut().filter(|doc| doc.id == document_id)
                {
                    document.info = Some(frame.info.clone());
                    document.frame = Some(frame);
                    document.open_status = None;
                    true
                } else {
                    false
                };
                if should_preview {
                    self.queue_preview(context, document_id);
                }
            }
            WorkerEvent::PreviewReady {
                ticket,
                image,
                timings,
            } => {
                if self.gpu.is_none()
                    && let Some(document) = self.document.as_mut()
                    && ticket.is_current(document.id, document.edits.revision())
                {
                    install_texture(context, document, image, TextureKind::DevelopedCpu);
                    document.preview_status = None;
                    document.last_preview_time = Some(timings.total);
                    document.error = None;
                }
            }
            WorkerEvent::GpuUploadReady {
                ticket,
                upload,
                base_timings,
                upload_preparation,
            } => {
                self.install_gpu_upload(context, ticket, *upload, base_timings, upload_preparation);
            }
            WorkerEvent::ExportReady {
                document_id,
                export_id,
                recipe_revision,
                destination,
                report,
                elapsed,
            } => {
                if let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id)
                    && document.export_status.as_ref().map(|job| job.id) == Some(export_id)
                {
                    document.export_status = None;
                    document.error = None;
                    document.notice = Some(format!(
                        "Exported revision {recipe_revision} as {}-bit {}×{} ({} bytes) to {} in {:.2} s.",
                        report.bit_depth.bits(),
                        report.width,
                        report.height,
                        report.bytes_written,
                        destination.display(),
                        elapsed.as_secs_f64()
                    ));
                }
            }
            WorkerEvent::Warning {
                document_id,
                message,
            } => {
                if let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id) {
                    document.warning = Some(message);
                }
            }
            WorkerEvent::Failed {
                document_id,
                job,
                revision,
                export_id,
                message,
            } => {
                let Some(document) = self.document.as_mut().filter(|doc| doc.id == document_id)
                else {
                    return;
                };
                let current = match job {
                    JobKind::Open => {
                        document.open_status = None;
                        true
                    }
                    JobKind::Preview => {
                        let current = revision == Some(document.edits.revision());
                        if current {
                            document.preview_status = None;
                        }
                        current
                    }
                    JobKind::Export => {
                        let current =
                            document.export_status.as_ref().map(|job| job.id) == export_id;
                        if current {
                            document.export_status = None;
                        }
                        current
                    }
                };
                if current {
                    document.error = Some(message);
                }
            }
            WorkerEvent::WorkerStopped { message } => {
                if let Some(document) = self.document.as_mut() {
                    document.open_status = None;
                    document.preview_status = None;
                    document.export_status = None;
                    document.error = Some(message);
                }
            }
        }
    }

    fn queue_preview(&mut self, context: &egui::Context, document_id: u64) {
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
        if self.gpu.is_some() {
            let gpu_base_is_current = self
                .document
                .as_ref()
                .and_then(|document| document.gpu_preview.as_ref())
                .is_some_and(|preview| preview.source.white_balance() == recipe.white_balance);
            if gpu_base_is_current {
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
        if let Some(document) = self.document.as_mut() {
            document.preview_status = Some((ticket.revision, "Queued CPU preview".to_owned()));
        }
        if let Err(error) = self.coordinator.preview(ticket, frame, recipe)
            && let Some(document) = self.document.as_mut()
        {
            document.preview_status = None;
            document.error = Some(error);
        }
    }

    fn install_gpu_upload(
        &mut self,
        context: &egui::Context,
        ticket: PreviewTicket,
        upload: GpuPreviewUpload,
        base_timings: rohditor_core::StageTimings,
        upload_preparation: Duration,
    ) {
        let Some(document) = self.document.as_ref().filter(|document| {
            document.id == ticket.document_id
                && document.edits.recipe().white_balance == upload.white_balance()
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
                let texture_id =
                    register_or_update_gpu_texture(runtime, previous_texture_id, &frame);
                Ok::<_, String>((source, frame, texture_id))
            });

        match result {
            Ok((source, frame, texture_id)) => {
                let output_size = gpu_output_size(frame.output_dimensions());
                let elapsed = base_timings.total + upload_preparation + frame.submission_time();
                info!(
                    document_id = ticket.document_id,
                    revision = ticket.revision,
                    width = frame.output_dimensions().0,
                    height = frame.output_dimensions().1,
                    base_ms = base_timings.total.as_millis(),
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
                    document.texture = Some(DocumentTexture::Gpu {
                        id: texture_id,
                        size: output_size,
                    });
                    document.texture_kind = Some(TextureKind::DevelopedGpu);
                    document.preview_status = None;
                    document.last_preview_time = Some(elapsed);
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
                    document.texture = Some(DocumentTexture::Gpu {
                        id: texture_id,
                        size: output_size,
                    });
                    document.texture_kind = Some(TextureKind::DevelopedGpu);
                    document.preview_status = None;
                    document.last_preview_time = document
                        .gpu_preview
                        .as_ref()
                        .map(|preview| preview.frame.submission_time());
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
        if matches!(document.texture, Some(DocumentTexture::Gpu { .. })) {
            document.texture = None;
            document.texture_kind = None;
        }
    }

    fn clear_orphaned_gpu_display(&mut self, document_id: u64, texture_id: egui::TextureId) {
        self.free_gpu_texture(texture_id);
        if let Some(document) = self
            .document
            .as_mut()
            .filter(|document| document.id == document_id)
            && matches!(
                document.texture,
                Some(DocumentTexture::Gpu { id, .. }) if id == texture_id
            )
        {
            document.texture = None;
            document.texture_kind = None;
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
        ) && let Some(document) = self.document.as_mut()
        {
            document.export_status = None;
            document.error = Some(error);
        }
    }

    fn show_top_bar(&mut self, context: &egui::Context) {
        let mut open = false;
        let mut close = false;
        let mut changed_document = None;
        egui::TopBottomPanel::top("top_bar").show(context, |ui| {
            ui.horizontal(|ui| {
                open = ui.button("Open RAW…").clicked();
                close = ui
                    .add_enabled(self.document.is_some(), egui::Button::new("Close"))
                    .clicked();
                ui.separator();
                if let Some(document) = self.document.as_mut() {
                    if ui
                        .add_enabled(document.edits.can_undo(), egui::Button::new("Undo"))
                        .clicked()
                        && document.edits.undo()
                    {
                        changed_document = Some(document.id);
                    }
                    if ui
                        .add_enabled(document.edits.can_redo(), egui::Button::new("Redo"))
                        .clicked()
                        && document.edits.redo()
                    {
                        changed_document = Some(document.id);
                    }
                    if ui.button("Reset edits").clicked() && document.edits.reset() {
                        changed_document = Some(document.id);
                    }
                    ui.separator();
                    if ui.button("Fit").clicked() {
                        document.view.fit();
                    }
                    if ui.button("100%").clicked() {
                        document.view.actual_size();
                    }
                    let zoom_label = if document.view.fit {
                        "Fit".to_owned()
                    } else {
                        format!("{:.0}%", document.view.zoom * 100.0)
                    };
                    ui.label(zoom_label);
                }
            });
        });
        if open {
            self.open_dialog(context);
        }
        if close {
            self.close_document(context);
        }
        if let Some(document_id) = changed_document {
            self.queue_preview(context, document_id);
        }
    }

    fn show_adjustment_panel(&mut self, context: &egui::Context) {
        let mut changed_document = None;
        let mut export = false;
        egui::SidePanel::right("adjustments")
            .default_width(310.0)
            .min_width(270.0)
            .show(context, |ui| {
                ui.heading("Adjustments");
                ui.separator();
                let Some(document) = self.document.as_mut() else {
                    ui.label("Open a Sony ARW file to begin.");
                    ui.add_space(8.0);
                    ui.weak("RAW decoding and image development run on the background CPU worker.");
                    return;
                };

                ui.strong(document.file_name());
                if let Some(info) = &document.info {
                    ui.label(format!("{} {}", info.clean_make, info.clean_model));
                    ui.label(format!("Sensor: {} × {}", info.width, info.height));
                }
                ui.label(format!("Recipe revision: {}", document.edits.revision()));
                ui.add_space(10.0);

                if show_recipe_controls(ui, &mut document.edits) {
                    changed_document = Some(document.id);
                    document.notice = None;
                }

                ui.add_space(12.0);
                ui.separator();
                ui.heading("Export");
                show_export_settings(ui, &mut self.export_settings);
                let ready = document.frame.is_some() && document.export_status.is_none();
                export = ui
                    .add_enabled(ready, egui::Button::new("Export…"))
                    .clicked();
                if document.frame.is_none() {
                    ui.weak("Export becomes available after RAW decoding.");
                }

                if let Some(message) = &document.error {
                    ui.add_space(10.0);
                    ui.colored_label(egui::Color32::LIGHT_RED, message);
                    if ui.small_button("Dismiss error").clicked() {
                        document.error = None;
                    }
                }
                if let Some(message) = &document.warning {
                    ui.add_space(10.0);
                    ui.colored_label(egui::Color32::YELLOW, message);
                    if ui.small_button("Dismiss warning").clicked() {
                        document.warning = None;
                    }
                }
                if let Some(message) = &document.notice {
                    ui.add_space(10.0);
                    ui.colored_label(egui::Color32::LIGHT_GREEN, message);
                }
            });
        if let Some(document_id) = changed_document {
            self.queue_preview(context, document_id);
        }
        if export {
            self.request_export();
        }
    }

    fn show_viewport(&mut self, context: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&context.style()).fill(egui::Color32::from_gray(18)))
            .show(context, |ui| {
                let viewport = ui.max_rect();
                let response = ui.allocate_rect(viewport, egui::Sense::drag());
                let Some(document) = self.document.as_mut() else {
                    ui.centered_and_justified(|ui| {
                        ui.label("Open a Sony ARW file to develop a photo");
                    });
                    return;
                };
                let Some(texture) = document.texture.clone() else {
                    ui.centered_and_justified(|ui| {
                        if document.open_status.is_some() || document.preview_status.is_some() {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Preparing preview…");
                            });
                        } else {
                            ui.label("No preview is available");
                        }
                    });
                    return;
                };

                let image_size = texture.size_vec2();
                let fit_scale = (viewport.width() / image_size.x)
                    .min(viewport.height() / image_size.y)
                    .max(0.01);
                if response.dragged_by(egui::PointerButton::Primary) {
                    if document.view.fit {
                        document.view.fit = false;
                        document.view.zoom = fit_scale;
                    }
                    document.view.pan += response.drag_delta();
                }
                if response.hovered() {
                    let scroll = context.input(|input| input.smooth_scroll_delta.y);
                    if scroll.abs() > f32::EPSILON {
                        let current = if document.view.fit {
                            fit_scale
                        } else {
                            document.view.zoom
                        };
                        document.view.fit = false;
                        document.view.zoom = (current * (scroll * 0.002).exp()).clamp(0.03, 16.0);
                    }
                }

                let scale = if document.view.fit {
                    fit_scale
                } else {
                    document.view.zoom
                };
                let size = image_size * scale;
                let image_rect =
                    egui::Rect::from_center_size(viewport.center() + document.view.pan, size);
                texture.paint(ui, image_rect);

                if let Some(kind) = document.texture_kind {
                    let badge = egui::Rect::from_min_size(
                        viewport.left_top() + egui::vec2(10.0, 10.0),
                        egui::vec2(245.0, 28.0),
                    );
                    ui.painter()
                        .rect_filled(badge, 4.0, egui::Color32::from_black_alpha(190));
                    ui.painter().text(
                        badge.center(),
                        egui::Align2::CENTER_CENTER,
                        kind.label(),
                        egui::FontId::proportional(13.0),
                        egui::Color32::WHITE,
                    );
                }
            });
    }

    fn show_status_bar(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "UI renderer: {}  ·  Processor: {}",
                    self.ui_renderer,
                    self.processor_description()
                ));
                if let Some(note) = &self.processor_note {
                    ui.separator();
                    ui.weak(note);
                }
                if let Some(document) = &self.document {
                    if let Some(status) = &document.open_status {
                        ui.separator();
                        ui.spinner();
                        ui.label(status);
                    } else if let Some((revision, status)) = &document.preview_status {
                        ui.separator();
                        ui.spinner();
                        ui.label(format!("{status} (revision {revision})"));
                    } else if let Some(duration) = document.last_preview_time {
                        ui.separator();
                        ui.label(format!(
                            "Preview revision {} in {:.0} ms",
                            document.edits.revision(),
                            duration.as_secs_f64() * 1_000.0
                        ));
                    }
                    if let Some(export) = &document.export_status {
                        ui.separator();
                        ui.spinner();
                        ui.label(format!(
                            "{} (recipe revision {})",
                            export.detail, export.recipe_revision
                        ));
                    }
                }
                if let Some(error) = &self.startup_error {
                    ui.separator();
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
            });
        });
    }
}

fn initialize_gpu_runtime(
    context: &eframe::CreationContext<'_>,
    preference: ProcessorPreference,
) -> std::io::Result<(Option<GpuRuntime>, Option<String>)> {
    match preference {
        ProcessorPreference::Cpu => Ok((None, None)),
        ProcessorPreference::Auto => match create_gpu_runtime(context) {
            Ok(runtime) => Ok((Some(runtime), None)),
            Err(error) => Ok((
                None,
                Some(format!("GPU unavailable; using CPU preview ({error})")),
            )),
        },
        ProcessorPreference::Gpu => create_gpu_runtime(context)
            .map(|runtime| (Some(runtime), None))
            .map_err(std::io::Error::other),
    }
}

fn create_gpu_runtime(context: &eframe::CreationContext<'_>) -> Result<GpuRuntime, String> {
    let render_state = context.wgpu_render_state.clone().ok_or_else(|| {
        "the selected UI renderer does not expose a shared wgpu device".to_owned()
    })?;
    let capabilities = GpuCapabilities::detect(
        &render_state.adapter,
        &render_state.device,
        render_state.target_format,
    );
    if !capabilities.is_hardware_adapter() {
        return Err(format!(
            "wgpu selected the {} CPU adapter; GPU processing is intentionally disabled",
            capabilities.adapter_name
        ));
    }
    let processor = GpuPreviewProcessor::new(
        &render_state.adapter,
        &render_state.device,
        &render_state.queue,
        render_state.target_format,
    )
    .map_err(|error| error.to_string())?;
    let capabilities = processor.capabilities();
    info!(
        adapter = capabilities.adapter_name,
        backend = capabilities.backend,
        device_type = capabilities.device_type,
        target_format = capabilities.target_format,
        timestamp_queries = capabilities.timestamp_queries,
        "GPU preview processor initialized from eframe's shared wgpu state"
    );
    Ok(GpuRuntime {
        render_state,
        processor,
    })
}

fn register_or_update_gpu_texture(
    runtime: &GpuRuntime,
    existing: Option<egui::TextureId>,
    frame: &GpuPreviewFrame,
) -> egui::TextureId {
    let mut renderer = runtime.render_state.renderer.write();
    if let Some(texture_id) = existing {
        renderer.update_egui_texture_from_wgpu_texture(
            &runtime.render_state.device,
            frame.display_view(),
            wgpu::FilterMode::Linear,
            texture_id,
        );
        texture_id
    } else {
        renderer.register_native_texture(
            &runtime.render_state.device,
            frame.display_view(),
            wgpu::FilterMode::Linear,
        )
    }
}

fn gpu_output_size((width, height): (u32, u32)) -> egui::Vec2 {
    egui::vec2(width as f32, height as f32)
}

impl eframe::App for RohditorApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_worker_events(context);
        self.show_top_bar(context);
        self.show_status_bar(context);
        self.show_adjustment_panel(context);
        self.show_viewport(context);
    }
}

fn show_recipe_controls(ui: &mut egui::Ui, edits: &mut EditSession) -> bool {
    let mut changed = false;
    let mut manual_white_balance = matches!(
        edits.recipe().white_balance,
        WhiteBalance::ManualMultipliers { .. }
    );
    let response = ui.checkbox(&mut manual_white_balance, "Manual white balance");
    if response.changed() {
        let mut next = edits.recipe().clone();
        next.white_balance = if manual_white_balance {
            WhiteBalance::ManualMultipliers {
                red: 1.0,
                green: 1.0,
                blue: 1.0,
            }
        } else {
            WhiteBalance::AsShot
        };
        changed |= edits.set_discrete(next);
    }
    ui.weak("Relative to the camera's as-shot multipliers");

    if let WhiteBalance::ManualMultipliers { red, green, blue } = edits.recipe().white_balance {
        for (label, channel, value) in [("Red", 0, red), ("Green", 1, green), ("Blue", 2, blue)] {
            let mut value = value;
            let response = ui.add(
                egui::Slider::new(
                    &mut value,
                    WHITE_BALANCE_MULTIPLIER_RANGE.minimum..=WHITE_BALANCE_MULTIPLIER_RANGE.maximum,
                )
                .text(label)
                .fixed_decimals(2),
            );
            let mut next = edits.recipe().clone();
            let WhiteBalance::ManualMultipliers {
                red: mut next_red,
                green: mut next_green,
                blue: mut next_blue,
            } = next.white_balance
            else {
                continue;
            };
            match channel {
                0 => next_red = value,
                1 => next_green = value,
                _ => next_blue = value,
            }
            next.white_balance = WhiteBalance::ManualMultipliers {
                red: next_red,
                green: next_green,
                blue: next_blue,
            };
            changed |= commit_slider_response(edits, &response, next);
        }
    }

    let mut exposure = edits.recipe().exposure_ev;
    let response = ui.add(
        egui::Slider::new(
            &mut exposure,
            EXPOSURE_EV_RANGE.minimum..=EXPOSURE_EV_RANGE.maximum,
        )
        .text("Exposure (EV)")
        .fixed_decimals(2),
    );
    let mut next = edits.recipe().clone();
    next.exposure_ev = exposure;
    changed |= commit_slider_response(edits, &response, next);

    let mut contrast = edits.recipe().contrast;
    let response = ui.add(
        egui::Slider::new(
            &mut contrast,
            CONTRAST_RANGE.minimum..=CONTRAST_RANGE.maximum,
        )
        .text("Contrast")
        .fixed_decimals(2),
    );
    let mut next = edits.recipe().clone();
    next.contrast = contrast;
    changed |= commit_slider_response(edits, &response, next);

    let mut saturation = edits.recipe().saturation;
    let response = ui.add(
        egui::Slider::new(
            &mut saturation,
            SATURATION_RANGE.minimum..=SATURATION_RANGE.maximum,
        )
        .text("Saturation")
        .fixed_decimals(2),
    );
    let mut next = edits.recipe().clone();
    next.saturation = saturation;
    changed |= commit_slider_response(edits, &response, next);

    if ui.button("Reset adjustments").clicked() {
        changed |= edits.reset();
    }
    changed
}

fn commit_slider_response(
    edits: &mut EditSession,
    response: &egui::Response,
    next: rohditor_core::EditRecipe,
) -> bool {
    if response.drag_started() {
        edits.begin_gesture();
    }
    let changed = if response.changed() {
        if response.dragged() || response.drag_stopped() || edits.gesture_active() {
            edits.set_during_gesture(next)
        } else {
            edits.set_discrete(next)
        }
    } else {
        false
    };
    if response.drag_stopped() {
        edits.finish_gesture();
    }
    changed
}

fn show_export_settings(ui: &mut egui::Ui, settings: &mut ExportUiSettings) {
    egui::ComboBox::from_label("Format")
        .selected_text(match settings.kind {
            ExportKind::Jpeg => "JPEG",
            ExportKind::Png => "PNG",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut settings.kind, ExportKind::Jpeg, "JPEG");
            ui.selectable_value(&mut settings.kind, ExportKind::Png, "PNG");
        });
    match settings.kind {
        ExportKind::Jpeg => {
            ui.add(egui::Slider::new(&mut settings.jpeg_quality, 1..=100).text("JPEG quality"));
        }
        ExportKind::Png => {
            egui::ComboBox::from_label("PNG depth")
                .selected_text(match settings.png_depth {
                    PngBitDepth::Eight => "8-bit",
                    PngBitDepth::Sixteen => "16-bit",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut settings.png_depth, PngBitDepth::Eight, "8-bit");
                    ui.selectable_value(&mut settings.png_depth, PngBitDepth::Sixteen, "16-bit");
                });
        }
    }
    ui.checkbox(&mut settings.dither, "Ordered output dithering");
    ui.checkbox(&mut settings.safe_metadata, "Include safe EXIF metadata");
    ui.checkbox(&mut settings.overwrite, "Allow replacing an existing file");
}

fn install_texture(
    context: &egui::Context,
    document: &mut Document,
    image: WorkerImage,
    kind: TextureKind,
) {
    let texture_name = format!("document-{}-preview", document.id);
    match document.texture.as_mut() {
        Some(DocumentTexture::Cpu(texture)) => {
            texture.set(image.color, egui::TextureOptions::LINEAR);
        }
        _ => {
            document.texture = Some(DocumentTexture::Cpu(context.load_texture(
                texture_name,
                image.color,
                egui::TextureOptions::LINEAR,
            )));
            document.view.fit();
        }
    }
    document.texture_kind = Some(kind);
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
            png_depth: PngBitDepth::Sixteen,
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
}
