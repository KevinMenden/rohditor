//! Translates render-worker events into document state.

use rohditor_engine::coordinator::{PreviewResolution, WorkerEvent};
use rohditor_engine::document::{PreviewIntent, PreviewTicket};
use rohditor_engine::open_document::OpenDocument;
use rohditor_engine::preview::{DocumentPreviewDiagnostics, GpuUpload, PreviewSource};

use super::Session;
use super::display::FrameKind;

impl Session {
    pub(super) fn process_worker_events(&mut self) {
        let events: Vec<_> = self.renderer.coordinator.try_events().collect();
        for event in events {
            self.process_worker_event(event);
        }
    }

    fn current(&mut self, document_id: u64) -> Option<&mut OpenDocument> {
        self.document.as_mut().filter(|open| open.id == document_id)
    }

    fn white_balance_picked(&mut self, ticket: PreviewTicket, sample: Result<[f32; 3], String>) {
        if self
            .current(ticket.document_id)
            .is_some_and(|document| document.white_balance_picked(ticket, sample))
        {
            self.queue_preview();
        }
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
            WorkerEvent::LensProfilesReady {
                document_id,
                profiles,
            } => {
                if let Some(document) = self.current(document_id) {
                    document.lens_profiles_ready(profiles);
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
                    self.display.show_cpu_image(
                        FrameKind::Embedded,
                        image.width,
                        image.height,
                        &image.pixels,
                    );
                }
            }
            WorkerEvent::RawReady { document_id, frame } => {
                let render_options = self.renderer.settings.render_options();
                if let Some(document) = self.current(document_id) {
                    document.raw_ready(frame);
                    self.queue_preview();
                    if let Some(document) = self.document.as_mut() {
                        document.start_default_noise_estimate(
                            &self.renderer.coordinator,
                            render_options,
                        );
                    }
                }
            }
            WorkerEvent::PreviewReady {
                ticket,
                resolution,
                image,
                histogram,
                diagnostics,
            } => {
                let source_scale = resolution == PreviewResolution::SourceScale;
                let now = self.renderer.now();
                let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|open| open.id == ticket.document_id)
                else {
                    return;
                };
                // A fit frame is stale once 1:1 was asked for, and the other way round.
                let intent = if source_scale {
                    PreviewIntent::SourceScale
                } else {
                    PreviewIntent::CommittedFit
                };
                let superseded = !source_scale
                    && document
                        .preview
                        .gpu()
                        .is_some_and(|gpu| gpu.ticket == ticket);
                if superseded
                    || document.preview.source_scale_requested != source_scale
                    || !document.accepts_preview(ticket, intent)
                {
                    return;
                }
                document.preview.release_gpu();
                let preview = &mut document.preview;
                let previous_source = preview.source;
                preview.source = Some(if source_scale {
                    PreviewSource::OneToOneCpu
                } else {
                    PreviewSource::developed(diagnostics.algorithm, false)
                });
                if source_scale {
                    preview.view.source_scale_frame_ready(previous_source, now);
                }
                preview.cpu_frame_installed(*histogram, ticket.revision);
                preview.status = None;
                preview.last_time = Some(diagnostics.timings.total);
                preview.diagnostics = Some(DocumentPreviewDiagnostics::cpu(diagnostics));
                self.display.sync(preview);
                document.error = None;
                self.display.show_cpu_image(
                    FrameKind::Cpu,
                    image.width,
                    image.height,
                    &image.pixels,
                );
            }
            WorkerEvent::GpuUploadReady {
                ticket,
                source,
                diagnostics,
                upload_preparation,
            } => {
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|open| open.id == ticket.document_id)
                {
                    let display = &mut self.display;
                    self.renderer.run(
                        document,
                        display,
                        |runtime, coordinator, preview, request| {
                            let upload = GpuUpload {
                                ticket,
                                prepared: *source,
                                diagnostics,
                                upload_preparation,
                            };
                            let outcome =
                                runtime.install_gpu_upload(coordinator, preview, request, upload);
                            ((), outcome)
                        },
                    );
                }
            }
            WorkerEvent::GpuSpatialFailed { ticket, message } => {
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|document| document.ticket() == ticket)
                {
                    let display = &mut self.display;
                    self.renderer.run(
                        document,
                        display,
                        |runtime, coordinator, preview, request| {
                            let outcome =
                                runtime.handle_gpu_failure(coordinator, preview, request, message);
                            ((), outcome)
                        },
                    );
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
                        document.camera_look_ready(
                            &self.renderer.coordinator,
                            &camera_profile,
                            result,
                        )
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
            WorkerEvent::WhiteBalanceSampleReady { ticket, sample } => {
                self.white_balance_picked(ticket, Ok(sample));
            }
            WorkerEvent::WhiteBalanceSampleFailed { ticket, message } => {
                self.white_balance_picked(ticket, Err(message));
            }
            WorkerEvent::ColorMixerSampled { ticket, channel } => {
                self.picked_channel = self
                    .current(ticket.document_id)
                    .and_then(|document| document.color_mixer_picked(ticket, channel));
            }
            // Features this frontend never requests: masks and the neural demosaic.
            WorkerEvent::ColorRangeSampleReady { .. }
            | WorkerEvent::ColorRangeSampleFailed { .. }
            | WorkerEvent::MaskOverlayReady { .. }
            | WorkerEvent::MaskOverlayFailed { .. }
            | WorkerEvent::NeuralDemosaicReady { .. } => {}
        }
    }
}
