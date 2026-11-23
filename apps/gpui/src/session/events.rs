//! Translates render-worker events into document state.

use rohditor_engine::coordinator::{PreviewResolution, WorkerEvent};
use rohditor_engine::document::PreviewIntent;
use rohditor_engine::open_document::OpenDocument;
use rohditor_engine::preview::{DocumentPreviewDiagnostics, GpuUpload, PreviewSource};

use super::Session;

impl Session {
    pub(super) fn process_worker_events(&mut self) {
        let events: Vec<_> = self.coordinator.try_events().collect();
        for event in events {
            self.process_worker_event(event);
        }
    }

    fn current(&mut self, document_id: u64) -> Option<&mut OpenDocument> {
        self.document.as_mut().filter(|open| open.id == document_id)
    }

    fn process_worker_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Progress {
                document_id,
                job,
                revision,
                preview_ticket,
                export_id,
                detail,
            } => {
                if let Some(document) = self.current(document_id) {
                    document.job_progress(job, revision, preview_ticket, export_id, detail);
                }
            }
            WorkerEvent::MetadataReady { document_id, info } => {
                if let Some(document) = self.current(document_id) {
                    document.metadata_ready(*info);
                }
            }
            WorkerEvent::OpticsMatchReady {
                document_id,
                result,
                candidates,
                database,
                error,
            } => {
                if let Some(document) = self.current(document_id) {
                    document.optics_match_ready(result, candidates, database, error);
                }
            }
            WorkerEvent::PlaceholderReady { document_id, image } => {
                if let Some(document) = self.current(document_id)
                    && !document
                        .preview
                        .source
                        .is_some_and(PreviewSource::is_developed)
                {
                    document.preview.source = Some(PreviewSource::Embedded);
                    self.show_cpu_image(image.width, image.height, &image.pixels);
                }
            }
            WorkerEvent::RawReady { document_id, frame } => {
                let render_options = self.settings.render_options();
                if let Some(document) = self.current(document_id) {
                    document.raw_ready(frame);
                    self.queue_preview();
                    if let Some(document) = self.document.as_mut() {
                        document.start_default_noise_estimate(&self.coordinator, render_options);
                    }
                }
            }
            WorkerEvent::PreviewReady {
                ticket,
                resolution: PreviewResolution::Fit,
                image,
                histogram,
                diagnostics,
            } => {
                let Some(document) = self.current(ticket.document_id) else {
                    return;
                };
                let superseded = document
                    .preview
                    .gpu()
                    .is_some_and(|gpu| gpu.ticket == ticket);
                if superseded || !document.accepts_preview(ticket, PreviewIntent::CommittedFit) {
                    return;
                }
                document.preview.release_gpu();
                let preview = &mut document.preview;
                preview.source = Some(PreviewSource::developed(diagnostics.algorithm, false));
                preview.histogram = Some(*histogram);
                preview.histogram_revision = Some(ticket.revision);
                preview.status = None;
                preview.last_time = Some(diagnostics.timings.total);
                preview.diagnostics = Some(DocumentPreviewDiagnostics::cpu(diagnostics));
                document.error = None;
                self.sync_display();
                self.show_cpu_image(image.width, image.height, &image.pixels);
            }
            WorkerEvent::GpuUploadReady {
                ticket,
                source,
                diagnostics,
                upload_preparation,
            } => {
                if self.current(ticket.document_id).is_some() {
                    self.with_preview(|runtime, coordinator, preview, request| {
                        let upload = GpuUpload {
                            ticket,
                            prepared: *source,
                            diagnostics,
                            upload_preparation,
                        };
                        let outcome =
                            runtime.install_gpu_upload(coordinator, preview, request, upload);
                        ((), outcome)
                    });
                }
            }
            WorkerEvent::GpuSpatialFailed { ticket, message } => {
                if self
                    .document
                    .as_ref()
                    .is_some_and(|document| document.ticket() == ticket)
                {
                    self.with_preview(|runtime, coordinator, preview, request| {
                        let outcome =
                            runtime.handle_gpu_failure(coordinator, preview, request, message);
                        ((), outcome)
                    });
                }
            }
            WorkerEvent::ExportReady {
                document_id,
                export_id,
                recipe_revision,
                destination,
                report,
                elapsed,
            } => {
                if let Some(document) = self.current(document_id) {
                    document.export_ready(
                        export_id,
                        recipe_revision,
                        &destination,
                        &report,
                        elapsed,
                    );
                }
            }
            WorkerEvent::NoiseProfileReady {
                document_id,
                result,
            } => {
                if self
                    .current(document_id)
                    .is_some_and(|document| document.noise_profile_ready(result))
                {
                    self.queue_preview();
                }
            }
            WorkerEvent::CameraLookReady {
                document_id,
                camera_profile,
                result,
            } => {
                let changed = self
                    .document
                    .as_mut()
                    .filter(|open| open.id == document_id)
                    .is_some_and(|document| {
                        document.camera_look_ready(&self.coordinator, &camera_profile, result)
                    });
                if changed {
                    self.queue_preview();
                }
            }
            WorkerEvent::Warning {
                document_id,
                message,
            } => {
                if let Some(document) = self.current(document_id) {
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
                if let Some(document) = self.current(document_id) {
                    document.job_failed(job, revision, export_id, message);
                }
            }
            WorkerEvent::WorkerStopped { message } => {
                if let Some(document) = self.document.as_mut() {
                    document.worker_stopped(message);
                }
            }
            // Features this frontend never requests: crop, 1:1, pickers, masks and the neural
            // demosaic.
            WorkerEvent::PreviewReady { .. }
            | WorkerEvent::WhiteBalanceSampleReady { .. }
            | WorkerEvent::WhiteBalanceSampleFailed { .. }
            | WorkerEvent::ColorRangeSampleReady { .. }
            | WorkerEvent::ColorRangeSampleFailed { .. }
            | WorkerEvent::MaskOverlayReady { .. }
            | WorkerEvent::MaskOverlayFailed { .. }
            | WorkerEvent::NeuralDemosaicReady { .. } => {}
        }
    }
}
