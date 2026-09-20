//! Full-resolution f32 capture bridge. Scratch is local to one operation and
//! reused across tiles, then released before color export allocates its source.
use crate::GpuPreviewError;
use rohditor_core::{
    CAPTURE_SHARPENING_FLOOR, CAPTURE_SHARPENING_ITERATIONS, CPU_WORKING_SET_LIMIT_BYTES,
    CancellationToken, CaptureSharpeningContract, CaptureSharpeningProvenance, CpuPipeline,
    DemosaicedCameraSource, ReconstructedPreview,
};
use rohditor_image::LinearRgbImage;
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod processor;
#[cfg(test)]
mod qualification;
mod resources;
#[cfg(test)]
mod tests;
use resources::{Resources, TilePlan};

#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureMetrics {
    pub combined_gpu_reservations: crate::GpuMemoryReservations,
    pub total: Duration,
    pub upload: Duration,
    pub compute_and_wait: Duration,
    pub readback: Duration,
    pub uploaded_bytes: u64,
    pub readback_bytes: u64,
    pub estimated_gpu_bytes: u64,
    pub estimated_host_bytes: usize,
    pub tile_edge: usize,
    pub halo: usize,
    pub tiles: usize,
}

pub struct GpuCaptureResult {
    pub image: LinearRgbImage<f32>,
    pub metrics: CaptureMetrics,
}

/// Supplied worker-owned device/queue; no presentation dependencies or resident
/// full-frame resources. Mutable access serializes capture operations.
pub struct GpuCaptureProcessor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    budget: u64,
    maximum_tile_edge: usize,
}

impl GpuCaptureProcessor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, GpuPreviewError> {
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("capture f32 shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("capture.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("capture f32 passes"),
            layout: None,
            module: &shader,
            entry_point: Some("capture"),
            compilation_options: Default::default(),
            cache: None,
        });
        let validation = pollster::block_on(device.pop_error_scope());
        let allocation = pollster::block_on(device.pop_error_scope());
        let internal = pollster::block_on(device.pop_error_scope());
        if let Some(e) = validation.or(allocation).or(internal) {
            return Err(error(e));
        }
        Ok(Self {
            device: device.clone(),
            queue: queue.clone(),
            pipeline,
            budget: resources::DEFAULT_BUDGET,
            maximum_tile_edge: 512,
        })
    }

    /// Account for other allocations on this device before choosing tile size.
    pub fn set_remaining_budget(&mut self, bytes: u64) {
        self.budget = bytes.min(resources::DEFAULT_BUDGET);
    }

    pub fn prepare(
        &mut self,
        cpu: &CpuPipeline,
        source: DemosaicedCameraSource,
        cancellation: &CancellationToken,
    ) -> Result<(ReconstructedPreview, CaptureMetrics), GpuPreviewError> {
        let (captured, metrics) = self.capture_source(source, cancellation)?;
        let prepared = cpu
            .complete_camera_source(captured, cancellation)
            .map_err(pipeline_error)?;
        Ok((prepared, metrics))
    }

    pub fn capture_source(
        &mut self,
        source: DemosaicedCameraSource,
        cancellation: &CancellationToken,
    ) -> Result<(rohditor_core::CapturedCameraSource, CaptureMetrics), GpuPreviewError> {
        let contract = source.capture_contract().map_err(error)?;
        let (captured, metrics) = match self.process(
            source.image(),
            &contract,
            source.decoded_raw_bytes(),
            cancellation,
        )? {
            None => (
                source.capture_cpu(cancellation).map_err(pipeline_error)?,
                CaptureMetrics::default(),
            ),
            Some(result) => {
                let metrics = result.metrics;
                (
                    source
                        .with_capture_result(
                            result.image,
                            CaptureSharpeningProvenance::for_settings(contract.settings()),
                            metrics.total,
                            metrics.estimated_host_bytes,
                            cancellation,
                        )
                        .map_err(pipeline_error)?,
                    metrics,
                )
            }
        };
        Ok((captured, metrics))
    }

    /// None is the exact bypass: no capture allocation, upload, or dispatch.
    /// On failure the immutable source remains suitable for CPU recovery.
    pub fn process(
        &mut self,
        image: &LinearRgbImage<f32>,
        contract: &CaptureSharpeningContract,
        retained_host_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<Option<GpuCaptureResult>, GpuPreviewError> {
        check_cancel(cancellation)?;
        contract.settings().validate().map_err(error)?;
        if !contract.settings().is_active() {
            return Ok(None);
        }
        if image.space() != rohditor_image::LinearRgbSpace::CameraNative {
            return Err(error(
                "capture requires camera-native RGB before white balance and optics",
            ));
        }
        let plan = TilePlan::new(
            image.width(),
            image.height(),
            contract.halo(),
            self.budget,
            &self.device.limits(),
            self.maximum_tile_edge,
        )?;
        let host = image
            .data()
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(retained_host_bytes))
            .and_then(|n| n.checked_add(plan.host_bytes))
            .ok_or_else(|| error("capture host allocation overflow"))?;
        if host > CPU_WORKING_SET_LIMIT_BYTES {
            return Err(error(
                "capture source, result and staging exceed CPU working-set limit",
            ));
        }
        self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.process_inner(image, contract, &plan, cancellation);
        let validation = pollster::block_on(self.device.pop_error_scope());
        let allocation = pollster::block_on(self.device.pop_error_scope());
        let internal = pollster::block_on(self.device.pop_error_scope());
        check_cancel(cancellation)?;
        if let Some(e) = validation.or(allocation).or(internal) {
            return Err(error(e));
        }
        result.map(Some)
    }

    pub(crate) fn process_to_resident(
        &mut self,
        image: &LinearRgbImage<f32>,
        contract: &CaptureSharpeningContract,
        retained_host_bytes: usize,
        resident: &crate::spatial::source::ResidentCameraPlanes,
        scatter: &crate::spatial::CaptureScatter,
        cancellation: &CancellationToken,
    ) -> Result<CaptureMetrics, GpuPreviewError> {
        check_cancel(cancellation)?;
        contract.settings().validate().map_err(error)?;
        if !contract.settings().is_active() {
            return Err(error(
                "active capture contract required for resident execution",
            ));
        }
        if image.space() != rohditor_image::LinearRgbSpace::CameraNative {
            return Err(error(
                "capture requires camera-native RGB before white balance and optics",
            ));
        }
        if (image.width(), image.height())
            != (
                resident.layout.width as usize,
                resident.layout.height as usize,
            )
        {
            return Err(error(
                "resident capture target dimensions do not match the source",
            ));
        }
        let plan = TilePlan::new_resident(
            image.width(),
            image.height(),
            contract.halo(),
            self.budget,
            &self.device.limits(),
            self.maximum_tile_edge,
        )?;
        let host = image
            .data()
            .len()
            .checked_mul(4)
            .and_then(|value| value.checked_add(retained_host_bytes))
            .and_then(|value| value.checked_add(plan.host_bytes))
            .ok_or_else(|| error("resident capture host allocation overflow"))?;
        if host > CPU_WORKING_SET_LIMIT_BYTES {
            return Err(error(
                "resident capture source and staging exceed CPU working-set limit",
            ));
        }
        self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result =
            self.process_resident_inner(image, contract, &plan, resident, scatter, cancellation);
        let validation = pollster::block_on(self.device.pop_error_scope());
        let allocation = pollster::block_on(self.device.pop_error_scope());
        let internal = pollster::block_on(self.device.pop_error_scope());
        check_cancel(cancellation)?;
        if let Some(error_value) = validation.or(allocation).or(internal) {
            return Err(error(error_value));
        }
        result
    }
}

fn error(e: impl std::fmt::Display) -> GpuPreviewError {
    GpuPreviewError::InvalidInput {
        reason: e.to_string(),
    }
}

fn pipeline_error(e: rohditor_core::PipelineError) -> GpuPreviewError {
    if matches!(e, rohditor_core::PipelineError::Cancelled) {
        GpuPreviewError::Cancelled
    } else {
        error(e)
    }
}
fn check_cancel(token: &CancellationToken) -> Result<(), GpuPreviewError> {
    if token.is_cancelled() {
        Err(GpuPreviewError::Cancelled)
    } else {
        Ok(())
    }
}
