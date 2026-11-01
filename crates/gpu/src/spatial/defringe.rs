//! GPU defringe: optics and defringe are baked into a second set of resident
//! planes, tile by tile, before preview reduction or full-resolution
//! development, so everything downstream reads the planes it already knows
//! (with optics marked [`rohditor_core::SpatialOptics::Applied`]).
//!
//! Each tile is the optics output of its core plus a [`DEFRINGE_HALO`] border,
//! defringed in one submission and scattered back without the border. The
//! luma noise profile comes from a readback of the sampled rows of the optics
//! output, as for noise reduction.
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use rohditor_core::{
    CancellationToken, DefringeContract, DetailContract, SpatialCompletionDescription,
};
use rohditor_detail::{
    DEFRINGE_HALO, DILATE_RADIUS, EDGE_FRACTION, HUE_ROLLOFF, LOCAL_MAX_RADIUS, NEUTRAL_SATURATION,
    NOISE_SIGMAS, NoiseProfile, TRUSTED_SATURATION, estimate_luma_noise_sampled, noise_sample_rows,
};
use rohditor_image::{LinearRgbImage, LinearRgbSpace};

use super::reduction::pack_optics_parameters;
use super::source::ResidentCameraPlanes;
use super::{GpuCapturedSource, GpuSpatialProcessor, check_cancel, invalid};
use crate::{GpuPreviewError, memory::Reservation};

/// Core edge of a tile, in pixels.
const TILE_EDGE: usize = 512;
/// Scalar planes of tile size the passes use.
const PLANES: u64 = 4;

/// Pass codes of `defringe.wgsl`.
mod mode {
    pub const LUMA: u32 = 0;
    pub const GRADIENT: u32 = 1;
    pub const MAX_HORIZONTAL: u32 = 2;
    pub const MAX_VERTICAL: u32 = 3;
    pub const EDGE: u32 = 4;
    pub const WEIGHT: u32 = 5;
    pub const REPLACEMENT: u32 = 6;
    pub const BLEND: u32 = 7;
}

/// One dispatch of `defringe.wgsl`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pass {
    mode: u32,
    radius: u32,
    /// Input and output plane of the maximum filters.
    planes: [u32; 2],
}

/// The passes for one tile, in order. Planes: 0 luma, 1 gradient / edge mask
/// / weights, 2 and 3 scratch (3 holds the local maximum, then the dilated
/// mask, then a replacement colour difference).
fn schedule() -> Vec<Pass> {
    let pass = |mode| Pass {
        mode,
        radius: 0,
        planes: [0, 0],
    };
    let maximum = |radius: usize| {
        [
            Pass {
                mode: mode::MAX_HORIZONTAL,
                radius: radius as u32,
                planes: [1, 2],
            },
            Pass {
                mode: mode::MAX_VERTICAL,
                radius: radius as u32,
                planes: [2, 3],
            },
        ]
    };
    let mut passes = vec![pass(mode::LUMA), pass(mode::GRADIENT)];
    passes.extend(maximum(LOCAL_MAX_RADIUS));
    passes.push(pass(mode::EDGE));
    passes.extend(maximum(DILATE_RADIUS));
    passes.extend([
        pass(mode::WEIGHT),
        pass(mode::REPLACEMENT),
        pass(mode::BLEND),
    ]);
    passes
}

/// Pipelines of the bake: the optics output of a region, and the defringe.
pub(super) struct DefringePipelines {
    region: wgpu::ComputePipeline,
    defringe: wgpu::ComputePipeline,
}

impl DefringePipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let optics = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("optics output of a region"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("spatial.wgsl"),
                    "\n",
                    include_str!("bake_region.wgsl")
                )
                .into(),
            ),
        });
        let defringe = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("defringe shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("defringe.wgsl").into()),
        });
        let pipeline = |module, entry_point, label| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: None,
                module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            region: pipeline(&optics, "materialize_region", "optics output of a region"),
            defringe: pipeline(&defringe, "defringe", "defringe passes"),
        }
    }
}

impl GpuSpatialProcessor {
    /// `source` with optics and defringe baked into its pixels.
    pub(super) fn baked_source(
        &self,
        source: &GpuCapturedSource,
        description: SpatialCompletionDescription,
        cancellation: &CancellationToken,
    ) -> Result<GpuCapturedSource, GpuPreviewError> {
        let (planes, description) = self.bake_defringe(source, description, cancellation)?;
        Ok(GpuCapturedSource {
            planes,
            description,
            detail_contract: source.detail_contract.clone(),
            uploaded_bytes: source.uploaded_bytes,
        })
    }

    /// Resident planes holding the optics output of `source` defringed as
    /// `description` asks, and the description of those pixels. Peak memory
    /// is one more set of resident planes plus a tile of scratch.
    pub(super) fn bake_defringe(
        &self,
        source: &GpuCapturedSource,
        description: SpatialCompletionDescription,
        cancellation: &CancellationToken,
    ) -> Result<(Arc<ResidentCameraPlanes>, SpatialCompletionDescription), GpuPreviewError> {
        let settings = description
            .defringe()
            .ok_or_else(|| invalid("defringe requested without active settings"))?
            .settings;
        let detail = &source.detail_contract;
        let contract = DefringeContract::new(
            settings,
            description.calibration(),
            description.profile_selection(),
            detail.balance(),
            detail.ceilings(),
        )
        .map_err(|error| invalid(&error.to_string()))?;
        let layout = source.planes.layout;
        let (width, height) = (layout.width as usize, layout.height as usize);
        let tile_pixels = (TILE_EDGE + 2 * DEFRINGE_HALO).pow(2);
        let scratch_bytes = tile_pixels as u64 * (12 + 4 * PLANES) + 64 * 1024;
        let _scratch = Reservation::try_new(scratch_bytes, self.budget)?;
        let baked = Arc::new(ResidentCameraPlanes::with_budget(
            &self.device,
            layout,
            self.budget,
        )?);
        let optics = crate::memory::initialized_buffer(
            &self.device,
            &self.queue,
            &wgpu::util::BufferInitDescriptor {
                label: Some("defringe optics parameters"),
                contents: bytemuck::cast_slice(&pack_optics_parameters(
                    &layout,
                    description.optics().execution(),
                )),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let failure = crate::memory::initialized_buffer(
            &self.device,
            &self.queue,
            &wgpu::util::BufferInitDescriptor {
                label: Some("defringe failure flag"),
                contents: &[0; 4],
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            },
        );
        let failure_staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("defringe failure readback"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let optics_group = {
            let entries = source.planes.sampled_entries();
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("defringe resident source and optics"),
                layout: &self.defringe.region.get_bind_group_layout(2),
                entries: &[
                    entries[0].clone(),
                    entries[1].clone(),
                    entries[2].clone(),
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: optics.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: failure.as_entire_binding(),
                    },
                ],
            })
        };
        let rgb = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("defringe tile RGB"),
            size: tile_pixels as u64 * 12,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let planes = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("defringe tile planes"),
            size: tile_pixels as u64 * 4 * PLANES,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let noise = self.profile_optics_noise(
            &optics_group,
            &rgb,
            tile_pixels,
            &layout,
            detail,
            cancellation,
        )?;
        let context = TileContext {
            contract: &contract,
            noise,
            rgb: &rgb,
            planes: &planes,
            optics_group: &optics_group,
        };
        for top in (0..height).step_by(TILE_EDGE) {
            for left in (0..width).step_by(TILE_EDGE) {
                check_cancel(cancellation)?;
                let right = (left + TILE_EDGE).min(width);
                let bottom = (top + TILE_EDGE).min(height);
                let x0 = left.saturating_sub(DEFRINGE_HALO);
                let y0 = top.saturating_sub(DEFRINGE_HALO);
                let region = [
                    x0,
                    y0,
                    (right + DEFRINGE_HALO).min(width) - x0,
                    (bottom + DEFRINGE_HALO).min(height) - y0,
                ];
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("optics and defringe tile"),
                        });
                self.encode_tile(&mut encoder, &context, region);
                self.scatter.encode(
                    &self.device,
                    &self.queue,
                    &mut encoder,
                    &rgb,
                    &baked,
                    region[2],
                    x0,
                    y0,
                    left,
                    top,
                    right - left,
                    bottom - top,
                )?;
                self.queue.submit([encoder.finish()]);
                self.wait(cancellation)?;
            }
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("defringe validation readback"),
            });
        encoder.copy_buffer_to_buffer(&failure, 0, &failure_staging, 0, 4);
        self.queue.submit([encoder.finish()]);
        self.wait(cancellation)?;
        super::reduction::read_failure(&self.device, &failure_staging, cancellation)?;
        Ok((baked, description.with_defringe_baked()))
    }

    /// Record optics output, defringe passes for `region` (`x, y, width,
    /// height`) into `context.rgb`.
    fn encode_tile(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        context: &TileContext<'_>,
        region: [usize; 4],
    ) {
        let [x, y, width, height] = region;
        self.encode_region(encoder, context.optics_group, context.rgb, region);
        let groups: Vec<wgpu::BindGroup> = schedule()
            .iter()
            .map(|pass| self.pass_bindings(context, (width, height), pass))
            .collect();
        let _ = (x, y);
        let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("defringe tile"),
            timestamp_writes: None,
        });
        compute.set_pipeline(&self.defringe.defringe);
        for group in &groups {
            compute.set_bind_group(0, group, &[]);
            compute.dispatch_workgroups((width as u32).div_ceil(8), (height as u32).div_ceil(8), 1);
        }
    }

    /// Record the optics output of `region` as interleaved RGB at the start
    /// of `rgb`.
    fn encode_region(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        optics_group: &wgpu::BindGroup,
        rgb: &wgpu::Buffer,
        region: [usize; 4],
    ) {
        let words = region.map(|value| value as u32);
        let uniform = crate::memory::initialized_buffer(
            &self.device,
            &self.queue,
            &wgpu::util::BufferInitDescriptor {
                label: Some("optics region"),
                contents: bytemuck::cast_slice(&words),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let empty = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("empty optics region group zero"),
            layout: &self.defringe.region.get_bind_group_layout(0),
            entries: &[],
        });
        let output = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("optics region output"),
            layout: &self.defringe.region.get_bind_group_layout(1),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: rgb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("optics output of a region"),
            timestamp_writes: None,
        });
        compute.set_pipeline(&self.defringe.region);
        compute.set_bind_group(0, &empty, &[]);
        compute.set_bind_group(1, &output, &[]);
        compute.set_bind_group(2, optics_group, &[]);
        compute.dispatch_workgroups(
            (region[2] as u32).div_ceil(8),
            (region[3] as u32).div_ceil(8),
            1,
        );
    }

    fn pass_bindings(
        &self,
        context: &TileContext<'_>,
        (width, height): (usize, usize),
        pass: &Pass,
    ) -> wgpu::BindGroup {
        let contract = context.contract;
        let (purple, green) = (contract.purple(), contract.green());
        let (balance, ceilings) = (contract.balance(), contract.ceilings());
        let matrix = contract.to_rec2020();
        let noise = context.noise.map_or([0.0; 2], |n| [n.a, n.b]);
        let mut words = vec![
            width as u32,
            height as u32,
            pass.mode,
            pass.radius,
            pass.planes[0],
            pass.planes[1],
            u32::from(context.noise.is_some()),
            0,
        ];
        let floats = [
            purple.amount,
            green.amount,
            EDGE_FRACTION,
            NOISE_SIGMAS,
            purple.hue[0],
            purple.hue[1],
            green.hue[0],
            green.hue[1],
            NEUTRAL_SATURATION,
            TRUSTED_SATURATION,
            HUE_ROLLOFF,
            0.0,
            balance[0],
            balance[1],
            balance[2],
            0.0,
            ceilings[0],
            ceilings[1],
            ceilings[2],
            0.0,
            noise[0],
            noise[1],
            0.0,
            0.0,
        ];
        words.extend(floats.iter().map(|value| value.to_bits()));
        for row in matrix {
            words.extend(row.iter().map(|value| value.to_bits()));
            words.push(0);
        }
        // Avoid mapped-at-creation helpers: a lost device can make their
        // immediate get_mapped_range panic before an error scope is drained.
        let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("defringe pass parameters"),
            size: (words.len() * 4) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&uniform, 0, bytemuck::cast_slice(&words));
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("defringe pass bindings"),
            layout: &self.defringe.defringe.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: context.rgb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: context.planes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        })
    }

    /// Luma noise profile of the optics output, read back from the sampled
    /// rows only. `None` when the image is too small to profile.
    fn profile_optics_noise(
        &self,
        optics_group: &wgpu::BindGroup,
        rgb: &wgpu::Buffer,
        tile_pixels: usize,
        layout: &super::resources::ResidentLayout,
        detail: &DetailContract,
        cancellation: &CancellationToken,
    ) -> Result<Option<NoiseProfile>, GpuPreviewError> {
        let (width, height) = (layout.width as usize, layout.height as usize);
        // Whole rows, as many as the tile buffer holds.
        let chunk_rows = (tile_pixels / width).max(1);
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("optics noise sample readback"),
            size: (chunk_rows.min(height) * width * 12) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampled_rows: usize = noise_sample_rows(height).iter().map(|r| r.len()).sum();
        let mut samples = Vec::<f32>::new();
        samples
            .try_reserve_exact(sampled_rows * width * 3)
            .map_err(|_| invalid("optics noise sample allocation failed"))?;
        for band in noise_sample_rows(height) {
            for top in band.clone().step_by(chunk_rows) {
                check_cancel(cancellation)?;
                let rows = chunk_rows.min(band.end - top);
                let bytes = (rows * width * 12) as u64;
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("optics noise sample"),
                        });
                self.encode_region(&mut encoder, optics_group, rgb, [0, top, width, rows]);
                encoder.copy_buffer_to_buffer(rgb, 0, &staging, 0, bytes);
                self.queue.submit([encoder.finish()]);
                self.wait(cancellation)?;
                read_floats(&self.device, &staging, bytes, &mut samples)?;
            }
        }
        let sampled = LinearRgbImage::new(
            width,
            sampled_rows,
            width * 3,
            LinearRgbSpace::CameraNative,
            samples,
        )
        .map_err(|error| invalid(&error.to_string()))?;
        estimate_luma_noise_sampled(&sampled, detail.balance(), &|| cancellation.is_cancelled())
            .map_err(|error| match error {
                rohditor_detail::DetailError::Cancelled => GpuPreviewError::Cancelled,
                other => invalid(&other.to_string()),
            })
    }
}

/// Everything the passes of one bake share.
struct TileContext<'a> {
    contract: &'a DefringeContract,
    noise: Option<NoiseProfile>,
    rgb: &'a wgpu::Buffer,
    planes: &'a wgpu::Buffer,
    optics_group: &'a wgpu::BindGroup,
}

/// Append the first `bytes` of the mapped `staging` buffer to `out`.
fn read_floats(
    device: &wgpu::Device,
    staging: &wgpu::Buffer,
    bytes: u64,
    out: &mut Vec<f32>,
) -> Result<(), GpuPreviewError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    staging
        .slice(..bytes)
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
                return Err(invalid("optics noise readback disconnected"));
            }
            Err(mpsc::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    out.extend_from_slice(bytemuck::cast_slice(
        &staging.slice(..bytes).get_mapped_range(),
    ));
    staging.unmap();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_runs_the_mask_then_the_hue_test_then_the_blend() {
        let modes: Vec<u32> = schedule().iter().map(|pass| pass.mode).collect();
        assert_eq!(
            modes,
            [
                mode::LUMA,
                mode::GRADIENT,
                mode::MAX_HORIZONTAL,
                mode::MAX_VERTICAL,
                mode::EDGE,
                mode::MAX_HORIZONTAL,
                mode::MAX_VERTICAL,
                mode::WEIGHT,
                mode::REPLACEMENT,
                mode::BLEND,
            ]
        );
        // The local maximum uses the larger radius, the dilation the smaller.
        let radii: Vec<u32> = schedule()
            .iter()
            .filter(|pass| pass.mode == mode::MAX_HORIZONTAL)
            .map(|pass| pass.radius)
            .collect();
        assert_eq!(radii, [LOCAL_MAX_RADIUS as u32, DILATE_RADIUS as u32]);
    }
}
