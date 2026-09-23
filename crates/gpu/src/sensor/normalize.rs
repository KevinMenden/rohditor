use std::sync::mpsc;
use std::time::{Duration, Instant};

use rohditor_core::{CancellationToken, NormalizationContract};
use rohditor_image::BayerPattern;
use rohditor_raw::RawFrame;
use wgpu::util::DeviceExt;

use super::resources::{DEFAULT_BUDGET, MosaicLayout};
use crate::{GpuMemoryReservations, GpuPreviewError, memory::Reservation};

pub(super) const WORKGROUP_EDGE: u32 = 8;
const PARAMETER_WORDS: usize = 12;

/// Timing and allocation facts for one bounded sensor normalization.
#[derive(Debug, Clone, Copy, Default)]
pub struct SensorMetrics {
    pub combined_gpu_reservations: GpuMemoryReservations,
    pub total: Duration,
    pub upload: Duration,
    pub normalization: Duration,
    pub uploaded_bytes: u64,
    /// Retained bytes after u16 input and level-table resources are released.
    pub resident_gpu_bytes: u64,
    /// Conservative peak reservation including one u16 tile and level tables.
    pub estimated_gpu_bytes: u64,
    pub tiles: usize,
    pub submissions: u32,
}

/// A resident, crop-local normalized R32Float Bayer mosaic.
///
/// It is intentionally not convertible to a CPU mosaic: later GPU highlight
/// and demosaic stages consume this state directly. The immutable decoded RAW
/// frame remains available to the caller for CPU recovery.
pub struct GpuNormalizedMosaic {
    pub(super) _memory: Reservation,
    pub(super) _texture: wgpu::Texture,
    pub(super) layout: MosaicLayout,
    pub(super) contract: NormalizationContract,
}

impl GpuNormalizedMosaic {
    #[must_use]
    pub fn dimensions(&self) -> (usize, usize) {
        (self.layout.width as usize, self.layout.height as usize)
    }

    #[must_use]
    pub const fn estimated_bytes(&self) -> u64 {
        self.layout.resident_bytes
    }

    #[must_use]
    pub const fn contract(&self) -> &NormalizationContract {
        &self.contract
    }

    #[must_use]
    pub fn matches_contract(&self, contract: &NormalizationContract) -> bool {
        &self.contract == contract
    }
}

impl std::fmt::Debug for GpuNormalizedMosaic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GpuNormalizedMosaic")
            .field("dimensions", &self.dimensions())
            .field("estimated_bytes", &self.estimated_bytes())
            .finish_non_exhaustive()
    }
}

/// Ordered GPU executor for the first sensor-development stage.
pub struct GpuSensorProcessor {
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    normalization_pipeline: wgpu::ComputePipeline,
    pub(super) clip_pipeline: wgpu::ComputePipeline,
    pub(super) demosaic_planes_pipeline: wgpu::ComputePipeline,
    pub(super) demosaic_tile_pipeline: wgpu::ComputePipeline,
    pub(super) budget: u64,
    pub(super) maximum_tile_edge: u32,
}

impl GpuSensorProcessor {
    /// Build a sensor executor from a worker-owned adapter/device/queue.
    ///
    /// The adapter is required to reject missing integer-texture support before
    /// a decoded RAW tile is packed or any GPU resource is allocated.
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, GpuPreviewError> {
        validate_texture_support(adapter)?;
        validate_limits(&device.limits())?;
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RAW u16 normalization shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("normalize.wgsl").into()),
        });
        let normalization_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("RAW u16 to normalized R32Float mosaic"),
                layout: None,
                module: &shader,
                entry_point: Some("normalize"),
                compilation_options: Default::default(),
                cache: None,
            });
        let clip_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("RAW Clip highlight shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("highlight.wgsl").into()),
        });
        let clip_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("resident RAW Clip highlight"),
            layout: None,
            module: &clip_shader,
            entry_point: Some("clip"),
            compilation_options: Default::default(),
            cache: None,
        });
        let demosaic_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Bayer bilinear and MHC"),
            source: wgpu::ShaderSource::Wgsl(include_str!("demosaic.wgsl").into()),
        });
        let demosaic_pipeline = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &demosaic_shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let demosaic_planes_pipeline = demosaic_pipeline("demosaic_planes");
        let demosaic_tile_pipeline = demosaic_pipeline("demosaic_tile");
        let validation = pollster::block_on(device.pop_error_scope());
        let allocation = pollster::block_on(device.pop_error_scope());
        let internal = pollster::block_on(device.pop_error_scope());
        if let Some(error) = validation.or(allocation).or(internal) {
            return Err(invalid(&error.to_string()));
        }
        Ok(Self {
            device: device.clone(),
            queue: queue.clone(),
            normalization_pipeline,
            clip_pipeline,
            demosaic_planes_pipeline,
            demosaic_tile_pipeline,
            budget: DEFAULT_BUDGET,
            maximum_tile_edge: device.limits().max_texture_dimension_2d,
        })
    }

    /// Set a conservative operation budget, never above the global policy.
    pub fn set_budget(&mut self, bytes: u64) {
        self.budget = bytes.min(DEFAULT_BUDGET);
    }

    #[cfg(test)]
    pub(crate) fn force_maximum_tile_edge(&mut self, edge: u32) {
        self.maximum_tile_edge = edge.max(1);
    }

    /// Normalize the selected RAW crop into a resident f32 mosaic.
    ///
    /// Work is submitted one bounded tile at a time. No full-image readback is
    /// performed, and cancellation or device failure returns no GPU state.
    pub fn normalize(
        &self,
        frame: &RawFrame,
        contract: &NormalizationContract,
        cancellation: &CancellationToken,
    ) -> Result<(GpuNormalizedMosaic, SensorMetrics), GpuPreviewError> {
        check_cancel(cancellation)?;
        contract.validate_frame(frame).map_err(pipeline_error)?;
        validate_finite_output_range(contract)?;
        let crop = contract.crop();
        let (width, height) = crop.dimensions();
        let level_bytes = level_bytes(contract)?;
        let layout = MosaicLayout::new_bounded(
            width,
            height,
            level_bytes,
            self.budget,
            &self.device.limits(),
            self.maximum_tile_edge,
        )?;
        validate_host_staging(frame, layout)?;

        self.device.push_error_scope(wgpu::ErrorFilter::Internal);
        self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.normalize_inner(frame, contract, layout, cancellation);
        let validation = pollster::block_on(self.device.pop_error_scope());
        let allocation = pollster::block_on(self.device.pop_error_scope());
        let internal = pollster::block_on(self.device.pop_error_scope());
        check_cancel(cancellation)?;
        if let Some(error) = validation.or(allocation).or(internal) {
            return Err(invalid(&error.to_string()));
        }
        result
    }

    fn normalize_inner(
        &self,
        frame: &RawFrame,
        contract: &NormalizationContract,
        layout: MosaicLayout,
        cancellation: &CancellationToken,
    ) -> Result<(GpuNormalizedMosaic, SensorMetrics), GpuPreviewError> {
        check_cancel(cancellation)?;
        // Hold the whole lifetime before touching driver resources. Once input
        // tiles and level tables are released, this reservation shrinks to the
        // only retained state: the normalized mosaic.
        let mut memory = Reservation::try_new(layout.estimated_peak_bytes, self.budget)?;
        let started = Instant::now();
        let output = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("resident normalized RAW mosaic"),
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
        let output_view = output.create_view(&wgpu::TextureViewDescriptor {
            label: Some("resident normalized RAW mosaic array view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            base_array_layer: 0,
            array_layer_count: Some(layout.layers),
            ..Default::default()
        });
        let input = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("reused RAW u16 upload tile"),
            size: wgpu::Extent3d {
                width: layout.tile_width,
                height: layout.tile_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Uint,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());
        let black_levels = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("RAW black level pattern"),
                contents: bytemuck::cast_slice(&contract.black_levels().values),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let white_levels = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("RAW white level table"),
                contents: bytemuck::cast_slice(contract.white_levels()),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let parameters = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("RAW normalization tile parameters"),
            size: (PARAMETER_WORDS * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("RAW normalization bindings"),
            layout: &self.normalization_pipeline.get_bind_group_layout(0),
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
                    resource: black_levels.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: white_levels.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: parameters.as_entire_binding(),
                },
            ],
        });

        let crop = contract.crop();
        let (crop_origin_x, crop_origin_y) = crop.origin();
        let black = contract.black_levels();
        let white_mode = white_mode(contract)?;
        let pattern = bayer_pattern_code(crop.pattern());
        let mut packed = Vec::<u16>::new();
        let mut metrics = SensorMetrics {
            estimated_gpu_bytes: layout.estimated_peak_bytes,
            ..SensorMetrics::default()
        };
        for tile_y in 0..layout.rows {
            for tile_x in 0..layout.columns {
                check_cancel(cancellation)?;
                let left = usize::try_from(tile_x * layout.tile_width)
                    .map_err(|_| invalid("RAW tile x does not fit usize"))?;
                let top = usize::try_from(tile_y * layout.tile_height)
                    .map_err(|_| invalid("RAW tile y does not fit usize"))?;
                let tile_width = usize::try_from(layout.tile_width.min(layout.width - left as u32))
                    .map_err(|_| invalid("RAW tile width does not fit usize"))?;
                let tile_height =
                    usize::try_from(layout.tile_height.min(layout.height - top as u32))
                        .map_err(|_| invalid("RAW tile height does not fit usize"))?;
                let layer = tile_y * layout.columns + tile_x;
                let upload_started = Instant::now();
                pack_raw_tile(
                    frame,
                    crop_origin_x,
                    crop_origin_y,
                    left,
                    top,
                    tile_width,
                    tile_height,
                    cancellation,
                    &mut packed,
                )?;
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &input,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytemuck::cast_slice(&packed),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some((tile_width * std::mem::size_of::<u16>()) as u32),
                        rows_per_image: Some(tile_height as u32),
                    },
                    wgpu::Extent3d {
                        width: tile_width as u32,
                        height: tile_height as u32,
                        depth_or_array_layers: 1,
                    },
                );
                metrics.upload += upload_started.elapsed();
                metrics.uploaded_bytes += packed.len() as u64 * std::mem::size_of::<u16>() as u64;

                let words = parameters_for_tile(
                    tile_width,
                    tile_height,
                    left,
                    top,
                    crop_origin_x,
                    crop_origin_y,
                    layer as usize,
                    black.repeat_width,
                    black.repeat_height,
                    white_mode,
                    pattern,
                )?;
                self.queue
                    .write_buffer(&parameters, 0, bytemuck::cast_slice(&words));
                let normalization_started = Instant::now();
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("RAW u16 normalization tile"),
                        });
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("RAW u16 normalization tile"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&self.normalization_pipeline);
                    pass.set_bind_group(0, &bindings, &[]);
                    pass.dispatch_workgroups(
                        (tile_width as u32).div_ceil(WORKGROUP_EDGE),
                        (tile_height as u32).div_ceil(WORKGROUP_EDGE),
                        1,
                    );
                }
                self.queue.submit([encoder.finish()]);
                self.wait(cancellation)?;
                metrics.normalization += normalization_started.elapsed();
                metrics.tiles += 1;
                metrics.submissions += 1;
            }
        }
        check_cancel(cancellation)?;
        // Input, level tables, parameters, and their host packing buffer are
        // dropped before this state becomes visible to a downstream executor.
        drop(bindings);
        drop(parameters);
        drop(white_levels);
        drop(black_levels);
        drop(input_view);
        drop(input);
        memory.release_to(layout.resident_bytes);
        metrics.total = started.elapsed();
        metrics.resident_gpu_bytes = layout.resident_bytes;
        metrics.combined_gpu_reservations = crate::gpu_memory_reservations();
        Ok((
            GpuNormalizedMosaic {
                _memory: memory,
                _texture: output,
                layout,
                contract: contract.clone(),
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
                .map_err(|error| synchronization(&format!("{error:?}")))?;
            match receiver.try_recv() {
                Ok(()) => return check_cancel(cancellation),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(synchronization(
                        "normalization completion callback disconnected",
                    ));
                }
                // Keep reservations alive until the submitted unit finishes,
                // even if cancellation arrived while it was in flight.
                Err(mpsc::TryRecvError::Empty) => {}
            }
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
pub(super) fn output_usage() -> wgpu::TextureUsages {
    wgpu::TextureUsages::TEXTURE_BINDING
        | wgpu::TextureUsages::STORAGE_BINDING
        | wgpu::TextureUsages::COPY_SRC
        | wgpu::TextureUsages::COPY_DST
}

#[cfg(not(test))]
pub(super) fn output_usage() -> wgpu::TextureUsages {
    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING
}

fn validate_texture_support(adapter: &wgpu::Adapter) -> Result<(), GpuPreviewError> {
    let raw = adapter.get_texture_format_features(wgpu::TextureFormat::R16Uint);
    let output = adapter.get_texture_format_features(wgpu::TextureFormat::R32Float);
    let raw_required = wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING;
    let output_required = output_usage();
    let mut missing = Vec::new();
    if !raw.allowed_usages.contains(raw_required) {
        missing.push("R16Uint upload/unfiltered textureLoad support");
    }
    if !output.allowed_usages.contains(output_required) {
        missing.push("R32Float sampled/storage mosaic support");
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(GpuPreviewError::Unsupported {
            reason: format!("missing {}", missing.join(", ")),
        })
    }
}

fn validate_limits(limits: &wgpu::Limits) -> Result<(), GpuPreviewError> {
    if limits.max_texture_dimension_2d == 0 || limits.max_compute_workgroups_per_dimension == 0 {
        return Err(GpuPreviewError::Unsupported {
            reason: "device has no usable RAW normalization texture or dispatch limit".into(),
        });
    }
    Ok(())
}

fn level_bytes(contract: &NormalizationContract) -> Result<u64, GpuPreviewError> {
    let values = contract
        .black_levels()
        .values
        .len()
        .checked_add(contract.white_levels().len())
        .ok_or_else(|| invalid("RAW level table count overflowed"))?;
    let bytes = values
        .checked_mul(std::mem::size_of::<f32>())
        .and_then(|value| value.checked_add(PARAMETER_WORDS * std::mem::size_of::<u32>()))
        .ok_or_else(|| invalid("RAW level table byte count overflowed"))?;
    u64::try_from(bytes).map_err(|_| invalid("RAW level table byte count exceeds u64"))
}

fn validate_host_staging(frame: &RawFrame, layout: MosaicLayout) -> Result<(), GpuPreviewError> {
    let decoded_bytes = frame
        .mosaic
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or_else(|| invalid("decoded RAW byte count overflowed"))?;
    let tile_bytes = (layout.tile_width as usize)
        .checked_mul(layout.tile_height as usize)
        .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<u16>()))
        .ok_or_else(|| invalid("RAW upload staging byte count overflowed"))?;
    let peak = decoded_bytes
        .checked_add(tile_bytes)
        .ok_or_else(|| invalid("RAW host working-set byte count overflowed"))?;
    if peak > rohditor_core::CPU_WORKING_SET_LIMIT_BYTES {
        return Err(GpuPreviewError::Unsupported {
            reason: format!(
                "immutable decoded RAW plus one bounded u16 upload tile require {peak} bytes, exceeding the {}-byte CPU working-set limit",
                rohditor_core::CPU_WORKING_SET_LIMIT_BYTES
            ),
        });
    }
    Ok(())
}

fn white_mode(contract: &NormalizationContract) -> Result<usize, GpuPreviewError> {
    let black_count = contract.black_levels().values.len();
    match contract.white_levels().len() {
        1 => Ok(0),
        3 => Ok(1),
        count if count == black_count => Ok(2),
        _ => Err(invalid("RAW contract has an unsupported white-level form")),
    }
}

fn validate_finite_output_range(contract: &NormalizationContract) -> Result<(), GpuPreviewError> {
    let black_levels = contract.black_levels();
    let black = &black_levels.values;
    let white_levels = contract.white_levels();
    let mode = white_mode(contract)?;
    let crop = contract.crop();
    let (origin_x, origin_y) = crop.origin();
    let (width, height) = crop.dimensions();
    let end_x = origin_x
        .checked_add(width)
        .ok_or_else(|| invalid("RAW crop x range overflowed"))?;
    let end_y = origin_y
        .checked_add(height)
        .ok_or_else(|| invalid("RAW crop y range overflowed"))?;

    for black_y in 0..black_levels.repeat_height {
        let y_parities =
            repeating_coordinate_parities(black_y, black_levels.repeat_height, origin_y, end_y)?;
        for black_x in 0..black_levels.repeat_width {
            let x_parities =
                repeating_coordinate_parities(black_x, black_levels.repeat_width, origin_x, end_x)?;
            let index = black_y * black_levels.repeat_width + black_x;
            let black = black[index];
            for sensor_y in &y_parities {
                for sensor_x in &x_parities {
                    let white = match mode {
                        0 => white_levels[0],
                        1 => {
                            let local_x = sensor_x
                                .checked_sub(origin_x)
                                .ok_or_else(|| invalid("RAW local x underflowed"))?;
                            let local_y = sensor_y
                                .checked_sub(origin_y)
                                .ok_or_else(|| invalid("RAW local y underflowed"))?;
                            let channel = crop.pattern().color_at(local_x, local_y).channel_index();
                            white_levels[channel]
                        }
                        2 => white_levels[index],
                        _ => unreachable!("white mode was validated above"),
                    };
                    for sample in [0.0_f32, f32::from(u16::MAX)] {
                        let normalized = (sample - black) / (white - black);
                        if !normalized.is_finite() {
                            return Err(invalid(
                                "RAW normalization levels can produce non-finite f32 output",
                            ));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Return one absolute coordinate for each parity a repeating pattern reaches
/// within an interval. The Bayer pattern is 2x2, so further repetitions cannot
/// introduce another relevant phase.
fn repeating_coordinate_parities(
    phase: usize,
    repeat: usize,
    start: usize,
    end: usize,
) -> Result<Vec<usize>, GpuPreviewError> {
    let remainder = start % repeat;
    let delta = (phase + repeat - remainder) % repeat;
    let first = start
        .checked_add(delta)
        .ok_or_else(|| invalid("RAW repeating coordinate overflowed"))?;
    if first >= end {
        return Ok(Vec::new());
    }
    let mut coordinates = vec![first];
    if repeat % 2 == 1 && first.checked_add(repeat).is_some_and(|next| next < end) {
        coordinates.push(first + repeat);
    }
    Ok(coordinates)
}

#[allow(clippy::too_many_arguments)]
fn parameters_for_tile(
    width: usize,
    height: usize,
    left: usize,
    top: usize,
    crop_origin_x: usize,
    crop_origin_y: usize,
    layer: usize,
    black_repeat_width: usize,
    black_repeat_height: usize,
    white_mode: usize,
    bayer_pattern: usize,
) -> Result<[u32; PARAMETER_WORDS], GpuPreviewError> {
    let convert =
        |value, name| u32::try_from(value).map_err(|_| invalid(&format!("RAW {name} exceeds u32")));
    Ok([
        convert(width, "tile width")?,
        convert(height, "tile height")?,
        convert(left, "tile left")?,
        convert(top, "tile top")?,
        convert(crop_origin_x, "crop origin x")?,
        convert(crop_origin_y, "crop origin y")?,
        convert(layer, "destination layer")?,
        convert(black_repeat_width, "black-level repeat width")?,
        convert(black_repeat_height, "black-level repeat height")?,
        convert(white_mode, "white-level mode")?,
        convert(bayer_pattern, "Bayer pattern")?,
        0,
    ])
}

#[allow(clippy::too_many_arguments)]
fn pack_raw_tile(
    frame: &RawFrame,
    crop_origin_x: usize,
    crop_origin_y: usize,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
    cancellation: &CancellationToken,
    packed: &mut Vec<u16>,
) -> Result<(), GpuPreviewError> {
    let elements = width
        .checked_mul(height)
        .ok_or_else(|| invalid("RAW upload tile element count overflowed"))?;
    packed.clear();
    packed
        .try_reserve_exact(elements)
        .map_err(|_| invalid("RAW upload tile staging allocation failed"))?;
    let source_x = crop_origin_x
        .checked_add(left)
        .ok_or_else(|| invalid("RAW upload tile x overflowed"))?;
    let source_y = crop_origin_y
        .checked_add(top)
        .ok_or_else(|| invalid("RAW upload tile y overflowed"))?;
    for offset_y in 0..height {
        check_cancel(cancellation)?;
        let sensor_y = source_y
            .checked_add(offset_y)
            .ok_or_else(|| invalid("RAW upload sensor y overflowed"))?;
        let start = sensor_y
            .checked_mul(frame.row_stride)
            .and_then(|offset| offset.checked_add(source_x))
            .ok_or_else(|| invalid("RAW upload row offset overflowed"))?;
        let end = start
            .checked_add(width)
            .ok_or_else(|| invalid("RAW upload row end overflowed"))?;
        let row = frame
            .mosaic
            .get(start..end)
            .ok_or_else(|| invalid("RAW upload tile lies outside the decoded frame"))?;
        packed.extend_from_slice(row);
    }
    Ok(())
}

pub(super) fn bayer_pattern_code(pattern: BayerPattern) -> usize {
    match pattern {
        BayerPattern::Rggb => 0,
        BayerPattern::Bggr => 1,
        BayerPattern::Grbg => 2,
        BayerPattern::Gbrg => 3,
    }
}

pub(super) fn invalid(reason: &str) -> GpuPreviewError {
    GpuPreviewError::InvalidInput {
        reason: reason.to_owned(),
    }
}

pub(super) fn synchronization(reason: &str) -> GpuPreviewError {
    GpuPreviewError::Synchronization {
        reason: reason.to_owned(),
    }
}

fn pipeline_error(error: rohditor_core::PipelineError) -> GpuPreviewError {
    if matches!(error, rohditor_core::PipelineError::Cancelled) {
        GpuPreviewError::Cancelled
    } else {
        invalid(&error.to_string())
    }
}

pub(super) fn check_cancel(token: &CancellationToken) -> Result<(), GpuPreviewError> {
    if token.is_cancelled() {
        Err(GpuPreviewError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn readback_for_qualification(
    processor: &GpuSensorProcessor,
    mosaic: &GpuNormalizedMosaic,
    cancellation: &CancellationToken,
) -> Result<Vec<f32>, GpuPreviewError> {
    readback_texture_for_qualification(processor, &mosaic._texture, mosaic.layout, cancellation)
}

#[cfg(test)]
pub(super) fn readback_texture_for_qualification(
    processor: &GpuSensorProcessor,
    texture: &wgpu::Texture,
    layout: MosaicLayout,
    cancellation: &CancellationToken,
) -> Result<Vec<f32>, GpuPreviewError> {
    check_cancel(cancellation)?;
    let width = layout.width as usize;
    let height = layout.height as usize;
    let elements = width
        .checked_mul(height)
        .ok_or_else(|| invalid("normalized qualification output overflowed"))?;
    let mut result = vec![0.0_f32; elements];
    for tile_y in 0..layout.rows {
        for tile_x in 0..layout.columns {
            check_cancel(cancellation)?;
            let left = (tile_x * layout.tile_width) as usize;
            let top = (tile_y * layout.tile_height) as usize;
            let tile_width = layout.tile_width.min(layout.width - left as u32) as usize;
            let tile_height = layout.tile_height.min(layout.height - top as u32) as usize;
            let layer = tile_y * layout.columns + tile_x;
            let row_bytes = tile_width
                .checked_mul(std::mem::size_of::<f32>())
                .ok_or_else(|| invalid("normalized qualification row overflowed"))?;
            let padded_row_bytes = row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize)
                * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
            let staging_bytes = padded_row_bytes
                .checked_mul(tile_height)
                .ok_or_else(|| invalid("normalized qualification staging overflowed"))?;
            let staging = processor.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("normalized RAW qualification readback"),
                size: staging_bytes as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut encoder =
                processor
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("normalized RAW qualification copy"),
                    });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_row_bytes as u32),
                        rows_per_image: Some(tile_height as u32),
                    },
                },
                wgpu::Extent3d {
                    width: tile_width as u32,
                    height: tile_height as u32,
                    depth_or_array_layers: 1,
                },
            );
            processor.queue.submit([encoder.finish()]);
            processor.wait(cancellation)?;
            let (sender, receiver) = mpsc::sync_channel(1);
            staging
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = sender.send(result);
                });
            loop {
                processor
                    .device
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
                            reason: "normalized qualification map callback disconnected".into(),
                        });
                    }
                    Err(mpsc::TryRecvError::Empty) => check_cancel(cancellation)?,
                }
                std::thread::yield_now();
            }
            {
                let data = staging.slice(..).get_mapped_range();
                for y in 0..tile_height {
                    let row = &data[y * padded_row_bytes..y * padded_row_bytes + row_bytes];
                    let values: &[f32] = bytemuck::cast_slice(row);
                    let destination = (top + y) * width + left;
                    result[destination..destination + tile_width].copy_from_slice(values);
                }
            }
            staging.unmap();
        }
    }
    Ok(result)
}
