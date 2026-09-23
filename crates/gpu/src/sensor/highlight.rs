use std::sync::mpsc;
use std::time::{Duration, Instant};

use rohditor_core::{
    CancellationToken, ClipStats, HighlightDiagnostics, HighlightExecution,
    SensorDevelopmentDescription,
};
use wgpu::util::DeviceExt;

use super::normalize::{
    GpuNormalizedMosaic, GpuSensorProcessor, WORKGROUP_EDGE, check_cancel, invalid, output_usage,
    synchronization,
};
use super::resources::MosaicLayout;
use crate::{GpuMemoryReservations, GpuPreviewError, memory::Reservation};

const CLIP_PARAMETER_WORDS: usize = 12;
const CLIP_COUNTER_WORDS: usize = 6;
const CLIP_COUNTER_BYTES: u64 = (CLIP_COUNTER_WORDS * std::mem::size_of::<u32>()) as u64;

/// Timing and allocation facts for one resident GPU highlight operation.
#[derive(Debug, Clone, Copy, Default)]
pub struct HighlightMetrics {
    pub combined_gpu_reservations: GpuMemoryReservations,
    pub total: Duration,
    pub processing: Duration,
    /// The Clip diagnostic transfer is six u32 counters, never image pixels.
    pub diagnostic_readback_bytes: u64,
    /// Retained bytes after the prior normalized mosaic is released.
    pub resident_gpu_bytes: u64,
    /// Conservative peak reservation for this stage. Clip's reservation is
    /// held while its normalized input is still resident.
    pub estimated_gpu_bytes: u64,
    pub tiles: usize,
    pub submissions: u32,
}

/// A resident normalized mosaic after its resolved RAW highlight operation.
///
/// It retains the exact normalization and highlight provenance needed by the
/// upcoming GPU demosaic stage. It deliberately has no CPU pixel conversion.
pub struct GpuHighlightedMosaic {
    _memory: Reservation,
    pub(super) _texture: wgpu::Texture,
    pub(super) layout: MosaicLayout,
    pub(super) normalization: rohditor_core::NormalizationContract,
    execution: HighlightExecution,
}

impl GpuHighlightedMosaic {
    #[must_use]
    pub fn dimensions(&self) -> (usize, usize) {
        (self.layout.width as usize, self.layout.height as usize)
    }

    #[must_use]
    pub const fn estimated_bytes(&self) -> u64 {
        self.layout.resident_bytes
    }

    #[must_use]
    pub const fn highlight_execution(&self) -> HighlightExecution {
        self.execution
    }

    #[must_use]
    pub fn matches_description(&self, description: &SensorDevelopmentDescription) -> bool {
        self.normalization == *description.normalization()
            && self.execution == description.highlight_execution()
    }
}

impl std::fmt::Debug for GpuHighlightedMosaic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GpuHighlightedMosaic")
            .field("dimensions", &self.dimensions())
            .field("estimated_bytes", &self.estimated_bytes())
            .field("highlight_execution", &self.execution)
            .finish_non_exhaustive()
    }
}

impl GpuSensorProcessor {
    /// Apply the resolved RAW highlight operation to a resident normalized
    /// mosaic. Only Off and Clip are implemented in this slice; the remaining
    /// methods keep using the CPU sensor path until their GPU ports exist.
    pub fn apply_highlight(
        &self,
        normalized: GpuNormalizedMosaic,
        description: &SensorDevelopmentDescription,
        cancellation: &CancellationToken,
    ) -> Result<(GpuHighlightedMosaic, HighlightDiagnostics, HighlightMetrics), GpuPreviewError>
    {
        check_cancel(cancellation)?;
        if !normalized.matches_contract(description.normalization()) {
            return Err(GpuPreviewError::BaseMismatch {
                reason: "normalized GPU mosaic does not match the sensor-development contract"
                    .into(),
            });
        }
        match description.highlight_execution() {
            HighlightExecution::Off => Ok(self.pass_off(normalized)),
            HighlightExecution::Clip(levels) => self.clip(normalized, levels, cancellation),
            unsupported => Err(GpuPreviewError::UnsupportedEdits {
                reason: format!(
                    "GPU sensor highlight method '{}' is not implemented",
                    unsupported.stable_name()
                ),
            }),
        }
    }

    fn pass_off(
        &self,
        normalized: GpuNormalizedMosaic,
    ) -> (GpuHighlightedMosaic, HighlightDiagnostics, HighlightMetrics) {
        let GpuNormalizedMosaic {
            _memory,
            _texture,
            layout,
            contract,
        } = normalized;
        let metrics = HighlightMetrics {
            combined_gpu_reservations: crate::gpu_memory_reservations(),
            resident_gpu_bytes: layout.resident_bytes,
            estimated_gpu_bytes: layout.resident_bytes,
            ..HighlightMetrics::default()
        };
        (
            GpuHighlightedMosaic {
                _memory,
                _texture,
                layout,
                normalization: contract,
                execution: HighlightExecution::Off,
            },
            HighlightDiagnostics::Off,
            metrics,
        )
    }

    fn clip(
        &self,
        normalized: GpuNormalizedMosaic,
        levels: rohditor_core::ChannelClipLevels,
        cancellation: &CancellationToken,
    ) -> Result<(GpuHighlightedMosaic, HighlightDiagnostics, HighlightMetrics), GpuPreviewError>
    {
        validate_clip_levels(levels)?;
        validate_counter_range(normalized.layout)?;
        self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.clip_inner(normalized, levels, cancellation);
        let validation = pollster::block_on(self.device.pop_error_scope());
        let allocation = pollster::block_on(self.device.pop_error_scope());
        let internal = pollster::block_on(self.device.pop_error_scope());
        check_cancel(cancellation)?;
        if let Some(error) = validation.or(allocation).or(internal) {
            return Err(invalid(&error.to_string()));
        }
        result
    }

    fn clip_inner(
        &self,
        normalized: GpuNormalizedMosaic,
        levels: rohditor_core::ChannelClipLevels,
        cancellation: &CancellationToken,
    ) -> Result<(GpuHighlightedMosaic, HighlightDiagnostics, HighlightMetrics), GpuPreviewError>
    {
        check_cancel(cancellation)?;
        let layout = normalized.layout;
        // The input remains resident until every tile has completed. Reserve
        // the complete additional output lifetime before creating resources so
        // it cannot displace the currently valid normalized state.
        let transient_bytes = CLIP_COUNTER_BYTES
            .checked_mul(2)
            .ok_or_else(|| invalid("GPU Clip counter estimate overflowed"))?;
        let estimated_gpu_bytes = layout.additional_mosaic_peak_bytes(transient_bytes)?;
        let mut memory = Reservation::try_new(estimated_gpu_bytes, self.budget)?;
        let started = Instant::now();

        let output = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("resident Clip-completed RAW mosaic"),
            size: wgpu::Extent3d {
                width: layout.tile_width,
                height: layout.tile_height,
                depth_or_array_layers: layout.layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: output_usage(),
            view_formats: &[],
        });
        let input_view = normalized
            ._texture
            .create_view(&wgpu::TextureViewDescriptor {
                label: Some("resident normalized RAW mosaic array view"),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                base_array_layer: 0,
                array_layer_count: Some(layout.layers),
                ..Default::default()
            });
        let output_view = output.create_view(&wgpu::TextureViewDescriptor {
            label: Some("resident Clip-completed RAW mosaic array view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            base_array_layer: 0,
            array_layer_count: Some(layout.layers),
            ..Default::default()
        });
        let diagnostics = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("GPU Clip diagnostics"),
                contents: bytemuck::cast_slice(&[0_u32; CLIP_COUNTER_WORDS]),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            });
        let diagnostic_readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU Clip diagnostic readback"),
            size: CLIP_COUNTER_BYTES,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let parameters = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU Clip tile parameters"),
            size: (CLIP_PARAMETER_WORDS * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("GPU Clip bindings"),
            layout: &self.clip_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: diagnostics.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: parameters.as_entire_binding(),
                },
            ],
        });

        let pattern = bayer_pattern_code(normalized.contract.crop().pattern());
        let mut metrics = HighlightMetrics {
            estimated_gpu_bytes,
            ..HighlightMetrics::default()
        };
        for tile_y in 0..layout.rows {
            for tile_x in 0..layout.columns {
                check_cancel(cancellation)?;
                let left = usize::try_from(tile_x * layout.tile_width)
                    .map_err(|_| invalid("GPU Clip tile x does not fit usize"))?;
                let top = usize::try_from(tile_y * layout.tile_height)
                    .map_err(|_| invalid("GPU Clip tile y does not fit usize"))?;
                let tile_width = usize::try_from(layout.tile_width.min(layout.width - left as u32))
                    .map_err(|_| invalid("GPU Clip tile width does not fit usize"))?;
                let tile_height =
                    usize::try_from(layout.tile_height.min(layout.height - top as u32))
                        .map_err(|_| invalid("GPU Clip tile height does not fit usize"))?;
                let layer = tile_y * layout.columns + tile_x;
                let words = parameters_for_tile(
                    tile_width,
                    tile_height,
                    left,
                    top,
                    layer as usize,
                    pattern,
                    levels,
                )?;
                self.queue
                    .write_buffer(&parameters, 0, bytemuck::cast_slice(&words));
                let processing_started = Instant::now();
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("GPU Clip highlight tile"),
                        });
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("GPU Clip highlight tile"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&self.clip_pipeline);
                    pass.set_bind_group(0, &bindings, &[]);
                    pass.dispatch_workgroups(
                        (tile_width as u32).div_ceil(WORKGROUP_EDGE),
                        (tile_height as u32).div_ceil(WORKGROUP_EDGE),
                        1,
                    );
                }
                self.queue.submit([encoder.finish()]);
                self.wait(cancellation)?;
                metrics.processing += processing_started.elapsed();
                metrics.tiles += 1;
                metrics.submissions += 1;
            }
        }
        check_cancel(cancellation)?;
        let mut copy = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("GPU Clip diagnostic copy"),
            });
        copy.copy_buffer_to_buffer(&diagnostics, 0, &diagnostic_readback, 0, CLIP_COUNTER_BYTES);
        self.queue.submit([copy.finish()]);
        self.wait(cancellation)?;
        metrics.submissions += 1;
        metrics.diagnostic_readback_bytes = CLIP_COUNTER_BYTES;
        let statistics = read_clip_statistics(&self.device, &diagnostic_readback, cancellation)?;
        check_cancel(cancellation)?;

        // No submitted work uses the input after the waits above. Drop its
        // reservation before publishing the new state, then release the small
        // counter/readback allocation from the output reservation.
        drop(bindings);
        drop(parameters);
        drop(diagnostic_readback);
        drop(diagnostics);
        drop(output_view);
        drop(input_view);
        let GpuNormalizedMosaic {
            _memory: input_memory,
            _texture: _input_texture,
            layout: _input_layout,
            contract,
        } = normalized;
        drop(input_memory);
        drop(_input_texture);
        memory.release_to(layout.resident_bytes);
        metrics.total = started.elapsed();
        metrics.resident_gpu_bytes = layout.resident_bytes;
        metrics.combined_gpu_reservations = crate::gpu_memory_reservations();
        Ok((
            GpuHighlightedMosaic {
                _memory: memory,
                _texture: output,
                layout,
                normalization: contract,
                execution: HighlightExecution::Clip(levels),
            },
            HighlightDiagnostics::Clip(statistics),
            metrics,
        ))
    }
}

fn validate_clip_levels(levels: rohditor_core::ChannelClipLevels) -> Result<(), GpuPreviewError> {
    for (channel, value) in [
        ("red", levels.red),
        ("green", levels.green),
        ("blue", levels.blue),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(invalid(&format!(
                "GPU Clip {channel} ceiling must be finite and positive"
            )));
        }
    }
    Ok(())
}

fn validate_counter_range(layout: MosaicLayout) -> Result<(), GpuPreviewError> {
    let sites = u64::from(layout.width)
        .checked_mul(u64::from(layout.height))
        .ok_or_else(|| invalid("GPU Clip pixel count overflowed"))?;
    if sites > u64::from(u32::MAX) {
        return Err(GpuPreviewError::Unsupported {
            reason: "GPU Clip diagnostics require more than u32 counter capacity".into(),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn parameters_for_tile(
    width: usize,
    height: usize,
    left: usize,
    top: usize,
    layer: usize,
    pattern: usize,
    levels: rohditor_core::ChannelClipLevels,
) -> Result<[u32; CLIP_PARAMETER_WORDS], GpuPreviewError> {
    let convert = |value, name| {
        u32::try_from(value).map_err(|_| invalid(&format!("GPU Clip {name} exceeds u32")))
    };
    Ok([
        convert(width, "tile width")?,
        convert(height, "tile height")?,
        convert(left, "tile left")?,
        convert(top, "tile top")?,
        convert(layer, "destination layer")?,
        convert(pattern, "Bayer pattern")?,
        0,
        0,
        levels.red.to_bits(),
        levels.green.to_bits(),
        levels.blue.to_bits(),
        0,
    ])
}

fn bayer_pattern_code(pattern: rohditor_image::BayerPattern) -> usize {
    match pattern {
        rohditor_image::BayerPattern::Rggb => 0,
        rohditor_image::BayerPattern::Bggr => 1,
        rohditor_image::BayerPattern::Grbg => 2,
        rohditor_image::BayerPattern::Gbrg => 3,
    }
}

fn read_clip_statistics(
    device: &wgpu::Device,
    staging: &wgpu::Buffer,
    cancellation: &CancellationToken,
) -> Result<ClipStats, GpuPreviewError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    staging
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    loop {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(|error| synchronization(&format!("{error:?}")))?;
        match receiver.try_recv() {
            Ok(Ok(())) => break,
            Ok(Err(error)) => {
                return Err(GpuPreviewError::Readback {
                    reason: error.to_string(),
                });
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(GpuPreviewError::Readback {
                    reason: "GPU Clip diagnostic map callback disconnected".into(),
                });
            }
            Err(mpsc::TryRecvError::Empty) => check_cancel(cancellation)?,
        }
        std::thread::yield_now();
    }
    let mut counters = [0_u32; CLIP_COUNTER_WORDS];
    {
        let mapped = staging.slice(..).get_mapped_range();
        counters.copy_from_slice(bytemuck::cast_slice(&mapped));
    }
    staging.unmap();
    Ok(ClipStats {
        affected_sites: counters[0] as usize,
        changed_sites: counters[1] as usize,
        nominal_over_white_sites: counters[2] as usize,
        affected_by_channel: [
            counters[3] as usize,
            counters[4] as usize,
            counters[5] as usize,
        ],
    })
}

#[cfg(test)]
pub(super) fn readback_for_qualification(
    processor: &GpuSensorProcessor,
    mosaic: &GpuHighlightedMosaic,
    cancellation: &CancellationToken,
) -> Result<Vec<f32>, GpuPreviewError> {
    super::normalize::readback_texture_for_qualification(
        processor,
        &mosaic._texture,
        mosaic.layout,
        cancellation,
    )
}
