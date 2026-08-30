use std::any::Any;
use std::collections::HashSet;
use std::fmt;
use std::mem::size_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::egui;
use image::RgbImage;
use rohditor_core::{
    CancellationToken, CpuPipeline, EditRecipe, ExportReport, ExportSettings, MemoryEstimate,
    OrientationMap, PipelineError, PreviewOptions, StageTimings, export_image,
};
use rohditor_gpu::{GpuPreviewError, GpuPreviewUpload};
use rohditor_raw::{RawDecoder, RawFileInfo, RawFrame, RawOrientation, RawSession, RawlerDecoder};
use tracing::{info, info_span};

use crate::document::PreviewTicket;
use crate::preview_cache::{PreviewCache, PreviewCacheHits, PreviewCacheKeys};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobKind {
    Open,
    Preview,
    Export,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewBackend {
    Cpu,
    GpuBase,
}

impl PreviewBackend {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::GpuBase => "GPU base",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewQueueStats {
    pub requested: u64,
    pub coalesced: u64,
    pub cancellation_requests: u64,
    pub cancelled: u64,
    pub completed: u64,
    pub failed: u64,
    pub pending: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkerPreviewDiagnostics {
    pub backend: PreviewBackend,
    pub cache_hits: PreviewCacheHits,
    pub timings: StageTimings,
    pub memory: MemoryEstimate,
    pub cache_resident_bytes: usize,
    pub workspace_reused: bool,
}

impl fmt::Display for JobKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Open => "open",
            Self::Preview => "preview",
            Self::Export => "export",
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WorkerImage {
    pub width: usize,
    pub height: usize,
    pub color: egui::ColorImage,
}

impl WorkerImage {
    fn from_display(image: rohditor_core::DisplayRgbImage<u8>) -> Result<Self, String> {
        let width = image.width();
        let height = image.height();
        let packed_stride = width
            .checked_mul(3)
            .ok_or_else(|| "preview row size overflowed".to_owned())?;
        if image.row_stride() == packed_stride {
            return Self::from_rgb(width, height, image.into_data());
        }

        let capacity = packed_stride
            .checked_mul(height)
            .ok_or_else(|| "preview sample count overflowed".to_owned())?;
        let mut pixels = Vec::with_capacity(capacity);
        for row in image.data().chunks(image.row_stride()).take(height) {
            pixels.extend_from_slice(&row[..packed_stride]);
        }
        Self::from_rgb(width, height, pixels)
    }

    fn from_rgb(width: usize, height: usize, pixels: Vec<u8>) -> Result<Self, String> {
        let expected = width
            .checked_mul(height)
            .and_then(|count| count.checked_mul(3))
            .ok_or_else(|| "preview sample count overflowed".to_owned())?;
        if pixels.len() != expected {
            return Err(format!(
                "preview contains {} RGB bytes, expected {expected}",
                pixels.len()
            ));
        }
        let color = egui::ColorImage::from_rgb([width, height], &pixels);
        Ok(Self {
            width,
            height,
            color,
        })
    }
}

#[derive(Debug)]
pub(crate) enum WorkerEvent {
    Progress {
        document_id: u64,
        job: JobKind,
        revision: Option<u64>,
        export_id: Option<u64>,
        detail: String,
    },
    MetadataReady {
        document_id: u64,
        info: Box<RawFileInfo>,
    },
    PlaceholderReady {
        document_id: u64,
        image: WorkerImage,
    },
    RawReady {
        document_id: u64,
        frame: Arc<RawFrame>,
    },
    PreviewReady {
        ticket: PreviewTicket,
        image: WorkerImage,
        diagnostics: WorkerPreviewDiagnostics,
    },
    GpuUploadReady {
        ticket: PreviewTicket,
        upload: Box<GpuPreviewUpload>,
        diagnostics: WorkerPreviewDiagnostics,
        upload_preparation: Duration,
    },
    ExportReady {
        document_id: u64,
        export_id: u64,
        recipe_revision: u64,
        destination: PathBuf,
        report: ExportReport,
        elapsed: Duration,
    },
    Warning {
        document_id: u64,
        message: String,
    },
    Failed {
        document_id: u64,
        job: JobKind,
        revision: Option<u64>,
        export_id: Option<u64>,
        message: String,
    },
    WorkerStopped {
        message: String,
    },
}

#[derive(Debug)]
struct PreviewJob {
    ticket: PreviewTicket,
    frame: Arc<RawFrame>,
    recipe: EditRecipe,
    backend: PreviewBackend,
}

#[derive(Debug)]
struct ExportJob {
    document_id: u64,
    export_id: u64,
    recipe_revision: u64,
    destination: PathBuf,
    frame: Arc<RawFrame>,
    recipe: EditRecipe,
    settings: ExportSettings,
}

#[derive(Debug)]
enum WorkerRequest {
    Open { document_id: u64, path: PathBuf },
    PreviewAvailable,
    Export(ExportJob),
    AbandonDocument(u64),
    Shutdown,
}

#[derive(Debug)]
struct ActivePreview {
    ticket: PreviewTicket,
    cancellation: CancellationToken,
}

#[derive(Debug, Default)]
struct PreviewMailboxState {
    pending: Option<PreviewJob>,
    active: Option<ActivePreview>,
    wake_queued: bool,
    stats: PreviewQueueStats,
}

#[derive(Debug, Default)]
struct PreviewMailbox {
    state: Mutex<PreviewMailboxState>,
}

struct ScheduledPreview {
    job: PreviewJob,
    cancellation: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewCompletion {
    Completed,
    Cancelled,
    Failed,
}

impl PreviewMailbox {
    fn queue(&self, job: PreviewJob, requests: &mpsc::Sender<WorkerRequest>) -> Result<(), String> {
        let mut state = self.lock();
        state.stats.requested = state.stats.requested.saturating_add(1);

        if let Some(active) = &state.active
            && (active.ticket.document_id != job.ticket.document_id
                || should_replace_preview(active.ticket, job.ticket))
            && !active.cancellation.is_cancelled()
        {
            active.cancellation.cancel();
            state.stats.cancellation_requests = state.stats.cancellation_requests.saturating_add(1);
        }

        let replace_pending = state.pending.as_ref().is_none_or(|pending| {
            pending.ticket.document_id != job.ticket.document_id
                || should_replace_preview(pending.ticket, job.ticket)
        });
        if replace_pending {
            if state.pending.replace(job).is_some() {
                state.stats.coalesced = state.stats.coalesced.saturating_add(1);
            }
        } else {
            state.stats.coalesced = state.stats.coalesced.saturating_add(1);
            return Ok(());
        }

        if state.wake_queued {
            return Ok(());
        }
        state.wake_queued = true;
        drop(state);

        if requests.send(WorkerRequest::PreviewAvailable).is_err() {
            let mut state = self.lock();
            state.pending = None;
            state.wake_queued = false;
            return Err("the background CPU worker stopped unexpectedly".to_owned());
        }
        Ok(())
    }

    fn take(&self) -> Option<ScheduledPreview> {
        let mut state = self.lock();
        state.wake_queued = false;
        let job = state.pending.take()?;
        let cancellation = CancellationToken::new();
        state.active = Some(ActivePreview {
            ticket: job.ticket,
            cancellation: cancellation.clone(),
        });
        Some(ScheduledPreview { job, cancellation })
    }

    fn finish(&self, ticket: PreviewTicket, completion: PreviewCompletion) {
        let mut state = self.lock();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.ticket == ticket)
        {
            state.active = None;
        }
        match completion {
            PreviewCompletion::Completed => {
                state.stats.completed = state.stats.completed.saturating_add(1);
            }
            PreviewCompletion::Cancelled => {
                state.stats.cancelled = state.stats.cancelled.saturating_add(1);
            }
            PreviewCompletion::Failed => {
                state.stats.failed = state.stats.failed.saturating_add(1);
            }
        }
    }

    fn abandon(&self, document_id: u64) {
        let mut state = self.lock();
        if state
            .pending
            .as_ref()
            .is_some_and(|job| job.ticket.document_id == document_id)
        {
            state.pending = None;
            state.stats.coalesced = state.stats.coalesced.saturating_add(1);
        }
        if let Some(active) = &state.active
            && active.ticket.document_id == document_id
            && !active.cancellation.is_cancelled()
        {
            active.cancellation.cancel();
            state.stats.cancellation_requests = state.stats.cancellation_requests.saturating_add(1);
        }
    }

    fn cancel_all(&self) {
        let mut state = self.lock();
        state.pending = None;
        if let Some(active) = &state.active
            && !active.cancellation.is_cancelled()
        {
            active.cancellation.cancel();
            state.stats.cancellation_requests = state.stats.cancellation_requests.saturating_add(1);
        }
    }

    fn stats(&self) -> PreviewQueueStats {
        let state = self.lock();
        PreviewQueueStats {
            pending: state.pending.is_some(),
            active: state.active.is_some(),
            ..state.stats
        }
    }

    fn lock(&self) -> MutexGuard<'_, PreviewMailboxState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(crate) struct RenderCoordinator {
    requests: mpsc::Sender<WorkerRequest>,
    events: mpsc::Receiver<WorkerEvent>,
    previews: Arc<PreviewMailbox>,
    worker: Option<JoinHandle<()>>,
}

impl RenderCoordinator {
    pub(crate) fn new(context: egui::Context) -> Result<Self, String> {
        Self::new_with_decoder(context, Arc::new(RawlerDecoder::default()))
    }

    fn new_with_decoder(
        context: egui::Context,
        decoder: Arc<dyn RawDecoder>,
    ) -> Result<Self, String> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let previews = Arc::new(PreviewMailbox::default());
        let worker_previews = Arc::clone(&previews);
        let stopped_sender = event_sender.clone();
        let stopped_context = context.clone();
        let worker = thread::Builder::new()
            .name("rohditor-cpu-worker".to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    worker_loop(
                        request_receiver,
                        event_sender,
                        context,
                        decoder,
                        worker_previews,
                    );
                }));
                if let Err(payload) = result {
                    send_event(
                        &stopped_sender,
                        &stopped_context,
                        WorkerEvent::WorkerStopped {
                            message: format!(
                                "The background CPU worker stopped unexpectedly: {}",
                                panic_message(payload.as_ref())
                            ),
                        },
                    );
                }
            })
            .map_err(|error| format!("could not start the CPU worker: {error}"))?;
        Ok(Self {
            requests: request_sender,
            events: event_receiver,
            previews,
            worker: Some(worker),
        })
    }

    pub(crate) fn open(&self, document_id: u64, path: PathBuf) -> Result<(), String> {
        self.send(WorkerRequest::Open { document_id, path })
    }

    pub(crate) fn preview(
        &self,
        ticket: PreviewTicket,
        frame: Arc<RawFrame>,
        recipe: EditRecipe,
    ) -> Result<(), String> {
        self.previews.queue(
            PreviewJob {
                ticket,
                frame,
                recipe,
                backend: PreviewBackend::Cpu,
            },
            &self.requests,
        )
    }

    pub(crate) fn prepare_gpu_base(
        &self,
        ticket: PreviewTicket,
        frame: Arc<RawFrame>,
        recipe: EditRecipe,
    ) -> Result<(), String> {
        self.previews.queue(
            PreviewJob {
                ticket,
                frame,
                recipe,
                backend: PreviewBackend::GpuBase,
            },
            &self.requests,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn export(
        &self,
        document_id: u64,
        export_id: u64,
        recipe_revision: u64,
        destination: PathBuf,
        frame: Arc<RawFrame>,
        recipe: EditRecipe,
        settings: ExportSettings,
    ) -> Result<(), String> {
        self.send(WorkerRequest::Export(ExportJob {
            document_id,
            export_id,
            recipe_revision,
            destination,
            frame,
            recipe,
            settings,
        }))
    }

    pub(crate) fn abandon(&self, document_id: u64) {
        self.previews.abandon(document_id);
        drop(
            self.requests
                .send(WorkerRequest::AbandonDocument(document_id)),
        );
    }

    pub(crate) fn try_events(&self) -> impl Iterator<Item = WorkerEvent> + '_ {
        self.events.try_iter()
    }

    pub(crate) fn preview_queue_stats(&self) -> PreviewQueueStats {
        self.previews.stats()
    }

    fn send(&self, request: WorkerRequest) -> Result<(), String> {
        self.requests
            .send(request)
            .map_err(|_| "the background CPU worker stopped unexpectedly".to_owned())
    }
}

impl Drop for RenderCoordinator {
    fn drop(&mut self) {
        // Do not join here: a close action must never wait for an in-flight RAW
        // render. Reap an already-finished thread, while an active worker exits
        // after its current operation observes shutdown or channel disconnect.
        self.previews.cancel_all();
        drop(self.requests.send(WorkerRequest::Shutdown));
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(worker) = self.worker.take()
        {
            drop(worker.join());
        }
    }
}

fn worker_loop(
    receiver: mpsc::Receiver<WorkerRequest>,
    sender: mpsc::Sender<WorkerEvent>,
    context: egui::Context,
    decoder: Arc<dyn RawDecoder>,
    previews: Arc<PreviewMailbox>,
) {
    let mut abandoned = HashSet::new();
    let mut preview_cache = PreviewCache::default();

    while let Ok(request) = receiver.recv() {
        if request_belongs_to_abandoned_document(&request, &abandoned) {
            continue;
        }

        match request {
            WorkerRequest::Open { document_id, path } => {
                abandoned.remove(&document_id);
                process_open(document_id, &path, &sender, &context, decoder.as_ref());
            }
            WorkerRequest::PreviewAvailable => {
                let Some(scheduled) = previews.take() else {
                    continue;
                };
                let ticket = scheduled.job.ticket;
                let completion = if abandoned.contains(&ticket.document_id) {
                    PreviewCompletion::Cancelled
                } else {
                    match scheduled.job.backend {
                        PreviewBackend::Cpu => process_preview(
                            scheduled.job,
                            &sender,
                            &context,
                            &scheduled.cancellation,
                            &mut preview_cache,
                        ),
                        PreviewBackend::GpuBase => process_gpu_base(
                            scheduled.job,
                            &sender,
                            &context,
                            &scheduled.cancellation,
                            &mut preview_cache,
                        ),
                    }
                };
                previews.finish(ticket, completion);
            }
            WorkerRequest::Export(job) => {
                if !abandoned.contains(&job.document_id) {
                    process_export(job, &sender, &context);
                }
            }
            WorkerRequest::AbandonDocument(document_id) => {
                abandoned.clear();
                abandoned.insert(document_id);
                preview_cache.clear_document(document_id);
            }
            WorkerRequest::Shutdown => break,
        };
    }
}

fn request_belongs_to_abandoned_document(
    request: &WorkerRequest,
    abandoned: &HashSet<u64>,
) -> bool {
    request
        .document_id()
        .is_some_and(|document_id| abandoned.contains(&document_id))
}

impl WorkerRequest {
    const fn document_id(&self) -> Option<u64> {
        match self {
            Self::Open { document_id, .. } | Self::AbandonDocument(document_id) => {
                Some(*document_id)
            }
            Self::Export(job) => Some(job.document_id),
            Self::PreviewAvailable | Self::Shutdown => None,
        }
    }
}

fn should_replace_preview(current: PreviewTicket, candidate: PreviewTicket) -> bool {
    current.document_id == candidate.document_id && candidate.revision >= current.revision
}

fn process_open(
    document_id: u64,
    path: &Path,
    sender: &mpsc::Sender<WorkerEvent>,
    context: &egui::Context,
    decoder: &dyn RawDecoder,
) {
    let file_name = display_file_name(path);
    let span = info_span!("desktop.open", document_id, file = %file_name);
    let _guard = span.enter();
    send_progress(
        sender,
        context,
        document_id,
        JobKind::Open,
        None,
        None,
        "Reading RAW metadata",
    );
    let mut session = match decoder.open(path) {
        Ok(session) => session,
        Err(error) => {
            send_failure(
                sender,
                context,
                document_id,
                JobKind::Open,
                None,
                None,
                format!("Could not open {file_name}: {error}"),
            );
            return;
        }
    };
    let info = match session.probe() {
        Ok(info) => info,
        Err(error) => {
            send_failure(
                sender,
                context,
                document_id,
                JobKind::Open,
                None,
                None,
                format!("Could not open {file_name}: {error}"),
            );
            return;
        }
    };
    send_event(
        sender,
        context,
        WorkerEvent::MetadataReady {
            document_id,
            info: Box::new(info.clone()),
        },
    );

    process_embedded_placeholder(
        document_id,
        info.orientation,
        session.as_mut(),
        sender,
        context,
    );

    send_progress(
        sender,
        context,
        document_id,
        JobKind::Open,
        None,
        None,
        "Decoding sensor data",
    );
    match session.decode() {
        Ok(frame) => {
            info!(
                width = frame.info.width,
                height = frame.info.height,
                "RAW decoded"
            );
            send_event(
                sender,
                context,
                WorkerEvent::RawReady {
                    document_id,
                    frame: Arc::new(frame),
                },
            );
        }
        Err(error) => send_failure(
            sender,
            context,
            document_id,
            JobKind::Open,
            None,
            None,
            format!("Could not decode {file_name}: {error}"),
        ),
    }
}

fn process_embedded_placeholder(
    document_id: u64,
    orientation: RawOrientation,
    session: &mut dyn RawSession,
    sender: &mpsc::Sender<WorkerEvent>,
    context: &egui::Context,
) {
    send_progress(
        sender,
        context,
        document_id,
        JobKind::Open,
        None,
        None,
        "Loading embedded preview",
    );
    match session.embedded_preview() {
        Ok(Some(preview)) => match decode_placeholder(&preview.bytes, orientation) {
            Ok(image) => send_event(
                sender,
                context,
                WorkerEvent::PlaceholderReady { document_id, image },
            ),
            Err(error) => send_event(
                sender,
                context,
                WorkerEvent::Warning {
                    document_id,
                    message: format!(
                        "The embedded preview could not be displayed ({error}); RAW development will continue."
                    ),
                },
            ),
        },
        Ok(None) => {}
        Err(error) => send_event(
            sender,
            context,
            WorkerEvent::Warning {
                document_id,
                message: format!(
                    "The embedded preview could not be extracted ({error}); RAW development will continue."
                ),
            },
        ),
    }
}

fn process_preview(
    job: PreviewJob,
    sender: &mpsc::Sender<WorkerEvent>,
    context: &egui::Context,
    cancellation: &CancellationToken,
    preview_cache: &mut PreviewCache,
) -> PreviewCompletion {
    let span = info_span!(
        "desktop.preview",
        document_id = job.ticket.document_id,
        revision = job.ticket.revision
    );
    let _guard = span.enter();
    let options = PreviewOptions::default();
    let keys = PreviewCacheKeys::new(job.ticket.document_id, &job.frame, &job.recipe, options);
    let cache_hits = preview_cache.prepare(&keys, &job.frame);
    send_progress(
        sender,
        context,
        job.ticket.document_id,
        JobKind::Preview,
        Some(job.ticket.revision),
        None,
        if cache_hits.adjusted {
            "Restoring cached adjusted CPU preview"
        } else if cache_hits.demosaiced {
            "Applying edits to cached demosaiced CPU base"
        } else if cache_hits.normalized {
            "Demosaicing cached normalized CPU preview"
        } else {
            "Developing 2560 px CPU preview"
        },
    );
    match develop_preview(
        &job,
        options,
        &keys,
        cache_hits,
        cancellation,
        preview_cache,
    ) {
        Ok((display, diagnostics)) => match WorkerImage::from_display(display) {
            Ok(image) => {
                log_preview_diagnostics(job.ticket, image.width, image.height, diagnostics);
                send_event(
                    sender,
                    context,
                    WorkerEvent::PreviewReady {
                        ticket: job.ticket,
                        image,
                        diagnostics,
                    },
                );
                PreviewCompletion::Completed
            }
            Err(error) => {
                send_failure(
                    sender,
                    context,
                    job.ticket.document_id,
                    JobKind::Preview,
                    Some(job.ticket.revision),
                    None,
                    format!("Could not prepare the CPU preview for display: {error}"),
                );
                PreviewCompletion::Failed
            }
        },
        Err(PipelineError::Cancelled) => {
            info!("CPU preview cancelled after being superseded");
            PreviewCompletion::Cancelled
        }
        Err(error) => {
            send_failure(
                sender,
                context,
                job.ticket.document_id,
                JobKind::Preview,
                Some(job.ticket.revision),
                None,
                format!("CPU preview development failed: {error}"),
            );
            PreviewCompletion::Failed
        }
    }
}

fn process_gpu_base(
    job: PreviewJob,
    sender: &mpsc::Sender<WorkerEvent>,
    context: &egui::Context,
    cancellation: &CancellationToken,
    preview_cache: &mut PreviewCache,
) -> PreviewCompletion {
    let span = info_span!(
        "desktop.gpu_base",
        document_id = job.ticket.document_id,
        revision = job.ticket.revision
    );
    let _guard = span.enter();
    let options = PreviewOptions::default();
    let keys = PreviewCacheKeys::new(job.ticket.document_id, &job.frame, &job.recipe, options);
    let cache_hits = preview_cache.prepare(&keys, &job.frame);
    send_progress(
        sender,
        context,
        job.ticket.document_id,
        JobKind::Preview,
        Some(job.ticket.revision),
        None,
        if cache_hits.demosaiced {
            "Packing cached linear base for GPU preview"
        } else if cache_hits.normalized {
            "Demosaicing cached normalized GPU preview base"
        } else {
            "Preparing linear 2560 px GPU preview base"
        },
    );
    let timings = match ensure_preview_base(
        &job,
        options,
        &keys,
        cache_hits,
        cancellation,
        preview_cache,
    ) {
        Ok(timings) => timings,
        Err(PipelineError::Cancelled) => {
            info!("GPU preview base cancelled after being superseded");
            return PreviewCompletion::Cancelled;
        }
        Err(error) => {
            send_failure(
                sender,
                context,
                job.ticket.document_id,
                JobKind::Preview,
                Some(job.ticket.revision),
                None,
                format!("GPU preview base development failed: {error}"),
            );
            return PreviewCompletion::Failed;
        }
    };
    let Some(base) = preview_cache.demosaiced(&keys) else {
        send_failure(
            sender,
            context,
            job.ticket.document_id,
            JobKind::Preview,
            Some(job.ticket.revision),
            None,
            "GPU preview cache lost its demosaiced base unexpectedly".to_owned(),
        );
        return PreviewCompletion::Failed;
    };
    let width = base.image().width();
    let height = base.image().height();
    let upload_started = Instant::now();
    let upload = match GpuPreviewUpload::from_demosaiced_base_cancellable(base, cancellation) {
        Ok(upload) => upload,
        Err(GpuPreviewError::Cancelled) => {
            info!("GPU upload packing cancelled after being superseded");
            return PreviewCompletion::Cancelled;
        }
        Err(error) => {
            send_failure(
                sender,
                context,
                job.ticket.document_id,
                JobKind::Preview,
                Some(job.ticket.revision),
                None,
                format!("Could not prepare the GPU preview upload: {error}"),
            );
            return PreviewCompletion::Failed;
        }
    };
    let upload_preparation = upload_started.elapsed();
    let cache_resident_bytes = preview_cache.resident_bytes();
    let memory = gpu_base_memory(&job.frame, base, cache_resident_bytes);
    let diagnostics = WorkerPreviewDiagnostics {
        backend: PreviewBackend::GpuBase,
        cache_hits,
        timings,
        memory,
        cache_resident_bytes,
        workspace_reused: false,
    };
    log_preview_diagnostics(job.ticket, width, height, diagnostics);
    send_event(
        sender,
        context,
        WorkerEvent::GpuUploadReady {
            ticket: job.ticket,
            upload: Box::new(upload),
            diagnostics,
            upload_preparation,
        },
    );
    PreviewCompletion::Completed
}

fn develop_preview(
    job: &PreviewJob,
    options: PreviewOptions,
    keys: &PreviewCacheKeys,
    cache_hits: PreviewCacheHits,
    cancellation: &CancellationToken,
    preview_cache: &mut PreviewCache,
) -> Result<(rohditor_core::DisplayRgbImage<u8>, WorkerPreviewDiagnostics), PipelineError> {
    if let Some(cached) = preview_cache.adjusted(keys) {
        let copy_started = Instant::now();
        let image = cached.image.clone();
        let memory = cached.memory;
        let timings = StageTimings {
            total: copy_started.elapsed(),
            ..StageTimings::default()
        };
        return Ok((
            image,
            WorkerPreviewDiagnostics {
                backend: PreviewBackend::Cpu,
                cache_hits,
                timings,
                memory,
                cache_resident_bytes: preview_cache.resident_bytes(),
                workspace_reused: false,
            },
        ));
    }

    let base_timings =
        ensure_preview_base(job, options, keys, cache_hits, cancellation, preview_cache)?;
    let workspace_reused = preview_cache.workspace_reusable(keys);
    let Some((base, workspace)) = preview_cache.base_and_workspace(keys) else {
        return Err(cache_invariant(
            "demosaiced base was unavailable after preparation",
        ));
    };
    let mut result = CpuPipeline.render_preview_from_base_reusing_cancellable(
        base,
        &job.recipe,
        options.render.output_policy,
        workspace,
        cancellation,
    )?;
    add_stage_timings(&mut result.timings, base_timings);
    let memory = result.memory;
    let timings = result.timings;
    preview_cache.insert_adjusted(keys, result.image.clone(), memory);
    let diagnostics = WorkerPreviewDiagnostics {
        backend: PreviewBackend::Cpu,
        cache_hits,
        timings,
        memory,
        cache_resident_bytes: preview_cache.resident_bytes(),
        workspace_reused,
    };
    Ok((result.image, diagnostics))
}

fn ensure_preview_base(
    job: &PreviewJob,
    options: PreviewOptions,
    keys: &PreviewCacheKeys,
    cache_hits: PreviewCacheHits,
    cancellation: &CancellationToken,
    preview_cache: &mut PreviewCache,
) -> Result<StageTimings, PipelineError> {
    let mut timings = StageTimings::default();
    if !cache_hits.normalized {
        let normalized = CpuPipeline.prepare_preview_normalized_cancellable(
            &job.frame,
            options,
            cancellation,
        )?;
        add_stage_timings(&mut timings, normalized.timings());
        preview_cache.insert_normalized(keys, normalized);
    }
    if !cache_hits.demosaiced {
        let normalized = preview_cache
            .normalized(keys)
            .ok_or_else(|| cache_invariant("normalized preview was unavailable before demosaic"))?;
        let base = CpuPipeline.prepare_preview_base_from_normalized_cancellable(
            normalized,
            &job.recipe,
            options.render.demosaic,
            cancellation,
        )?;
        add_stage_timings(&mut timings, base.timings());
        preview_cache.insert_demosaiced(keys, base);
    }
    cancellation.checkpoint()?;
    Ok(timings)
}

fn add_stage_timings(target: &mut StageTimings, additional: StageTimings) {
    target.metadata += additional.metadata;
    target.normalization += additional.normalization;
    target.demosaic += additional.demosaic;
    target.color_conversion += additional.color_conversion;
    target.adjustments += additional.adjustments;
    target.output_conversion += additional.output_conversion;
    target.total += additional.total;
}

fn gpu_base_memory(
    frame: &RawFrame,
    base: &rohditor_core::DemosaicedBase,
    cache_resident_bytes: usize,
) -> MemoryEstimate {
    MemoryEstimate {
        decoded_raw_bytes: frame.mosaic.len().saturating_mul(size_of::<u16>()),
        normalized_mosaic_bytes: base
            .image()
            .width()
            .saturating_mul(base.image().height())
            .saturating_mul(size_of::<f32>()),
        linear_rgb_bytes: base.buffer_bytes(),
        display_rgb_bytes: 0,
        estimated_peak_bytes: cache_resident_bytes,
    }
}

fn log_preview_diagnostics(
    ticket: PreviewTicket,
    width: usize,
    height: usize,
    diagnostics: WorkerPreviewDiagnostics,
) {
    info!(
        document_id = ticket.document_id,
        revision = ticket.revision,
        backend = diagnostics.backend.label(),
        width,
        height,
        cache_decoded = diagnostics.cache_hits.decoded,
        cache_normalized = diagnostics.cache_hits.normalized,
        cache_demosaiced = diagnostics.cache_hits.demosaiced,
        cache_adjusted = diagnostics.cache_hits.adjusted,
        workspace_reused = diagnostics.workspace_reused,
        metadata_us = diagnostics.timings.metadata.as_micros(),
        normalization_us = diagnostics.timings.normalization.as_micros(),
        demosaic_us = diagnostics.timings.demosaic.as_micros(),
        color_us = diagnostics.timings.color_conversion.as_micros(),
        adjustments_us = diagnostics.timings.adjustments.as_micros(),
        output_us = diagnostics.timings.output_conversion.as_micros(),
        total_us = diagnostics.timings.total.as_micros(),
        cache_bytes = diagnostics.cache_resident_bytes,
        estimated_peak_bytes = diagnostics.memory.estimated_peak_bytes,
        "preview processing complete"
    );
}

fn cache_invariant(reason: &str) -> PipelineError {
    PipelineError::InvalidMetadata {
        field: "preview_cache",
        reason: reason.to_owned(),
    }
}

fn process_export(job: ExportJob, sender: &mpsc::Sender<WorkerEvent>, context: &egui::Context) {
    let file_name = display_file_name(&job.destination);
    let span = info_span!(
        "desktop.export",
        document_id = job.document_id,
        export_id = job.export_id,
        revision = job.recipe_revision,
        file = %file_name
    );
    let _guard = span.enter();
    let started = Instant::now();
    send_progress(
        sender,
        context,
        job.document_id,
        JobKind::Export,
        Some(job.recipe_revision),
        Some(job.export_id),
        "Developing full-resolution export on CPU",
    );
    let rendered = match CpuPipeline.render_export(
        &job.frame,
        &job.recipe,
        Default::default(),
        job.settings.format.bit_depth(),
        job.settings.dithering,
    ) {
        Ok(rendered) => rendered,
        Err(error) => {
            send_failure(
                sender,
                context,
                job.document_id,
                JobKind::Export,
                Some(job.recipe_revision),
                Some(job.export_id),
                format!("Full-resolution CPU development failed: {error}"),
            );
            return;
        }
    };

    send_progress(
        sender,
        context,
        job.document_id,
        JobKind::Export,
        Some(job.recipe_revision),
        Some(job.export_id),
        "Encoding and committing output",
    );
    match export_image(
        &job.destination,
        &rendered.image,
        &job.frame.info,
        job.settings,
    ) {
        Ok(report) => {
            let elapsed = started.elapsed();
            info!(
                elapsed_ms = elapsed.as_millis(),
                bytes = report.bytes_written,
                "export complete"
            );
            send_event(
                sender,
                context,
                WorkerEvent::ExportReady {
                    document_id: job.document_id,
                    export_id: job.export_id,
                    recipe_revision: job.recipe_revision,
                    destination: job.destination,
                    report,
                    elapsed,
                },
            );
        }
        Err(error) => send_failure(
            sender,
            context,
            job.document_id,
            JobKind::Export,
            Some(job.recipe_revision),
            Some(job.export_id),
            format!("Could not write {file_name}: {error}"),
        ),
    }
}

fn decode_placeholder(bytes: &[u8], orientation: RawOrientation) -> Result<WorkerImage, String> {
    let decoded = image::load_from_memory(bytes)
        .map_err(|error| format!("JPEG decoding failed: {error}"))?
        .to_rgb8();
    let oriented = orient_rgb8(&decoded, orientation)?;
    WorkerImage::from_rgb(
        oriented.width() as usize,
        oriented.height() as usize,
        oriented.into_raw(),
    )
}

fn orient_rgb8(source: &RgbImage, orientation: RawOrientation) -> Result<RgbImage, String> {
    let (source_width, source_height) = source.dimensions();
    let source_width = usize::try_from(source_width)
        .map_err(|_| "embedded preview width exceeds this system's usize".to_owned())?;
    let source_height = usize::try_from(source_height)
        .map_err(|_| "embedded preview height exceeds this system's usize".to_owned())?;
    let orientation_map = OrientationMap::new(source_width, source_height, orientation)
        .map_err(|error| error.to_string())?;
    let (output_width, output_height) = orientation_map.output_dimensions();
    let samples = output_width
        .checked_mul(output_height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| "embedded preview dimensions overflowed".to_owned())?;
    let mut output = vec![0_u8; samples];

    for output_y in 0..output_height {
        for output_x in 0..output_width {
            let (source_x, source_y) = orientation_map
                .source_coordinate(output_x, output_y)
                .ok_or_else(|| "embedded preview coordinate was out of range".to_owned())?;
            let source_x = u32::try_from(source_x)
                .map_err(|_| "embedded preview x coordinate exceeds u32".to_owned())?;
            let source_y = u32::try_from(source_y)
                .map_err(|_| "embedded preview y coordinate exceeds u32".to_owned())?;
            let source_pixel = source.get_pixel(source_x, source_y).0;
            let output_index = (output_y * output_width + output_x) * 3;
            output[output_index..output_index + 3].copy_from_slice(&source_pixel);
        }
    }
    let output_width =
        u32::try_from(output_width).map_err(|_| "oriented preview width exceeds u32".to_owned())?;
    let output_height = u32::try_from(output_height)
        .map_err(|_| "oriented preview height exceeds u32".to_owned())?;
    RgbImage::from_raw(output_width, output_height, output)
        .ok_or_else(|| "could not construct the oriented embedded preview".to_owned())
}

fn send_progress(
    sender: &mpsc::Sender<WorkerEvent>,
    context: &egui::Context,
    document_id: u64,
    job: JobKind,
    revision: Option<u64>,
    export_id: Option<u64>,
    detail: &str,
) {
    send_event(
        sender,
        context,
        WorkerEvent::Progress {
            document_id,
            job,
            revision,
            export_id,
            detail: detail.to_owned(),
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn send_failure(
    sender: &mpsc::Sender<WorkerEvent>,
    context: &egui::Context,
    document_id: u64,
    job: JobKind,
    revision: Option<u64>,
    export_id: Option<u64>,
    message: String,
) {
    send_event(
        sender,
        context,
        WorkerEvent::Failed {
            document_id,
            job,
            revision,
            export_id,
            message,
        },
    );
}

fn send_event(sender: &mpsc::Sender<WorkerEvent>, context: &egui::Context, event: WorkerEvent) {
    if sender.send(event).is_ok() {
        context.request_repaint();
    }
}

fn display_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("RAW file")
        .to_owned()
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic payload")
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use image::{Rgb, RgbImage};
    use rohditor_core::{ExportFormat, JPEG_QUALITY_DEFAULT, WhiteBalance};
    use rohditor_raw::{
        CameraColorMatrix, CaptureMetadata, CfaPattern, LevelPattern, PhotometricInterpretation,
        RawError, RawSession,
    };

    use super::*;

    #[test]
    fn newer_preview_replaces_only_the_same_document() {
        let current = PreviewTicket {
            document_id: 4,
            revision: 8,
        };
        assert!(should_replace_preview(
            current,
            PreviewTicket {
                document_id: 4,
                revision: 9,
            }
        ));
        assert!(!should_replace_preview(
            current,
            PreviewTicket {
                document_id: 4,
                revision: 7,
            }
        ));
        assert!(!should_replace_preview(
            current,
            PreviewTicket {
                document_id: 5,
                revision: 9,
            }
        ));
    }

    #[test]
    fn preview_queue_keeps_only_the_newest_revision_for_one_document() {
        let frame = Arc::new(fake_frame());
        let job = |document_id, revision| PreviewJob {
            ticket: PreviewTicket {
                document_id,
                revision,
            },
            frame: Arc::clone(&frame),
            recipe: EditRecipe::default(),
            backend: PreviewBackend::Cpu,
        };
        let mailbox = PreviewMailbox::default();
        let (sender, receiver) = mpsc::channel();
        for revision in 0..1_000 {
            mailbox
                .queue(job(4, revision), &sender)
                .expect("worker wake should remain connected");
        }

        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerRequest::PreviewAvailable)
        ));
        assert!(receiver.try_recv().is_err());
        let selected = mailbox
            .take()
            .expect("newest preview should remain pending");
        assert_eq!(selected.job.ticket.revision, 999);
        let stats = mailbox.stats();
        assert_eq!(stats.requested, 1_000);
        assert_eq!(stats.coalesced, 999);
        assert!(stats.active);
        assert!(!stats.pending);
    }

    #[test]
    fn a_newer_preview_cancels_the_active_revision() {
        let frame = Arc::new(fake_frame());
        let job = |revision| PreviewJob {
            ticket: PreviewTicket {
                document_id: 4,
                revision,
            },
            frame: Arc::clone(&frame),
            recipe: EditRecipe::default(),
            backend: PreviewBackend::Cpu,
        };
        let mailbox = PreviewMailbox::default();
        let (sender, receiver) = mpsc::channel();
        mailbox.queue(job(1), &sender).expect("queue active job");
        assert!(matches!(
            receiver.recv(),
            Ok(WorkerRequest::PreviewAvailable)
        ));
        let active = mailbox.take().expect("active preview");

        mailbox.queue(job(2), &sender).expect("queue replacement");

        assert!(active.cancellation.is_cancelled());
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerRequest::PreviewAvailable)
        ));
        let stats = mailbox.stats();
        assert_eq!(stats.cancellation_requests, 1);
        assert!(stats.pending);

        mailbox.finish(active.job.ticket, PreviewCompletion::Cancelled);
        let replacement = mailbox.take().expect("replacement should run next");
        assert_eq!(replacement.job.ticket.revision, 2);
    }

    #[test]
    fn downstream_edits_reuse_the_demosaiced_preview_base() {
        let frame = Arc::new(fake_frame());
        let options = PreviewOptions::default();
        let mut cache = PreviewCache::default();
        let initial = PreviewJob {
            ticket: PreviewTicket {
                document_id: 9,
                revision: 0,
            },
            frame: Arc::clone(&frame),
            recipe: EditRecipe::default(),
            backend: PreviewBackend::Cpu,
        };
        let initial_keys =
            PreviewCacheKeys::new(initial.ticket.document_id, &frame, &initial.recipe, options);
        let initial_hits = cache.prepare(&initial_keys, &frame);
        let first = develop_preview(
            &initial,
            options,
            &initial_keys,
            initial_hits,
            &CancellationToken::new(),
            &mut cache,
        )
        .expect("initial preview should build its base");

        let adjusted = PreviewJob {
            ticket: PreviewTicket {
                document_id: 9,
                revision: 1,
            },
            frame,
            recipe: EditRecipe {
                exposure_ev: 1.0,
                ..EditRecipe::default()
            },
            backend: PreviewBackend::Cpu,
        };
        let adjusted_keys = PreviewCacheKeys::new(
            adjusted.ticket.document_id,
            &adjusted.frame,
            &adjusted.recipe,
            options,
        );
        let adjusted_hits = cache.prepare(&adjusted_keys, &adjusted.frame);
        assert!(adjusted_hits.decoded);
        assert!(adjusted_hits.normalized);
        assert!(adjusted_hits.demosaiced);
        assert!(!adjusted_hits.adjusted);
        let second = develop_preview(
            &adjusted,
            options,
            &adjusted_keys,
            adjusted_hits,
            &CancellationToken::new(),
            &mut cache,
        )
        .expect("downstream edit should reuse its base");

        assert_ne!(first.0, second.0);
        assert_eq!(second.1.timings.normalization, Duration::ZERO);
        assert_eq!(second.1.timings.demosaic, Duration::ZERO);
        assert_eq!(second.1.timings.color_conversion, Duration::ZERO);
        assert!(second.1.workspace_reused);

        let invalid_schema_recipe = EditRecipe {
            schema_version: u32::MAX,
            ..adjusted.recipe.clone()
        };
        let invalid_schema_keys =
            PreviewCacheKeys::new(9, &adjusted.frame, &invalid_schema_recipe, options);
        let invalid_schema_hits = cache.prepare(&invalid_schema_keys, &adjusted.frame);
        assert!(invalid_schema_hits.decoded);
        assert!(invalid_schema_hits.normalized);
        assert!(!invalid_schema_hits.demosaiced);
        assert!(!invalid_schema_hits.adjusted);

        let white_balance_recipe = EditRecipe {
            white_balance: WhiteBalance::ManualMultipliers {
                red: 1.1,
                green: 1.0,
                blue: 0.9,
            },
            ..EditRecipe::default()
        };
        let white_balance_keys =
            PreviewCacheKeys::new(9, &adjusted.frame, &white_balance_recipe, options);
        let white_balance_hits = cache.prepare(&white_balance_keys, &adjusted.frame);
        assert!(white_balance_hits.decoded);
        assert!(white_balance_hits.normalized);
        assert!(!white_balance_hits.demosaiced);
        assert!(!white_balance_hits.adjusted);
    }

    #[test]
    fn gpu_preview_request_hands_a_prepared_upload_to_the_ui_without_cpu_display_conversion() {
        let (sender, receiver) = mpsc::channel();
        let job = PreviewJob {
            ticket: PreviewTicket {
                document_id: 12,
                revision: 3,
            },
            frame: Arc::new(fake_frame()),
            recipe: EditRecipe::default(),
            backend: PreviewBackend::GpuBase,
        };

        let completion = process_gpu_base(
            job,
            &sender,
            &egui::Context::default(),
            &CancellationToken::new(),
            &mut PreviewCache::default(),
        );
        assert_eq!(completion, PreviewCompletion::Completed);
        let events = receiver.try_iter().collect::<Vec<_>>();
        let upload = events.iter().find_map(|event| match event {
            WorkerEvent::GpuUploadReady { ticket, upload, .. }
                if *ticket
                    == PreviewTicket {
                        document_id: 12,
                        revision: 3,
                    } =>
            {
                Some(upload)
            }
            _ => None,
        });
        let upload = upload.expect("GPU preview request should return a prepared upload");
        assert_eq!(upload.source_dimensions(), (4, 4));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, WorkerEvent::PreviewReady { .. }))
        );
    }

    #[test]
    fn abandonment_tracks_exact_document_ids_instead_of_an_ordering_range() {
        let abandoned = HashSet::from([5_u64]);
        let request = |document_id| WorkerRequest::Open {
            document_id,
            path: PathBuf::from("fixture.raw"),
        };

        assert!(!request_belongs_to_abandoned_document(
            &request(4),
            &abandoned
        ));
        assert!(request_belongs_to_abandoned_document(
            &request(5),
            &abandoned
        ));
        assert!(!request_belongs_to_abandoned_document(
            &request(6),
            &abandoned
        ));
    }

    #[test]
    fn embedded_preview_orientation_uses_the_pipeline_coordinate_contract() {
        let mut source = RgbImage::new(2, 3);
        let mut value = 1_u8;
        for y in 0..3 {
            for x in 0..2 {
                source.put_pixel(x, y, Rgb([value, 0, 0]));
                value += 1;
            }
        }

        let rotated = orient_rgb8(&source, RawOrientation::Rotate90).expect("valid preview");
        assert_eq!(rotated.dimensions(), (3, 2));
        assert_eq!(rotated.get_pixel(0, 0).0[0], 5);
        assert_eq!(rotated.get_pixel(2, 0).0[0], 1);
        assert_eq!(rotated.get_pixel(0, 1).0[0], 6);
    }

    #[test]
    fn desktop_open_reuses_one_session_and_treats_preview_failure_as_optional() {
        let opens = Arc::new(AtomicUsize::new(0));
        let decoder = FailingPreviewDecoder {
            opens: Arc::clone(&opens),
            frame: fake_frame(),
        };
        let (sender, receiver) = mpsc::channel();
        process_open(
            41,
            Path::new("fixture.raw"),
            &sender,
            &egui::Context::default(),
            &decoder,
        );
        let events = receiver.try_iter().collect::<Vec<_>>();

        assert_eq!(opens.load(Ordering::Relaxed), 1);
        assert!(events.iter().any(|event| matches!(
            event,
            WorkerEvent::MetadataReady {
                document_id: 41,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            WorkerEvent::Warning {
                document_id: 41,
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            WorkerEvent::RawReady {
                document_id: 41,
                ..
            }
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, WorkerEvent::Failed { .. }))
        );
    }

    #[derive(Debug)]
    struct FailingPreviewDecoder {
        opens: Arc<AtomicUsize>,
        frame: RawFrame,
    }

    impl RawDecoder for FailingPreviewDecoder {
        fn open(&self, path: &Path) -> Result<Box<dyn RawSession>, RawError> {
            self.opens.fetch_add(1, Ordering::Relaxed);
            Ok(Box::new(FailingPreviewSession {
                path: path.to_path_buf(),
                frame: self.frame.clone(),
            }))
        }
    }

    #[derive(Debug)]
    struct FailingPreviewSession {
        path: PathBuf,
        frame: RawFrame,
    }

    impl RawSession for FailingPreviewSession {
        fn probe(&mut self) -> Result<RawFileInfo, RawError> {
            Ok(self.frame.info.clone())
        }

        fn decode(&mut self) -> Result<RawFrame, RawError> {
            Ok(self.frame.clone())
        }

        fn embedded_preview(&mut self) -> Result<Option<rohditor_raw::EncodedPreview>, RawError> {
            Err(RawError::Corrupt {
                path: self.path.clone(),
                reason: "damaged optional JPEG".to_owned(),
            })
        }
    }

    fn fake_frame() -> RawFrame {
        let width = 4;
        let height = 4;
        let mosaic = (0..width * height)
            .map(|index| match (index / width % 2, index % width % 2) {
                (0, 0) => 20_000,
                (1, 1) => 40_000,
                _ => 30_000,
            })
            .collect::<Vec<_>>();
        RawFrame {
            info: RawFileInfo {
                format: "synthetic".to_owned(),
                make: "Rohditor".to_owned(),
                model: "Worker fixture".to_owned(),
                clean_make: "Rohditor".to_owned(),
                clean_model: "Worker fixture".to_owned(),
                source_size_bytes: 4,
                source_identity: None,
                width,
                height,
                components_per_pixel: 1,
                source_bits_per_sample: Some(16),
                decoded_bits_per_sample: 16,
                compression: None,
                active_area: None,
                crop_area: None,
                photometric_interpretation: PhotometricInterpretation::Cfa {
                    pattern: CfaPattern {
                        name: "RGGB".to_owned(),
                        width: 2,
                        height: 2,
                    },
                },
                black_levels: LevelPattern {
                    values: vec![0.0; 4],
                    repeat_width: 2,
                    repeat_height: 2,
                    components_per_pixel: 1,
                },
                white_levels: vec![u16::MAX.into()],
                as_shot_white_balance: [Some(1.0); 4],
                xyz_to_camera: [[0.0; 3]; 4],
                color_matrices: vec![CameraColorMatrix {
                    illuminant: "D65".to_owned(),
                    values: vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                }],
                orientation: RawOrientation::Normal,
                capture: CaptureMetadata::default(),
                embedded_preview: None,
            },
            row_stride: width,
            mosaic: Arc::from(mosaic),
        }
    }

    #[test]
    #[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
    fn private_worker_opens_previews_and_exports_a_recipe_snapshot() -> Result<(), Box<dyn Error>> {
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private/DSC00851.ARW");
        let destination =
            std::env::temp_dir().join(format!("rohditor-phase4-worker-{}.jpg", std::process::id()));
        if destination.exists() {
            fs::remove_file(&destination)?;
        }

        let coordinator =
            RenderCoordinator::new(egui::Context::default()).map_err(std::io::Error::other)?;
        coordinator
            .open(17, source)
            .map_err(std::io::Error::other)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let frame = loop {
            let mut decoded = None;
            for event in coordinator.try_events() {
                match event {
                    WorkerEvent::RawReady {
                        document_id: 17,
                        frame,
                    } => decoded = Some(frame),
                    WorkerEvent::Failed { message, .. } => {
                        return Err(message.into());
                    }
                    _ => {}
                }
            }
            if let Some(frame) = decoded {
                break frame;
            }
            if Instant::now() >= deadline {
                return Err("timed out while decoding the private RAW".into());
            }
            thread::sleep(Duration::from_millis(10));
        };

        let snapshot = EditRecipe::default();
        coordinator
            .preview(
                PreviewTicket {
                    document_id: 17,
                    revision: 0,
                },
                Arc::clone(&frame),
                snapshot.clone(),
            )
            .map_err(std::io::Error::other)?;
        coordinator
            .export(
                17,
                23,
                0,
                destination.clone(),
                frame,
                snapshot,
                ExportSettings {
                    format: ExportFormat::Jpeg {
                        quality: JPEG_QUALITY_DEFAULT,
                    },
                    ..ExportSettings::default()
                },
            )
            .map_err(std::io::Error::other)?;

        let mut preview_complete = false;
        let mut export_complete = false;
        while !(preview_complete && export_complete) {
            for event in coordinator.try_events() {
                match event {
                    WorkerEvent::PreviewReady { ticket, image, .. } => {
                        assert_eq!(ticket.revision, 0);
                        assert_eq!((image.width, image.height), (2_560, 1_707));
                        preview_complete = true;
                    }
                    WorkerEvent::ExportReady {
                        export_id,
                        recipe_revision,
                        report,
                        ..
                    } => {
                        assert_eq!(export_id, 23);
                        assert_eq!(recipe_revision, 0);
                        assert_eq!((report.width, report.height), (6_000, 4_000));
                        export_complete = true;
                    }
                    WorkerEvent::Failed { message, .. } => {
                        return Err(message.into());
                    }
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                return Err("timed out while running private preview/export jobs".into());
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(destination.is_file());
        fs::remove_file(destination)?;
        Ok(())
    }

    #[test]
    #[ignore = "requires the ignored Sony ARW corpus in testdata/private"]
    fn private_cached_cpu_preview_reports_bounded_memory_and_stage_skips()
    -> Result<(), Box<dyn Error>> {
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private/DSC00851.ARW");
        if !source.is_file() {
            eprintln!(
                "skipping cache measurement because {} is absent",
                source.display()
            );
            return Ok(());
        }
        let decoder = RawlerDecoder::default();
        let mut session = decoder.open(&source)?;
        let frame = Arc::new(session.decode()?);
        let options = PreviewOptions::default();
        let mut cache = PreviewCache::default();

        let initial = PreviewJob {
            ticket: PreviewTicket {
                document_id: 71,
                revision: 0,
            },
            frame: Arc::clone(&frame),
            recipe: EditRecipe::default(),
            backend: PreviewBackend::Cpu,
        };
        let initial_keys =
            PreviewCacheKeys::new(initial.ticket.document_id, &frame, &initial.recipe, options);
        let initial_hits = cache.prepare(&initial_keys, &frame);
        let initial_started = Instant::now();
        let (initial_image, initial_diagnostics) = develop_preview(
            &initial,
            options,
            &initial_keys,
            initial_hits,
            &CancellationToken::new(),
            &mut cache,
        )?;
        let first_wall = initial_started.elapsed();
        let stable_cache_bytes = cache.resident_bytes();
        let mut cached_wall = Vec::new();

        for revision in 1..=24 {
            let recipe = EditRecipe {
                exposure_ev: (revision as f32 / 24.0) * 2.0 - 1.0,
                contrast: revision as f32 / 48.0,
                saturation: 0.8 + revision as f32 / 60.0,
                ..EditRecipe::default()
            };
            let job = PreviewJob {
                ticket: PreviewTicket {
                    document_id: 71,
                    revision,
                },
                frame: Arc::clone(&frame),
                recipe,
                backend: PreviewBackend::Cpu,
            };
            let keys = PreviewCacheKeys::new(job.ticket.document_id, &frame, &job.recipe, options);
            let hits = cache.prepare(&keys, &frame);
            assert!(hits.decoded && hits.normalized && hits.demosaiced && !hits.adjusted);
            let started = Instant::now();
            let (_, diagnostics) = develop_preview(
                &job,
                options,
                &keys,
                hits,
                &CancellationToken::new(),
                &mut cache,
            )?;
            cached_wall.push(started.elapsed());
            assert_eq!(diagnostics.timings.normalization, Duration::ZERO);
            assert_eq!(diagnostics.timings.demosaic, Duration::ZERO);
            assert_eq!(diagnostics.timings.color_conversion, Duration::ZERO);
            assert!(diagnostics.workspace_reused);
            assert_eq!(cache.resident_bytes(), stable_cache_bytes);
        }

        cached_wall.sort_unstable();
        let median = cached_wall[cached_wall.len() / 2];
        let worst = cached_wall.last().copied().unwrap_or(Duration::ZERO);
        eprintln!(
            "Phase 6 CPU cache measurement: {}x{}, first={:.2} ms, cached median={:.2} ms, cached max={:.2} ms, cache={:.1} MiB, render peak={:.1} MiB",
            initial_image.width(),
            initial_image.height(),
            first_wall.as_secs_f64() * 1_000.0,
            median.as_secs_f64() * 1_000.0,
            worst.as_secs_f64() * 1_000.0,
            stable_cache_bytes as f64 / 1_048_576.0,
            initial_diagnostics.memory.estimated_peak_bytes as f64 / 1_048_576.0,
        );
        Ok(())
    }
}
