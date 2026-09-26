//! One fixed RCD scratch tile and an explicit, ordered shader pass sequence.

use rohditor_core::CancellationToken;
use rohditor_demosaic::{RCD_TILE_SIZE, rcd_tiles};

use super::normalize::{bayer_pattern_code, check_cancel};
use super::{GpuHighlightedMosaic, GpuSensorProcessor};
use crate::spatial::source::ResidentCameraPlanes;
use crate::{GpuPreviewError, memory::Reservation};

const FULL: u64 = (RCD_TILE_SIZE * RCD_TILE_SIZE) as u64;
const SCRATCH_BYTES: u64 = (5 * FULL + 3 * FULL / 2) * 4;
const NAMES: [&str; 9] = [
    "clear_populate",
    "directions",
    "low_pass",
    "green_pass",
    "diagonal_hp",
    "diagonal_directions",
    "opposite_color",
    "at_green",
    "scatter_core",
];

pub(super) struct RcdExecutor {
    _memory: Reservation,
    scratch: wgpu::Buffer,
    uniform: wgpu::Buffer,
    pipelines: Vec<wgpu::ComputePipeline>,
}

impl RcdExecutor {
    pub(super) const fn scratch_bytes() -> u64 {
        SCRATCH_BYTES
    }

    pub(super) fn new(processor: &GpuSensorProcessor) -> Result<Self, GpuPreviewError> {
        let limits = processor.device.limits();
        if SCRATCH_BYTES > limits.max_buffer_size
            || SCRATCH_BYTES > u64::from(limits.max_storage_buffer_binding_size)
            || limits.max_compute_workgroups_per_dimension < (FULL as u32).div_ceil(64)
            || limits.max_storage_buffers_per_shader_stage < 2
            || limits.max_storage_textures_per_shader_stage < 3
            || limits.max_sampled_textures_per_shader_stage < 1
            || limits.max_uniform_buffers_per_shader_stage < 1
            || limits.max_uniform_buffer_binding_size < 64
            || limits.max_bindings_per_bind_group < 7
            || limits.max_bind_groups < 1
            || limits.max_compute_invocations_per_workgroup < 64
            || limits.max_compute_workgroup_size_x < 64
        {
            return Err(GpuPreviewError::Unsupported {
                reason: "device limits cannot hold one bounded GPU RCD scratch tile".into(),
            });
        }
        let memory = Reservation::try_new(SCRATCH_BYTES + 64, processor.budget)?;
        let scratch = processor.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("RCD tile scratch"),
            size: SCRATCH_BYTES,
            usage: wgpu::BufferUsages::STORAGE
                | if cfg!(test) {
                    wgpu::BufferUsages::COPY_SRC
                } else {
                    wgpu::BufferUsages::empty()
                },
            mapped_at_creation: false,
        });
        let uniform = processor.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("RCD tile parameters"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shader = processor
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("fixed tile RCD stages"),
                source: wgpu::ShaderSource::Wgsl(include_str!("rcd.wgsl").into()),
            });
        let binding = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let layout = processor
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("RCD common stage bindings"),
                entries: &[
                    binding(
                        0,
                        wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                    ),
                    binding(
                        1,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                    ),
                    binding(
                        2,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                    ),
                    binding(
                        3,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                    ),
                    binding(
                        4,
                        wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::R32Float,
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                        },
                    ),
                    binding(
                        5,
                        wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::R32Float,
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                        },
                    ),
                    binding(
                        6,
                        wgpu::BindingType::StorageTexture {
                            access: wgpu::StorageTextureAccess::WriteOnly,
                            format: wgpu::TextureFormat::R32Float,
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                        },
                    ),
                ],
            });
        let pipeline_layout =
            processor
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("RCD common stage pipeline layout"),
                    bind_group_layouts: &[&layout],
                    push_constant_ranges: &[],
                });
        let pipelines = NAMES
            .iter()
            .map(|name| {
                processor
                    .device
                    .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                        label: Some(name),
                        layout: Some(&pipeline_layout),
                        module: &shader,
                        entry_point: Some(name),
                        compilation_options: Default::default(),
                        cache: None,
                    })
            })
            .collect();
        Ok(Self {
            _memory: memory,
            scratch,
            uniform,
            pipelines,
        })
    }

    pub(super) fn run(
        &self,
        processor: &GpuSensorProcessor,
        mosaic: &GpuHighlightedMosaic,
        mosaic_view: &wgpu::TextureView,
        planes: &ResidentCameraPlanes,
        validation: &wgpu::Buffer,
        cancellation: &CancellationToken,
    ) -> Result<usize, GpuPreviewError> {
        self.execute(
            processor,
            mosaic,
            mosaic_view,
            planes,
            validation,
            cancellation,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn execute(
        &self,
        processor: &GpuSensorProcessor,
        mosaic: &GpuHighlightedMosaic,
        mosaic_view: &wgpu::TextureView,
        planes: &ResidentCameraPlanes,
        validation: &wgpu::Buffer,
        cancellation: &CancellationToken,
        mut probes: Option<&mut Vec<wgpu::Buffer>>,
        mut after_tile: Option<&mut dyn FnMut(usize)>,
    ) -> Result<usize, GpuPreviewError> {
        let views = planes.storage_views();
        let bindings = processor
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("RCD tile bindings"),
                layout: &self.pipelines[0].get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(mosaic_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.scratch.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: validation.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(views[0]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(views[1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(views[2]),
                    },
                ],
            });
        let src = mosaic.layout;
        let dst = planes.layout;
        let mut count = 0;
        for tile in rcd_tiles(src.width as usize, src.height as usize) {
            check_cancel(cancellation)?;
            let words = [
                src.width,
                src.height,
                src.tile_width,
                src.tile_height,
                src.columns,
                bayer_pattern_code(mosaic.normalization.crop().pattern()) as u32,
                0,
                0,
                dst.tile_width,
                dst.tile_height,
                dst.columns,
                0,
                tile.origin_x as u32,
                tile.origin_y as u32,
                tile.width as u32,
                tile.height as u32,
            ];
            processor
                .queue
                .write_buffer(&self.uniform, 0, bytemuck::cast_slice(&words));
            let mut encoder =
                processor
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("bounded RCD tile"),
                    });
            // Separate passes establish storage-buffer visibility between stages.
            // Opposite-color reads only measured diagonal sites; at-green reads
            // only completed red/blue sites, so their own writes are disjoint.
            for pipeline in &self.pipelines {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("RCD stage"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &bindings, &[]);
                pass.dispatch_workgroups((FULL as u32).div_ceil(64), 1, 1);
                drop(pass);
                if count == 0
                    && let Some(buffers) = probes.as_deref_mut()
                {
                    let staging = processor.device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some("test-only RCD stage snapshot"),
                        size: SCRATCH_BYTES,
                        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                        mapped_at_creation: false,
                    });
                    encoder.copy_buffer_to_buffer(&self.scratch, 0, &staging, 0, SCRATCH_BYTES);
                    buffers.push(staging);
                }
            }
            processor.queue.submit([encoder.finish()]);
            processor.wait(cancellation)?;
            count += 1;
            if let Some(callback) = after_tile.as_deref_mut() {
                callback(count);
            }
        }
        Ok(count)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn stage_probes(
        &self,
        processor: &GpuSensorProcessor,
        mosaic: &GpuHighlightedMosaic,
        mosaic_view: &wgpu::TextureView,
        planes: &ResidentCameraPlanes,
        validation: &wgpu::Buffer,
        cancellation: &CancellationToken,
    ) -> Result<Vec<Vec<f32>>, GpuPreviewError> {
        let _diagnostic_memory = Reservation::try_new(SCRATCH_BYTES * 9, processor.budget)?;
        let mut buffers = Vec::new();
        self.execute(
            processor,
            mosaic,
            mosaic_view,
            planes,
            validation,
            cancellation,
            Some(&mut buffers),
            None,
        )?;
        let mut snapshots = Vec::with_capacity(buffers.len());
        for buffer in buffers {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = sender.send(result);
                });
            loop {
                processor
                    .device
                    .poll(wgpu::PollType::Poll)
                    .map_err(|e| super::normalize::invalid(&format!("{e:?}")))?;
                match receiver.try_recv() {
                    Ok(result) => {
                        result.map_err(|e| super::normalize::invalid(&e.to_string()))?;
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        return Err(super::normalize::invalid(
                            "RCD stage map callback disconnected",
                        ));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => check_cancel(cancellation)?,
                }
                std::thread::yield_now();
            }
            let mapped = buffer.slice(..).get_mapped_range();
            snapshots.push(bytemuck::cast_slice::<u8, f32>(&mapped).to_vec());
            drop(mapped);
            buffer.unmap();
        }
        Ok(snapshots)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn cancel_after_first_tile(
        &self,
        processor: &GpuSensorProcessor,
        mosaic: &GpuHighlightedMosaic,
        mosaic_view: &wgpu::TextureView,
        planes: &ResidentCameraPlanes,
        validation: &wgpu::Buffer,
        cancellation: &CancellationToken,
    ) -> Result<usize, GpuPreviewError> {
        let mut cancel = |_| cancellation.cancel();
        self.execute(
            processor,
            mosaic,
            mosaic_view,
            planes,
            validation,
            cancellation,
            None,
            Some(&mut cancel),
        )
    }
}
