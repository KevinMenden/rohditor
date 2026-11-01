use std::time::{Duration, Instant};

use rohditor_core::{CancellationToken, DetailContract, SensorDevelopmentDescription};
use rohditor_demosaic::{ChannelBalance, DemosaicAlgorithm};

use super::amaze::{AmazeExecutor, BATCH_SIZES};
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
    capture: DetailContract,
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
    pub fn detail_contract(&self) -> &DetailContract {
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

// Algorithm codes passed to the per-pixel demosaic shader (0 and 1) and used to
// choose a tiled executor. RCD and AMaZE overwrite a bilinear base (code 0).
const RCD: u32 = 2;
const AMAZE: u32 = 3;

// Uniforms, validation flag/readback, bind groups, and bounded dispatch overhead.
const WORK_BYTES: u64 = 64 * 1024;

impl GpuSensorProcessor {
    /// Consume a highlighted mosaic with bilinear, MHC, RCD or AMaZE. An active
    /// detail stage receives each halo-expanded RGB tile directly from
    /// bilinear/MHC demosaic, never a second full RGB image; RCD, AMaZE and
    /// noise reduction (which profiles the whole image first) fill resident
    /// planes first. Unsupported algorithms return before allocating camera planes.
    pub fn demosaic(
        &self,
        mosaic: GpuHighlightedMosaic,
        description: &SensorDevelopmentDescription,
        capture: &DetailContract,
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
            DemosaicAlgorithm::Rcd => RCD,
            DemosaicAlgorithm::Amaze => AMAZE,
        };
        capture
            .capture()
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
        capture: &DetailContract,
        algorithm: u32,
        cancellation: &CancellationToken,
    ) -> Result<(GpuSensorCameraSource, DemosaicMetrics), GpuPreviewError> {
        let started = Instant::now();
        let (width, height) = mosaic.dimensions();
        let limits = self.device.limits();
        // RCD and AMaZE run a tiled scratch executor whose memory is reserved
        // beside the planes; AMaZE picks the largest batch that fits.
        let (layout, amaze_batch) = self.plan_layout(&mosaic, algorithm, &limits)?;
        let scratch_bytes = match algorithm {
            RCD => RcdExecutor::reserved_bytes(),
            AMAZE => AmazeExecutor::reserved_bytes(amaze_batch),
            _ => 0,
        };
        let _work = Reservation::try_new(WORK_BYTES, self.budget)?;
        let planes = ResidentCameraPlanes::with_budget(&self.device, layout, self.budget)?;
        let balance = ChannelBalance::new(description.demosaic_balance());
        let rcd = (algorithm == RCD)
            .then(|| RcdExecutor::new(self, balance))
            .transpose()?;
        let amaze = (algorithm == AMAZE)
            .then(|| AmazeExecutor::new(self, balance, amaze_batch))
            .transpose()?;
        // RCD and AMaZE fill resident planes by construction. Noise reduction
        // must profile the whole image before any tile is filtered, so it
        // takes the same two-step route.
        let resident_executor = algorithm >= RCD || capture.noise_reduction().is_active();
        let captured_planes = if resident_executor && capture.is_active() {
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
            let mut encode = |encoder: &mut wgpu::CommandEncoder,
                              region: [u32; 4],
                              target: Option<&wgpu::Buffer>| {
                self.encode_demosaic(
                    encoder,
                    &mosaic,
                    &view,
                    &planes,
                    &validation,
                    // RCD and AMaZE refine a bilinear base.
                    if algorithm >= RCD { 0 } else { algorithm },
                    region,
                    target,
                );
                metrics.tiles += 1;
            };
            if capture.is_active() && !resident_executor {
                let mut processor = GpuCaptureProcessor::new(&self.device, &self.queue)?;
                processor.set_remaining_budget(remaining);
                #[cfg(test)]
                processor.force_maximum_tile_edge(self.maximum_tile_edge as usize);
                // Each demosaic tile is recorded into its capture tile's
                // submission, so its time is part of the capture stage.
                metrics.capture = processor.process_generated_to_resident(
                    capture,
                    &planes,
                    cancellation,
                    |encoder, buffer, x, y, w, h| {
                        encode(
                            encoder,
                            [x as u32, y as u32, w as u32, h as u32],
                            Some(buffer),
                        );
                        Ok(0)
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
                        check_cancel(cancellation)?;
                        let mut encoder =
                            self.device
                                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                    label: Some("demosaic bounded region"),
                                });
                        encode(
                            &mut encoder,
                            [
                                x,
                                y,
                                edge.min(layout.width - x),
                                edge.min(layout.height - y),
                            ],
                            None,
                        );
                        self.queue.submit([encoder.finish()]);
                        self.wait(cancellation)?;
                        metrics.submissions += 1;
                    }
                }
            }
        }
        if let Some(rcd) = &rcd {
            metrics.submissions +=
                rcd.run(self, &mosaic, &view, &planes, &validation, cancellation)? as u32;
        }
        if let Some(amaze) = &amaze {
            metrics.submissions +=
                amaze.run(self, &mosaic, &view, &planes, &validation, cancellation)? as u32;
        }
        if let Some((processor, reservation)) = prepared_capture.take() {
            metrics.capture = processor.process_resident_from_resident(
                capture,
                &planes,
                captured_planes.as_ref().expect("reserved capture target"),
                reservation,
                cancellation,
            )?;
        }
        metrics.submissions += metrics.capture.submissions;
        self.validate_demosaic(&validation, cancellation)?;
        metrics.submissions += 1;
        metrics.diagnostic_readback_bytes = 4;
        metrics.estimated_gpu_bytes = mosaic.estimated_bytes()
            + layout.resident_bytes
            + WORK_BYTES
            + scratch_bytes
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

    /// Resident layout for `algorithm`, leaving room for its scratch. Returns
    /// the AMaZE batch size that fits (0 for other algorithms).
    fn plan_layout(
        &self,
        mosaic: &GpuHighlightedMosaic,
        algorithm: u32,
        limits: &wgpu::Limits,
    ) -> Result<(ResidentLayout, usize), GpuPreviewError> {
        let (width, height) = mosaic.dimensions();
        let layout = |scratch_bytes| {
            ResidentLayout::new_generated(
                width,
                height,
                mosaic.estimated_bytes(),
                WORK_BYTES + scratch_bytes,
                self.budget,
                limits,
                self.maximum_tile_edge,
            )
        };
        match algorithm {
            RCD => Ok((layout(RcdExecutor::reserved_bytes())?, 0)),
            AMAZE => {
                let mut failure = None;
                for batch in BATCH_SIZES {
                    if !AmazeExecutor::batch_fits(limits, batch) {
                        continue;
                    }
                    match layout(AmazeExecutor::reserved_bytes(batch)) {
                        Ok(layout) => return Ok((layout, batch)),
                        Err(error) => failure = Some(error),
                    }
                }
                Err(failure.unwrap_or_else(|| GpuPreviewError::Unsupported {
                    reason: "device limits cannot hold a GPU AMaZE scratch tile".into(),
                }))
            }
            _ => Ok((layout(0)?, 0)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_demosaic(
        &self,
        encoder: &mut wgpu::CommandEncoder,
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
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("demosaic"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.dispatch_workgroups(region[2].div_ceil(8), region[3].div_ceil(8), 1);
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
