//! Full-quality export using the same color kernels and parameters as preview.
//!
//! The source is resident RGBA32F. Output is quantized directly from f32 in
//! bounded bands, with one band in flight. CPU code only packs integer samples
//! for the codec. There is no half-float or display-texture export intermediate.

use super::*;
use rohditor_core::{CpuPipeline, DitherMode, ExportImage, OutputBitDepth, RenderOptions};
use rohditor_image::{DisplayRgbImage, DisplayTransfer};
use rohditor_raw::RawFrame;
use wgpu::util::DeviceExt;

mod readback;
#[cfg(test)]
mod tests;

const BAND_ROWS: u32 = 64;
/// Conservative per-export GPU allocation budget, including upload staging.
const MEMORY_BUDGET: u64 = 768 * 1024 * 1024;

/// Completed export pixels and separately measured preparation/GPU costs.
pub struct GpuExportResult {
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
    processor: GpuPreviewProcessor,
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
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
        Ok(Self {
            processor,
            pipeline,
            layout,
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

    /// CPU RAW preparation remains full resolution and observes cancellation.
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
        // Reject oversized sources before allocating full-resolution CPU RGB.
        check_cancel(cancellation)?;
        let (width, height) =
            rohditor_core::raw_crop_dimensions(&frame.info, options.raw_crop_policy)
                .map_err(input_error)?;
        self.validate_memory(width, height)?;
        let prepared = cpu
            .prepare_export_source(frame, recipe, options, cancellation)
            .map_err(|error| {
                if cancellation.is_cancelled() {
                    GpuPreviewError::Cancelled
                } else {
                    input_error(error)
                }
            })?;
        self.render_prepared(
            &prepared,
            recipe,
            options.output_policy,
            bit_depth,
            dithering,
            cancellation,
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
        let band_parameters = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rohditor export band parameters"),
            contents: &[0; 16],
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
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
