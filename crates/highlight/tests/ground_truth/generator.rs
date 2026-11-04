//! Synthetic ground-truth scene generator for the issue #63 harness.
//!
//! Scenes are smooth, analytically-defined channel fields with a known
//! unclipped truth. The pipeline is: field -> per-channel WB-like scale ->
//! `min(sat)` clamp -> RGGB mosaic. Truth kept for scoring is the
//! post-WB-scale, pre-clamp triple array, so per-site log-ratios against
//! method output need no extra white-balance bookkeeping.
//!
//! All scene math uses only `+ - * / sqrt` and `clamp` — no RNG and no
//! transcendental calls — so generation is a pure function of
//! `(SceneSpec, sat)` and bit-reproducible on every run.
//!
//! Dated note (2026-11-04): LLVM may contract `b0 + b1*u + b2*v`
//! into an FMA on aarch64, perturbing scene bits by ~1 ulp (~6e-8 at 1.0).
//! The margin guard (`margin_violations`, threshold 1e-4, ~1600x that drift)
//! keeps every truth far enough from `sat` that the `sample >= level`
//! predicate can never flip across architectures.

// The harness deliberately keeps multiplications and additions separate:
// the margin-guard argument above reasons about plain
// `+ - * / sqrt` bits, so a contracted `mul_add` rewrite is exactly what we
// do not want here, even though it is usually the faster form.
#![allow(clippy::suboptimal_flops)]

use rohditor_image::{BayerPattern, MosaicImage};

/// Minimum distance every generated truth value must keep from `sat`
/// (Guard A).
pub const MARGIN: f32 = 1e-4;

/// Radial profile of a disc-like object.
#[derive(Debug, Clone, Copy)]
pub enum Profile {
    /// `g(t) = (1 - t*t)^2` for `t < 1`, else `0` — smooth falloff.
    Smooth,
    /// `h(t) = 1 - t^4` for `t < 1`, else `0` — flat top with a wide
    /// super-saturation core.
    FlatTop,
}

/// Geometry of one light object added to every channel field.
#[derive(Debug, Clone, Copy)]
pub enum Shape {
    /// Circular disc, `t = sqrt(dx*dx + dy*dy) / r`.
    Disc {
        cx: f32,
        cy: f32,
        r: f32,
        profile: Profile,
    },
    /// Elliptical disc, `t = sqrt((dx/rx)^2 + (dy/ry)^2)`, smooth profile.
    Ellipse { cx: f32, cy: f32, rx: f32, ry: f32 },
    /// Plateau flush to the top-left corner, feathered on the right/bottom
    /// edges: `s(e) = ((edge - coord) / feather).clamp(0, 1)`, feather >= 3.
    CornerRect {
        x_end: f32,
        y_end: f32,
        feather: f32,
    },
}

impl Shape {
    /// Weight in `[0, 1]` of this object at pixel `(x, y)`.
    #[must_use]
    fn weight(&self, x: f32, y: f32) -> f32 {
        match *self {
            Self::Disc { cx, cy, r, profile } => {
                let dx = x - cx;
                let dy = y - cy;
                profile.weight(f32::sqrt(dx * dx + dy * dy) / r)
            }
            Self::Ellipse { cx, cy, rx, ry } => {
                let dx = (x - cx) / rx;
                let dy = (y - cy) / ry;
                Profile::Smooth.weight(f32::sqrt(dx * dx + dy * dy))
            }
            Self::CornerRect {
                x_end,
                y_end,
                feather,
            } => {
                let sx = ((x_end - x) / feather).clamp(0.0, 1.0);
                let sy = ((y_end - y) / feather).clamp(0.0, 1.0);
                sx * sy
            }
        }
    }
}

impl Profile {
    fn weight(&self, t: f32) -> f32 {
        if t >= 1.0 {
            return 0.0;
        }
        match self {
            Self::Smooth => {
                let w = 1.0 - t * t;
                w * w
            }
            Self::FlatTop => 1.0 - t * t * t * t,
        }
    }
}

/// One object: geometry plus per-channel additive peak (pre-WB-scale).
#[derive(Debug, Clone, Copy)]
pub struct Object {
    pub shape: Shape,
    pub peak: [f32; 3],
}

/// Full parametric description of one synthetic scene.
#[derive(Debug, Clone, Copy)]
pub struct SceneSpec {
    pub name: &'static str,
    /// Odd width (all scenes): with an odd row stride the `(x & 1, y & 1)`
    /// Bayer phase shifts at every row start, so width/height/stride
    /// mix-ups and edge handling at odd boundaries cannot hide.
    pub width: usize,
    pub height: usize,
    /// Per-channel background `b0 + b1*u + b2*v` with `u, v` in `[0, 1]`.
    pub bg: [[f32; 3]; 3],
    /// Camera-like per-channel scale applied to the field before clamping.
    pub wb_scale: [f32; 3],
    pub objects: &'static [Object],
    /// Channels expected to hold at least one suspected site at both sat
    /// levels; undeclared channels must hold none (Guard B).
    pub clips: [bool; 3],
}

/// A generated scene: unclipped truth + clamped mosaic fed to the crate.
pub struct Generated {
    /// Post-WB-scale, pre-clamp triples, row-major, `width` stride.
    pub truth: Vec<[f32; 3]>,
    pub mosaic: MosaicImage<f32>,
}

/// Scene-linear field (no WB scale, no clamp) at one pixel.
fn field(spec: &SceneSpec, x: usize, y: usize) -> [f32; 3] {
    let u = x as f32 / (spec.width - 1) as f32;
    let v = y as f32 / (spec.height - 1) as f32;
    let mut out = [
        spec.bg[0][0] + spec.bg[0][1] * u + spec.bg[0][2] * v,
        spec.bg[1][0] + spec.bg[1][1] * u + spec.bg[1][2] * v,
        spec.bg[2][0] + spec.bg[2][1] * u + spec.bg[2][2] * v,
    ];
    let xf = x as f32;
    let yf = y as f32;
    for obj in spec.objects {
        let w = obj.shape.weight(xf, yf);
        if w > 0.0 {
            for (channel, value) in out.iter_mut().enumerate() {
                *value += obj.peak[channel] * w;
            }
        }
    }
    out
}

/// Generate `(spec, sat)`: truth triples + clamped RGGB mosaic.
#[must_use]
pub fn generate(spec: &SceneSpec, sat: f32) -> Generated {
    let width = spec.width;
    let count = width * spec.height;
    let mut truth = vec![[0.0_f32; 3]; count];
    for (index, pixel) in truth.iter_mut().enumerate() {
        let (x, y) = (index % width, index / width);
        let scaled = field(spec, x, y);
        *pixel = [
            scaled[0] * spec.wb_scale[0],
            scaled[1] * spec.wb_scale[1],
            scaled[2] * spec.wb_scale[2],
        ];
    }
    // Mosaic builder copied (not imported — scripts/check.sh's
    // dependency-direction asserts forbid highlight -> demosaic) from
    // crates/demosaic/tests/demosaic_quality.rs. The `min(sat)`
    // clamp is applied per site through the Bayer phase.
    let data = truth
        .iter()
        .enumerate()
        .map(|(index, rgb)| {
            let (x, y) = (index % width, index / width);
            let channel = BayerPattern::Rggb.color_at(x, y).channel_index();
            rgb[channel].min(sat)
        })
        .collect();
    let mosaic = MosaicImage::new(spec.width, spec.height, width, BayerPattern::Rggb, data)
        .expect("harness scenes are valid mosaics");
    Generated { truth, mosaic }
}

/// One truth-site violation against a saturation level:
/// `(x, y, channel, value)`.
pub type Violation = (usize, usize, usize, f32);

/// Guard A (exact-sat): truth landing bit-exactly on `sat` would leave the
/// `sample >= level` predicate to rounding history, so it is checked first,
/// with the offending sites named in the failure message. The real guard is
/// [`margin_violations`]: it keeps every truth value at least [`MARGIN`]
/// away from `sat`, so no codegen drift can flip the predicate.
// The comparison is deliberately exact: landing bit-exactly on `sat` IS the
// defect, so clippy's approximate-float lint does not apply here.
#[allow(clippy::float_cmp)]
#[must_use]
pub fn exact_sat_violations(scene: &Generated, spec: &SceneSpec, sat: f32) -> Vec<Violation> {
    collect_violations(scene, spec, |value| value == sat)
}

/// Guard A (margin): every truth must sit at least [`MARGIN`] away from
/// `sat`, so no FMA/rounding drift (~6e-8) can flip the `>=` predicate
/// across architectures.
#[must_use]
pub fn margin_violations(scene: &Generated, spec: &SceneSpec, sat: f32) -> Vec<Violation> {
    collect_violations(scene, spec, |value| {
        !(value < sat - MARGIN || value > sat + MARGIN)
    })
}

fn collect_violations(
    scene: &Generated,
    spec: &SceneSpec,
    bad: impl Fn(f32) -> bool,
) -> Vec<Violation> {
    let mut out = Vec::new();
    for (index, triple) in scene.truth.iter().enumerate() {
        let (x, y) = (index % spec.width, index / spec.width);
        for (channel, &value) in triple.iter().enumerate() {
            if bad(value) {
                out.push((x, y, channel, value));
            }
        }
    }
    out
}

/// Global maximum of each scaled truth channel — used by the `scene_guards`
/// design rules (declared-clip peaks >= 1.2, undeclared maxima <= 0.90).
#[must_use]
pub fn channel_maxima(scene: &Generated) -> [f32; 3] {
    let mut maxima = [0.0_f32; 3];
    for triple in &scene.truth {
        for (channel, &value) in triple.iter().enumerate() {
            if value > maxima[channel] {
                maxima[channel] = value;
            }
        }
    }
    maxima
}
