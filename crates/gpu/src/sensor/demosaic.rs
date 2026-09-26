use std::time::{Duration, Instant};

use rohditor_core::{
    CAPTURE_SHARPENING_ITERATIONS, CancellationToken, CaptureSharpeningContract,
    SensorDevelopmentDescription,
};
use rohditor_demosaic::DemosaicAlgorithm;

use super::normalize::{bayer_pattern_code, check_cancel, invalid};
use super::rcd::RcdExecutor;
use super::{GpuHighlightedMosaic, GpuSensorProcessor};
use crate::spatial::{resources::ResidentLayout, source::ResidentCameraPlanes};
use crate::{CaptureMetrics, GpuCaptureProcessor, GpuPreviewError, memory::Reservation};

/// Camera-native, identity-WB RGB after demosaic and optional capture sharpening.
/// Pixel storage stays on the device; provenance accompanies it for integration
/// with the spatial source/cache boundary.
pub struct GpuSensorCameraSource {
    pub(crate) planes: ResidentCameraPlanes,
    description: SensorDevelopmentDescription,
    capture: CaptureSharpeningContract,
}

impl GpuSensorCameraSource {
    #[must_use]
    pub fn dimensions(&self) -> (usize, usize) {
        (
            self.planes.layout.width as usize,
            self.planes.layout.height as usize,
        )
    }

    #[must_use]
    pub fn estimated_bytes(&self) -> u64 {
        self.planes.layout.resident_bytes
    }

    #[must_use]
    pub fn description(&self) -> &SensorDevelopmentDescription {
        &self.description
    }

    #[must_use]
    pub fn capture_contract(&self) -> &CaptureSharpeningContract {
        &self.capture
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DemosaicMetrics {
    /// Wall time including bounded submissions and the validation flag readback.
    pub total: Duration,
    pub capture: CaptureMetrics,
    pub tiles: usize,
    pub submissions: u32,
    pub diagnostic_readback_bytes: u64,
    pub resident_gpu_bytes: u64,
    /// Logical mosaic, final RGB, and transient allocations, excluding driver pools.
    pub estimated_gpu_bytes: u64,
}

// Uniforms, validation flag/readback, bind groups, and bounded dispatch overhead.
const WORK_BYTES: u64 = 64 * 1024;

impl GpuSensorProcessor {
    /// Consume a highlighted mosaic with bilinear or MHC. Active capture receives
    /// each halo-expanded RGB tile directly from demosaic, never a second full RGB
    /// image. Unsupported algorithms return before allocating camera planes.
    pub fn demosaic(
        &self,
        mosaic: GpuHighlightedMosaic,
        description: &SensorDevelopmentDescription,
        capture: &CaptureSharpeningContract,
        cancellation: &CancellationToken,
    ) -> Result<(GpuSensorCameraSource, DemosaicMetrics), GpuPreviewError> {
        check_cancel(cancellation)?;
        if !mosaic.matches_description(description) {
            return Err(GpuPreviewError::BaseMismatch {
                reason: "GPU demosaic source provenance differs from its description".into(),
            });
        }
        let algorithm = match description.demosaic().algorithm() {
            DemosaicAlgorithm::Bilinear => 0,
            DemosaicAlgorithm::MalvarHeCutler => 1,
            DemosaicAlgorithm::Rcd => 2,
            other => {
                return Err(GpuPreviewError::UnsupportedEdits {
                    reason: format!("GPU demosaic {} is not implemented", other.stable_name()),
                });
            }
        };
        capture
            .settings()
            .validate()
            .map_err(|e| invalid(&e.to_string()))?;
        self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.demosaic_inner(mosaic, description, capture, algorithm, cancellation);
        let validation = pollster::block_on(self.device.pop_error_scope());
        let allocation = pollster::block_on(self.device.pop_error_scope());
        let internal = pollster::block_on(self.device.pop_error_scope());
        check_cancel(cancellation)?;
        if let Some(error) = validation.or(allocation).or(internal) {
            return Err(invalid(&error.to_string()));
        }
        result
    }

    fn demosaic_inner(
        &self,
        mosaic: GpuHighlightedMosaic,
        description: &SensorDevelopmentDescription,
        capture: &CaptureSharpeningContract,
        algorithm: u32,
        cancellation: &CancellationToken,
    ) -> Result<(GpuSensorCameraSource, DemosaicMetrics), GpuPreviewError> {
        let started = Instant::now();
        let (width, height) = mosaic.dimensions();
        let limits = self.device.limits();
        let rcd_bytes = if algorithm == 2 {
            RcdExecutor::scratch_bytes() + 64
        } else {
            0
        };
        let layout = ResidentLayout::new_generated(
            width,
            height,
            mosaic.estimated_bytes(),
            WORK_BYTES + rcd_bytes,
            self.budget,
            &limits,
            self.maximum_tile_edge,
        )?;
        let _work = Reservation::try_new(WORK_BYTES, self.budget)?;
        let planes = ResidentCameraPlanes::with_budget(&self.device, layout, self.budget)?;
        let rcd = if algorithm == 2 {
            Some(RcdExecutor::new(self)?)
        } else {
            None
        };
        let captured_planes = if algorithm == 2 && capture.settings().is_active() {
            Some(ResidentCameraPlanes::with_budget(
                &self.device,
                layout,
                self.budget,
            )?)
        } else {
            None
        };
        let remaining = self
            .budget
            .checked_sub(crate::gpu_memory_reservations().current_bytes)
            .ok_or_else(|| GpuPreviewError::Unsupported {
                reason: "sensor camera planes exceed the operation budget".into(),
            })?;
        let mut prepared_capture = if captured_planes.is_some() {
            let mut processor = GpuCaptureProcessor::new(&self.device, &self.queue)?;
            processor.set_remaining_budget(remaining);
            #[cfg(test)]
            processor.force_maximum_tile_edge(self.maximum_tile_edge as usize);
            let reservation = processor.reserve_generated(capture, (width, height))?;
            Some((processor, reservation))
        } else {
            None
        };
        let validation = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("demosaic finite flag"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let view = mosaic._texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let mut metrics = DemosaicMetrics::default();
        {
            let mut dispatch = |region: [u32; 4], target: Option<&wgpu::Buffer>| {
                check_cancel(cancellation)?;
                self.dispatch_demosaic(
                    &mosaic,
                    &view,
                    &planes,
                    &validation,
                    if algorithm == 2 { 0 } else { algorithm },
                    region,
                    target,
                );
                self.wait(cancellation)?;
                metrics.tiles += 1;
                metrics.submissions += 1;
                Ok::<_, GpuPreviewError>(0)
            };
            if capture.settings().is_active() && algorithm != 2 {
                let mut processor = GpuCaptureProcessor::new(&self.device, &self.queue)?;
                processor.set_remaining_budget(remaining);
                #[cfg(test)]
                processor.force_maximum_tile_edge(self.maximum_tile_edge as usize);
                metrics.capture = processor.process_generated_to_resident(
                    capture,
                    &planes,
                    cancellation,
                    |buffer, x, y, w, h| {
                        dispatch([x as u32, y as u32, w as u32, h as u32], Some(buffer))
                    },
                )?;
            } else {
                // Bound submission size independently of texture-array layout.
                let edge = 512.min(
                    limits
                        .max_compute_workgroups_per_dimension
                        .saturating_mul(8),
                );
                for y in (0..layout.height).step_by(edge as usize) {
                    for x in (0..layout.width).step_by(edge as usize) {
                        dispatch(
                            [
                                x,
                                y,
                                edge.min(layout.width - x),
                                edge.min(layout.height - y),
                            ],
                            None,
                        )?;
                    }
                }
            }
        }
        if let Some(rcd) = &rcd {
            metrics.submissions +=
                rcd.run(self, &mosaic, &view, &planes, &validation, cancellation)? as u32;
        }
        if let Some((processor, reservation)) = prepared_capture.take() {
            metrics.capture = processor.process_resident_from_resident(
                capture,
                &planes,
                captured_planes.as_ref().expect("reserved capture target"),
                reservation,
                cancellation,
            )?;
            metrics.submissions += metrics.capture.tiles as u32;
        }
        metrics.submissions +=
            metrics.capture.tiles as u32 * (CAPTURE_SHARPENING_ITERATIONS as u32 + 2);
        self.validate_demosaic(&validation, cancellation)?;
        metrics.submissions += 1;
        metrics.diagnostic_readback_bytes = 4;
        metrics.estimated_gpu_bytes = mosaic.estimated_bytes()
            + layout.resident_bytes
            + WORK_BYTES
            + rcd_bytes
            + captured_planes
                .as_ref()
                .map_or(0, |_| layout.resident_bytes)
            + metrics
                .capture
                .estimated_gpu_bytes
                .saturating_sub(layout.resident_bytes);
        drop(view);
        drop(mosaic);
        metrics.resident_gpu_bytes = layout.resident_bytes;
        metrics.total = started.elapsed();
        let planes = match captured_planes {
            Some(captured) => {
                drop(planes);
                captured
            }
            None => planes,
        };
        Ok((
            GpuSensorCameraSource {
                planes,
                description: description.clone(),
                capture: capture.clone(),
            },
            metrics,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_demosaic(
        &self,
        mosaic: &GpuHighlightedMosaic,
        view: &wgpu::TextureView,
        planes: &ResidentCameraPlanes,
        validation: &wgpu::Buffer,
        algorithm: u32,
        region: [u32; 4],
        target: Option<&wgpu::Buffer>,
    ) {
        let src = mosaic.layout;
        let dst = planes.layout;
        let words = [
            src.width,
            src.height,
            src.tile_width,
            src.tile_height,
            src.columns,
            bayer_pattern_code(mosaic.normalization.crop().pattern()) as u32,
            algorithm,
            0,
            region[0],
            region[1],
            region[2],
            region[3],
            dst.tile_width,
            dst.tile_height,
            dst.columns,
            0,
        ];
        let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("demosaic region"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&uniform, 0, bytemuck::cast_slice(&words));
        let pipeline = if target.is_some() {
            &self.demosaic_tile_pipeline
        } else {
            &self.demosaic_planes_pipeline
        };
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: validation.as_entire_binding(),
            },
        ];
        if let Some(buffer) = target {
            entries.push(wgpu::BindGroupEntry {
                binding: 6,
                resource: buffer.as_entire_binding(),
            });
        } else {
            for (channel, view) in planes.storage_views().into_iter().enumerate() {
                entries.push(wgpu::BindGroupEntry {
                    binding: 3 + channel as u32,
                    resource: wgpu::BindingResource::TextureView(view),
                });
            }
        }
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("demosaic bindings"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("demosaic bounded region"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("demosaic"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups(region[2].div_ceil(8), region[3].div_ceil(8), 1);
        }
        self.queue.submit([encoder.finish()]);
    }

    fn validate_demosaic(
        &self,
        flag: &wgpu::Buffer,
        cancellation: &CancellationToken,
    ) -> Result<(), GpuPreviewError> {
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("demosaic finite flag readback"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(flag, 0, &staging, 0, 4);
        self.queue.submit([encoder.finish()]);
        self.wait(cancellation)?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        loop {
            self.device
                .poll(wgpu::PollType::Poll)
                .map_err(|e| invalid(&format!("{e:?}")))?;
            match receiver.try_recv() {
                Ok(result) => {
                    result.map_err(|e| invalid(&e.to_string()))?;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err(invalid("demosaic validation callback disconnected"));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => check_cancel(cancellation)?,
            }
            std::thread::yield_now();
        }
        let value = u32::from_ne_bytes(
            staging.slice(..).get_mapped_range()[..4]
                .try_into()
                .expect("four-byte flag"),
        );
        staging.unmap();
        check_cancel(cancellation)?;
        if value != 0 {
            return Err(invalid("GPU demosaic produced non-finite camera RGB"));
        }
        Ok(())
    }
}
