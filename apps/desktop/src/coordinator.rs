use std::collections::VecDeque;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use image::RgbImage;
use rohditor_core::{
    CpuPipeline, EditRecipe, ExportReport, ExportSettings, PreviewOptions, StageTimings,
    export_image,
};
use rohditor_raw::{RawDecoder, RawFileInfo, RawFrame, RawOrientation, RawlerDecoder};
use tracing::{info, info_span};

use crate::document::PreviewTicket;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobKind {
    Open,
    Preview,
    Export,
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
}

#[derive(Debug)]
struct PreviewJob {
    ticket: PreviewTicket,
    frame: Arc<RawFrame>,
    recipe: EditRecipe,
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
}

impl RenderCoordinator {
    pub(crate) fn new(context: egui::Context) -> Result<Self, String> {
        let (request_sender, request_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::Builder::new()
            .name("rohditor-cpu-worker".to_owned())
            .spawn(move || worker_loop(request_receiver, event_sender, context))
            .map_err(|error| format!("could not start the CPU worker: {error}"))?;
        Ok(Self {
            requests: request_sender,
            events: event_receiver,
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
        // render. Dropping the JoinHandle detached the worker at construction;
        // it exits after the current operation observes this queued shutdown or
        // the request channel disconnects.
        drop(self.requests.send(WorkerRequest::Shutdown));
    }
}

fn worker_loop(
    receiver: mpsc::Receiver<WorkerRequest>,
    sender: mpsc::Sender<WorkerEvent>,
    context: egui::Context,
) {
    let mut pending = VecDeque::new();
    let mut abandoned_through = 0_u64;

    loop {
        let request = match pending.pop_front() {
            Some(request) => request,
            None => match receiver.recv() {
                Ok(request) => request,
                Err(_) => break,
            },
        };

        match request {
            WorkerRequest::Open { document_id, path } => {
                process_open(document_id, &path, &sender, &context);
            }
            WorkerRequest::Preview(mut job) => {
                let mut shutdown = false;
                while let Ok(next) = receiver.try_recv() {
                    match next {
                        WorkerRequest::Preview(candidate)
                            if should_replace_preview(job.ticket, candidate.ticket) =>
                        {
                            job = candidate;
                        }
                        WorkerRequest::AbandonDocument(document_id) => {
                            abandoned_through = abandoned_through.max(document_id);
                        }
                        WorkerRequest::Shutdown => shutdown = true,
                        other => pending.push_back(other),
                    }
                }
                if shutdown {
                    break;
                }
                if job.ticket.document_id > abandoned_through {
                    process_preview(job, &sender, &context);
                }
            }
            WorkerRequest::Export(job) => process_export(job, &sender, &context),
            WorkerRequest::AbandonDocument(document_id) => {
                abandoned_through = abandoned_through.max(document_id);
            }
            WorkerRequest::Shutdown => break,
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
    let decoder = RawlerDecoder::default();
    let info = match decoder.probe(path) {
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

    send_progress(
        sender,
        context,
        document_id,
        JobKind::Open,
        None,
        None,
        "Loading embedded preview",
    );
    match decoder.embedded_preview(path) {
        Ok(Some(preview)) => match decode_placeholder(&preview.bytes, info.orientation) {
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

    send_progress(
        sender,
        context,
        document_id,
        JobKind::Open,
        None,
        None,
        "Decoding sensor data",
    );
    match decoder.decode(path) {
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

fn process_preview(job: PreviewJob, sender: &mpsc::Sender<WorkerEvent>, context: &egui::Context) {
    let span = info_span!(
        "desktop.preview",
        document_id = job.ticket.document_id,
        revision = job.ticket.revision
    );
    let _guard = span.enter();
    send_progress(
        sender,
        context,
        job.ticket.document_id,
        JobKind::Preview,
        Some(job.ticket.revision),
        None,
        "Developing 2560 px CPU preview",
    );
    match CpuPipeline.render_preview(&job.frame, &job.recipe, PreviewOptions::default()) {
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
            format!("CPU preview development failed: {error}"),
        ),
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
    let (output_width, output_height) = match orientation {
        RawOrientation::Transpose
        | RawOrientation::Rotate90
        | RawOrientation::Transverse
        | RawOrientation::Rotate270 => (source_height, source_width),
        _ => (source_width, source_height),
    };
    let samples = u64::from(output_width)
        .checked_mul(u64::from(output_height))
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| "embedded preview dimensions overflowed".to_owned())?;
    let sample_count = usize::try_from(samples)
        .map_err(|_| "embedded preview is too large for this system".to_owned())?;
    let mut output = vec![0_u8; sample_count];

    for output_y in 0..output_height {
        for output_x in 0..output_width {
            let (source_x, source_y) = oriented_source_coordinate(
                output_x,
                output_y,
                source_width,
                source_height,
                orientation,
            );
            let source_pixel = source.get_pixel(source_x, source_y).0;
            let output_index = (output_y as usize * output_width as usize + output_x as usize) * 3;
            output[output_index..output_index + 3].copy_from_slice(&source_pixel);
        }
    }
    RgbImage::from_raw(output_width, output_height, output)
        .ok_or_else(|| "could not construct the oriented embedded preview".to_owned())
}

fn oriented_source_coordinate(
    output_x: u32,
    output_y: u32,
    source_width: u32,
    source_height: u32,
    orientation: RawOrientation,
) -> (u32, u32) {
    match orientation {
        RawOrientation::Normal | RawOrientation::Unknown => (output_x, output_y),
        RawOrientation::HorizontalFlip => (source_width - 1 - output_x, output_y),
        RawOrientation::Rotate180 => (source_width - 1 - output_x, source_height - 1 - output_y),
        RawOrientation::VerticalFlip => (output_x, source_height - 1 - output_y),
        RawOrientation::Transpose => (output_y, output_x),
        RawOrientation::Rotate90 => (output_y, source_height - 1 - output_x),
        RawOrientation::Transverse => (source_width - 1 - output_y, source_height - 1 - output_x),
        RawOrientation::Rotate270 => (source_width - 1 - output_y, output_x),
    }
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

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use image::{Rgb, RgbImage};
    use rohditor_core::{ExportFormat, JPEG_QUALITY_DEFAULT};

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
