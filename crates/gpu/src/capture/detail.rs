//! GPU passes of the source-resolution detail stage that run before capture
//! sharpening: false-colour suppression, chroma and luminance noise
//! reduction. `detail.wgsl` documents the plane layout of each pass.
use super::*;
use rohditor_detail::{
    CHROMA_LEVELS, CHROMA_RANGE_SIGMAS, LUMA_FLOOR, LUMA_LEVELS, LUMA_MAX_GAIN, LUMA_SIGNAL_FLOOR,
    NoiseProfile, chroma_levels, luma_levels,
};

/// Pass codes of `detail.wgsl`.
mod mode {
    pub const FALSE_COLOR_SPLIT: u32 = 0;
    pub const FALSE_COLOR_MEDIAN: u32 = 1;
    pub const FALSE_COLOR_WRITE: u32 = 2;
    pub const CHROMA_SPLIT: u32 = 3;
    pub const CHROMA_LEVEL: u32 = 4;
    pub const CHROMA_WRITE: u32 = 5;
    pub const LUMA_INIT: u32 = 6;
    pub const LUMA_SMOOTH_HORIZONTAL: u32 = 7;
    pub const LUMA_SMOOTH_VERTICAL: u32 = 8;
    pub const LUMA_ENERGY_HORIZONTAL: u32 = 9;
    pub const LUMA_ENERGY_VERTICAL: u32 = 10;
    pub const LUMA_SHRINK: u32 = 11;
    pub const LUMA_WRITE: u32 = 12;
}

/// One dispatch: its mode and the few values that differ between passes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct Pass {
    pub mode: u32,
    pub step: u32,
    /// Plane indices; their meaning is per mode (see `detail.wgsl`).
    pub planes: [u32; 4],
    pub scale: f32,
    pub noise_gain_squared: f32,
}

impl Pass {
    fn new(mode: u32) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    fn with_planes(mut self, planes: [u32; 4]) -> Self {
        self.planes = planes;
        self
    }

    fn with_level(mut self, level: u32, step: usize, scale: f32) -> Self {
        self.planes[0] = level % 2;
        self.step = step as u32;
        self.scale = scale;
        self
    }
}

/// The passes of the detail stage before capture sharpening, in order.
/// `noise` is `None` when the image was too small to profile, which skips
/// noise reduction as on the CPU.
pub(super) fn schedule(contract: &DetailContract, noise: Option<&NoiseProfiles>) -> Vec<Pass> {
    let mut passes = Vec::new();
    let steps = u32::from(contract.false_color().steps);
    if steps > 0 {
        // Two colour-difference pairs ping-pong through the medians.
        passes.push(Pass::new(mode::FALSE_COLOR_SPLIT).with_planes([0, 1, 0, 0]));
        for step in 0..steps {
            let (from, to) = (2 * (step % 2), 2 * ((step + 1) % 2));
            passes.push(Pass::new(mode::FALSE_COLOR_MEDIAN).with_planes([
                from,
                from + 1,
                to,
                to + 1,
            ]));
        }
        let last = 2 * (steps % 2);
        passes.push(Pass::new(mode::FALSE_COLOR_WRITE).with_planes([last, last + 1, 0, 0]));
    }
    let settings = contract.noise_reduction();
    if noise.is_none() {
        return passes;
    }
    if settings.chroma_active() {
        passes.push(Pass::new(mode::CHROMA_SPLIT));
        for (level, l) in chroma_levels(settings.chroma).into_iter().enumerate() {
            passes.push(Pass::new(mode::CHROMA_LEVEL).with_level(
                level as u32,
                l.step,
                l.threshold_scale,
            ));
        }
        passes.push(Pass::new(mode::CHROMA_WRITE).with_planes([CHROMA_LEVELS as u32 % 2, 0, 0, 0]));
    }
    if settings.luminance_active() {
        passes.push(Pass::new(mode::LUMA_INIT));
        let levels = luma_levels(settings.luminance, settings.luminance_detail);
        for (level, l) in levels.into_iter().enumerate() {
            let level = level as u32;
            let at = |mode| Pass::new(mode).with_level(level, l.step, l.scale);
            passes.push(at(mode::LUMA_SMOOTH_HORIZONTAL));
            passes.push(at(mode::LUMA_SMOOTH_VERTICAL));
            passes.push(at(mode::LUMA_ENERGY_HORIZONTAL));
            passes.push(at(mode::LUMA_ENERGY_VERTICAL));
            passes.push(Pass {
                noise_gain_squared: l.noise_gain_squared,
                ..at(mode::LUMA_SHRINK)
            });
        }
        passes.push(Pass::new(mode::LUMA_WRITE).with_planes([LUMA_LEVELS as u32 % 2, 0, 0, 0]));
    }
    passes
}

pub(super) struct DetailPasses {
    pipeline: wgpu::ComputePipeline,
}

impl DetailPasses {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("detail shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("detail.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("detail passes"),
            layout: None,
            module: &shader,
            entry_point: Some("detail"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline }
    }

    /// Record the detail passes for a `width` x `height` tile whose RGB is in
    /// `resources.rgb`. Does nothing when no detail step before capture
    /// sharpening is active.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        resources: &Resources,
        contract: &DetailContract,
        noise: Option<&NoiseProfiles>,
        (width, height): (usize, usize),
    ) {
        let passes = schedule(contract, noise);
        if passes.is_empty() {
            return;
        }
        let bindings: Vec<wgpu::BindGroup> = passes
            .iter()
            .map(|pass| {
                self.bindings(
                    device,
                    queue,
                    resources,
                    contract,
                    noise,
                    (width, height),
                    pass,
                )
            })
            .collect();
        let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("detail tile"),
            timestamp_writes: None,
        });
        compute.set_pipeline(&self.pipeline);
        for group in &bindings {
            compute.set_bind_group(0, group, &[]);
            compute.dispatch_workgroups((width as u32).div_ceil(8), (height as u32).div_ceil(8), 1);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn bindings(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        resources: &Resources,
        contract: &DetailContract,
        noise: Option<&NoiseProfiles>,
        (width, height): (usize, usize),
        pass: &Pass,
    ) -> wgpu::BindGroup {
        let profile = |p: Option<&NoiseProfile>| p.map_or([0.0; 4], |p| [p.a, p.b, 0.0, 0.0]);
        let (balance, ceilings) = (contract.balance(), contract.ceilings());
        let floats = [
            [
                pass.scale,
                pass.noise_gain_squared,
                CHROMA_RANGE_SIGMAS,
                LUMA_SIGNAL_FLOOR,
            ],
            [LUMA_FLOOR, LUMA_MAX_GAIN, 0.0, 0.0],
            [balance[0], balance[1], balance[2], 0.0],
            [ceilings[0], ceilings[1], ceilings[2], 0.0],
            profile(noise.map(|n| &n.luma)),
            profile(noise.map(|n| &n.chroma[0])),
            profile(noise.map(|n| &n.chroma[1])),
        ];
        let mut words = vec![
            width as u32,
            height as u32,
            pass.mode,
            pass.step,
            pass.planes[0],
            pass.planes[1],
            pass.planes[2],
            pass.planes[3],
        ];
        words.extend(floats.iter().flatten().map(|v| v.to_bits()));
        // Avoid mapped-at-creation helpers: a lost device can make their
        // immediate get_mapped_range panic before an error scope is drained.
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("detail pass parameters"),
            size: (words.len() * 4) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform, 0, bytemuck::cast_slice(&words));
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("detail pass bindings"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: resources.rgb.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: resources.planes.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        })
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::*;
    use rohditor_demosaic::WhiteBalanceGains;
    use rohditor_edit::EditRecipe;

    fn contract(configure: impl FnOnce(&mut EditRecipe)) -> DetailContract {
        let mut recipe = EditRecipe::default();
        configure(&mut recipe);
        let one = WhiteBalanceGains::identity();
        DetailContract::new(&recipe, one, one).expect("contract")
    }

    #[test]
    fn inactive_detail_has_no_passes() {
        assert!(schedule(&contract(|_| {}), None).is_empty());
    }

    #[test]
    fn false_color_ping_pongs_between_two_pairs() {
        let passes = schedule(&contract(|r| r.false_color.steps = 3), None);
        let modes: Vec<u32> = passes.iter().map(|p| p.mode).collect();
        assert_eq!(
            modes,
            [
                mode::FALSE_COLOR_SPLIT,
                mode::FALSE_COLOR_MEDIAN,
                mode::FALSE_COLOR_MEDIAN,
                mode::FALSE_COLOR_MEDIAN,
                mode::FALSE_COLOR_WRITE,
            ]
        );
        // Medians alternate pairs; an odd step count ends in the second.
        assert_eq!(passes[1].planes, [0, 1, 2, 3]);
        assert_eq!(passes[2].planes, [2, 3, 0, 1]);
        assert_eq!(passes[3].planes, [0, 1, 2, 3]);
        assert_eq!(passes[4].planes[..2], [2, 3]);
    }

    #[test]
    fn noise_reduction_needs_a_profile() {
        let contract = contract(|r| {
            r.noise_reduction.chroma = 0.5;
            r.noise_reduction.luminance = 0.5;
        });
        assert!(schedule(&contract, None).is_empty());
        let flat = NoiseProfile {
            a: 0.0,
            b: 1.0e-4,
            bins: 0,
        };
        let noise = NoiseProfiles {
            luma: flat,
            chroma: [flat; 2],
        };
        let passes = schedule(&contract, Some(&noise));
        // Split + levels + write for chroma; init + 5 per level + write for luma.
        assert_eq!(passes.len(), (2 + CHROMA_LEVELS) + (2 + 5 * LUMA_LEVELS));
    }
}
