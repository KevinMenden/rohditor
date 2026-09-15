//! Budgeted camera-source and capture caches, before optics and reduction.
use super::*;
use rohditor_core::{
    CancellationToken, CapturedCameraSource, DemosaicedCameraSource, PipelineError,
};
use rohditor_gpu::{GpuCaptureProcessor, GpuPreviewError};

const CACHE_BUDGET: usize = 768 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreCaptureKey {
    decoded: DecodedRawKey,
    crop: RawCropPolicy,
    demosaic: DemosaicAlgorithm,
    highlight: HighlightKey,
    profile: CameraProfileKey,
    version: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CaptureKey {
    source: PreCaptureKey,
    settings: Option<([u32; 3], u16)>,
    ceilings: [u32; 3],
    gpu: bool,
}

#[derive(Default)]
pub(super) struct SpatialCache {
    pub recovery: Option<String>,
    source: Option<(PreCaptureKey, DemosaicedCameraSource)>,
    captured: Option<(CaptureKey, CapturedCameraSource)>,
    gpu: Option<GpuCaptureProcessor>,
    device: Option<(wgpu::Device, wgpu::Queue)>,
}

impl std::fmt::Debug for SpatialCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpatialCache")
            .field("resident_bytes", &self.resident_bytes())
            .finish_non_exhaustive()
    }
}

impl SpatialCache {
    pub fn configure(&mut self, device: wgpu::Device, queue: wgpu::Queue) {
        self.gpu = None;
        self.device = Some((device, queue));
        self.clear_images();
    }

    pub fn clear_images(&mut self) {
        self.source = None;
        self.captured = None;
        self.recovery = None;
    }
    pub fn resident_bytes(&self) -> usize {
        self.source.as_ref().map_or(0, |(_, s)| s.buffer_bytes())
            + self.captured.as_ref().map_or(0, |(_, s)| s.buffer_bytes())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        cpu: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        keys: &PreviewCacheKeys,
        prefer_gpu: bool,
        retained_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<ReconstructedPreview, PipelineError> {
        cancellation.checkpoint()?;
        let key = PreCaptureKey {
            decoded: keys.decoded.clone(),
            crop: options.render.raw_crop_policy,
            demosaic: options.render.demosaic,
            highlight: keys.reconstructed.highlight.clone(),
            profile: camera_profile_key(&recipe.color.camera_profile),
            version: keys.reconstructed.reconstruction_version,
        };
        if self.source.as_ref().is_some_and(|(k, _)| k != &key) {
            self.source = None;
            self.captured = None;
        }
        let source = if let Some((_, source)) = &self.source {
            source.clone().with_recipe(recipe, options)?
        } else {
            cpu.prepare_camera_source(frame, recipe, options, cancellation)?
        };
        let contract = source.capture_contract()?;
        let capture_key = CaptureKey {
            source: key.clone(),
            settings: keys.reconstructed.capture_sharpening,
            ceilings: contract.ceilings().map(f32::to_bits),
            gpu: prefer_gpu && contract.settings().is_active(),
        };
        if self
            .captured
            .as_ref()
            .is_some_and(|(k, _)| k != &capture_key)
        {
            self.captured = None;
        }

        // Reserve room for reference capture scratch, copy-on-write completion,
        // optics, and existing preview buffers before retaining full images.
        let bytes = source.buffer_bytes();
        let retain = bytes
            .checked_mul(6)
            .and_then(|n| n.checked_add(retained_bytes))
            .is_some_and(|n| n <= rohditor_core::CPU_WORKING_SET_LIMIT_BYTES)
            && bytes <= CACHE_BUDGET / 2;
        if !retain {
            self.source = None;
            self.captured = None;
        }
        if let Some((_, captured)) = &self.captured {
            let captured = captured.clone().with_recipe(recipe, options)?;
            return cpu.complete_camera_source(captured, cancellation);
        }
        self.recovery = None;
        if retain {
            self.source = Some((key, source.clone()));
        }
        let captured = if capture_key.gpu {
            let result = (|| {
                if self.gpu.is_none() {
                    let (device, queue) =
                        self.device
                            .as_ref()
                            .ok_or_else(|| GpuPreviewError::Unsupported {
                                reason: "preview capture device is unavailable".into(),
                            })?;
                    let mut gpu = GpuCaptureProcessor::new(device, queue)?;
                    // Fit color source, display targets, and UI resources share
                    // this device. Reserve only 64 MiB of the existing budget
                    // for capture; the maximum 512-core tile needs under 24 MiB.
                    gpu.set_remaining_budget(64 * 1024 * 1024);
                    self.gpu = Some(gpu);
                }
                let gpu = self.gpu.as_mut().expect("capture device initialized");
                gpu.capture_source(source.clone(), cancellation)
            })();
            match result {
                Ok((captured, metrics)) => {
                    tracing::info!(
                        capture_ms = metrics.total.as_millis(),
                        gpu_bytes = metrics.estimated_gpu_bytes,
                        combined_gpu_reserved_bytes =
                            metrics.combined_gpu_reservations.current_bytes,
                        combined_gpu_peak_reserved_bytes =
                            metrics.combined_gpu_reservations.peak_bytes,
                        upload_bytes = metrics.uploaded_bytes,
                        readback_bytes = metrics.readback_bytes,
                        tile = metrics.tile_edge,
                        halo = metrics.halo,
                        "GPU capture bridge complete"
                    );
                    captured
                }
                Err(GpuPreviewError::Cancelled) => return Err(PipelineError::Cancelled),
                Err(error) => {
                    self.recovery = Some(error.to_string());
                    self.gpu = None;
                    self.captured = None;
                    cancellation.checkpoint()?;
                    tracing::warn!(%error, "GPU capture unavailable; restarting capture from unmodified camera source on CPU");
                    // Evict the extra reference before CPU's budgeted scratch.
                    self.source = None;
                    source.capture_cpu(cancellation)?
                }
            }
        } else {
            source.capture_cpu(cancellation)?
        };
        cancellation.checkpoint()?;
        if retain {
            self.captured = Some((capture_key, captured.clone()));
        }
        cpu.complete_camera_source(captured, cancellation)
    }
}
