//! Serial export execution, independent of preview/source preparation requests.
use super::*;
use rohditor_gpu::{ExportBackend, ExportRender, FallbackExporter};

pub(super) fn start(
    pipeline: CpuPipeline,
    sender: mpsc::Sender<WorkerEvent>,
    wake: WakeFn,
) -> Option<mpsc::SyncSender<Box<ExportJob>>> {
    // One active export and one queued immutable snapshot bound retained work.
    let (requests, receiver) = mpsc::sync_channel::<Box<ExportJob>>(1);
    let worker = thread::Builder::new()
        .name("rohditor-export-worker".to_owned())
        .spawn(move || {
            let mut exporter = FallbackExporter::default();
            while let Ok(job) = receiver.recv() {
                let identity = (job.document_id, job.export_id, job.recipe_revision);
                let result = catch_unwind(AssertUnwindSafe(|| {
                    process_export(*job, &sender, &wake, &pipeline, &mut exporter);
                }));
                if let Err(payload) = result {
                    exporter.reset();
                    send_failure(
                        &sender,
                        &wake,
                        identity.0,
                        JobKind::Export,
                        Some(identity.2),
                        Some(identity.1),
                        format!("Export failed: {}", panic_message(payload.as_ref())),
                    );
                }
            }
        });
    match worker {
        Ok(_) => Some(requests),
        Err(error) => {
            tracing::warn!(%error, "could not start export worker");
            None
        }
    }
}

pub(super) fn submit(
    worker: &Option<mpsc::SyncSender<Box<ExportJob>>>,
    job: Box<ExportJob>,
) -> Result<(), Box<ExportJob>> {
    match worker {
        Some(worker) => worker.try_send(job).map_err(|error| match error {
            mpsc::TrySendError::Full(job) | mpsc::TrySendError::Disconnected(job) => job,
        }),
        None => Err(job),
    }
}

fn process_export(
    job: ExportJob,
    sender: &mpsc::Sender<WorkerEvent>,
    wake: &WakeFn,
    pipeline: &CpuPipeline,
    exporter: &mut FallbackExporter,
) {
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
        wake,
        job.document_id,
        JobKind::Export,
        Some(job.recipe_revision),
        Some(job.export_id),
        "Developing full-resolution export",
    );
    let backend = if job.prefer_gpu {
        ExportBackend::PreferGpu
    } else {
        ExportBackend::Cpu
    };
    let rendered = exporter.render(
        backend,
        pipeline,
        &job.frame,
        &job.recipe,
        job.render_options,
        job.settings.format.bit_depth(),
        job.settings.dithering,
        job.settings.size,
        job.settings.sharpening,
        |error| {
            tracing::warn!(%error, "GPU export failed; using CPU reference");
            send_progress(
                sender,
                wake,
                job.document_id,
                JobKind::Export,
                Some(job.recipe_revision),
                Some(job.export_id),
                &format!("GPU unavailable; exporting on CPU: {error}"),
            );
        },
    );
    let image = match rendered {
        Ok(rendered) => {
            if let ExportRender::Gpu {
                result,
                capabilities,
            } = &rendered
            {
                info!(adapter = %capabilities.adapter_name, driver = %capabilities.driver_info, "GPU export device");
                info!(
                    sensor_backend = if result.sensor_gpu { "GPU" } else { "CPU" },
                    upload_ms = result.upload_time.as_millis(),
                    color_readback_ms = result.color_and_readback_time.as_millis(),
                    gpu_bytes = result.estimated_gpu_bytes,
                    combined_gpu_reserved_bytes = result.combined_gpu_reservations.current_bytes,
                    combined_gpu_peak_reserved_bytes = result.combined_gpu_reservations.peak_bytes,
                    bands = result.submissions,
                    "GPU export color processing complete"
                );
            }
            rendered.into_image()
        }
        Err(error) => {
            send_failure(
                sender,
                wake,
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
        wake,
        job.document_id,
        JobKind::Export,
        Some(job.recipe_revision),
        Some(job.export_id),
        "Encoding and committing output",
    );
    match export_image(&job.destination, &image, &job.frame.info, job.settings) {
        Ok(report) => {
            let elapsed = started.elapsed();
            info!(
                elapsed_ms = elapsed.as_millis(),
                bytes = report.bytes_written,
                "export complete"
            );
            send_event(
                sender,
                wake,
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
            wake,
            job.document_id,
            JobKind::Export,
            Some(job.recipe_revision),
            Some(job.export_id),
            format!("Could not write {file_name}: {error}"),
        ),
    }
}
