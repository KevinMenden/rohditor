//! Bounded GPU AMaZE: a batch of fixed 256x256 scratch tiles and an ordered
//! sequence of shader passes mirroring `rohditor_demosaic`'s CPU stages.
//!
//! Tiles come from the shared geometry ([`amaze_tiles`]), so partial tiles,
//! mirrored halos and the bilinear border ring match the CPU. Each batch of up
//! to [`MAX_BATCH`] tiles is one submission: every pass is dispatched for all
//! tiles of the batch, with the tile slot in the dispatch's z coordinate.

use rohditor_core::CancellationToken;
use rohditor_demosaic::{AMAZE_TILE_SIZE, AmazeTileGeometry, ChannelBalance, amaze_tiles};

use super::normalize::{bayer_pattern_code, check_cancel};
use super::{GpuHighlightedMosaic, GpuSensorProcessor};
use crate::spatial::source::ResidentCameraPlanes;
use crate::{GpuPreviewError, memory::Reservation};

const FULL: u64 = (AMAZE_TILE_SIZE * AMAZE_TILE_SIZE) as u64;
/// 12 full-resolution planes and 16 half-resolution ("pair") planes of f32;
/// must match the plane table in `prelude.wgsl`.
const TILE_WORDS: u64 = 12 * FULL + 16 * (FULL / 2);
const TILE_BYTES: u64 = TILE_WORDS * 4;
/// Batch sizes to try, largest first. A larger batch keeps more of the device
/// busy (the ordered passes run one workgroup per tile) at 5 MiB per tile.
pub(super) const BATCH_SIZES: [usize; 5] = [16, 8, 4, 2, 1];
const MAX_BATCH: usize = BATCH_SIZES[0];
/// Fixed-size uniform header: 12 words of image layout, then forward and
/// inverse channel balance and clip points as 4 floats each.
const HEADER_BYTES: u64 = 96;
/// First/last flagged Nyquist row, first/last flagged column, per tile slot.
const BOUNDS_WORDS_PER_TILE: u64 = 4;

/// How a pass maps invocations onto a tile.
#[derive(Clone, Copy)]
enum Grid {
    /// One invocation per tile pixel.
    Pixels,
    /// One invocation per horizontal pixel pair (red/blue site) of the tile.
    Pairs,
    /// One invocation per tile column or row of an ordered chain.
    Lines,
    /// One workgroup per tile, looping over rows.
    Tile,
}

/// Pass order of `amaze::reconstruct_tile`; entry points live in `*.wgsl`.
const STAGES: [(&str, Grid); 19] = [
    ("clear_populate", Grid::Pixels),
    ("gradients", Grid::Pixels),
    ("colour_differences", Grid::Pixels),
    ("bound_vcd", Grid::Lines),
    ("bound_hcd", Grid::Lines),
    ("cddiffsq", Grid::Pixels),
    ("direction_weights", Grid::Pairs),
    ("nyquist_detect", Grid::Pairs),
    ("nyquist_vote", Grid::Pairs),
    ("nyquist_area_weights", Grid::Pairs),
    ("populate_green", Grid::Tile),
    ("nyquist_refine_green", Grid::Pairs),
    ("diagonal_gradients", Grid::Pairs),
    ("diagonal_estimates", Grid::Pairs),
    ("combine_diagonals", Grid::Tile),
    ("refine_green_with_rb", Grid::Pairs),
    ("chroma_split", Grid::Pairs),
    ("chroma_interpolate", Grid::Pairs),
    ("finish_scatter", Grid::Pixels),
];

pub(super) struct AmazeExecutor {
    _memory: Reservation,
    batch: usize,
    scratch: wgpu::Buffer,
    uniform: wgpu::Buffer,
    bounds: wgpu::Buffer,
    pipelines: Vec<(Grid, wgpu::ComputePipeline)>,
}

impl AmazeExecutor {
    const fn uniform_bytes(batch: usize) -> u64 {
        HEADER_BYTES + 16 * batch as u64
    }

    pub(super) const fn scratch_bytes(batch: usize) -> u64 {
        TILE_BYTES * batch as u64
    }

    /// Scratch, tile parameters and Nyquist bounds, as reserved by [`Self::new`].
    pub(super) const fn reserved_bytes(batch: usize) -> u64 {
        Self::scratch_bytes(batch)
            + Self::uniform_bytes(batch)
            + BOUNDS_WORDS_PER_TILE * 4 * batch as u64
    }

    /// Whether the device can hold a scratch of `batch` tiles in one binding.
    pub(super) fn batch_fits(limits: &wgpu::Limits, batch: usize) -> bool {
        let bytes = Self::scratch_bytes(batch);
        (1..=MAX_BATCH).contains(&batch)
            && bytes <= limits.max_buffer_size
            && bytes <= u64::from(limits.max_storage_buffer_binding_size)
            && limits.max_compute_workgroups_per_dimension >= (FULL as u32).div_ceil(64)
            && limits.max_compute_workgroups_per_dimension >= batch as u32
            && limits.max_storage_buffers_per_shader_stage >= 3
            && limits.max_storage_textures_per_shader_stage >= 3
            && limits.max_sampled_textures_per_shader_stage >= 1
            && limits.max_uniform_buffers_per_shader_stage >= 1
            && u64::from(limits.max_uniform_buffer_binding_size) >= Self::uniform_bytes(batch)
            && limits.max_bindings_per_bind_group >= 8
            && limits.max_bind_groups >= 1
            && limits.max_compute_invocations_per_workgroup >= 128
            && limits.max_compute_workgroup_size_x >= 128
            && limits.max_compute_workgroup_storage_size >= 2 * 128 * 4
    }

    pub(super) fn new(
        processor: &GpuSensorProcessor,
        balance: ChannelBalance,
        batch: usize,
    ) -> Result<Self, GpuPreviewError> {
        if !Self::batch_fits(&processor.device.limits(), batch) {
            return Err(GpuPreviewError::Unsupported {
                reason: format!("device limits cannot hold {batch} GPU AMaZE scratch tiles"),
            });
        }
        let memory = Reservation::try_new(Self::reserved_bytes(batch), processor.budget)?;
        let scratch = processor.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("AMaZE tile scratch"),
            size: Self::scratch_bytes(batch),
            usage: wgpu::BufferUsages::STORAGE
                | if cfg!(test) {
                    wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST
                } else {
                    wgpu::BufferUsages::empty()
                },
            mapped_at_creation: false,
        });
        let uniform = processor.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("AMaZE tile parameters"),
            size: Self::uniform_bytes(batch),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bounds = processor.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("AMaZE Nyquist bounds"),
            size: BOUNDS_WORDS_PER_TILE * 4 * batch as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        // The clip points follow the 48 bytes of image layout words. Like the
        // CPU, reconstruction is on the balanced mosaic, which clips at the
        // smallest forward gain; highlights count as clipped from 80% of it.
        let [forward_red, forward_green, forward_blue] = balance.forward();
        let [inverse_red, inverse_green, inverse_blue] = balance.inverse();
        let clip_point = balance.clip_point();
        processor.queue.write_buffer(
            &uniform,
            48,
            bytemuck::cast_slice(&[
                forward_red,
                forward_green,
                forward_blue,
                1.0,
                inverse_red,
                inverse_green,
                inverse_blue,
                1.0,
                clip_point,
                0.8 * clip_point,
                0.0,
                0.0,
            ]),
        );
        let source = [
            include_str!("prelude.wgsl"),
            include_str!("green.wgsl"),
            include_str!("nyquist.wgsl"),
            include_str!("diagonal.wgsl"),
            include_str!("chroma.wgsl"),
        ]
        .concat()
        // The uniform's tile array is sized for this executor's batch.
        .replace("BATCH>", &format!("{batch}>"));
        let shader = processor
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("fixed tile AMaZE stages"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let layout = Self::bind_group_layout(&processor.device);
        let pipeline_layout =
            processor
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("AMaZE stage pipeline layout"),
                    bind_group_layouts: &[&layout],
                    push_constant_ranges: &[],
                });
        let pipelines = STAGES
            .iter()
            .map(|&(name, grid)| {
                let pipeline =
                    processor
                        .device
                        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                            label: Some(name),
                            layout: Some(&pipeline_layout),
                            module: &shader,
                            entry_point: Some(name),
                            compilation_options: Default::default(),
                            cache: None,
                        });
                (grid, pipeline)
            })
            .collect();
        Ok(Self {
            _memory: memory,
            batch,
            scratch,
            uniform,
            bounds,
            pipelines,
        })
    }

    fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let buffer = |ty| wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let storage = || buffer(wgpu::BufferBindingType::Storage { read_only: false });
        let plane = || wgpu::BindingType::StorageTexture {
            access: wgpu::StorageTextureAccess::WriteOnly,
            format: wgpu::TextureFormat::R32Float,
            view_dimension: wgpu::TextureViewDimension::D2Array,
        };
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("AMaZE common stage bindings"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                ),
                entry(1, buffer(wgpu::BufferBindingType::Uniform)),
                entry(2, storage()),
                entry(3, storage()),
                entry(4, plane()),
                entry(5, plane()),
                entry(6, plane()),
                entry(7, storage()),
            ],
        })
    }

    /// Reconstruct the core of every tile into `planes`. Returns the number of
    /// submissions (one per batch of tiles).
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
        mut after_batch: Option<&mut dyn FnMut(usize)>,
    ) -> Result<usize, GpuPreviewError> {
        let views = planes.storage_views();
        let entry = |binding, resource| wgpu::BindGroupEntry { binding, resource };
        let bindings = processor
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("AMaZE tile bindings"),
                layout: &self.pipelines[0].1.get_bind_group_layout(0),
                entries: &[
                    entry(0, wgpu::BindingResource::TextureView(mosaic_view)),
                    entry(1, self.uniform.as_entire_binding()),
                    entry(2, self.scratch.as_entire_binding()),
                    entry(3, validation.as_entire_binding()),
                    entry(4, wgpu::BindingResource::TextureView(views[0])),
                    entry(5, wgpu::BindingResource::TextureView(views[1])),
                    entry(6, wgpu::BindingResource::TextureView(views[2])),
                    entry(7, self.bounds.as_entire_binding()),
                ],
            });
        let src = mosaic.layout;
        let dst = planes.layout;
        let layout_words = [
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
        ];
        processor
            .queue
            .write_buffer(&self.uniform, 0, bytemuck::cast_slice(&layout_words));

        let tiles: Vec<AmazeTileGeometry> =
            amaze_tiles(src.width as usize, src.height as usize).collect();
        let mut submissions = 0;
        for batch in tiles.chunks(self.batch) {
            check_cancel(cancellation)?;
            let origins: Vec<u32> = batch
                .iter()
                .flat_map(|tile| [tile.origin_x as u32, tile.origin_y as u32, 0, 0])
                .collect();
            processor.queue.write_buffer(
                &self.uniform,
                HEADER_BYTES,
                bytemuck::cast_slice(&origins),
            );
            let mut encoder =
                processor
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("bounded AMaZE tile batch"),
                    });
            // Separate passes establish storage-buffer visibility between stages.
            for (grid, pipeline) in &self.pipelines {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("AMaZE stage"),
                    timestamp_writes: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &bindings, &[]);
                let groups = match grid {
                    Grid::Pixels => FULL as u32 / 64,
                    Grid::Pairs => FULL as u32 / 2 / 64,
                    Grid::Lines => AMAZE_TILE_SIZE as u32 / 64,
                    Grid::Tile => 1,
                };
                pass.dispatch_workgroups(groups, 1, batch.len() as u32);
            }
            processor.queue.submit([encoder.finish()]);
            processor.wait(cancellation)?;
            submissions += 1;
            if let Some(callback) = after_batch.as_deref_mut() {
                callback(submissions);
            }
        }
        Ok(submissions)
    }

    /// Run all tiles, then return the scratch (the last batch's final state).
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_and_read_scratch(
        &self,
        processor: &GpuSensorProcessor,
        mosaic: &GpuHighlightedMosaic,
        mosaic_view: &wgpu::TextureView,
        planes: &ResidentCameraPlanes,
        validation: &wgpu::Buffer,
        cancellation: &CancellationToken,
    ) -> Result<Vec<f32>, GpuPreviewError> {
        self.execute(
            processor,
            mosaic,
            mosaic_view,
            planes,
            validation,
            cancellation,
            None,
        )?;
        let size = Self::scratch_bytes(self.batch);
        let staging = processor.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test-only AMaZE scratch snapshot"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = processor.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&self.scratch, 0, &staging, 0, size);
        processor.queue.submit([encoder.finish()]);
        processor.wait(cancellation)?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        staging
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
                    return Err(super::normalize::invalid("AMaZE map callback disconnected"));
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => check_cancel(cancellation)?,
            }
            std::thread::yield_now();
        }
        let mapped = staging.slice(..).get_mapped_range();
        let words = bytemuck::cast_slice::<u8, f32>(&mapped).to_vec();
        drop(mapped);
        staging.unmap();
        Ok(words)
    }

    /// Fill the scratch with NaN, as stale data from an earlier tile might be.
    #[cfg(test)]
    pub(super) fn poison_scratch(&self, processor: &GpuSensorProcessor) {
        let words = vec![f32::NAN; (Self::scratch_bytes(self.batch) / 4) as usize];
        processor
            .queue
            .write_buffer(&self.scratch, 0, bytemuck::cast_slice(&words));
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn cancel_after_first_batch(
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
            Some(&mut cancel),
        )
    }
}
