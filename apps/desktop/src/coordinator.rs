use std::any::Any;
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::egui;
use image::RgbImage;
use rohditor_core::{
    CpuPipeline, DemosaicedBase, EditRecipe, ExportReport, ExportSettings, OrientationMap,
    PreviewOptions, StageTimings, export_image,
};
use rohditor_gpu::GpuPreviewUpload;
use rohditor_raw::{RawDecoder, RawFileInfo, RawFrame, RawOrientation, RawSession, RawlerDecoder};
use tracing::{info, info_span};

use crate::document::PreviewTicket;

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

impl fmt::Display for JobKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Open => "open",
            Self::Preview => "preview",
            Self::Export => "export",
        })
    }
}

#[derive(Debug)]
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
        timings: StageTimings,
    },
    GpuUploadReady {
        ticket: PreviewTicket,
        upload: Box<GpuPreviewUpload>,
        base_timings: StageTimings,
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
struct PreviewCache {
    document_id: u64,
    frame: Arc<RawFrame>,
    options: PreviewOptions,
    base: DemosaicedBase,
}

impl PreviewCache {
    fn matches(&self, job: &PreviewJob, options: PreviewOptions) -> bool {
        self.document_id == job.ticket.document_id
            && Arc::ptr_eq(&self.frame, &job.frame)
            && self.options == options
            && self.base.white_balance() == job.recipe.white_balance
    }
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
    Preview(PreviewJob),
    Export(ExportJob),
    AbandonDocument(u64),
    Shutdown,
}

pub(crate) struct RenderCoordinator {
    requests: mpsc::Sender<WorkerRequest>,
    events: mpsc::Receiver<WorkerEvent>,
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
        let stopped_sender = event_sender.clone();
        let stopped_context = context.clone();
        let worker = thread::Builder::new()
            .name("rohditor-cpu-worker".to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    worker_loop(request_receiver, event_sender, context, decoder);
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
        self.send(WorkerRequest::Preview(PreviewJob {
            ticket,
            frame,
            recipe,
            backend: PreviewBackend::Cpu,
        }))
    }

    pub(crate) fn prepare_gpu_base(
        &self,
        ticket: PreviewTicket,
        frame: Arc<RawFrame>,
        recipe: EditRecipe,
    ) -> Result<(), String> {
        self.send(WorkerRequest::Preview(PreviewJob {
            ticket,
            frame,
            recipe,
            backend: PreviewBackend::GpuBase,
        }))
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
        drop(
            self.requests
                .send(WorkerRequest::AbandonDocument(document_id)),
        );
    }

    pub(crate) fn try_events(&self) -> impl Iterator<Item = WorkerEvent> + '_ {
        self.events.try_iter()
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
) {
    let mut pending = VecDeque::new();
    let mut abandoned = HashSet::new();
    let mut preview_cache: Option<PreviewCache> = None;

    loop {
        let request = match pending.pop_front() {
            Some(request) => request,
            None => match receiver.recv() {
                Ok(request) => request,
                Err(_) => break,
            },
        };

        if drain_waiting_requests(&receiver, &mut pending, &mut abandoned, &mut preview_cache) {
            break;
        }

        if request_belongs_to_abandoned_document(&request, &abandoned) {
            continue;
        }

        match request {
            WorkerRequest::Open { document_id, path } => {
                process_open(document_id, &path, &sender, &context, decoder.as_ref());
            }
            WorkerRequest::Preview(job) => {
                let job = coalesce_pending_preview(job, &mut pending);
                if !abandoned.contains(&job.ticket.document_id) {
                    match job.backend {
                        PreviewBackend::Cpu => {
                            process_preview(job, &sender, &context, &mut preview_cache);
                        }
                        PreviewBackend::GpuBase => {
                            process_gpu_base(job, &sender, &context);
                        }
                    }
                }
            }
            WorkerRequest::Export(job) => {
                if !abandoned.contains(&job.document_id) {
                    process_export(job, &sender, &context);
                }
            }
            WorkerRequest::AbandonDocument(document_id) => {
                abandon_document(document_id, &mut abandoned, &mut preview_cache);
            }
            WorkerRequest::Shutdown => break,
        }
    }
}

fn drain_waiting_requests(
    receiver: &mpsc::Receiver<WorkerRequest>,
    pending: &mut VecDeque<WorkerRequest>,
    abandoned: &mut HashSet<u64>,
    preview_cache: &mut Option<PreviewCache>,
) -> bool {
    let mut shutdown = false;
    while let Ok(request) = receiver.try_recv() {
        match request {
            WorkerRequest::AbandonDocument(document_id) => {
                abandon_document(document_id, abandoned, preview_cache);
            }
            WorkerRequest::Shutdown => shutdown = true,
            other => pending.push_back(other),
        }
    }
    shutdown
}

fn abandon_document(
    document_id: u64,
    abandoned: &mut HashSet<u64>,
    preview_cache: &mut Option<PreviewCache>,
) {
    abandoned.insert(document_id);
    if preview_cache
        .as_ref()
        .is_some_and(|cache| cache.document_id == document_id)
    {
        *preview_cache = None;
    }
}

fn coalesce_pending_preview(
    mut current: PreviewJob,
    pending: &mut VecDeque<WorkerRequest>,
) -> PreviewJob {
    let mut retained = VecDeque::with_capacity(pending.len());
    while let Some(request) = pending.pop_front() {
        match request {
            WorkerRequest::Preview(candidate)
                if candidate.ticket.document_id == current.ticket.document_id =>
            {
                if should_replace_preview(current.ticket, candidate.ticket) {
                    current = candidate;
                }
            }
            other => retained.push_back(other),
        }
    }
    *pending = retained;
    current
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
            Self::Preview(job) => Some(job.ticket.document_id),
            Self::Export(job) => Some(job.document_id),
            Self::Shutdown => None,
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
    preview_cache: &mut Option<PreviewCache>,
) {
    let span = info_span!(
        "desktop.preview",
        document_id = job.ticket.document_id,
        revision = job.ticket.revision
    );
    let _guard = span.enter();
    let options = PreviewOptions::default();
    let cache_hit = preview_cache
        .as_ref()
        .is_some_and(|cache| cache.matches(&job, options));
    send_progress(
        sender,
        context,
        job.ticket.document_id,
        JobKind::Preview,
        Some(job.ticket.revision),
        None,
        if cache_hit {
            "Applying edits to cached 2560 px CPU preview"
        } else {
            "Developing 2560 px CPU preview"
        },
    );
    match develop_preview(&job, options, cache_hit, preview_cache) {
        Ok(result) => match WorkerImage::from_display(result.image) {
            Ok(image) => {
                info!(
                    width = image.width,
                    height = image.height,
                    elapsed_ms = result.timings.total.as_millis(),
                    "CPU preview complete"
                );
                send_event(
                    sender,
                    context,
                    WorkerEvent::PreviewReady {
                        ticket: job.ticket,
                        image,
                        timings: result.timings,
                    },
                );
            }
            Err(error) => send_failure(
                sender,
                context,
                job.ticket.document_id,
                JobKind::Preview,
                Some(job.ticket.revision),
                None,
                format!("Could not prepare the CPU preview for display: {error}"),
            ),
        },
        Err(error) => send_failure(
            sender,
            context,
            job.ticket.document_id,
            JobKind::Preview,
            Some(job.ticket.revision),
            None,
            error,
        ),
    }
}

fn process_gpu_base(job: PreviewJob, sender: &mpsc::Sender<WorkerEvent>, context: &egui::Context) {
    let span = info_span!(
        "desktop.gpu_base",
        document_id = job.ticket.document_id,
        revision = job.ticket.revision
    );
    let _guard = span.enter();
    let options = PreviewOptions::default();
    send_progress(
        sender,
        context,
        job.ticket.document_id,
        JobKind::Preview,
        Some(job.ticket.revision),
        None,
        "Preparing linear 2560 px GPU preview base",
    );
    match CpuPipeline.prepare_preview_base(&job.frame, &job.recipe, options) {
        Ok(base) => {
            let timings = base.timings();
            info!(
                width = base.image().width(),
                height = base.image().height(),
                elapsed_ms = timings.total.as_millis(),
                "GPU preview base complete"
            );
            let upload_started = Instant::now();
            match GpuPreviewUpload::from_demosaiced_base(&base) {
                Ok(upload) => send_event(
                    sender,
                    context,
                    WorkerEvent::GpuUploadReady {
                        ticket: job.ticket,
                        upload: Box::new(upload),
                        base_timings: timings,
                        upload_preparation: upload_started.elapsed(),
                    },
                ),
                Err(error) => send_failure(
                    sender,
                    context,
                    job.ticket.document_id,
                    JobKind::Preview,
                    Some(job.ticket.revision),
                    None,
                    format!("Could not prepare the GPU preview upload: {error}"),
                ),
            }
        }
        Err(error) => send_failure(
            sender,
            context,
            job.ticket.document_id,
            JobKind::Preview,
            Some(job.ticket.revision),
            None,
            format!("GPU preview base development failed: {error}"),
        ),
    }
}

fn develop_preview(
    job: &PreviewJob,
    options: PreviewOptions,
    cache_hit: bool,
    preview_cache: &mut Option<PreviewCache>,
) -> Result<rohditor_core::RenderResult, String> {
    if !cache_hit {
        let base = CpuPipeline
            .prepare_preview_base(&job.frame, &job.recipe, options)
            .map_err(|error| format!("CPU preview base development failed: {error}"))?;
        *preview_cache = Some(PreviewCache {
            document_id: job.ticket.document_id,
            frame: Arc::clone(&job.frame),
            options,
            base,
        });
    }
    let cached = preview_cache
        .as_ref()
        .ok_or_else(|| "CPU preview cache was unexpectedly unavailable".to_owned())?;
    let mut result = CpuPipeline
        .render_preview_from_base(&cached.base, &job.recipe, options.render.output_policy)
        .map_err(|error| format!("CPU preview development failed: {error}"))?;
    if !cache_hit {
        let base_timings = cached.base.timings();
        result.timings.metadata = base_timings.metadata;
        result.timings.normalization = base_timings.normalization;
        result.timings.demosaic = base_timings.demosaic;
        result.timings.color_conversion = base_timings.color_conversion;
        result.timings.total += base_timings.total;
    }
    Ok(result)
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
    use rohditor_core::{ExportFormat, JPEG_QUALITY_DEFAULT};
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
        let mut pending = VecDeque::from([
            WorkerRequest::Preview(job(4, 7)),
            WorkerRequest::Preview(job(5, 20)),
            WorkerRequest::Preview(job(4, 10)),
            WorkerRequest::Preview(job(4, 8)),
        ]);

        let selected = coalesce_pending_preview(job(4, 9), &mut pending);
        assert_eq!(selected.ticket.revision, 10);
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            pending.front(),
            Some(WorkerRequest::Preview(PreviewJob {
                ticket: PreviewTicket { document_id: 5, .. },
                ..
            }))
        ));
    }

    #[test]
    fn downstream_edits_reuse_the_demosaiced_preview_base() {
        let frame = Arc::new(fake_frame());
        let options = PreviewOptions::default();
        let mut cache = None;
        let initial = PreviewJob {
            ticket: PreviewTicket {
                document_id: 9,
                revision: 0,
            },
            frame: Arc::clone(&frame),
            recipe: EditRecipe::default(),
            backend: PreviewBackend::Cpu,
        };
        let first = develop_preview(&initial, options, false, &mut cache)
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
        assert!(
            cache
                .as_ref()
                .is_some_and(|cache| cache.matches(&adjusted, options))
        );
        let second = develop_preview(&adjusted, options, true, &mut cache)
            .expect("downstream edit should reuse its base");

        assert_ne!(first.image, second.image);
        assert_eq!(second.timings.normalization, Duration::ZERO);
        assert_eq!(second.timings.demosaic, Duration::ZERO);
        assert_eq!(second.timings.color_conversion, Duration::ZERO);
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

        process_gpu_base(job, &sender, &egui::Context::default());
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
}
