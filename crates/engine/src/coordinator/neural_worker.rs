//! Background neural demosaic for interactive previews.
//!
//! A full-resolution neural pass takes seconds on the CPU, far too long for
//! the newest-wins preview queue, where every slider move cancels the active
//! job. So previews that ask for [`DemosaicAlgorithm::Neural`] are developed
//! with the classic stand-in ([`NEURAL_FALLBACK_DEMOSAIC`]) at once, while this
//! thread builds the neural camera source. Only a change of the camera source
//! itself (another photo, crop, highlights, camera profile) cancels it; edits
//! after the demosaic do not. The finished source goes to the render worker's
//! cache and the frontend is told to ask for a fresh preview, which then uses
//! it. Exports run the neural demosaic directly and never come here.

use super::*;
use rohditor_core::{DemosaicedCameraSource, NEURAL_FALLBACK_DEMOSAIC};

use crate::preview_cache::NeuralKey;

/// One neural camera source to build.
struct NeuralJob {
    document_id: u64,
    keys: PreviewCacheKeys,
    key: NeuralKey,
    frame: Arc<RawFrame>,
    recipe: EditRecipe,
    options: PreviewOptions,
    cancellation: CancellationToken,
}

/// A finished (or failed) job, handed back to the render worker.
#[derive(Debug)]
pub struct NeuralResult {
    document_id: u64,
    keys: PreviewCacheKeys,
    key: NeuralKey,
    result: Result<DemosaicedCameraSource, PipelineError>,
}

impl NeuralResult {
    pub(super) const fn document_id(&self) -> u64 {
        self.document_id
    }
}

struct Active {
    document_id: u64,
    key: NeuralKey,
    cancellation: CancellationToken,
}

/// Render-worker side: decides which demosaic a job uses now, and owns the
/// background job.
pub(super) struct NeuralStage {
    /// Shared with the render worker; used for cache keys.
    pipeline: CpuPipeline,
    jobs: Option<mpsc::Sender<NeuralJob>>,
    active: Option<Active>,
    /// The last key that failed, so a broken model is not retried on every preview.
    failed: Option<NeuralKey>,
}

impl NeuralStage {
    /// Starts the thread only when the pipeline has a model.
    pub(super) fn start(pipeline: &CpuPipeline, requests: mpsc::Sender<WorkerRequest>) -> Self {
        let jobs = pipeline
            .neural()
            .is_some()
            .then(|| spawn(pipeline.clone(), requests))
            .flatten();
        Self {
            pipeline: pipeline.clone(),
            jobs,
            active: None,
            failed: None,
        }
    }

    /// The options an interactive job runs with now. Neural requests use it
    /// only when its source is cached; otherwise they get the stand-in and,
    /// with `schedule`, the neural pass is started in the background.
    pub(super) fn route(
        &mut self,
        document_id: u64,
        frame: &Arc<RawFrame>,
        recipe: &EditRecipe,
        options: PreviewOptions,
        cache: &PreviewCache,
        schedule: bool,
    ) -> PreviewOptions {
        if options.render.demosaic != DemosaicAlgorithm::Neural {
            return options;
        }
        let stand_in = PreviewOptions {
            render: RenderOptions {
                demosaic: NEURAL_FALLBACK_DEMOSAIC,
                ..options.render
            },
            ..options
        };
        if self.jobs.is_none() {
            return stand_in;
        }
        let keys =
            PreviewCacheKeys::new_for_pipeline(document_id, frame, recipe, options, &self.pipeline);
        let key = NeuralKey::new(&keys, options, recipe);
        if cache.has_neural(&key) {
            return options;
        }
        let running = self.active.as_ref().is_some_and(|active| active.key == key);
        if !schedule || running || self.failed.as_ref() == Some(&key) {
            return stand_in;
        }
        self.cancel_active();
        let cancellation = CancellationToken::new();
        let job = NeuralJob {
            document_id,
            keys,
            key: key.clone(),
            frame: Arc::clone(frame),
            recipe: recipe.clone(),
            options,
            cancellation: cancellation.clone(),
        };
        if self
            .jobs
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok())
        {
            info!(document_id, "neural demosaic started in the background");
            self.active = Some(Active {
                document_id,
                key,
                cancellation,
            });
        }
        stand_in
    }

    /// Install a finished source and tell the frontend; report failures once.
    pub(super) fn finish(
        &mut self,
        result: NeuralResult,
        cache: &mut PreviewCache,
        sender: &mpsc::Sender<WorkerEvent>,
        wake: &WakeFn,
    ) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.key == result.key)
        {
            self.active = None;
        }
        let NeuralResult {
            document_id,
            keys,
            key,
            result,
        } = result;
        match result {
            Ok(source) => {
                if cache.install_neural(&keys, key, source) {
                    send_event(
                        sender,
                        wake,
                        WorkerEvent::NeuralDemosaicReady { document_id },
                    );
                }
            }
            Err(PipelineError::Cancelled) => {}
            Err(error) => {
                tracing::warn!(%error, "neural demosaic failed");
                self.failed = Some(key);
                send_event(
                    sender,
                    wake,
                    WorkerEvent::Warning {
                        document_id,
                        message: format!(
                            "The neural demosaic failed ({error}); showing {} instead.",
                            NEURAL_FALLBACK_DEMOSAIC.stable_name()
                        ),
                    },
                );
            }
        }
    }

    /// Stop work for a document that was closed.
    pub(super) fn abandon(&mut self, document_id: u64) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.document_id == document_id)
        {
            self.cancel_active();
        }
    }

    fn cancel_active(&mut self) {
        if let Some(active) = self.active.take() {
            active.cancellation.cancel();
        }
    }
}

fn spawn(
    pipeline: CpuPipeline,
    requests: mpsc::Sender<WorkerRequest>,
) -> Option<mpsc::Sender<NeuralJob>> {
    let (jobs, receiver) = mpsc::channel::<NeuralJob>();
    let worker = thread::Builder::new()
        .name("rohditor-neural-worker".to_owned())
        .spawn(move || {
            while let Ok(job) = receiver.recv() {
                if job.cancellation.is_cancelled() {
                    continue;
                }
                let started = Instant::now();
                let result = catch_unwind(AssertUnwindSafe(|| {
                    pipeline.prepare_camera_source(
                        &job.frame,
                        &job.recipe,
                        job.options,
                        &job.cancellation,
                    )
                }))
                .unwrap_or_else(|payload| {
                    Err(PipelineError::Neural {
                        reason: panic_message(payload.as_ref()).to_owned(),
                    })
                });
                info!(
                    document_id = job.document_id,
                    elapsed_ms = started.elapsed().as_millis(),
                    ok = result.is_ok(),
                    "neural demosaic finished"
                );
                let finished = NeuralResult {
                    document_id: job.document_id,
                    keys: job.keys,
                    key: job.key,
                    result,
                };
                if requests
                    .send(WorkerRequest::NeuralFinished(Box::new(finished)))
                    .is_err()
                {
                    break;
                }
            }
        });
    match worker {
        Ok(_) => Some(jobs),
        Err(error) => {
            tracing::warn!(%error, "could not start the neural demosaic worker");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_edit::HighlightMethod;
    use rohditor_neural::{ModelId, NeuralDemosaicer, NeuralError, TileRunner, TileTensor};

    /// A "network" with a flat output, so no ONNX Runtime is needed.
    struct Flat;
    impl TileRunner for Flat {
        fn run_tile(&self, input: &TileTensor) -> Result<TileTensor, NeuralError> {
            let (h, w) = (2 * input.height(), 2 * input.width());
            TileTensor::new(3, h, w, vec![0.25; 3 * h * w])
        }
    }

    fn neural_pipeline() -> CpuPipeline {
        let id = ModelId {
            contract_version: 1,
            sha256: [3; 32],
        };
        CpuPipeline::default().with_neural(Arc::new(NeuralDemosaicer::new(Box::new(Flat), id)))
    }

    fn neural_options() -> PreviewOptions {
        PreviewOptions {
            render: RenderOptions {
                demosaic: DemosaicAlgorithm::Neural,
                ..RenderOptions::default()
            },
            ..PreviewOptions::default()
        }
    }

    fn next_result(receiver: &mpsc::Receiver<WorkerRequest>) -> Box<NeuralResult> {
        match receiver.recv_timeout(Duration::from_secs(30)) {
            Ok(WorkerRequest::NeuralFinished(result)) => result,
            other => panic!("expected a neural result, got {other:?}"),
        }
    }

    #[test]
    fn previews_use_the_stand_in_until_the_background_pass_is_installed() {
        let pipeline = neural_pipeline();
        let (requests, finished) = mpsc::channel();
        let (events, received) = mpsc::channel();
        let wake = crate::wake::noop();
        let mut stage = NeuralStage::start(&pipeline, requests);
        let mut cache = PreviewCache::default();
        let frame = Arc::new(super::super::tests::fake_frame());
        let mut recipe = EditRecipe::default();
        recipe.raw.highlights.method = HighlightMethod::Off;
        let options = neural_options();
        let keys = PreviewCacheKeys::new_for_pipeline(5, &frame, &recipe, options, &pipeline);
        cache.prepare(&keys, &frame);

        let route = |stage: &mut NeuralStage, cache: &PreviewCache, recipe: &EditRecipe| {
            stage
                .route(5, &frame, recipe, options, cache, true)
                .render
                .demosaic
        };
        assert_eq!(route(&mut stage, &cache, &recipe), NEURAL_FALLBACK_DEMOSAIC);
        // The same source is not scheduled twice while it runs.
        assert_eq!(route(&mut stage, &cache, &recipe), NEURAL_FALLBACK_DEMOSAIC);
        let result = next_result(&finished);
        assert!(finished.try_recv().is_err(), "one job only");

        stage.finish(*result, &mut cache, &events, &wake);
        assert!(matches!(
            received.try_recv(),
            Ok(WorkerEvent::NeuralDemosaicReady { document_id: 5 })
        ));
        assert_eq!(
            route(&mut stage, &cache, &recipe),
            DemosaicAlgorithm::Neural
        );

        // Edits after the demosaic reuse the neural source...
        recipe.light.exposure_ev = 1.0;
        assert_eq!(
            route(&mut stage, &cache, &recipe),
            DemosaicAlgorithm::Neural
        );
        // ...a highlight change needs a new pass.
        recipe.raw.highlights.method = HighlightMethod::Clip;
        assert_eq!(route(&mut stage, &cache, &recipe), NEURAL_FALLBACK_DEMOSAIC);
        assert!(next_result(&finished).result.is_ok());
    }

    #[test]
    fn without_a_model_neural_requests_get_the_stand_in() {
        let pipeline = CpuPipeline::default();
        let (requests, _finished) = mpsc::channel();
        let mut stage = NeuralStage::start(&pipeline, requests);
        let frame = Arc::new(super::super::tests::fake_frame());
        let routed = stage.route(
            1,
            &frame,
            &EditRecipe::default(),
            neural_options(),
            &PreviewCache::default(),
            true,
        );
        assert_eq!(routed.render.demosaic, NEURAL_FALLBACK_DEMOSAIC);
    }
}
