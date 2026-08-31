//! Translation from background-worker events into current-document UI state.
//!
//! This module deliberately owns no scheduler or GPU resources. It is the
//! narrow identity/revision guard between asynchronous results and `app.rs`.

use eframe::egui;

use crate::coordinator::{JobKind, PreviewResolution, WorkerEvent};
use crate::ui::viewport::PreviewSource;

use super::{DocumentPreviewDiagnostics, RohditorApp, install_texture};

impl RohditorApp {
    pub(super) fn process_worker_events(&mut self, context: &egui::Context) {
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
                        document.preview_source,
                        Some(source) if source.is_developed()
                    )
                {
                    install_texture(context, document, image, PreviewSource::Embedded);
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
                resolution,
                image,
                diagnostics,
            } => {
                if let Some(document) = self.document.as_mut()
                    && ticket.is_current(document.id, document.edits.revision())
                    && document.source_scale_requested
                        == (resolution == PreviewResolution::SourceScale)
                    && (self.gpu.is_none() || resolution == PreviewResolution::SourceScale)
                {
                    let source = if resolution == PreviewResolution::SourceScale {
                        PreviewSource::OneToOneCpu
                    } else {
                        PreviewSource::developed(diagnostics.algorithm, false)
                    };
                    install_texture(context, document, image, source);
                    if resolution == PreviewResolution::SourceScale {
                        document.view.actual_size(context.input(|input| input.time));
                    } else {
                        document.view.fit(context.input(|input| input.time));
                    }
                    document.preview_status = None;
                    document.last_preview_time = Some(diagnostics.timings.total);
                    document.preview_diagnostics =
                        Some(DocumentPreviewDiagnostics::cpu(diagnostics));
                    document.error = None;
                }
            }
            WorkerEvent::GpuUploadReady {
                ticket,
                upload,
                diagnostics,
                upload_preparation,
            } => {
                self.install_gpu_upload(context, ticket, *upload, diagnostics, upload_preparation);
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
}
