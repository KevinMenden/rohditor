//! Bounded resident camera RGB, GPU optics, and exact preview reduction.

use std::time::{Duration, Instant};

use rohditor_core::{CancellationToken, CpuPipeline, DemosaicedCameraSource};

use crate::GpuPreviewError;

pub(crate) mod reduction;
pub(crate) mod resources;
pub(crate) mod source;
#[cfg(test)]
mod tests;

pub use source::{GpuCapturedSource, GpuSpatialFullSource, GpuSpatialPreview};

#[derive(Debug, Clone, Copy, Default)]
pub struct SpatialMetrics {
    pub capture: crate::CaptureMetrics,
    pub upload: Duration,
    pub spatial: Duration,
    pub uploaded_bytes: u64,
    pub readback_bytes: u64,
    pub estimated_gpu_bytes: u64,
    pub submissions: u32,
}

/// Ordered executor for capture residency, optics, and exact preview reduction.
pub struct GpuSpatialProcessor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    horizontal_pipeline: wgpu::ComputePipeline,
    vertical_pipeline: wgpu::ComputePipeline,
    materialize_pipeline: wgpu::ComputePipeline,
    capture: crate::GpuCaptureProcessor,
    scatter: CaptureScatter,
    budget: u64,
    maximum_tile_edge: u32,
}

impl GpuSpatialProcessor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self, GpuPreviewError> {
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("GPU optics and exact area reduction"),
            source: wgpu::ShaderSource::Wgsl(include_str!("spatial.wgsl").into()),
        });
        let horizontal_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("optics plus horizontal area reduction"),
                layout: None,
                module: &shader,
                entry_point: Some("reduce_horizontal"),
                compilation_options: Default::default(),
                cache: None,
            });
        let vertical_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("vertical exact area reduction"),
            layout: None,
            module: &shader,
            entry_point: Some("reduce_vertical"),
            compilation_options: Default::default(),
            cache: None,
        });
        let materialize_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("direct optics materialization without area reduction"),
                layout: None,
                module: &shader,
                entry_point: Some("materialize_optics"),
                compilation_options: Default::default(),
                cache: None,
            });
        let scatter = CaptureScatter::new(device);
        let capture = crate::GpuCaptureProcessor::new(device, queue);
        let validation = pollster::block_on(device.pop_error_scope());
        let allocation = pollster::block_on(device.pop_error_scope());
        let internal = pollster::block_on(device.pop_error_scope());
        if let Some(error) = validation.or(allocation).or(internal) {
            return Err(invalid(&error.to_string()));
        }
        let capture = capture?;
        Ok(Self {
            device: device.clone(),
            queue: queue.clone(),
            horizontal_pipeline,
            vertical_pipeline,
            materialize_pipeline,
            capture,
            scatter,
            budget: resources::DEFAULT_BUDGET,
            maximum_tile_edge: device.limits().max_texture_dimension_2d,
        })
    }

    pub fn set_budget(&mut self, bytes: u64) {
        self.budget = bytes.min(resources::DEFAULT_BUDGET);
    }

    #[cfg(test)]
    pub(crate) fn force_maximum_tile_edge(&mut self, edge: u32) {
        self.maximum_tile_edge = edge.max(1);
    }

    pub fn upload_captured_source(
        &mut self,
        cpu: &CpuPipeline,
        source: &DemosaicedCameraSource,
        cancellation: &CancellationToken,
    ) -> Result<(GpuCapturedSource, SpatialMetrics), GpuPreviewError> {
        check_cancel(cancellation)?;
        let contract = source
            .capture_contract()
            .map_err(|error| invalid(&error.to_string()))?;
        let description = cpu
            .describe_spatial_completion(source, cancellation)
            .map_err(|error| invalid(&error.to_string()))?;
        let target = description.target_dimensions();
        let bytes_per_target_pixel = if target == description.source_dimensions() {
            4
        } else {
            28
        };
        let retained = (target.0 as u64)
            .checked_mul(target.1 as u64)
            .and_then(|value| value.checked_mul(bytes_per_target_pixel))
            .ok_or_else(|| invalid("preview allocation estimate overflowed"))?;
        let layout = resources::ResidentLayout::new_bounded(
            source.image().width(),
            source.image().height(),
            retained,
            if contract.settings().is_active() {
                32 * 1024 * 1024
            } else {
                0
            },
            self.budget,
            &self.device.limits(),
            self.maximum_tile_edge,
        )?;
        self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let started = Instant::now();
        let operation = (|| {
            let planes =
                std::sync::Arc::new(source::ResidentCameraPlanes::new(&self.device, layout));
            let (uploaded_bytes, capture, upload) = if contract.settings().is_active() {
                let remaining = self
                    .budget
                    .saturating_sub(layout.resident_bytes)
                    .saturating_sub(retained);
                self.capture.set_remaining_budget(remaining);
                let metrics = self.capture.process_to_resident(
                    source.image(),
                    &contract,
                    source.decoded_raw_bytes(),
                    &planes,
                    &self.scatter,
                    cancellation,
                )?;
                (metrics.uploaded_bytes, metrics, Duration::ZERO)
            } else {
                let uploaded_bytes = planes.upload(&self.queue, source, cancellation, || {
                    self.wait(cancellation)
                })?;
                (
                    uploaded_bytes,
                    crate::CaptureMetrics::default(),
                    started.elapsed(),
                )
            };
            Ok::<_, GpuPreviewError>((planes, uploaded_bytes, capture, upload))
        })();
        let validation = pollster::block_on(self.device.pop_error_scope());
        let allocation = pollster::block_on(self.device.pop_error_scope());
        let internal = pollster::block_on(self.device.pop_error_scope());
        if let Some(error) = validation.or(allocation).or(internal) {
            return Err(invalid(&error.to_string()));
        }
        let (planes, uploaded_bytes, capture, upload) = operation?;
        Ok((
            GpuCapturedSource {
                planes,
                description,
                capture_contract: contract,
                uploaded_bytes,
            },
            SpatialMetrics {
                capture,
                upload,
                uploaded_bytes,
                estimated_gpu_bytes: layout.estimated_peak_bytes,
                ..Default::default()
            },
        ))
    }

    pub fn full_resolution_source(
        &self,
        source: &GpuCapturedSource,
        description: rohditor_core::SpatialCompletionDescription,
    ) -> Result<GpuSpatialFullSource, GpuPreviewError> {
        if description.target_dimensions() != description.source_dimensions()
            || !source.matches(&description, &source.capture_contract)
        {
            return Err(GpuPreviewError::BaseMismatch {
                reason: "full-resolution spatial description does not match the resident source"
                    .into(),
            });
        }
        Ok(GpuSpatialFullSource {
            planes: std::sync::Arc::clone(&source.planes),
            description,
        })
    }
}

pub(crate) struct CaptureScatter {
    pipeline: wgpu::ComputePipeline,
}

impl CaptureScatter {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("capture core resident scatter"),
            source: wgpu::ShaderSource::Wgsl(include_str!("scatter.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("capture core resident scatter"),
            layout: None,
            module: &shader,
            entry_point: Some("scatter_capture_core"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        rgb: &wgpu::Buffer,
        planes: &source::ResidentCameraPlanes,
        input_width: usize,
        input_x: usize,
        input_y: usize,
        core_left: usize,
        core_top: usize,
        core_width: usize,
        core_height: usize,
    ) -> Result<(), GpuPreviewError> {
        use wgpu::util::DeviceExt;
        let layout = planes.layout;
        let words = [
            u32::try_from(input_width).map_err(|_| invalid("capture tile width exceeds u32"))?,
            u32::try_from(input_x).map_err(|_| invalid("capture tile x exceeds u32"))?,
            u32::try_from(input_y).map_err(|_| invalid("capture tile y exceeds u32"))?,
            u32::try_from(core_left).map_err(|_| invalid("capture core x exceeds u32"))?,
            u32::try_from(core_top).map_err(|_| invalid("capture core y exceeds u32"))?,
            u32::try_from(core_width).map_err(|_| invalid("capture core width exceeds u32"))?,
            u32::try_from(core_height).map_err(|_| invalid("capture core height exceeds u32"))?,
            layout.tile_width,
            layout.tile_height,
            layout.columns,
            0,
            0,
        ];
        let parameters = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("capture resident scatter parameters"),
            contents: bytemuck::cast_slice(&words),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let views = planes.storage_views();
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("capture resident scatter bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: rgb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(views[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(views[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(views[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: parameters.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("scatter completed capture core"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            u32::try_from(core_width)
                .map_err(|_| invalid("capture width exceeds u32"))?
                .div_ceil(16),
            u32::try_from(core_height)
                .map_err(|_| invalid("capture height exceeds u32"))?
                .div_ceil(16),
            1,
        );
        Ok(())
    }
}

fn invalid(reason: &str) -> GpuPreviewError {
    GpuPreviewError::InvalidInput {
        reason: reason.to_owned(),
    }
}

fn check_cancel(token: &CancellationToken) -> Result<(), GpuPreviewError> {
    if token.is_cancelled() {
        Err(GpuPreviewError::Cancelled)
    } else {
        Ok(())
    }
}
