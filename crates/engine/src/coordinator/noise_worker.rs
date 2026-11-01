//! Noise estimation on its own thread. Profiling needs a full-resolution
//! demosaic, so it must never delay interactive previews.
use super::*;
use rohditor_core::{NoiseProfile, estimate_noise_profile};

/// Everything the estimate needs; the frame is shared, not copied.
#[derive(Debug)]
pub struct NoiseProfileJob {
    pub(super) document_id: u64,
    pub(super) frame: Arc<RawFrame>,
    /// Demosaic and output options the photo is developed with, so the estimate
    /// sees the same reconstruction the noise filters will.
    pub(super) render_options: RenderOptions,
}

pub(super) fn start(
    pipeline: CpuPipeline,
    sender: mpsc::Sender<WorkerEvent>,
    wake: WakeFn,
) -> Option<mpsc::Sender<Box<NoiseProfileJob>>> {
    let (requests, receiver) = mpsc::channel::<Box<NoiseProfileJob>>();
    let worker = thread::Builder::new()
        .name("rohditor-noise-worker".to_owned())
        .spawn(move || {
            while let Ok(job) = receiver.recv() {
                let document_id = job.document_id;
                let result = catch_unwind(AssertUnwindSafe(|| estimate(&job, &pipeline)))
                    .unwrap_or_else(|payload| {
                        Err(format!(
                            "Noise estimate failed: {}",
                            panic_message(payload.as_ref())
                        ))
                    });
                send_event(
                    &sender,
                    &wake,
                    WorkerEvent::NoiseProfileReady {
                        document_id,
                        result,
                    },
                );
            }
        });
    match worker {
        Ok(_) => Some(requests),
        Err(error) => {
            tracing::warn!(%error, "could not start noise worker");
            None
        }
    }
}

/// `Ok(None)`: the frame is too small to profile.
fn estimate(job: &NoiseProfileJob, pipeline: &CpuPipeline) -> Result<Option<NoiseProfile>, String> {
    estimate_noise_profile(
        pipeline,
        &job.frame,
        &EditRecipe::default(),
        job.render_options,
    )
    .map_err(|error| error.to_string())
}
