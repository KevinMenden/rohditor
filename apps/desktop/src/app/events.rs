//! Translation from background-worker events into current-document UI state.
//!
//! This module deliberately owns no scheduler or GPU resources. It is the
//! narrow identity/revision guard between asynchronous results and `app.rs`.

use eframe::egui;

use rohditor_engine::coordinator::{PreviewResolution, WorkerEvent};
use rohditor_engine::document::PreviewTicket;
use rohditor_engine::preview::{DocumentPreviewDiagnostics, PreviewSource};

use super::{Document, RohditorApp, install_texture};

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
                preview_ticket,
                export_id,
                detail,
            } => {
                if let Some(document) = self.current_document_mut(document_id) {
                    document
                        .core
                        .job_progress(job, revision, preview_ticket, export_id, detail);
                }
            }
            WorkerEvent::MetadataReady { document_id, info } => {
                if let Some(document) = self.current_document_mut(document_id) {
                    document.core.metadata_ready(*info);
                }
            }
            WorkerEvent::OpticsMatchReady {
                document_id,
                result,
                candidates,
                database,
                error,
            } => {
                if let Some(document) = self.current_document_mut(document_id) {
                    document
                        .core
                        .optics_match_ready(result, candidates, database, error);
                }
            }
            WorkerEvent::PlaceholderReady { document_id, image } => {
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|doc| doc.core.id == document_id)
                    && !matches!(
                        document.core.preview.source,
                        Some(source) if source.is_developed()
                    )
                {
                    install_texture(context, document, image, PreviewSource::Embedded);
                }
            }
            WorkerEvent::RawReady { document_id, frame } => {
                if let Some(document) = self.current_document_mut(document_id) {
                    document.core.raw_ready(frame);
                    self.queue_preview(context, document_id);
                    self.start_default_noise_estimate(document_id);
                }
            }
            WorkerEvent::PreviewReady {
                ticket,
                resolution,
                image,
                histogram,
                diagnostics,
            } => {
                let intent = match resolution {
                    PreviewResolution::Fit => {
                        rohditor_engine::document::PreviewIntent::CommittedFit
                    }
                    PreviewResolution::CropToolFullFrame => {
                        rohditor_engine::document::PreviewIntent::CropToolFullFrame
                    }
                    PreviewResolution::SourceScale => {
                        rohditor_engine::document::PreviewIntent::SourceScale
                    }
                };
                let should_install = self.document.as_ref().is_some_and(|document| {
                    document.core.accepts_preview(ticket, intent)
                        && match resolution {
                            PreviewResolution::Fit => {
                                !document.core.preview.source_scale_requested
                                    && self.tool.crop().is_none()
                            }
                            PreviewResolution::CropToolFullFrame => self.tool.crop().is_some(),
                            PreviewResolution::SourceScale => {
                                document.core.preview.source_scale_requested
                            }
                        }
                        && (matches!(
                            resolution,
                            PreviewResolution::SourceScale | PreviewResolution::CropToolFullFrame
                        ) || document
                            .core
                            .preview
                            .gpu()
                            .is_none_or(|preview| preview.ticket != ticket))
                });
                if should_install {
                    // Swap directly from the retained GPU frame to the ready
                    // CPU frame during one update, so an unsupported edit such
                    // as HSL never exposes an empty viewport between backends.
                    // A crop-tool frame is a temporary CPU presentation. Keep
                    // the resident uncropped GPU source so leaving the crop tool can
                    // return to an interactive GPU crop without another upload.
                    if resolution != PreviewResolution::CropToolFullFrame {
                        self.release_document_gpu_preview(ticket.document_id);
                    }
                    let Some(document) = self.document.as_mut() else {
                        return;
                    };
                    let previous_source = document.core.preview.source;
                    let source = if resolution == PreviewResolution::SourceScale {
                        PreviewSource::OneToOneCpu
                    } else {
                        PreviewSource::developed(diagnostics.algorithm, false)
                    };
                    install_texture(context, document, image, source);
                    if resolution != PreviewResolution::CropToolFullFrame {
                        document.core.preview.histogram = Some(*histogram);
                        document.core.preview.histogram_revision = Some(ticket.revision);
                    }
                    if resolution == PreviewResolution::SourceScale {
                        document.core.preview.view.source_scale_frame_ready(
                            previous_source,
                            context.input(|input| input.time),
                        );
                    }
                    document.core.preview.status = None;
                    document.core.preview.last_time = Some(diagnostics.timings.total);
                    document.core.preview.diagnostics =
                        Some(DocumentPreviewDiagnostics::cpu(diagnostics));
                    document.core.error = None;
                    if resolution == PreviewResolution::CropToolFullFrame
                        && let Some(session) = self.tool.crop_mut()
                    {
                        session.set_full_dimensions(
                            document
                                .texture
                                .as_ref()
                                .map_or(1, |texture| texture.dimensions().0),
                            document
                                .texture
                                .as_ref()
                                .map_or(1, |texture| texture.dimensions().1),
                        );
                    }
                }
            }
            WorkerEvent::GpuUploadReady {
                ticket,
                source,
                diagnostics,
                upload_preparation,
            } => {
                self.install_gpu_upload(context, ticket, *source, diagnostics, upload_preparation);
            }
            WorkerEvent::GpuSpatialFailed { ticket, message } => {
                if self
                    .document
                    .as_ref()
                    .is_some_and(|document| document.core.ticket() == ticket)
                {
                    self.handle_gpu_failure(context, ticket.document_id, message);
                }
            }
            WorkerEvent::WhiteBalanceSampleReady { ticket, sample } => {
                self.white_balance_picked(context, ticket, Ok(sample));
            }
            WorkerEvent::ColorMixerSampled { ticket, channel } => {
                let picked = self
                    .current_document_mut(ticket.document_id)
                    .and_then(|document| document.core.color_mixer_picked(ticket, channel));
                self.color_mixer_channel = picked.unwrap_or(self.color_mixer_channel);
            }
            WorkerEvent::ColorRangeSampleReady {
                ticket,
                pick,
                sample,
            } => self.apply_color_sample(context, ticket, pick, sample),
            WorkerEvent::ColorRangeSampleFailed { ticket, message } => {
                self.fail_color_sample(ticket, message);
            }
            WorkerEvent::MaskOverlayReady {
                ticket,
                layer,
                overlay,
            } => self.install_range_overlay(context, ticket, layer, &overlay),
            WorkerEvent::MaskOverlayFailed { ticket, .. } => self.fail_range_overlay(ticket),
            WorkerEvent::WhiteBalanceSampleFailed { ticket, message } => {
                self.white_balance_picked(context, ticket, Err(message));
            }
            WorkerEvent::ExportReady {
                document_id,
                export_id,
                recipe_revision,
                destination,
                report,
                elapsed,
            } => {
                if let Some(document) = self.current_document_mut(document_id) {
                    document.core.export_ready(
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
            } => self.noise_profile_ready(context, document_id, result),
            WorkerEvent::CameraLookReady {
                document_id,
                camera_profile,
                result,
            } => self.camera_look_ready(context, document_id, &camera_profile, result),
            WorkerEvent::NeuralDemosaicReady { document_id } => {
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|doc| doc.core.id == document_id)
                {
                    document.core.preview.neural_ready = true;
                    self.refresh_current_presentation(context);
                }
            }
            WorkerEvent::Warning {
                document_id,
                message,
            } => {
                if let Some(document) = self.current_document_mut(document_id) {
                    document.core.warning = Some(message);
                }
            }
            WorkerEvent::Failed {
                document_id,
                job,
                revision,
                export_id,
                message,
            } => {
                if let Some(document) = self.current_document_mut(document_id) {
                    document.core.job_failed(job, revision, export_id, message);
                }
            }
            WorkerEvent::WorkerStopped { message } => {
                if let Some(document) = self.document.as_mut() {
                    document.core.worker_stopped(message);
                }
            }
        }
    }

    /// The open document, when it is the one a worker result is for.
    fn current_document_mut(&mut self, document_id: u64) -> Option<&mut Document> {
        self.document
            .as_mut()
            .filter(|doc| doc.core.id == document_id)
    }

    fn white_balance_picked(
        &mut self,
        context: &egui::Context,
        ticket: PreviewTicket,
        sample: Result<[f32; 3], String>,
    ) {
        if self
            .current_document_mut(ticket.document_id)
            .is_some_and(|document| document.core.white_balance_picked(ticket, sample))
        {
            self.queue_preview(context, ticket.document_id);
        }
    }
}
