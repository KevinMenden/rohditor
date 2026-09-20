use std::sync::mpsc;
use std::time::{Duration, Instant};

use rohditor_core::{
    AreaReductionAxis, CancellationToken, DistortionModel, OpticsExecution,
    SpatialCompletionDescription, TcaModel, VignettingModel,
};
use wgpu::util::DeviceExt;

use super::{GpuCapturedSource, GpuSpatialPreview, check_cancel, invalid};
use crate::{GpuPreviewError, memory::Reservation};

const BAND_ROWS: usize = 16;

impl super::GpuSpatialProcessor {
    pub fn reduce_preview(
        &self,
        source: &GpuCapturedSource,
        description: SpatialCompletionDescription,
        cancellation: &CancellationToken,
    ) -> Result<(GpuSpatialPreview, super::SpatialMetrics), GpuPreviewError> {
        check_cancel(cancellation)?;
        let contract = source.capture_contract.clone();
        if !source.matches(&description, &contract) {
            return Err(GpuPreviewError::BaseMismatch {
                reason: "resident captured source does not match the spatial description".into(),
            });
        }
        let started = Instant::now();
        let (target_width, target_height) = description.target_dimensions();
        let target_width_u32 =
            u32::try_from(target_width).map_err(|_| invalid("preview target width exceeds u32"))?;
        let target_height_u32 = u32::try_from(target_height)
            .map_err(|_| invalid("preview target height exceeds u32"))?;
        let output_bytes = target_width_u32 as u64 * target_height_u32 as u64 * 16;
        if description.target_dimensions() == description.source_dimensions() {
            let estimated_bytes = output_bytes.saturating_add(64 * 1024);
            if source
                .estimated_bytes()
                .checked_add(estimated_bytes)
                .is_none_or(|value| value > self.budget)
            {
                return Err(GpuPreviewError::Unsupported {
                    reason: "resident source plus direct optics output exceeds the GPU budget"
                        .into(),
                });
            }
            self.device.push_error_scope(wgpu::ErrorFilter::Internal);
            self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
            self.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let result = self.materialize_preview_inner(
                source,
                description,
                cancellation,
                output_bytes,
                estimated_bytes,
            );
            let validation = pollster::block_on(self.device.pop_error_scope());
            let allocation = pollster::block_on(self.device.pop_error_scope());
            let internal = pollster::block_on(self.device.pop_error_scope());
            if let Some(error) = validation.or(allocation).or(internal) {
                return Err(invalid(&error.to_string()));
            }
            let (preview, mut metrics) = result?;
            metrics.spatial = started.elapsed();
            return Ok((preview, metrics));
        }
        let maximum_source_rows =
            maximum_band_source_rows(description.area_reduction().vertical(), target_height)?;
        let scratch_bytes = (target_width as u64)
            .checked_mul(maximum_source_rows as u64)
            .and_then(|value| value.checked_mul(12))
            .ok_or_else(|| invalid("area reduction scratch size overflowed"))?;
        let table_bytes = plan_table_bytes(description.area_reduction())?;
        let estimated_bytes = output_bytes
            .checked_add(scratch_bytes)
            .and_then(|value| value.checked_add(table_bytes))
            .and_then(|value| value.checked_add(64 * 1024))
            .ok_or_else(|| invalid("spatial allocation estimate overflowed"))?;
        if source
            .estimated_bytes()
            .checked_add(estimated_bytes)
            .is_none_or(|value| value > self.budget)
        {
            return Err(GpuPreviewError::Unsupported {
                reason: format!(
                    "resident source plus exact preview reduction needs more than the {}-byte GPU budget",
                    self.budget
                ),
            });
        }

        let limits = self.device.limits();
        if scratch_bytes > limits.max_buffer_size
            || scratch_bytes > u64::from(limits.max_storage_buffer_binding_size)
            || target_width_u32.div_ceil(16) > limits.max_compute_workgroups_per_dimension
            || (maximum_source_rows as u32).div_ceil(8)
                > limits.max_compute_workgroups_per_dimension
        {
            return Err(GpuPreviewError::Unsupported {
                reason: "area reduction exceeds a storage-buffer or dispatch limit".into(),
            });
        }

        self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.reduce_preview_inner(
            source,
            description,
            cancellation,
            output_bytes,
            scratch_bytes,
            estimated_bytes,
        );
        let validation = pollster::block_on(self.device.pop_error_scope());
        let allocation = pollster::block_on(self.device.pop_error_scope());
        let internal = pollster::block_on(self.device.pop_error_scope());
        if let Some(error) = validation.or(allocation).or(internal) {
            return Err(invalid(&error.to_string()));
        }
        let (preview, mut metrics) = result?;
        metrics.spatial = started.elapsed();
        Ok((preview, metrics))
    }

    fn materialize_preview_inner(
        &self,
        source: &GpuCapturedSource,
        description: SpatialCompletionDescription,
        cancellation: &CancellationToken,
        output_bytes: u64,
        estimated_bytes: u64,
    ) -> Result<(GpuSpatialPreview, super::SpatialMetrics), GpuPreviewError> {
        check_cancel(cancellation)?;
        let (width, height) = description.target_dimensions();
        let output = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("spatial camera source without area reduction"),
            size: wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let optics = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("direct materialization optics parameters"),
                contents: bytemuck::cast_slice(&pack_optics_parameters(
                    &source.planes.layout,
                    description.optics().execution(),
                )),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let failure = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("direct materialization failure flag"),
                contents: &[0; 4],
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            });
        let failure_staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("direct materialization failure readback"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let empty = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("empty direct materialization group zero"),
            layout: &self.materialize_pipeline.get_bind_group_layout(0),
            entries: &[],
        });
        let output_bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("direct materialization output"),
            layout: &self.materialize_pipeline.get_bind_group_layout(1),
            entries: &[wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(&output_view),
            }],
        });
        let plane_entries = source.planes.sampled_entries();
        let spatial_bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("direct materialization resident source"),
            layout: &self.materialize_pipeline.get_bind_group_layout(2),
            entries: &[
                plane_entries[0].clone(),
                plane_entries[1].clone(),
                plane_entries[2].clone(),
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: optics.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: failure.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("direct optics materialization"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("direct optics materialization"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.materialize_pipeline);
            pass.set_bind_group(0, &empty, &[]);
            pass.set_bind_group(1, &output_bindings, &[]);
            pass.set_bind_group(2, &spatial_bindings, &[]);
            pass.dispatch_workgroups((width as u32).div_ceil(16), (height as u32).div_ceil(16), 1);
        }
        encoder.copy_buffer_to_buffer(&failure, 0, &failure_staging, 0, 4);
        self.queue.submit([encoder.finish()]);
        self.wait(cancellation)?;
        read_failure(&self.device, &failure_staging, cancellation)?;
        Ok((
            GpuSpatialPreview {
                _memory: Reservation::new(output_bytes),
                texture: output,
                view: output_view,
                description,
            },
            super::SpatialMetrics {
                estimated_gpu_bytes: source.estimated_bytes() + estimated_bytes,
                uploaded_bytes: source.uploaded_bytes(),
                readback_bytes: 4,
                submissions: 1,
                ..Default::default()
            },
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn reduce_preview_inner(
        &self,
        source: &GpuCapturedSource,
        description: SpatialCompletionDescription,
        cancellation: &CancellationToken,
        output_bytes: u64,
        scratch_bytes: u64,
        estimated_bytes: u64,
    ) -> Result<(GpuSpatialPreview, super::SpatialMetrics), GpuPreviewError> {
        let (target_width, target_height) = description.target_dimensions();
        let output = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("spatially reduced camera source"),
            size: wgpu::Extent3d {
                width: target_width as u32,
                height: target_height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        let scratch = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bounded horizontal area-reduction scratch"),
            size: scratch_bytes.max(4),
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let horizontal = axis_buffers(
            &self.device,
            description.area_reduction().horizontal(),
            "horizontal",
        )?;
        let vertical = axis_buffers(
            &self.device,
            description.area_reduction().vertical(),
            "vertical",
        )?;
        let optics_parameters = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("GPU optics execution parameters"),
                contents: bytemuck::cast_slice(&pack_optics_parameters(
                    &source.planes.layout,
                    description.optics().execution(),
                )),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let failure = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("GPU spatial failure flag"),
                contents: &[0; 4],
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            });
        let failure_staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU spatial failure readback"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let band_parameters = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("GPU area-reduction band parameters"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let horizontal_empty = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("empty horizontal spatial group zero"),
            layout: &self.horizontal_pipeline.get_bind_group_layout(0),
            entries: &[],
        });
        let vertical_empty = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("empty vertical spatial group zero"),
            layout: &self.vertical_pipeline.get_bind_group_layout(0),
            entries: &[],
        });
        let source_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resident camera source and optics"),
            layout: &self.horizontal_pipeline.get_bind_group_layout(2),
            entries: &[
                source.planes.sampled_entries()[0].clone(),
                source.planes.sampled_entries()[1].clone(),
                source.planes.sampled_entries()[2].clone(),
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: optics_parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: failure.as_entire_binding(),
                },
            ],
        });
        let vertical_source_bind_group =
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("spatial failure flag for vertical reduction"),
                layout: &self.vertical_pipeline.get_bind_group_layout(2),
                entries: &[wgpu::BindGroupEntry {
                    binding: 4,
                    resource: failure.as_entire_binding(),
                }],
            });
        let horizontal_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("horizontal exact area reduction"),
            layout: &self.horizontal_pipeline.get_bind_group_layout(1),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: horizontal.samples.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: horizontal.weights.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: scratch.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: band_parameters.as_entire_binding(),
                },
            ],
        });
        let vertical_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vertical exact area reduction"),
            layout: &self.vertical_pipeline.get_bind_group_layout(1),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: scratch.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: band_parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: vertical.samples.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: vertical.weights.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
            ],
        });

        let mut submissions = 0_u32;
        for target_first in (0..target_height).step_by(BAND_ROWS) {
            check_cancel(cancellation)?;
            let target_count = BAND_ROWS.min(target_height - target_first);
            let (source_first, source_count) = source_rows_for_band(
                description.area_reduction().vertical(),
                target_first,
                target_count,
            )?;
            let words = [
                source_first as u32,
                source_count as u32,
                target_first as u32,
                target_count as u32,
            ];
            self.queue
                .write_buffer(&band_parameters, 0, bytemuck::cast_slice(&words));
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("bounded optics and exact area reduction"),
                });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("optics plus horizontal area reduction"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.horizontal_pipeline);
                pass.set_bind_group(0, &horizontal_empty, &[]);
                pass.set_bind_group(2, &source_bind_group, &[]);
                pass.set_bind_group(1, &horizontal_bind_group, &[]);
                pass.dispatch_workgroups(
                    (target_width as u32).div_ceil(16),
                    (source_count as u32).div_ceil(8),
                    1,
                );
            }
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("vertical exact area reduction"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(&self.vertical_pipeline);
                pass.set_bind_group(0, &vertical_empty, &[]);
                pass.set_bind_group(2, &vertical_source_bind_group, &[]);
                pass.set_bind_group(1, &vertical_bind_group, &[]);
                pass.dispatch_workgroups(
                    (target_width as u32).div_ceil(16),
                    (target_count as u32).div_ceil(8),
                    1,
                );
            }
            self.queue.submit([encoder.finish()]);
            self.wait(cancellation)?;
            submissions += 1;
        }
        check_cancel(cancellation)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("spatial validation readback"),
            });
        encoder.copy_buffer_to_buffer(&failure, 0, &failure_staging, 0, 4);
        self.queue.submit([encoder.finish()]);
        self.wait(cancellation)?;
        read_failure(&self.device, &failure_staging, cancellation)?;
        check_cancel(cancellation)?;

        let metrics = super::SpatialMetrics {
            estimated_gpu_bytes: source.estimated_bytes() + estimated_bytes,
            uploaded_bytes: source.uploaded_bytes(),
            readback_bytes: 4,
            submissions,
            ..Default::default()
        };
        Ok((
            GpuSpatialPreview {
                _memory: Reservation::new(output_bytes),
                texture: output,
                view: output_view,
                description,
            },
            metrics,
        ))
    }

    pub(super) fn wait(&self, cancellation: &CancellationToken) -> Result<(), GpuPreviewError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.queue.on_submitted_work_done(move || {
            let _ = sender.send(());
        });
        loop {
            self.device
                .poll(wgpu::PollType::Poll)
                .map_err(|error| invalid(&format!("{error:?}")))?;
            match receiver.try_recv() {
                Ok(()) => return check_cancel(cancellation),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(invalid("GPU spatial completion disconnected"));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}

struct AxisBuffers {
    samples: wgpu::Buffer,
    weights: wgpu::Buffer,
}

fn axis_buffers(
    device: &wgpu::Device,
    axis: &AreaReductionAxis,
    label: &str,
) -> Result<AxisBuffers, GpuPreviewError> {
    let samples: Vec<[u32; 4]> = axis
        .samples()
        .iter()
        .map(|sample| {
            Ok([
                u32::try_from(sample.first())
                    .map_err(|_| invalid("area sample first exceeds u32"))?,
                u32::try_from(sample.weight_offset())
                    .map_err(|_| invalid("area sample offset exceeds u32"))?,
                u32::try_from(sample.weight_count())
                    .map_err(|_| invalid("area sample count exceeds u32"))?,
                0,
            ])
        })
        .collect::<Result<_, GpuPreviewError>>()?;
    Ok(AxisBuffers {
        samples: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&format!("{label} area samples")),
            contents: bytemuck::cast_slice(&samples),
            usage: wgpu::BufferUsages::STORAGE,
        }),
        weights: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&format!("{label} area weights")),
            contents: bytemuck::cast_slice(axis.weights()),
            usage: wgpu::BufferUsages::STORAGE,
        }),
    })
}

pub(crate) fn pack_optics_parameters(
    layout: &super::resources::ResidentLayout,
    execution: Option<&OpticsExecution>,
) -> [u32; 32] {
    let mut words = [0_u32; 32];
    words[0] = layout.width;
    words[1] = layout.height;
    words[2] = layout.tile_width;
    words[3] = layout.tile_height;
    words[4] = layout.columns;
    let Some(execution) = execution else {
        words[8] = 1.0_f32.to_bits();
        words[9] = 1.0_f32.to_bits();
        words[12] = 1.0_f32.to_bits();
        return words;
    };
    let (norm_scale, norm_unscale) = execution.normalization();
    let (center_x, center_y) = execution.center();
    words[8] = (norm_scale as f32).to_bits();
    words[9] = (norm_unscale as f32).to_bits();
    words[10] = (center_x as f32).to_bits();
    words[11] = (center_y as f32).to_bits();
    words[12] = execution.scale().to_bits();
    match execution.distortion() {
        None => {}
        Some(DistortionModel::Poly3 { k1 }) => {
            words[5] = 1;
            words[16] = k1.to_bits();
        }
        Some(DistortionModel::Poly5 { k1, k2 }) => {
            words[5] = 2;
            words[16] = k1.to_bits();
            words[17] = k2.to_bits();
        }
        Some(DistortionModel::Ptlens { a, b, c }) => {
            words[5] = 3;
            words[16] = a.to_bits();
            words[17] = b.to_bits();
            words[18] = c.to_bits();
        }
    }
    match execution.tca() {
        None => {}
        Some(TcaModel::Linear { kr, kb }) => {
            words[6] = 1;
            words[20] = kr.to_bits();
            words[24] = kb.to_bits();
        }
        Some(TcaModel::Poly3 { red, blue }) => {
            words[6] = 2;
            for index in 0..3 {
                words[20 + index] = red[index].to_bits();
                words[24 + index] = blue[index].to_bits();
            }
        }
    }
    if let Some(VignettingModel::Pa { k1, k2, k3 }) = execution.vignetting() {
        words[7] = 1;
        words[28] = k1.to_bits();
        words[29] = k2.to_bits();
        words[30] = k3.to_bits();
    }
    words
}

fn maximum_band_source_rows(
    vertical: &AreaReductionAxis,
    target_height: usize,
) -> Result<usize, GpuPreviewError> {
    (0..target_height)
        .step_by(BAND_ROWS)
        .map(|first| {
            source_rows_for_band(vertical, first, BAND_ROWS.min(target_height - first))
                .map(|(_, count)| count)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .ok_or_else(|| invalid("area reduction has no output bands"))
}

fn source_rows_for_band(
    vertical: &AreaReductionAxis,
    target_first: usize,
    target_count: usize,
) -> Result<(usize, usize), GpuPreviewError> {
    let samples = vertical.samples();
    let selected = samples
        .get(target_first..target_first + target_count)
        .ok_or_else(|| invalid("area reduction band is outside the vertical plan"))?;
    let first = selected
        .first()
        .ok_or_else(|| invalid("empty area reduction band"))?
        .first();
    let end = selected
        .last()
        .map(|sample| sample.first() + sample.weight_count())
        .ok_or_else(|| invalid("empty area reduction band"))?;
    Ok((first, end - first))
}

fn plan_table_bytes(plan: &rohditor_core::AreaReductionPlan) -> Result<u64, GpuPreviewError> {
    let entries = plan
        .horizontal()
        .samples()
        .len()
        .checked_add(plan.vertical().samples().len())
        .and_then(|value| value.checked_mul(16))
        .ok_or_else(|| invalid("area table size overflowed"))?;
    let weights = plan
        .horizontal()
        .weights()
        .len()
        .checked_add(plan.vertical().weights().len())
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(|| invalid("area weight size overflowed"))?;
    u64::try_from(entries + weights).map_err(|_| invalid("area table bytes exceed u64"))
}

pub(crate) fn read_failure(
    device: &wgpu::Device,
    staging: &wgpu::Buffer,
    cancellation: &CancellationToken,
) -> Result<(), GpuPreviewError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    staging
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    loop {
        device
            .poll(wgpu::PollType::Poll)
            .map_err(|error| invalid(&format!("{error:?}")))?;
        match receiver.try_recv() {
            Ok(result) => {
                result.map_err(|error| invalid(&error.to_string()))?;
                break;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(invalid("spatial failure readback disconnected"));
            }
            Err(mpsc::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    let failed = {
        let mapped = staging.slice(..).get_mapped_range();
        let value = u32::from_le_bytes(
            mapped[..4]
                .try_into()
                .map_err(|_| invalid("invalid failure flag"))?,
        );
        drop(mapped);
        value != 0
    };
    staging.unmap();
    check_cancel(cancellation)?;
    if failed {
        Err(invalid(
            "optics produced an invalid footprint or non-finite value",
        ))
    } else {
        Ok(())
    }
}
