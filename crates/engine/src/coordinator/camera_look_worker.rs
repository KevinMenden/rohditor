//! Camera-look fitting on its own thread, so the one-off CPU render and JPEG
//! decode never delay interactive previews.
use super::*;
use rohditor_core::{CameraLook, match_camera_look};
use rohditor_edit::CameraProfileSelection;

/// Everything the fit needs. The embedded preview is read from `path` on the
/// worker thread, so the frontend only hands over cheap shared state.
#[derive(Debug)]
pub struct CameraLookJob {
    pub(super) document_id: u64,
    pub(super) path: PathBuf,
    pub(super) frame: Arc<RawFrame>,
    pub(super) camera_profile: CameraProfileSelection,
}

pub(super) fn start(
    pipeline: CpuPipeline,
    decoder: Arc<dyn RawDecoder>,
    sender: mpsc::Sender<WorkerEvent>,
    wake: WakeFn,
) -> Option<mpsc::Sender<Box<CameraLookJob>>> {
    let (requests, receiver) = mpsc::channel::<Box<CameraLookJob>>();
    let worker = thread::Builder::new()
        .name("rohditor-camera-look-worker".to_owned())
        .spawn(move || {
            while let Ok(job) = receiver.recv() {
                let document_id = job.document_id;
                let result =
                    catch_unwind(AssertUnwindSafe(|| fit(&job, &pipeline, decoder.as_ref())))
                        .unwrap_or_else(|payload| {
                            Err(format!(
                                "Camera look fit failed: {}",
                                panic_message(payload.as_ref())
                            ))
                        });
                send_event(
                    &sender,
                    &wake,
                    WorkerEvent::CameraLookReady {
                        document_id,
                        camera_profile: job.camera_profile.clone(),
                        result: result.map(Box::new),
                    },
                );
            }
        });
    match worker {
        Ok(_) => Some(requests),
        Err(error) => {
            tracing::warn!(%error, "could not start camera look worker");
            None
        }
    }
}

fn fit(
    job: &CameraLookJob,
    pipeline: &CpuPipeline,
    decoder: &dyn RawDecoder,
) -> Result<CameraLook, String> {
    let preview = decoder
        .embedded_preview(&job.path)
        .map_err(|error| format!("Could not read the embedded preview: {error}"))?
        .ok_or_else(|| "The file has no embedded preview to match".to_owned())?;
    match_camera_look(pipeline, &job.frame, &preview, &job.camera_profile)
        .map_err(|error| error.to_string())
}
