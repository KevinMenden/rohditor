//! Export with resize and/or output sharpening, in bounded bands of output
//! rows (`staged_output.wgsl` documents the passes).
//!
//! Resize reduces the developed linear image with the CPU's exact area plan;
//! sharpening blurs transfer-encoded luma, so a band also holds the blur
//! radius of rows around the ones it writes. Reading never goes back to an
//! earlier band, and nothing larger than a band is allocated.
use super::*;
use rohditor_core::AreaReductionPlan;
use rohditor_detail::{OutputSharpen, gaussian_kernel};

use crate::spatial::reduction::{plan_table_bytes, source_rows_for_band};

/// Pass codes of `staged_output.wgsl`.
mod mode {
    pub const REDUCE_HORIZONTAL: u32 = 0;
    pub const REDUCE_VERTICAL: u32 = 1;
    pub const ENCODE: u32 = 2;
    pub const LUMA: u32 = 3;
    pub const BLUR_HORIZONTAL: u32 = 4;
    pub const SHARPEN: u32 = 5;
    pub const QUANTIZE: u32 = 6;
}

/// One pass over `domain` (columns, rows) of its band.
struct Pass {
    mode: u32,
    domain: (u32, u32),
    source_rows: (u32, u32),
}

/// The rows of one band: the ones written (`core`) and the ones held.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BandRows {
    core: (u32, u32),
    held: (u32, u32),
}

impl BandRows {
    /// `core_first..core_first + core_rows` of an image `height` rows tall,
    /// extended by `radius` rows on each side where the image has them.
    fn new(core_first: u32, core_rows: u32, radius: u32, height: u32) -> Self {
        let first = core_first.saturating_sub(radius);
        let end = (core_first + core_rows + radius).min(height);
        Self {
            core: (core_first, core_rows),
            held: (first, end - first),
        }
    }
}

/// The passes of one band, in order.
fn band_passes(
    width: u32,
    band: BandRows,
    plan: Option<&AreaReductionPlan>,
    sharpening: bool,
) -> Result<Vec<Pass>, GpuPreviewError> {
    let mut passes = Vec::new();
    let pass = |mode, rows| Pass {
        mode,
        domain: (width, rows),
        source_rows: (0, 0),
    };
    if let Some(plan) = plan {
        let (first, count) =
            source_rows_for_band(plan.vertical(), band.held.0 as usize, band.held.1 as usize)?;
        passes.push(Pass {
            mode: mode::REDUCE_HORIZONTAL,
            domain: (width, count as u32),
            source_rows: (first as u32, count as u32),
        });
        passes.push(Pass {
            mode: mode::REDUCE_VERTICAL,
            domain: (width, band.held.1),
            source_rows: (first as u32, count as u32),
        });
    } else {
        passes.push(pass(mode::ENCODE, band.held.1));
    }
    if sharpening {
        passes.push(pass(mode::LUMA, band.held.1));
        passes.push(pass(mode::BLUR_HORIZONTAL, band.held.1));
        passes.push(pass(mode::SHARPEN, band.core.1));
    } else {
        passes.push(pass(mode::QUANTIZE, band.core.1));
    }
    Ok(passes)
}

impl GpuExportProcessor {
    /// Resize `cropped` to `target` and/or sharpen, entirely on the GPU.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_staged(
        &self,
        source: &crate::GpuSpatialFullSource,
        cropped: (usize, usize),
        target: (usize, usize),
        depth: OutputBitDepth,
        dithering: DitherMode,
        sharpening: Option<OutputSharpening>,
        cancellation: &CancellationToken,
        spatial_metrics: crate::SpatialMetrics,
    ) -> Result<GpuExportResult, GpuPreviewError> {
        let description = &source.description;
        let device = &self.processor.device;
        let queue = &self.processor.queue;
        self.processor
            .capabilities
            .validate_dimensions(target.0, target.1)?;
        let plan = (target != cropped)
            .then(|| AreaReductionPlan::new(cropped.0, cropped.1, target.0, target.1))
            .transpose()
            .map_err(input_error)?;
        let sharpen: Option<(OutputSharpen, Vec<f32>)> = sharpening.map(|preset| {
            let params = preset.params();
            (params, gaussian_kernel(params.sigma))
        });
        let radius = sharpen
            .as_ref()
            .map_or(0, |(_, kernel)| (kernel.len() / 2) as u32);
        let (width, height) = (target.0 as u32, target.1 as u32);

        // Largest buffers any band needs.
        let held_rows = BAND_ROWS + 2 * radius;
        let mut scratch_rows = 1;
        if let Some(plan) = &plan {
            for first in (0..height).step_by(BAND_ROWS as usize) {
                let band = BandRows::new(first, BAND_ROWS.min(height - first), radius, height);
                let (_, count) = source_rows_for_band(
                    plan.vertical(),
                    band.held.0 as usize,
                    band.held.1 as usize,
                )?;
                scratch_rows = scratch_rows.max(count as u64);
            }
        }
        let pixels = |rows: u64| u64::from(width) * rows;
        // One buffer holds the reduction scratch, the encoded band and the
        // sharpening planes (see `staged_output.wgsl`).
        let scratch_elements = if plan.is_some() {
            pixels(scratch_rows) * 3
        } else {
            0
        };
        let encoded_elements = pixels(u64::from(held_rows)) * 3;
        let planes_elements = if sharpen.is_some() {
            pixels(u64::from(held_rows)) * 2
        } else {
            0
        };
        let work_bytes = (scratch_elements + encoded_elements + planes_elements) * 4;
        let band_bytes = pixels(u64::from(BAND_ROWS)) * 12;
        let tables_bytes = match &plan {
            Some(plan) => plan_table_bytes(plan)?,
            None => 0,
        } + sharpen
            .as_ref()
            .map_or(0, |(_, kernel)| 4 * kernel.len() as u64);
        let working_bytes = work_bytes + band_bytes * 2 + tables_bytes * 2 + 64 * 1024;
        let limits = device.limits();
        let largest = work_bytes.max(band_bytes);
        if largest > limits.max_buffer_size
            || largest > u64::from(limits.max_storage_buffer_binding_size)
            || width.div_ceil(WORKGROUP_EDGE) > limits.max_compute_workgroups_per_dimension
        {
            return Err(GpuPreviewError::Unsupported {
                reason: "resized or sharpened export exceeds a buffer or dispatch limit".into(),
            });
        }
        let estimated_gpu_bytes = source
            .estimated_bytes()
            .saturating_add(working_bytes)
            .max(spatial_metrics.estimated_gpu_bytes)
            .max(spatial_metrics.capture.estimated_gpu_bytes);
        if estimated_gpu_bytes > MEMORY_BUDGET {
            return Err(GpuPreviewError::Unsupported {
                reason: format!(
                    "resident export needs an estimated {estimated_gpu_bytes} GPU bytes; budget is {MEMORY_BUDGET}"
                ),
            });
        }
        let output_pixels = target.0 as u64 * target.1 as u64;
        let source_pixels =
            description.source_dimensions().0 as u64 * description.source_dimensions().1 as u64;
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
        let _reservation = crate::memory::Reservation::try_new(working_bytes, MEMORY_BUDGET)?;

        let storage = |label, size: u64, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: size.max(16),
                usage: wgpu::BufferUsages::STORAGE | usage,
                mapped_at_creation: false,
            })
        };
        let work = storage(
            "staged export work",
            work_bytes,
            wgpu::BufferUsages::empty(),
        );
        let output = storage(
            "staged export integer band",
            band_bytes,
            wgpu::BufferUsages::COPY_SRC,
        );
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staged export band readback"),
            size: band_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tables = Tables::new(
            plan.as_ref(),
            sharpen.as_ref().map(|(_, kernel)| kernel.as_slice()),
        );
        let area_samples = storage(
            "staged export area samples",
            16 * tables.samples.len() as u64,
            wgpu::BufferUsages::COPY_DST,
        );
        let area_weights = storage(
            "staged export area weights",
            4 * tables.weights.len() as u64,
            wgpu::BufferUsages::COPY_DST,
        );
        queue.write_buffer(&area_samples, 0, bytemuck::cast_slice(&tables.samples));
        queue.write_buffer(&area_weights, 0, bytemuck::cast_slice(&tables.weights));
        let offsets = Offsets {
            encoded: scratch_elements as u32,
            planes: (scratch_elements + encoded_elements) as u32,
            vertical_samples: tables.vertical_samples,
            vertical_weights: tables.vertical_weights,
            gaussian: tables.gaussian,
        };
        let shared = self.shared_bindings(&self.staged_pipeline, source);
        let layout = self.staged_pipeline.get_bind_group_layout(1);
        let params = sharpen.as_ref().map(|(params, _)| *params);

        let started = Instant::now();
        let image = self.read_bands_with(
            target.0,
            target.1,
            depth,
            cancellation,
            &output,
            &staging,
            |encoder, first, rows, maximum| {
                let band = BandRows::new(first, rows, radius, height);
                let passes = band_passes(width, band, plan.as_ref(), sharpen.is_some())?;
                // Each pass has its own parameters; they are all written
                // before this band's submission.
                let groups: Vec<wgpu::BindGroup> = passes
                    .iter()
                    .map(|pass| {
                        let words = stage_words(
                            pass,
                            (width, height),
                            band,
                            radius,
                            maximum,
                            dithering,
                            params,
                            offsets,
                        );
                        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("staged export pass parameters"),
                            size: 80,
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&words));
                        device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("staged export pass bindings"),
                            layout: &layout,
                            entries: &[
                                entry(7, &work),
                                entry(8, &area_samples),
                                entry(9, &uniform),
                                entry(10, &area_weights),
                                entry(11, &output),
                            ],
                        })
                    })
                    .collect();
                let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: Some("staged export band"),
                    timestamp_writes: None,
                });
                compute.set_pipeline(&self.staged_pipeline);
                compute.set_bind_group(0, &shared.colour, &[]);
                compute.set_bind_group(2, &shared.camera, &[]);
                for (pass, group) in passes.iter().zip(&groups) {
                    compute.set_bind_group(1, group, &[]);
                    compute.dispatch_workgroups(
                        pass.domain.0.div_ceil(WORKGROUP_EDGE),
                        pass.domain.1.div_ceil(WORKGROUP_EDGE),
                        1,
                    );
                }
                Ok(())
            },
        )?;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("staged export failure readback"),
        });
        encoder.copy_buffer_to_buffer(&shared.failure, 0, &shared.failure_staging, 0, 4);
        queue.submit([encoder.finish()]);
        crate::spatial::reduction::read_failure(device, &shared.failure_staging, cancellation)?;

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
            submissions: spatial_metrics.submissions + height.div_ceil(BAND_ROWS) + 1,
        })
    }
}

fn entry(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

/// The `ExportStage` uniform of `staged_output.wgsl` for one pass.
#[allow(clippy::too_many_arguments)]
fn stage_words(
    pass: &Pass,
    (width, height): (u32, u32),
    band: BandRows,
    radius: u32,
    maximum: u32,
    dithering: DitherMode,
    sharpen: Option<OutputSharpen>,
    offsets: Offsets,
) -> [u32; 20] {
    let params = sharpen.unwrap_or(OutputSharpen {
        sigma: 1.0,
        amount: 0.0,
        threshold: 0.0,
        overshoot: 0.0,
    });
    [
        pass.mode,
        width,
        height,
        band.held.0,
        band.held.1,
        band.core.0,
        band.core.1,
        radius,
        pass.source_rows.0,
        pass.source_rows.1,
        maximum,
        u32::from(dithering == DitherMode::Ordered8x8),
        params.amount.to_bits(),
        params.threshold.to_bits(),
        params.overshoot.to_bits(),
        offsets.vertical_samples,
        offsets.encoded,
        offsets.planes,
        offsets.vertical_weights,
        offsets.gaussian,
    ]
}

/// Element offsets into the shared buffers (see `staged_output.wgsl`).
#[derive(Debug, Clone, Copy)]
struct Offsets {
    encoded: u32,
    planes: u32,
    vertical_samples: u32,
    vertical_weights: u32,
    gaussian: u32,
}

/// The area tables (horizontal, then vertical) and the Gaussian kernel, packed
/// into one samples array and one weights array.
struct Tables {
    samples: Vec<[u32; 4]>,
    weights: Vec<f32>,
    vertical_samples: u32,
    vertical_weights: u32,
    gaussian: u32,
}

impl Tables {
    fn new(plan: Option<&AreaReductionPlan>, kernel: Option<&[f32]>) -> Self {
        let mut samples = Vec::new();
        let mut weights = Vec::new();
        let (mut vertical_samples, mut vertical_weights) = (0, 0);
        if let Some(plan) = plan {
            for (index, axis) in [plan.horizontal(), plan.vertical()].into_iter().enumerate() {
                if index == 1 {
                    vertical_samples = samples.len() as u32;
                    vertical_weights = weights.len() as u32;
                }
                samples.extend(axis.samples().iter().map(|sample| {
                    [
                        sample.first() as u32,
                        sample.weight_offset() as u32,
                        sample.weight_count() as u32,
                        0,
                    ]
                }));
                weights.extend_from_slice(axis.weights());
            }
        }
        let gaussian = weights.len() as u32;
        weights.extend_from_slice(kernel.unwrap_or(&[]));
        // Buffers are never empty.
        if samples.is_empty() {
            samples.push([0; 4]);
        }
        if weights.is_empty() {
            weights.push(0.0);
        }
        Self {
            samples,
            weights,
            vertical_samples,
            vertical_weights,
            gaussian,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bands_extend_by_the_blur_radius_only_inside_the_image() {
        // A band in the middle holds `radius` rows on each side.
        assert_eq!(BandRows::new(64, 64, 3, 200).held, (61, 70));
        // The first and last bands are clipped by the image.
        assert_eq!(BandRows::new(0, 64, 3, 200).held, (0, 67));
        assert_eq!(BandRows::new(192, 8, 3, 200).held, (189, 11));
        // Without sharpening a band holds exactly the rows it writes.
        assert_eq!(BandRows::new(64, 64, 0, 200).held, (64, 64));
    }

    #[test]
    fn passes_follow_resize_then_sharpen_or_quantize() {
        let band = BandRows::new(0, 4, 0, 4);
        let modes = |plan: Option<&AreaReductionPlan>, sharpening| -> Vec<u32> {
            band_passes(8, band, plan, sharpening)
                .unwrap()
                .iter()
                .map(|pass| pass.mode)
                .collect()
        };
        let plan = AreaReductionPlan::new(16, 8, 8, 4).unwrap();
        assert_eq!(modes(None, false), [mode::ENCODE, mode::QUANTIZE]);
        assert_eq!(
            modes(Some(&plan), false),
            [
                mode::REDUCE_HORIZONTAL,
                mode::REDUCE_VERTICAL,
                mode::QUANTIZE
            ]
        );
        assert_eq!(
            modes(None, true),
            [
                mode::ENCODE,
                mode::LUMA,
                mode::BLUR_HORIZONTAL,
                mode::SHARPEN
            ]
        );
    }
}
