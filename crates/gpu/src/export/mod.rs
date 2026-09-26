//! Full-quality export using the same color kernels and parameters as preview.
//!
//! The source is resident RGBA32F. Output is quantized directly from f32 in
//! bounded bands, with one band in flight. CPU code only packs integer samples
//! for the codec. There is no half-float or display-texture export intermediate.

use super::*;
use rohditor_core::{CpuPipeline, DitherMode, ExportImage, OutputBitDepth, RenderOptions};
use rohditor_image::{DisplayRgbImage, DisplayTransfer};
use rohditor_raw::RawFrame;

mod readback;
#[cfg(test)]
mod tests;

const BAND_ROWS: u32 = 64;
/// Conservative per-export GPU allocation budget, including upload staging.
const MEMORY_BUDGET: u64 = 768 * 1024 * 1024;

/// Completed export pixels and separately measured preparation/GPU costs.
pub struct GpuExportResult {
    pub sensor_gpu: bool,
    pub combined_gpu_reservations: crate::GpuMemoryReservations,
    pub capture: crate::CaptureMetrics,
    pub image: ExportImage,
    pub preparation: rohditor_core::StageTimings,
    pub upload_time: Duration,
    /// Includes color execution, band readbacks, and integer packing.
    pub color_and_readback_time: Duration,
    pub estimated_gpu_bytes: u64,
    /// Conservative CPU buffer peak, including upload staging but not encoding.
    pub estimated_cpu_bytes: u64,
    pub uploaded_bytes: u64,
    pub readback_bytes: u64,
    pub submissions: u32,
}

/// A presentation-independent export processor. Use a dedicated instance per
/// worker; mutable rendering prevents concurrent writes to shared uniforms.
pub struct GpuExportProcessor {
    sensor: Option<crate::GpuSensorProcessor>,
    adapter: wgpu::Adapter,
    capture: Option<crate::GpuCaptureProcessor>,
    spatial: crate::GpuSpatialProcessor,
    processor: GpuPreviewProcessor,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    spatial_pipeline: wgpu::ComputePipeline,
}

impl GpuExportProcessor {
    /// Supply a device without depending on egui or a window/surface.
    pub fn new(
        adapter: &wgpu::Adapter,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Self, GpuPreviewError> {
        let processor =
            GpuPreviewProcessor::new(adapter, device, queue, wgpu::TextureFormat::Rgba8Unorm)?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rohditor shared color export shader"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("../preview.wgsl"),
                    "\n",
                    include_str!("../color_adjustments.wgsl"),
                    "\n",
                    include_str!("output.wgsl")
                )
                .into(),
            ),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rohditor full-quality GPU color export"),
            layout: None,
            module: &shader,
            entry_point: Some("develop_export"),
            compilation_options: Default::default(),
            cache: None,
        });
        let layout = pipeline.get_bind_group_layout(0);
        let spatial_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rohditor direct spatial export shader"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("../preview.wgsl"),
                    "\n",
                    include_str!("../color_adjustments.wgsl"),
                    "\n",
                    include_str!("../spatial/spatial.wgsl"),
                    "\n",
                    include_str!("spatial_output.wgsl")
                )
                .into(),
            ),
        });
        let spatial_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rohditor direct resident GPU export"),
            layout: None,
            module: &spatial_shader,
            entry_point: Some("develop_spatial_export"),
            compilation_options: Default::default(),
            cache: None,
        });
        let spatial = crate::GpuSpatialProcessor::new(device, queue)?;
        Ok(Self {
            sensor: None,
            adapter: adapter.clone(),
            capture: None,
            spatial,
            processor,
            pipeline,
            layout,
            spatial_pipeline,
        })
    }

    /// Create a hardware Vulkan device for a headless/background export.
    /// Unavailable hardware is an explicit error for the caller's CPU fallback.
    pub fn headless() -> Result<Self, GpuPreviewError> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(input_error)?;
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return Err(GpuPreviewError::Unsupported {
                reason: "export requires a hardware adapter".into(),
            });
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rohditor headless export"),
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(input_error)?;
        Self::new(&adapter, &device, &queue)
    }

    pub fn capabilities(&self) -> &GpuCapabilities {
        self.processor.capabilities()
    }

    /// Shared spatial executor on this worker's supplied device. Scratch is
    /// released before returning; no color source is allocated here.
    pub fn capture_source(
        &mut self,
        source: rohditor_core::DemosaicedCameraSource,
        cancellation: &CancellationToken,
    ) -> Result<(rohditor_core::CapturedCameraSource, crate::CaptureMetrics), GpuPreviewError> {
        if !source
            .capture_contract()
            .map_err(input_error)?
            .settings()
            .is_active()
        {
            return Ok((
                source.capture_cpu(cancellation).map_err(input_error)?,
                crate::CaptureMetrics::default(),
            ));
        }
        if self.capture.is_none() {
            self.capture = Some(crate::GpuCaptureProcessor::new(
                &self.processor.device,
                &self.processor.queue,
            )?);
        }
        let processor = self
            .capture
            .as_mut()
            .expect("capture processor initialized");
        processor.set_remaining_budget(MEMORY_BUDGET - 64 * 1024);
        processor.capture_source(source, cancellation)
    }

    /// Develop supported RAW sensor methods on the GPU at full resolution.
    /// Any error returns before encoding; the caller owns the backend policy.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        cpu: &CpuPipeline,
        frame: &RawFrame,
        recipe: &EditRecipe,
        options: RenderOptions,
        bit_depth: OutputBitDepth,
        dithering: DitherMode,
        cancellation: &CancellationToken,
    ) -> Result<GpuExportResult, GpuPreviewError> {
        check_cancel(cancellation)?;
        let (width, height) =
            rohditor_core::raw_crop_dimensions(&frame.info, options.raw_crop_policy)
                .map_err(input_error)?;
        self.processor
            .capabilities
            .validate_dimensions(width, height)?;
        if self.sensor.is_none() {
            self.sensor = Some(crate::GpuSensorProcessor::new(
                &self.adapter,
                &self.processor.device,
                &self.processor.queue,
            )?);
        }
        let (resident, metrics) = self.sensor.as_ref().expect("sensor initialized").develop(
            cpu,
            frame,
            recipe,
            rohditor_core::PreviewOptions {
                render: options,
                max_long_edge: frame.info.width.max(frame.info.height),
            },
            cancellation,
        )?;
        let description = resident.initial_description().clone();
        let full = self
            .spatial
            .full_resolution_source(resident.captured(), description)?;
        self.render_spatial_source(
            &full,
            recipe,
            options.output_policy,
            bit_depth,
            dithering,
            cancellation,
            metrics,
        )
    }

    /// Export an already prepared camera source, including matched-resolution
    /// preview/export qualification. Exact source provenance is mandatory.
    pub fn render_prepared(
        &mut self,
        prepared: &ReconstructedPreview,
        recipe: &EditRecipe,
        policy: OutputPolicy,
        depth: OutputBitDepth,
        dithering: DitherMode,
        cancellation: &CancellationToken,
    ) -> Result<GpuExportResult, GpuPreviewError> {
        check_cancel(cancellation)?;
        recipe.validate().map_err(input_error)?;
        let estimated_gpu_bytes =
            self.validate_memory(prepared.image().width(), prepared.image().height())?;
        let device = &self.processor.device;
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.render_inner(
            prepared,
            recipe,
            policy,
            depth,
            dithering,
            cancellation,
            estimated_gpu_bytes,
        );
        // Drain scopes on every return path so allocation/device failures are
        // recoverable by the outer export workflow before any file is written.
        let validation = pollster::block_on(device.pop_error_scope());
        let allocation = pollster::block_on(device.pop_error_scope());
        if let Some(error) = validation.or(allocation) {
            return Err(input_error(error));
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn render_spatial_source(
        &self,
        source: &crate::GpuSpatialFullSource,
        recipe: &EditRecipe,
        policy: OutputPolicy,
        depth: OutputBitDepth,
        dithering: DitherMode,
        cancellation: &CancellationToken,
        spatial_metrics: crate::SpatialMetrics,
    ) -> Result<GpuExportResult, GpuPreviewError> {
        check_cancel(cancellation)?;
        recipe.validate().map_err(input_error)?;
        let device = &self.processor.device;
        device.push_error_scope(wgpu::ErrorFilter::Internal);
        device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.render_spatial_inner(
            source,
            recipe,
            policy,
            depth,
            dithering,
            cancellation,
            spatial_metrics,
        );
        let validation = pollster::block_on(device.pop_error_scope());
        let allocation = pollster::block_on(device.pop_error_scope());
        let internal = pollster::block_on(device.pop_error_scope());
        if let Some(error) = validation.or(allocation).or(internal) {
            return Err(input_error(error));
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn render_spatial_inner(
        &self,
        source: &crate::GpuSpatialFullSource,
        recipe: &EditRecipe,
        policy: OutputPolicy,
        depth: OutputBitDepth,
        dithering: DitherMode,
        cancellation: &CancellationToken,
        spatial_metrics: crate::SpatialMetrics,
    ) -> Result<GpuExportResult, GpuPreviewError> {
        let description = &source.description;
        if description.capture_sharpening()
            != rohditor_core::CaptureSharpeningProvenance::for_settings(recipe.capture_sharpening)
            || !highlight_adjustments_match(
                description.highlight_adjustments(),
                recipe.raw.highlights,
            )
            || !source.optics_matches_recipe(&recipe.optics)
            || description.camera_profile_key() != &camera_profile_key(&recipe.color.camera_profile)
            || (description.highlight_adjustments().method == rohditor_edit::HighlightMethod::Clip
                && description.highlight_white_balance() != recipe.color.white_balance)
        {
            return Err(GpuPreviewError::BaseMismatch {
                reason: "resident export source provenance does not match the recipe".into(),
            });
        }
        let resolved = resolve_camera_colour(
            description.calibration(),
            &recipe.color.camera_profile,
            recipe.color.white_balance,
        )
        .map_err(input_error)?;
        let orientation = recipe
            .geometry
            .orientation_override
            .unwrap_or(description.source_orientation());
        let source_dimensions = description.source_dimensions();
        let geometry = OutputGeometry::new(
            source_dimensions.0,
            source_dimensions.1,
            orientation,
            recipe.geometry.crop,
        )
        .map_err(input_error)?;
        let (width, height) = geometry.output_dimensions();
        let (source_width, source_height) = (
            u32::try_from(source_dimensions.0).map_err(input_error)?,
            u32::try_from(source_dimensions.1).map_err(input_error)?,
        );
        self.processor
            .capabilities
            .validate_dimensions(width, height)?;
        let parameters = build_parameters(
            (source_width, source_height),
            orientation,
            geometry,
            recipe,
            policy,
            resolved.white_balance_gains,
            resolved.camera_to_linear_rec2020,
        );
        let queue = &self.processor.queue;
        queue.write_buffer(
            &self.processor.parameters,
            0,
            bytemuck::cast_slice(&parameters),
        );
        queue.write_buffer(
            &self.processor.light_tone_lut,
            0,
            bytemuck::cast_slice(LightToneLut::new(&recipe.light).values()),
        );

        let band_bytes = width as u64 * u64::from(BAND_ROWS) * 12;
        let output_peak = source
            .estimated_bytes()
            .saturating_add(band_bytes.saturating_mul(2))
            .saturating_add(64 * 1024);
        let estimated_gpu_bytes = output_peak
            .max(spatial_metrics.estimated_gpu_bytes)
            .max(spatial_metrics.capture.estimated_gpu_bytes);
        if estimated_gpu_bytes > MEMORY_BUDGET {
            return Err(GpuPreviewError::Unsupported {
                reason: format!(
                    "resident export needs an estimated {estimated_gpu_bytes} GPU bytes; budget is {MEMORY_BUDGET}"
                ),
            });
        }
        let output_pixels = width as u64 * height as u64;
        let source_pixels = source_dimensions.0 as u64 * source_dimensions.1 as u64;
        let estimated_cpu_bytes = description.decoded_raw_bytes() as u64
            + description.normalized_mosaic_bytes() as u64
            + description.highlight_scratch_bytes() as u64
            + if spatial_metrics.sensor_gpu {
                0
            } else {
                source_pixels * 12
            }
            + output_pixels * 3 * u64::from(depth.bits() / 8)
            + band_bytes;
        if estimated_cpu_bytes > rohditor_core::CPU_WORKING_SET_LIMIT_BYTES as u64 {
            return Err(GpuPreviewError::Unsupported {
                reason: format!(
                    "export CPU buffers need {estimated_cpu_bytes} bytes, exceeding the CPU working-set budget"
                ),
            });
        }
        let _reservation = crate::memory::Reservation::try_new(
            band_bytes.saturating_mul(2).saturating_add(64 * 1024),
            MEMORY_BUDGET,
        )?;
        let device = &self.processor.device;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resident spatial export integer band"),
            size: band_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resident spatial export band readback"),
            size: band_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let band_parameters = crate::memory::initialized_buffer(
            device,
            queue,
            &wgpu::util::BufferInitDescriptor {
                label: Some("resident spatial export band parameters"),
                contents: &[0; 16],
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            },
        );
        let optics = crate::memory::initialized_buffer(
            device,
            queue,
            &wgpu::util::BufferInitDescriptor {
                label: Some("resident export optics parameters"),
                contents: bytemuck::cast_slice(&crate::spatial::reduction::pack_optics_parameters(
                    &source.planes.layout,
                    description.optics().execution(),
                )),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let failure = crate::memory::initialized_buffer(
            device,
            queue,
            &wgpu::util::BufferInitDescriptor {
                label: Some("resident export spatial failure"),
                contents: &[0; 4],
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            },
        );
        let failure_staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resident export spatial failure readback"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let colour_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resident export colour bindings"),
            layout: &self.spatial_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.processor.parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.processor.light_tone_lut.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.processor.base_rendering_lut.as_entire_binding(),
                },
            ],
        });
        let output_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resident export output bindings"),
            layout: &self.spatial_pipeline.get_bind_group_layout(1),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: band_parameters.as_entire_binding(),
                },
            ],
        });
        let plane_entries = source.planes.sampled_entries();
        let spatial_bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("resident export camera bindings"),
            layout: &self.spatial_pipeline.get_bind_group_layout(2),
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
        let started = Instant::now();
        let image = self.read_bands_with_pipeline(
            width,
            height,
            depth,
            dithering,
            cancellation,
            &output,
            &staging,
            &band_parameters,
            &self.spatial_pipeline,
            &[
                (0, &colour_bindings),
                (1, &output_bindings),
                (2, &spatial_bindings),
            ],
        )?;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("resident export failure readback"),
        });
        encoder.copy_buffer_to_buffer(&failure, 0, &failure_staging, 0, 4);
        queue.submit([encoder.finish()]);
        crate::spatial::reduction::read_failure(device, &failure_staging, cancellation)?;
        let mut preparation = description.preparation_timings();
        preparation.capture_sharpening = spatial_metrics.capture.total;
        preparation.total += spatial_metrics.capture.total;
        if !spatial_metrics.sensor_gpu {
            preparation.total += spatial_metrics.upload;
        }
        Ok(GpuExportResult {
            sensor_gpu: spatial_metrics.sensor_gpu,
            combined_gpu_reservations: crate::gpu_memory_reservations(),
            capture: spatial_metrics.capture,
            image,
            preparation,
            upload_time: spatial_metrics.upload,
            color_and_readback_time: started.elapsed(),
            estimated_gpu_bytes,
            estimated_cpu_bytes,
            uploaded_bytes: spatial_metrics.uploaded_bytes,
            readback_bytes: output_pixels * 12 + 4 + spatial_metrics.readback_bytes,
            submissions: spatial_metrics.submissions + (height as u32).div_ceil(BAND_ROWS) + 1,
        })
    }

    fn validate_memory(&self, width: usize, height: usize) -> Result<u64, GpuPreviewError> {
        let (w, h) = self
            .processor
            .capabilities
            .validate_dimensions(width, height)?;
        let limits = self.processor.device.limits();
        let band = u64::from(w.max(h)) * u64::from(BAND_ROWS) * 12;
        // Source + upload staging + output/readback band + uniforms/LUTs.
        let bytes = u64::from(w) * u64::from(h) * 32 + band * 2 + 64 * 1024;
        if bytes > MEMORY_BUDGET
            || band > limits.max_buffer_size
            || band > u64::from(limits.max_storage_buffer_binding_size)
            || w.max(h).div_ceil(WORKGROUP_EDGE) > limits.max_compute_workgroups_per_dimension
        {
            return Err(GpuPreviewError::Unsupported {
                reason: format!(
                    "export {w}x{h} needs an estimated {bytes} GPU bytes; budget is {MEMORY_BUDGET}, or a buffer/dispatch limit was exceeded"
                ),
            });
        }
        Ok(bytes)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_inner(
        &self,
        prepared: &ReconstructedPreview,
        recipe: &EditRecipe,
        policy: OutputPolicy,
        depth: OutputBitDepth,
        dithering: DitherMode,
        cancellation: &CancellationToken,
        estimated_gpu_bytes: u64,
    ) -> Result<GpuExportResult, GpuPreviewError> {
        let upload_started = Instant::now();
        // Dimensions have already passed device limits. Conservatively include
        // source RGB, both packed upload and staging, full integer output, and
        // a mapped band, even though their lifetimes do not all overlap.
        let pixels = prepared.image().width() as u64 * prepared.image().height() as u64;
        // Source/upload and processor constants have their own reservations.
        let _reservation = crate::memory::Reservation::try_new(
            estimated_gpu_bytes - pixels * 32 - 64 * 1024,
            MEMORY_BUDGET,
        )?;
        let estimated_cpu_bytes = (prepared.decoded_raw_bytes() as u64
            + pixels * (12 + 32 + 3 * u64::from(depth.bits() / 8))
            + prepared.image().width().max(prepared.image().height()) as u64
                * u64::from(BAND_ROWS)
                * 12)
            .max(prepared.preparation_peak_bytes() as u64);
        if estimated_cpu_bytes > rohditor_core::CPU_WORKING_SET_LIMIT_BYTES as u64 {
            return Err(GpuPreviewError::Unsupported {
                reason: format!(
                    "export CPU buffers need {estimated_cpu_bytes} bytes, exceeding the CPU working-set budget"
                ),
            });
        }
        let upload = GpuPreviewUpload::from_reconstructed_preview_for_recipe(
            prepared,
            recipe,
            cancellation,
        )?;
        let source = self.processor.upload_prepared(upload)?;
        self.processor.wait_for_queue()?;
        let upload_time = upload_started.elapsed();
        let started = Instant::now();
        if !source.optics_matches_recipe(&recipe.optics) {
            return Err(GpuPreviewError::BaseMismatch {
                reason: "export optics differ from prepared source".into(),
            });
        }
        let (gains, matrix) = source.resolve_recipe_colour(recipe, false)?;
        let orientation = recipe
            .geometry
            .orientation_override
            .unwrap_or(source.source_orientation);
        let geometry = OutputGeometry::new(
            source.width as usize,
            source.height as usize,
            orientation,
            recipe.geometry.crop,
        )
        .map_err(input_error)?;
        let (width, height) = geometry.output_dimensions();
        let parameters = build_parameters(
            source.source_dimensions(),
            orientation,
            geometry,
            recipe,
            policy,
            gains,
            matrix,
        );
        let device = &self.processor.device;
        let queue = &self.processor.queue;
        queue.write_buffer(
            &self.processor.parameters,
            0,
            bytemuck::cast_slice(&parameters),
        );
        queue.write_buffer(
            &self.processor.light_tone_lut,
            0,
            bytemuck::cast_slice(LightToneLut::new(&recipe.light).values()),
        );
        let band_bytes = width as u64 * u64::from(BAND_ROWS) * 12;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rohditor export integer band"),
            size: band_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rohditor export band readback"),
            size: band_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let band_parameters = crate::memory::initialized_buffer(
            device,
            queue,
            &wgpu::util::BufferInitDescriptor {
                label: Some("rohditor export band parameters"),
                contents: &[0; 16],
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            },
        );
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rohditor export bindings"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: band_parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.processor.parameters.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.processor.light_tone_lut.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.processor.base_rendering_lut.as_entire_binding(),
                },
            ],
        });
        let image = self.read_bands(
            width,
            height,
            depth,
            dithering,
            cancellation,
            &output,
            &staging,
            &band_parameters,
            &bindings,
        )?;
        Ok(GpuExportResult {
            combined_gpu_reservations: crate::gpu_memory_reservations(),
            sensor_gpu: false,
            capture: crate::CaptureMetrics::default(),
            image,
            preparation: prepared.timings(),
            upload_time,
            color_and_readback_time: started.elapsed(),
            estimated_gpu_bytes,
            estimated_cpu_bytes,
            uploaded_bytes: u64::from(source.width) * u64::from(source.height) * 16,
            readback_bytes: width as u64 * height as u64 * 12,
            submissions: (height as u32).div_ceil(BAND_ROWS),
        })
    }
}

fn input_error(error: impl std::fmt::Display) -> GpuPreviewError {
    GpuPreviewError::InvalidInput {
        reason: error.to_string(),
    }
}

fn check_cancel(token: &CancellationToken) -> Result<(), GpuPreviewError> {
    if token.is_cancelled() {
        Err(GpuPreviewError::Cancelled)
    } else {
        Ok(())
    }
}
