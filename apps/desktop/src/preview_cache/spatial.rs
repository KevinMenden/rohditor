//! Budgeted camera-source and capture caches, before optics and reduction.
use super::*;
use rohditor_core::{
    CancellationToken, CapturedCameraSource, DemosaicedCameraSource, OutputPolicy, PipelineError,
};
use rohditor_gpu::{
    GpuCapturedSource, GpuPreviewError, GpuPreviewFrame, GpuPreviewProcessor, GpuSpatialFullSource,
    GpuSpatialPreview, GpuSpatialProcessor, SpatialMetrics,
};

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
    gpu_captured: Option<(CaptureKey, GpuCapturedSource)>,
    sensor_source: Option<(CaptureKey, rohditor_gpu::GpuDevelopedSource)>,
    sensor: Option<rohditor_gpu::GpuSensorProcessor>,
    gpu: Option<GpuSpatialProcessor>,
    gpu_display: Option<GpuPreviewProcessor>,
    device: Option<(wgpu::Device, wgpu::Queue)>,
    display: Option<(wgpu::Adapter, wgpu::TextureFormat)>,
}

impl std::fmt::Debug for SpatialCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpatialCache")
            .field("resident_bytes", &self.resident_bytes())
            .finish_non_exhaustive()
    }
}

impl SpatialCache {
    pub fn configure(
        &mut self,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        target_format: wgpu::TextureFormat,
    ) {
        self.gpu = None;
        self.sensor = None;
        self.gpu_display = None;
        self.device = Some((device, queue));
        self.display = Some((adapter, target_format));
        self.clear_images();
    }

    pub fn clear_images(&mut self) {
        self.source = None;
        self.captured = None;
        self.gpu_captured = None;
        self.sensor_source = None;
        self.recovery = None;
    }
    pub fn release_gpu_images(&mut self) {
        self.gpu_captured = None;
        self.sensor_source = None;
        self.gpu_display = None;
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
        _prefer_gpu: bool,
        retained_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<ReconstructedPreview, PipelineError> {
        cancellation.checkpoint()?;
        // CPU selection and automatic recovery must release worker-owned GPU
        // images even when the upstream camera-source key is unchanged.
        self.gpu_captured = None;
        self.gpu = None;
        self.sensor_source = None;
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
            self.gpu_captured = None;
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
            gpu: false,
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
        let captured = source.capture_cpu(cancellation)?;
        cancellation.checkpoint()?;
        if retain {
            self.captured = Some((capture_key, captured.clone()));
        }
        cpu.complete_camera_source(captured, cancellation)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_gpu(
        &mut self,
        cpu: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        keys: &PreviewCacheKeys,
        cancellation: &CancellationToken,
    ) -> Result<(GpuSpatialPreview, SpatialMetrics), GpuPreviewError> {
        let result = self.prepare_gpu_inner(cpu, frame, recipe, options, keys, cancellation, false);
        self.record_gpu_result(&result);
        result.map(|(source, metrics)| match source {
            PreparedSpatial::Preview(preview) => (preview, metrics),
            PreparedSpatial::Full(_) => unreachable!("preview preparation returned a full source"),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_gpu_full(
        &mut self,
        cpu: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        keys: &PreviewCacheKeys,
        cancellation: &CancellationToken,
    ) -> Result<(GpuSpatialFullSource, SpatialMetrics), GpuPreviewError> {
        let result = self.prepare_gpu_inner(cpu, frame, recipe, options, keys, cancellation, true);
        self.record_gpu_result(&result);
        result.map(|(source, metrics)| match source {
            PreparedSpatial::Full(source) => (source, metrics),
            PreparedSpatial::Preview(_) => unreachable!("full preparation returned a preview"),
        })
    }

    pub fn render_gpu_full(
        &mut self,
        source: &GpuSpatialFullSource,
        recipe: &EditRecipe,
        output_policy: OutputPolicy,
        cancellation: &CancellationToken,
    ) -> Result<GpuPreviewFrame, GpuPreviewError> {
        let result = (|| {
            if self.gpu_display.is_none() {
                let (adapter, target_format) =
                    self.display
                        .as_ref()
                        .ok_or_else(|| GpuPreviewError::Unsupported {
                            reason: "preview display device is unavailable".into(),
                        })?;
                let (device, queue) =
                    self.device
                        .as_ref()
                        .ok_or_else(|| GpuPreviewError::Unsupported {
                            reason: "preview display device is unavailable".into(),
                        })?;
                self.gpu_display = Some(GpuPreviewProcessor::new(
                    adapter,
                    device,
                    queue,
                    *target_format,
                )?);
            }
            self.gpu_display
                .as_ref()
                .expect("display processor initialized")
                .render_spatial_full_cancellable(source, recipe, output_policy, None, cancellation)
        })();
        self.record_gpu_result(&result);
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_gpu_inner(
        &mut self,
        cpu: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        keys: &PreviewCacheKeys,
        cancellation: &CancellationToken,
        full_resolution: bool,
    ) -> Result<(PreparedSpatial, SpatialMetrics), GpuPreviewError> {
        if cancellation.is_cancelled() {
            return Err(GpuPreviewError::Cancelled);
        }
        let key = PreCaptureKey {
            decoded: keys.decoded.clone(),
            crop: options.render.raw_crop_policy,
            demosaic: options.render.demosaic,
            highlight: keys.reconstructed.highlight.clone(),
            profile: camera_profile_key(&recipe.color.camera_profile),
            version: keys.reconstructed.reconstruction_version,
        };
        let sensor_result = self.prepare_sensor(
            cpu,
            frame,
            recipe,
            options,
            keys,
            key.clone(),
            cancellation,
            full_resolution,
        );
        let sensor_failure = match sensor_result {
            Ok(result) => {
                self.recovery = None;
                return Ok(result);
            }
            Err(GpuPreviewError::Cancelled) => return Err(GpuPreviewError::Cancelled),
            Err(error) => {
                self.sensor_source = None;
                self.sensor = None;
                if !matches!(error, GpuPreviewError::UnsupportedEdits { .. }) {
                    self.gpu = None;
                    self.gpu_display = None;
                }
                tracing::warn!(%error, "GPU sensor development unavailable; using CPU sensor with GPU spatial/color processing");
                Some(error.to_string())
            }
        };
        if self
            .source
            .as_ref()
            .is_some_and(|(stored, _)| stored != &key)
        {
            self.source = None;
            self.captured = None;
            self.gpu_captured = None;
        }
        let source = if let Some((_, source)) = &self.source {
            source
                .clone()
                .with_recipe(recipe, options)
                .map_err(gpu_error)?
        } else {
            // Without the retained immutable camera source, the newly prepared
            // description has a new identity and cannot relabel an old upload.
            self.gpu_captured = None;
            cpu.prepare_camera_source(frame, recipe, options, cancellation)
                .map_err(gpu_error)?
        };
        let contract = source.capture_contract().map_err(gpu_error)?;
        let capture_key = CaptureKey {
            source: key.clone(),
            settings: keys.reconstructed.capture_sharpening,
            ceilings: contract.ceilings().map(f32::to_bits),
            gpu: true,
        };
        if self
            .gpu_captured
            .as_ref()
            .is_some_and(|(stored, _)| stored != &capture_key)
        {
            self.gpu_captured = None;
        }
        let description = cpu
            .describe_spatial_completion(&source, cancellation)
            .map_err(gpu_error)?;
        // Retaining the CPU source is an optimization, not a requirement for
        // resident GPU reuse. Leave room for subsequent CPU fallback scratch.
        if source.buffer_bytes() <= CACHE_BUDGET / 2
            && source
                .buffer_bytes()
                .checked_mul(6)
                .is_some_and(|bytes| bytes <= rohditor_core::CPU_WORKING_SET_LIMIT_BYTES)
        {
            self.source = Some((key, source.clone()));
        } else {
            self.source = None;
        }
        self.captured = None;
        if self.gpu.is_none() {
            let (device, queue) =
                self.device
                    .as_ref()
                    .ok_or_else(|| GpuPreviewError::Unsupported {
                        reason: "preview spatial device is unavailable".into(),
                    })?;
            self.gpu = Some(GpuSpatialProcessor::new(device, queue)?);
        }
        let mut source_metrics = SpatialMetrics::default();
        if self.gpu_captured.is_none() {
            let (resident, metrics) = self
                .gpu
                .as_mut()
                .expect("spatial processor initialized")
                .upload_captured_source(cpu, &source, cancellation)?;
            source_metrics = metrics;
            self.gpu_captured = Some((capture_key, resident));
        }
        let (_, resident) = self
            .gpu_captured
            .as_ref()
            .expect("resident source initialized");
        let (prepared, mut metrics) = if full_resolution {
            let full = self
                .gpu
                .as_ref()
                .expect("spatial processor initialized")
                .full_resolution_source(resident, description)?;
            (
                PreparedSpatial::Full(full),
                SpatialMetrics {
                    estimated_gpu_bytes: resident.estimated_bytes(),
                    ..SpatialMetrics::default()
                },
            )
        } else {
            let (preview, metrics) = self
                .gpu
                .as_ref()
                .expect("spatial processor initialized")
                .reduce_preview(resident, description, cancellation)?;
            (PreparedSpatial::Preview(preview), metrics)
        };
        metrics.capture = source_metrics.capture;
        metrics.upload = source_metrics.upload;
        metrics.uploaded_bytes = source_metrics.uploaded_bytes;
        metrics.estimated_gpu_bytes = metrics
            .estimated_gpu_bytes
            .max(source_metrics.estimated_gpu_bytes);
        self.recovery = sensor_failure;
        Ok((prepared, metrics))
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_sensor(
        &mut self,
        cpu: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: PreviewOptions,
        keys: &PreviewCacheKeys,
        key: PreCaptureKey,
        cancellation: &CancellationToken,
        full_resolution: bool,
    ) -> Result<(PreparedSpatial, SpatialMetrics), GpuPreviewError> {
        let metadata =
            rohditor_core::SensorCameraMetadata::new(frame, recipe, options).map_err(gpu_error)?;
        if !rohditor_gpu::GpuSensorProcessor::supports(metadata.sensor()) {
            return Err(GpuPreviewError::UnsupportedEdits {
                reason: format!(
                    "GPU sensor {} / {} is unavailable",
                    metadata.sensor().highlight_execution().stable_name(),
                    metadata.sensor().demosaic().stable_name()
                ),
            });
        }
        let contract = metadata.capture_contract().map_err(gpu_error)?;
        let key = CaptureKey {
            source: key,
            settings: keys.reconstructed.capture_sharpening,
            ceilings: contract.ceilings().map(f32::to_bits),
            gpu: true,
        };
        if self
            .sensor_source
            .as_ref()
            .is_some_and(|(stored, _)| stored != &key)
        {
            self.sensor_source = None;
        }
        self.source = None;
        self.captured = None;
        self.gpu_captured = None;
        let (device, queue) = self
            .device
            .as_ref()
            .ok_or_else(|| GpuPreviewError::Unsupported {
                reason: "GPU sensor device unavailable".into(),
            })?;
        if self.sensor.is_none() {
            let (adapter, _) =
                self.display
                    .as_ref()
                    .ok_or_else(|| GpuPreviewError::Unsupported {
                        reason: "GPU sensor adapter unavailable".into(),
                    })?;
            self.sensor = Some(rohditor_gpu::GpuSensorProcessor::new(
                adapter, device, queue,
            )?);
        }
        if self.gpu.is_none() {
            self.gpu = Some(GpuSpatialProcessor::new(device, queue)?);
        }
        let mut metrics = SpatialMetrics {
            sensor_gpu: true,
            ..Default::default()
        };
        let cached_description = match self
            .sensor_source
            .as_ref()
            .map(|(_, source)| source.describe(cpu, frame, recipe, options, cancellation))
        {
            Some(Ok(description)) => Some(description),
            Some(Err(GpuPreviewError::BaseMismatch { .. })) => {
                self.sensor_source = None;
                None
            }
            Some(Err(error)) => return Err(error),
            None => None,
        };
        let description =
            if let Some(description) = cached_description {
                description
            } else {
                let (source, preparation) = self
                    .sensor
                    .as_ref()
                    .expect("sensor initialized")
                    .develop(cpu, frame, recipe, options, cancellation)?;
                metrics = preparation;
                let description = source.initial_description().clone();
                self.sensor_source = Some((key, source));
                description
            };
        let resident = self
            .sensor_source
            .as_ref()
            .expect("sensor source initialized")
            .1
            .captured();
        let spatial = self.gpu.as_ref().expect("spatial initialized");
        if full_resolution {
            metrics.estimated_gpu_bytes =
                metrics.estimated_gpu_bytes.max(resident.estimated_bytes());
            Ok((
                PreparedSpatial::Full(spatial.full_resolution_source(resident, description)?),
                metrics,
            ))
        } else {
            let (preview, reduction) =
                spatial.reduce_preview(resident, description, cancellation)?;
            metrics.spatial = reduction.spatial;
            metrics.readback_bytes += reduction.readback_bytes;
            metrics.submissions += reduction.submissions;
            metrics.estimated_gpu_bytes = metrics
                .estimated_gpu_bytes
                .max(reduction.estimated_gpu_bytes);
            Ok((PreparedSpatial::Preview(preview), metrics))
        }
    }

    fn record_gpu_result<T>(&mut self, result: &Result<T, GpuPreviewError>) {
        if let Err(error) = result
            && !matches!(error, GpuPreviewError::Cancelled)
        {
            self.recovery = Some(error.to_string());
            self.gpu_captured = None;
            self.sensor_source = None;
            self.sensor = None;
            self.gpu = None;
            self.gpu_display = None;
        }
    }
}

enum PreparedSpatial {
    Preview(GpuSpatialPreview),
    Full(GpuSpatialFullSource),
}

fn gpu_error(error: PipelineError) -> GpuPreviewError {
    if matches!(error, PipelineError::Cancelled) {
        GpuPreviewError::Cancelled
    } else {
        GpuPreviewError::InvalidInput {
            reason: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_failures_schedule_cpu_recovery_but_cancellation_does_not_evict() {
        let mut cache = SpatialCache::default();
        cache.record_gpu_result::<()>(&Err(GpuPreviewError::Cancelled));
        assert!(cache.recovery.is_none());

        cache.record_gpu_result::<()>(&Err(GpuPreviewError::Unsupported {
            reason: "simulated device loss".into(),
        }));
        assert_eq!(
            cache.recovery.as_deref(),
            Some("the selected wgpu device cannot support GPU processing: simulated device loss")
        );
        assert!(cache.gpu.is_none());
        assert!(cache.gpu_display.is_none());
        assert!(cache.gpu_captured.is_none());
    }
}

#[cfg(test)]
#[path = "sensor_tests.rs"]
mod sensor_tests;
