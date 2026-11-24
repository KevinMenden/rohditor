//! Preview orchestration: routes each request to the CPU worker or the GPU,
//! keeps the resident GPU base current, and falls back to the CPU when the GPU
//! fails.
//!
//! [`PreviewRuntime`] holds what is shared by every document (the GPU
//! processor and the CPU/GPU policy); [`DocumentPreview`] holds the state of
//! one document. Every operation reports what the frontend must do next
//! through a [`PreviewOutcome`] instead of touching UI state.

use std::sync::Arc;
use std::time::Duration;

use rohditor_core::{
    HighlightDiagnostics, MemoryEstimate, PreviewOptions, StageTimings, cpu_only_stage_reason,
};
use rohditor_edit::EditRecipe;
use rohditor_gpu::GpuDisplayReadback;
use rohditor_raw::RawFrame;
use tracing::{info, warn};

use super::gpu::{GpuDocumentPreview, GpuDocumentSource, GpuRuntime, WgpuContext};
use super::routing::{
    gpu_base_algorithm_matches, gpu_supports_recipe, preview_intent_matches_resolution,
};
use super::{DocumentPreview, DocumentPreviewDiagnostics};
use crate::coordinator::{
    PreparedGpuSource, PreviewBackend, PreviewResolution, RenderCoordinator,
    WorkerPreviewDiagnostics,
};
use crate::document::{PreviewIntent, PreviewTicket};
use crate::preview_cache::PreviewCacheHits;

/// Polling interval while waiting on GPU completion or a readback.
const GPU_POLL_INTERVAL: Duration = Duration::from_millis(16);
/// Prefix of the processor note that explains a CPU-only recipe stage.
const CPU_ONLY_NOTE_PREFIX: &str = "CPU-only stage: ";

/// Which processor previews and exports should use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProcessorPreference {
    /// Prefer GPU previews and export color processing with CPU fallback.
    #[default]
    Auto,
    /// Require GPU previews; prefer GPU export with recovery to CPU if needed.
    Gpu,
    /// Use the CPU reference for previews and exports.
    Cpu,
}

impl ProcessorPreference {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Gpu => "GPU",
            Self::Cpu => "CPU",
        }
    }
}

/// A change to a message slot (such as a document's error line).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum MessageUpdate {
    #[default]
    Unchanged,
    Clear,
    Set(String),
}

impl MessageUpdate {
    pub fn apply(self, slot: &mut Option<String>) {
        match self {
            Self::Unchanged => {}
            Self::Clear => *slot = None,
            Self::Set(message) => *slot = Some(message),
        }
    }

    fn or(self, later: Self) -> Self {
        if later == Self::Unchanged {
            self
        } else {
            later
        }
    }
}

/// What a frontend must do after a preview operation.
#[derive(Debug, Default)]
pub struct PreviewOutcome {
    pub error: MessageUpdate,
    pub warning: MessageUpdate,
    /// The displayed frame changed: redraw. Also re-register the GPU texture
    /// when [`DocumentPreview::display_generation`] moved.
    pub repaint: bool,
}

impl PreviewOutcome {
    fn failed(message: String) -> Self {
        Self {
            error: MessageUpdate::Set(message),
            ..Self::default()
        }
    }

    /// Combine with the outcome of a follow-up operation; its messages win.
    fn then(mut self, later: Self) -> Self {
        self.error = self.error.or(later.error);
        self.warning = self.warning.or(later.warning);
        self.repaint |= later.repaint;
        self
    }
}

/// The document inputs of one preview operation. Owned, so the operation can
/// borrow the document's [`DocumentPreview`] mutably at the same time.
#[derive(Clone)]
pub struct PreviewRequest {
    /// The decoded RAW; `None` while the file is still opening.
    pub frame: Option<Arc<RawFrame>>,
    pub recipe: EditRecipe,
    pub revision: u64,
    pub options: PreviewOptions,
    /// Read the histogram back after every GPU frame, not only fit frames
    /// (the clipping overlay needs the pixels).
    pub histogram_always: bool,
    /// Frontend clock in seconds, for the view's zoom feedback.
    pub now: f64,
}

/// A GPU base the worker finished preparing (a `GpuUploadReady` event).
pub struct GpuUpload {
    pub ticket: PreviewTicket,
    pub prepared: PreparedGpuSource,
    pub diagnostics: WorkerPreviewDiagnostics,
    /// Worker time spent preparing the upload.
    pub upload_preparation: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResidentRender {
    Exact,
    WhiteBalanceDraft,
}

/// Processor choice and the GPU runtime shared by all documents.
pub struct PreviewRuntime {
    pub(super) gpu: Option<GpuRuntime>,
    preference: ProcessorPreference,
    note: Option<String>,
    startup_error: Option<String>,
}

impl PreviewRuntime {
    /// Set up the processor. `context` is the frontend's shared wgpu state, or
    /// `None` when its renderer does not expose one.
    pub fn new(preference: ProcessorPreference, context: Option<WgpuContext>) -> Self {
        let created = || {
            let context = context.ok_or_else(|| {
                "the selected UI renderer does not expose a shared wgpu device".to_owned()
            })?;
            GpuRuntime::new(context)
        };
        let (gpu, note, startup_error) = match preference {
            ProcessorPreference::Cpu => (None, None, None),
            ProcessorPreference::Auto => match created() {
                Ok(runtime) => (Some(runtime), None, None),
                Err(error) => (
                    None,
                    Some(format!("GPU unavailable; using CPU preview ({error})")),
                    None,
                ),
            },
            ProcessorPreference::Gpu => match created() {
                Ok(runtime) => (Some(runtime), None, None),
                Err(error) => (
                    None,
                    None,
                    Some(format!("GPU preview is required but unavailable: {error}")),
                ),
            },
        };
        Self {
            gpu,
            preference,
            note,
            startup_error,
        }
    }

    pub const fn gpu(&self) -> Option<&GpuRuntime> {
        self.gpu.as_ref()
    }

    pub const fn preference(&self) -> ProcessorPreference {
        self.preference
    }

    /// Why the processor is not what was asked for, or a routing note.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Set when the GPU was required but could not be started.
    pub fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    pub fn gpu_required_but_unavailable(&self) -> bool {
        self.preference == ProcessorPreference::Gpu && self.gpu.is_none()
    }

    /// Show why the CPU renders this recipe although the GPU is available.
    /// This is a routing choice, not a GPU failure: the GPU stays enabled.
    fn note_cpu_only_stage(&mut self, recipe: &EditRecipe) {
        match cpu_only_stage_reason(recipe) {
            Some(reason) if self.gpu.is_some() => {
                self.note = Some(format!("{CPU_ONLY_NOTE_PREFIX}{reason}"));
            }
            None if self
                .note
                .as_deref()
                .is_some_and(|note| note.starts_with(CPU_ONLY_NOTE_PREFIX)) =>
            {
                self.note = None;
            }
            _ => {}
        }
    }

    /// Queue the preview of the document's current recipe on whichever
    /// backend can serve it, rendering at once when a resident GPU base can.
    pub fn queue_preview(
        &mut self,
        coordinator: &RenderCoordinator,
        preview: &mut DocumentPreview,
        request: &PreviewRequest,
    ) -> PreviewOutcome {
        let request = &*preview.presented(request);
        if self.gpu_required_but_unavailable() {
            preview.status = None;
            return PreviewOutcome {
                error: self
                    .startup_error
                    .clone()
                    .map_or(MessageUpdate::Clear, MessageUpdate::Set),
                ..PreviewOutcome::default()
            };
        }
        let Some(frame) = request.frame.as_ref().map(Arc::clone) else {
            return PreviewOutcome::default();
        };
        let options = request.options;
        let recipe = request.recipe.clone();
        let source_scale_requested = preview.source_scale_requested;
        let ticket = preview.begin_request(
            request.revision,
            if source_scale_requested {
                PreviewIntent::SourceScale
            } else {
                PreviewIntent::CommittedFit
            },
        );
        // A histogram is only a valid Auto Tone input for the recipe that
        // produced it. Keep the old graph visible, but never apply it to
        // a newer revision.
        preview.histogram_revision = None;
        // Do not describe the previous backend as active while the new
        // recipe is still being rendered. The status bar can therefore
        // distinguish an old visible frame from the current work.
        preview.diagnostics = None;

        if source_scale_requested {
            // Keep the last complete Source 1:1 frame visible while the
            // worker prepares its replacement. The shared reservation budget
            // accounts for both frames and forces a safe CPU recovery first.
            preview.status = Some((
                ticket.revision,
                "Queued full-resolution 1:1 inspection".to_owned(),
            ));
            return match coordinator.source_scale_preview(
                ticket,
                frame,
                recipe,
                options,
                self.gpu.is_some(),
            ) {
                Ok(()) => PreviewOutcome::default(),
                Err(error) => {
                    preview.status = None;
                    PreviewOutcome::failed(error)
                }
            };
        }
        self.note_cpu_only_stage(&recipe);
        if self.gpu.is_some() && gpu_supports_recipe(&recipe) {
            let gpu_base_is_current = preview.gpu.as_ref().is_some_and(|resident| {
                gpu_base_algorithm_matches(
                    resident.algorithm,
                    options.render.demosaic,
                    preview.neural_ready,
                ) && resident.source.matches_recipe(&recipe)
                    && resident.source.optics_matches_recipe(&recipe.optics)
            });
            if gpu_base_is_current {
                coordinator.cancel_preview(preview.document_id());
                return self.render_gpu_preview(coordinator, preview, request);
            }
            preview.status = Some((
                ticket.revision,
                "Preparing linear base for GPU preview".to_owned(),
            ));
            return match coordinator.prepare_gpu_base(ticket, frame, recipe, options) {
                Ok(()) => PreviewOutcome::default(),
                Err(error) => {
                    preview.status = None;
                    PreviewOutcome::failed(error)
                }
            };
        }
        // Keep the last GPU frame installed while a CPU fallback
        // preview is rendered. Releasing it here leaves the viewport without
        // a texture for one or more frames and produces a visible black flash.
        // The worker-event handoff releases it immediately before installing
        // the completed CPU texture in the same UI update.
        preview.status = Some((ticket.revision, "Queued CPU preview".to_owned()));
        match coordinator.preview(ticket, frame, recipe, options) {
            Ok(()) => {
                info!(
                    document_id = ticket.document_id,
                    revision = ticket.revision,
                    processor_preference = ?self.preference,
                    "CPU preview queued"
                );
                PreviewOutcome::default()
            }
            Err(error) => {
                preview.status = None;
                PreviewOutcome::failed(error)
            }
        }
    }

    fn render_gpu_preview(
        &mut self,
        coordinator: &RenderCoordinator,
        preview: &mut DocumentPreview,
        request: &PreviewRequest,
    ) -> PreviewOutcome {
        let source_scale = preview
            .gpu
            .as_ref()
            .is_some_and(|resident| matches!(resident.source, GpuDocumentSource::Full(_)));
        if source_scale {
            // Source 1:1 must remain worker-owned even for a downstream-only
            // edit. Keep the current display installed until that ticket
            // returns; never make the UI thread submit a full image dispatch.
            return self.queue_preview(coordinator, preview, request);
        }
        self.render_resident(coordinator, preview, request, ResidentRender::Exact)
            .1
    }

    /// Apply a white-balance edit to the resident GPU source right away.
    /// Returns `false` (and does nothing) when the resident source cannot
    /// draft it, so the caller queues an exact rebuild instead.
    pub fn render_white_balance_draft(
        &mut self,
        coordinator: &RenderCoordinator,
        preview: &mut DocumentPreview,
        request: &PreviewRequest,
    ) -> (bool, PreviewOutcome) {
        let request = &*preview.presented(request);
        let supported = preview.gpu.as_ref().is_some_and(|resident| {
            gpu_supports_recipe(&request.recipe)
                && resident
                    .source
                    .supports_white_balance_draft(&request.recipe)
                && resident
                    .source
                    .optics_matches_recipe(&request.recipe.optics)
        });
        if !supported {
            return (false, PreviewOutcome::default());
        }
        coordinator.cancel_preview(preview.document_id());
        self.render_resident(
            coordinator,
            preview,
            request,
            ResidentRender::WhiteBalanceDraft,
        )
    }

    fn render_resident(
        &mut self,
        coordinator: &RenderCoordinator,
        preview: &mut DocumentPreview,
        request: &PreviewRequest,
        render: ResidentRender,
    ) -> (bool, PreviewOutcome) {
        let Some(resident) = preview.gpu.take() else {
            return (false, PreviewOutcome::default());
        };
        let GpuDocumentPreview {
            algorithm,
            resolution,
            source,
            frame,
            ..
        } = resident;
        // A histogram readback in flight goes on: it shows a recent frame, and
        // dropping it would mean no update until the drag pauses.
        preview.histogram_due = None;
        preview.histogram_revision = None;
        let revision = request.revision;
        let current_ticket = preview.ticket(revision);
        let previous_worker = preview.diagnostics.map(|diagnostics| diagnostics.worker);
        preview.status = Some((
            revision,
            match render {
                ResidentRender::Exact => "Applying edits to resident GPU preview",
                ResidentRender::WhiteBalanceDraft => {
                    "Applying interactive white balance on the GPU"
                }
            }
            .to_owned(),
        ));

        let output_policy = request.options.render.output_policy;
        let result = self
            .gpu
            .as_ref()
            .ok_or_else(|| "the GPU processor is no longer active while applying edits".to_owned())
            .and_then(|runtime| {
                let processor = runtime.processor();
                match (&source, render) {
                    (GpuDocumentSource::Reduced(source), ResidentRender::Exact) => {
                        processor.render(source, &request.recipe, output_policy, Some(frame))
                    }
                    (GpuDocumentSource::Reduced(source), ResidentRender::WhiteBalanceDraft) => {
                        processor.render_white_balance_draft(
                            source,
                            &request.recipe,
                            output_policy,
                            Some(frame),
                        )
                    }
                    (GpuDocumentSource::Full(_), ResidentRender::Exact) => {
                        return Err(
                            "full-resolution GPU inspection must be rebuilt by the worker"
                                .to_owned(),
                        );
                    }
                    (GpuDocumentSource::Full(_), ResidentRender::WhiteBalanceDraft) => {
                        return Err(
                            "full-resolution GPU inspection requires an exact white-balance rebuild"
                                .to_owned(),
                        );
                    }
                }
                .map_err(|error| error.to_string())
            });

        match result {
            Ok(frame) => {
                let submission = frame.submission_time();
                let mut worker = previous_worker.unwrap_or_else(|| draft_worker(algorithm));
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
                let diagnostics = DocumentPreviewDiagnostics {
                    worker,
                    gpu_upload_preparation: None,
                    gpu_submission: Some(submission),
                    gpu_queue_completion: frame.queue_completion_time(),
                    gpu_histogram_readback: None,
                    gpu_textures_reused: Some(frame.textures_reused()),
                    gpu_resident_bytes: source
                        .estimated_bytes()
                        .saturating_add(frame.estimated_bytes()),
                };
                info!(
                    document_id = preview.document_id(),
                    revision,
                    width = frame.output_dimensions().0,
                    height = frame.output_dimensions().1,
                    submission_us = submission.as_micros(),
                    white_balance_draft = render == ResidentRender::WhiteBalanceDraft,
                    "GPU preview adjustment complete"
                );
                let mut outcome = PreviewOutcome {
                    repaint: true,
                    ..PreviewOutcome::default()
                };
                if preview_intent_matches_resolution(preview.intent(), resolution) {
                    preview.install_gpu(
                        GpuDocumentPreview {
                            ticket: current_ticket,
                            algorithm,
                            resolution,
                            source,
                            frame,
                        },
                        request.histogram_always,
                    );
                    preview.last_time = Some(submission);
                    preview.diagnostics = Some(diagnostics);
                    outcome.error = MessageUpdate::Clear;
                } else {
                    // The viewport moved on to another presentation while the
                    // resident frame rendered; its frame is no longer wanted.
                    preview.release_gpu();
                }
                (true, outcome)
            }
            Err(error) => {
                // The old frame was consumed by the failed render.
                preview.release_gpu();
                (
                    false,
                    self.handle_gpu_failure(coordinator, preview, request, error),
                )
            }
        }
    }

    /// Adopt a GPU base the worker prepared and render it.
    pub fn install_gpu_upload(
        &mut self,
        coordinator: &RenderCoordinator,
        preview: &mut DocumentPreview,
        request: &PreviewRequest,
        upload: GpuUpload,
    ) -> PreviewOutcome {
        let request = &*preview.presented(request);
        let GpuUpload {
            ticket,
            prepared,
            diagnostics,
            upload_preparation,
        } = upload;
        if !preview.gpu_upload_matches(request.revision, ticket, &request.recipe) {
            return PreviewOutcome::default();
        }
        preview.pending_histogram = None;
        preview.histogram_due = None;
        let reusable_frame = preview.gpu.take().map(|resident| resident.frame);

        let output_policy = request.options.render.output_policy;
        let result = self
            .gpu
            .as_ref()
            .ok_or_else(|| {
                "the GPU processor is no longer active while its preview base was prepared"
                    .to_owned()
            })
            .and_then(|runtime| match prepared {
                PreparedGpuSource::Reduced(prepared) => {
                    let processor = runtime.processor();
                    let source = processor
                        .adopt_spatial_preview(prepared, &request.recipe)
                        .map_err(|error| error.to_string())?;
                    let frame = processor
                        .render(&source, &request.recipe, output_policy, reusable_frame)
                        .map_err(|error| error.to_string())?;
                    Ok((GpuDocumentSource::Reduced(Box::new(source)), frame))
                }
                // Source 1:1 development completed on the worker in
                // cancellable row bands; the UI only adopts its result.
                PreparedGpuSource::Full { source, frame } => {
                    Ok((GpuDocumentSource::Full(Box::new(source)), *frame))
                }
            });

        match result {
            Ok((source, frame)) => {
                let elapsed =
                    diagnostics.timings.total + upload_preparation + frame.submission_time();
                let preview_diagnostics = DocumentPreviewDiagnostics {
                    worker: diagnostics,
                    gpu_upload_preparation: Some(upload_preparation),
                    gpu_submission: Some(frame.submission_time()),
                    gpu_queue_completion: frame.queue_completion_time(),
                    gpu_histogram_readback: None,
                    gpu_textures_reused: Some(frame.textures_reused()),
                    gpu_resident_bytes: source
                        .estimated_bytes()
                        .saturating_add(frame.estimated_bytes()),
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
                let previous_source = preview.source;
                preview.install_gpu(
                    GpuDocumentPreview {
                        ticket,
                        algorithm: diagnostics.algorithm,
                        resolution: diagnostics.resolution,
                        source,
                        frame,
                    },
                    request.histogram_always,
                );
                if diagnostics.resolution == PreviewResolution::SourceScale {
                    preview
                        .view
                        .source_scale_frame_ready(previous_source, request.now);
                }
                preview.last_time = Some(elapsed);
                preview.diagnostics = Some(preview_diagnostics);
                PreviewOutcome {
                    error: MessageUpdate::Clear,
                    warning: MessageUpdate::Clear,
                    repaint: true,
                }
            }
            Err(error) => {
                // The previous frame was handed to the failed render.
                preview.release_gpu();
                self.handle_gpu_failure(coordinator, preview, request, error)
            }
        }
    }

    /// React to a GPU error. Under `Auto` the GPU is switched off for the rest
    /// of the run and the preview is re-queued on the CPU; otherwise the
    /// failure is reported.
    pub fn handle_gpu_failure(
        &mut self,
        coordinator: &RenderCoordinator,
        preview: &mut DocumentPreview,
        request: &PreviewRequest,
        error: String,
    ) -> PreviewOutcome {
        warn!(
            document_id = preview.document_id(),
            processor_preference = ?self.preference,
            %error,
            "GPU preview failed"
        );
        if self.preference != ProcessorPreference::Auto {
            preview.status = None;
            return PreviewOutcome::failed(format!("GPU preview failed: {error}"));
        }
        preview.release_gpu();
        preview.status = None;
        self.gpu = None;
        self.note = Some(format!("GPU fallback: {error}"));
        let outcome = PreviewOutcome {
            warning: MessageUpdate::Set(format!(
                "GPU preview failed ({error}); continuing with the CPU processor."
            )),
            repaint: true,
            ..PreviewOutcome::default()
        };
        outcome.then(self.queue_preview(coordinator, preview, request))
    }

    /// Let the GPU report finished work.
    pub fn poll_device(&self) {
        if let Some(runtime) = self.gpu.as_ref()
            && let Err(error) = runtime.processor().poll()
        {
            warn!(%error, "GPU polling failed while refreshing preview diagnostics");
        }
    }

    /// Poll the GPU and record the completion time of the displayed frame.
    /// Returns when to call again while a completion is outstanding.
    pub fn poll_completion(&self, preview: &mut DocumentPreview) -> Option<Duration> {
        self.poll_device();
        preview
            .refresh_queue_completion()
            .then_some(GPU_POLL_INTERVAL)
    }

    /// Read the displayed GPU frame back (a blocking diagnostics/picker path).
    pub fn readback_display(&self, preview: &DocumentPreview) -> Option<GpuDisplayReadback> {
        let resident = preview.gpu.as_ref()?;
        self.gpu
            .as_ref()?
            .processor()
            .readback_display(&resident.frame)
            .ok()
    }
}

/// Diagnostics for a resident re-render that has no earlier worker result.
fn draft_worker(algorithm: rohditor_demosaic::DemosaicAlgorithm) -> WorkerPreviewDiagnostics {
    WorkerPreviewDiagnostics {
        sensor_gpu: false,
        backend: PreviewBackend::GpuBase,
        resolution: PreviewResolution::Fit,
        algorithm,
        cache_hits: PreviewCacheHits::default(),
        timings: StageTimings::default(),
        highlight_diagnostics: HighlightDiagnostics::Off,
        capture_cpu_recovery: false,
        detail: None,
        output_gamut_diagnostics: None,
        memory: MemoryEstimate::default(),
        cache_resident_bytes: 0,
        workspace_reused: false,
        optics_applied: None,
        optics_used_infinity_distance_fallback: false,
        optics_scale: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinator() -> RenderCoordinator {
        RenderCoordinator::new(crate::wake::noop()).expect("worker starts")
    }

    #[test]
    fn message_updates_apply_to_a_slot() {
        let mut slot = Some("old".to_owned());
        MessageUpdate::Unchanged.apply(&mut slot);
        assert_eq!(slot.as_deref(), Some("old"));
        MessageUpdate::Set("new".to_owned()).apply(&mut slot);
        assert_eq!(slot.as_deref(), Some("new"));
        MessageUpdate::Clear.apply(&mut slot);
        assert_eq!(slot, None);
    }

    #[test]
    fn a_follow_up_outcome_overrides_only_the_messages_it_sets() {
        let first = PreviewOutcome {
            error: MessageUpdate::Set("first".to_owned()),
            warning: MessageUpdate::Set("fallback".to_owned()),
            repaint: false,
        };
        let later = PreviewOutcome {
            error: MessageUpdate::Clear,
            warning: MessageUpdate::Unchanged,
            repaint: true,
        };
        let merged = first.then(later);
        assert_eq!(merged.error, MessageUpdate::Clear);
        assert_eq!(merged.warning, MessageUpdate::Set("fallback".to_owned()));
        assert!(merged.repaint);
    }

    #[test]
    fn auto_falls_back_to_the_cpu_when_the_gpu_fails() {
        let mut runtime = PreviewRuntime::new(ProcessorPreference::Auto, None);
        let mut preview = DocumentPreview::new(1);
        preview.status = Some((3, "Applying edits".to_owned()));
        let request = request_without_frame();

        let outcome = runtime.handle_gpu_failure(
            &coordinator(),
            &mut preview,
            &request,
            "device lost".to_owned(),
        );

        assert_eq!(preview.status, None);
        assert!(runtime.gpu().is_none());
        assert_eq!(runtime.note(), Some("GPU fallback: device lost"));
        assert!(matches!(outcome.warning, MessageUpdate::Set(ref message)
            if message.contains("device lost") && message.contains("CPU")));
        assert!(outcome.repaint);
    }

    #[test]
    fn a_required_gpu_reports_the_failure_instead_of_falling_back() {
        let mut runtime = PreviewRuntime::new(ProcessorPreference::Gpu, None);
        let mut preview = DocumentPreview::new(1);
        let request = request_without_frame();

        let outcome = runtime.handle_gpu_failure(
            &coordinator(),
            &mut preview,
            &request,
            "device lost".to_owned(),
        );

        assert_eq!(
            outcome.error,
            MessageUpdate::Set("GPU preview failed: device lost".to_owned())
        );
        assert_eq!(outcome.warning, MessageUpdate::Unchanged);
        assert!(runtime.gpu_required_but_unavailable());
    }

    #[test]
    fn queueing_without_the_required_gpu_reports_the_startup_error() {
        let mut runtime = PreviewRuntime::new(ProcessorPreference::Gpu, None);
        let mut preview = DocumentPreview::new(1);
        preview.status = Some((1, "Queued".to_owned()));

        let outcome = runtime.queue_preview(&coordinator(), &mut preview, &request_without_frame());

        assert_eq!(preview.status, None);
        assert!(matches!(outcome.error, MessageUpdate::Set(ref message)
            if message.starts_with("GPU preview is required but unavailable")));
    }

    #[test]
    fn auto_without_a_wgpu_device_starts_on_the_cpu_with_a_note() {
        let runtime = PreviewRuntime::new(ProcessorPreference::Auto, None);
        assert!(runtime.gpu().is_none());
        assert!(runtime.startup_error().is_none());
        assert!(
            runtime
                .note()
                .is_some_and(|note| note.starts_with("GPU unavailable"))
        );
    }

    fn request_without_frame() -> PreviewRequest {
        PreviewRequest {
            frame: None,
            recipe: EditRecipe::default(),
            revision: 0,
            options: PreviewOptions::default(),
            histogram_always: false,
            now: 0.0,
        }
    }
}
