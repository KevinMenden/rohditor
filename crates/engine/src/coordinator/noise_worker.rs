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
            while let Ok(first) = receiver.recv() {
                let job = newest(first, &receiver);
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

/// Skip jobs that queued up behind `first`, keeping the newest. A frontend shows
/// one document at a time and ignores profiles of any other, so a job that a
/// later one has overtaken (the user moved on to another photo) would cost a
/// full-resolution demosaic for a result nobody uses.
fn newest<T>(first: T, receiver: &mpsc::Receiver<T>) -> T {
    receiver.try_iter().last().unwrap_or(first)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_newest_queued_job_runs() {
        let (sender, receiver) = mpsc::channel();
        for job in [2, 3, 4] {
            sender.send(job).expect("queue job");
        }
        assert_eq!(newest(1, &receiver), 4);
        assert!(receiver.try_recv().is_err(), "the queue is drained");
    }

    #[test]
    fn a_lone_job_runs_as_is() {
        let (_sender, receiver) = mpsc::channel();
        assert_eq!(newest(1, &receiver), 1);
    }
}
