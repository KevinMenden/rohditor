//! Radial multiplier LUT for the CPU remap branch (issue #55).
//!
//! ## Why the mapping is radial (structural proof)
//!
//! Every repo-reachable Lensfun inverse kernel has the form
//! `(x, y) -> (x * f(rd), y * f(rd))` where `rd = |input|` and the scalar is
//! either a constant (`None` distortion, `Linear` TCA — `plan.rs` does the
//! multiply itself) or the result of a **1-D Newton solve on `rd`**
//! (`undist_poly3`/`undist_poly5`/`undist_ptlens` in `lensfun::mod_coord::distortion`,
//! `tca_poly3_reverse` per channel via `invert_one_channel` in
//! `lensfun::mod_subpix`). Composed with `plan.rs`'s isotropic pre/post
//! transforms, the full mapping is `source_c = ((input * m_c(r) + center) *
//! norm_unscale)`: the per-channel multiplier depends only on the input
//! radius. Profile data only selects model + coefficients — non-radiality is
//! structurally impossible for the closed [`DistortionModel`] / [`TcaModel`]
//! enums, which are exhaustively matched here, so a future non-radial variant
//! is a compile error, not a runtime surprise. The 0 deg / 45 deg knot check
//! below is a cheap tripwire for non-radial kernel changes, not the proof.
//!
//! ## Guard semantics (cliff detector, not an accuracy bound)
//!
//! Kernel non-convergence per lensfun 0.7.0: Poly3 distortion returns **NaN**
//! (`distortion.rs:30-63`), Poly5 / Ptlens / TCA-Poly3 return the input
//! **unchanged, silently** (`distortion.rs:97-103`, `distortion.rs:140-146`,
//! `mod_subpix.rs:124-155`). The kernels report no convergence flag, so the
//! guard detects cliffs and inconsistencies; coordinate accuracy is governed
//! by the formula-bound parity gate below.
//!
//! Newton's early break (`|F(rd)| < NEWTON_EPS` before the first step) freezes
//! the multiplier at exactly `m = 1` for small radii, then jumps — one
//! legitimate outlier knot per kernel per profile (measured 6e-4 … 3.4e-3).
//! The build reproduces each kernel's step-0 break state at every knot; when
//! the state flips between adjacent knots it locates the break radius by
//! deterministic f64 bisection and **pins it as an extra knot**
//! ([`PinnedSegment`]). Lookup inside a pinned segment selects the knot on the
//! pixel's own side of the break (nearest-knot, never a lerp across the
//! discontinuity), which also mirrors the authority's per-pixel freeze
//! decision exactly. For TCA channels the break is evaluated on the
//! post-distortion geometry radius inside the predicate, so bisection on the
//! input radius maps it back through the distortion scale automatically.
//!
//! An adjacent `|delta m| > 1e-3` **without** an explained kernel-state flip
//! in the same knot segment marks the table invalid; the remap then runs the
//! untouched authority path (per-apply branch decision — a partially
//! LUT-applied frame is structurally impossible).
//!
//! ## Parity bound (formula-bound, stated per profile and size)
//!
//! `B(profile, size) = max(0.15 px, 1.5 * NEWTON_EPS * norm_unscale)` with
//! `norm_unscale = crop * diag_px * real_focal / hypot(36, 24)`. The plateau
//! term is lens- and size-dependent: ~0.005 px at the ultra-wide end, ~0.09 px
//! at the 35 mm full-size fixture, ~0.18 px at 400 mm test scale (~0.5 px
//! class at production scale). There is no universal pixel bound. Comparison
//! basis is LUT vs AUTHORITY (the shipped Newton kernels; the converged f64
//! root is a research oracle — on the Poly3-tele freeze class
//! the authority itself sits ~1.3 px off the converged root and the LUT
//! matches the authority's own solution quality there, not worse).

use lensfun::mod_coord::{undist_poly3, undist_poly5, undist_ptlens};
use lensfun::mod_subpix::tca_poly3_reverse;

use crate::plan::{DistortionModel, OpticsExecution, TcaModel};

/// Uniform knot count (12 KB table, L1-resident; accuracy verified
/// knot-count-independent from 256 to 4096).
const KNOTS: usize = 1_024;

/// Newton epsilon of the lensfun inverse kernels (`distortion.rs:21`).
const NEWTON_EPS: f64 = 0.00001;

/// Relative tolerance for the 0 deg / 45 deg radiality tripwire. It bounds
/// non-radial kernels; on bundled data it can also fire at freeze boundary
/// knots where the sample rays straddle step-0 (m = 1 vs a converged
/// m ~= 1 + 1e-3, grid-dependent). Those invalidations are conservative:
/// the fallback stays bit-exact. Where the check passes, measured margins
/// stay <= 6e-7, far below the 1e-4 limit.
const TRIPWIRE_RELATIVE_TOLERANCE: f32 = 1e-4;

/// Adjacent-knot multiplier jump that still counts as "unexplained" when no
/// kernel-state flip lands in the same knot segment.
const JUMP_LIMIT: f32 = 1e-3;

/// Deterministic bisection budget for locating a pinned break radius.
const BISECTION_STEPS: usize = 30;

/// One radial kernel whose step-0 Newton break can flip across the grid.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BreakKernel {
    Distortion,
    TcaRed,
    TcaBlue,
}

/// A knot segment crossed by a kernel freeze boundary. Pixels inside the
/// segment pick the table knot on their own side of `break_radius` instead of
/// interpolating, so no lerp can straddle the jump.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PinnedSegment {
    /// Uniform segment between `table[index]` and `table[index + 1]`.
    index: usize,
    /// Radius at which the kernel's step-0 break state flips.
    break_radius: f32,
}

/// Per-channel radial multipliers over a uniform grid, validity-guarded at
/// build time. Built sequentially (deterministic f64 knot evaluation, no
/// order-dependent accumulation) once before the row loop.
pub(crate) struct RadialMultipliers {
    /// Radius distance between adjacent knots.
    step: f32,
    /// Uniform `[m_red, m_green, m_blue]` knots from radius 0 to the farthest
    /// mapped corner.
    table: Vec<[f32; 3]>,
    /// Segments with a pinned freeze boundary, ascending by index.
    step_segments: Vec<PinnedSegment>,
    /// False when any guard rule tripped; the remap then keeps the authority
    /// path byte-for-byte.
    pub(crate) valid: bool,
}

impl RadialMultipliers {
    /// Build the table for one resolved optics execution. Returns `None` only when
    /// there is no radial work to accelerate (or the grid is degenerate).
    pub(crate) fn build(execution: &OpticsExecution) -> Option<Self> {
        if execution.distortion().is_none() && execution.tca().is_none() {
            return None;
        }
        let corner_radius = corner_input_radius(execution)?;
        let step = corner_radius / (KNOTS - 1) as f32;
        let direction = std::f32::consts::FRAC_1_SQRT_2;
        // Sequential loop: deterministic f64 knot evaluation per knot, no
        // order-dependent accumulation.
        let mut table = Vec::with_capacity(KNOTS);
        let mut tripwire_ok = true;
        for index in 0..KNOTS {
            let radius = index as f32 * step;
            let multipliers = knot_multipliers(execution, radius * direction, radius * direction);
            // Tripwire: the same radius re-evaluated on the x axis must agree
            // radially. Catches future kernels that break radiality.
            let straight = knot_multipliers(execution, radius, 0.0);
            for channel in 0..3 {
                let margin = (multipliers[channel] - straight[channel]).abs()
                    / multipliers[channel].abs().max(1e-3);
                if margin.is_nan() || margin > TRIPWIRE_RELATIVE_TOLERANCE {
                    tripwire_ok = false;
                }
            }
            table.push(multipliers);
        }
        let (step_segments, guard_ok) = assess(execution, &table, step);
        Some(Self {
            step,
            table,
            step_segments,
            valid: tripwire_ok && guard_ok,
        })
    }

    /// Interpolated `[m_red, m_green, m_blue]` at an input radius. Inside a
    /// pinned segment the lookup is nearest-knot on the pixel's side of the
    /// freeze boundary; elsewhere it is a linear blend of the adjacent knots.
    #[inline]
    pub(crate) fn lookup(&self, radius: f32) -> [f32; 3] {
        debug_assert!(
            radius.is_finite() && radius >= 0.0,
            "call sites pass hypots over finite plan constants"
        );
        let position = radius / self.step;
        let index = (position as usize).min(self.table.len() - 2);
        if !self.step_segments.is_empty() {
            for segment in &self.step_segments {
                if segment.index == index {
                    // Below the break the pixel carries the low knot's kernel
                    // state; at or above it, the high knot's state.
                    return self.table[usize::from(radius >= segment.break_radius) + index];
                }
            }
        }
        // The outermost corner pixel reproduces `corner_input_radius` through
        // the same f32 arithmetic, so `position` can round to just past the
        // last knot while `index` is clamped one segment back. Clamping the
        // blend turns that rounding into selecting the outer knot instead of
        // extrapolating past it. Only the upper bound is reachable (`index`
        // truncates a non-negative position); the bare comparison also keeps
        // a hypothetical NaN propagating to the sampler's finiteness guard
        // instead of silently selecting a knot.
        let blend = position - index as f32;
        let blend = if blend > 1.0 { 1.0 } else { blend };
        let low = self.table[index];
        let high = self.table[index + 1];
        [
            low[0] * (1.0 - blend) + high[0] * blend,
            low[1] * (1.0 - blend) + high[1] * blend,
            low[2] * (1.0 - blend) + high[2] * blend,
        ]
    }
}

/// Radius of the farthest mapped image corner in input (lens) units, matching
/// the branch's own per-pixel narrowing points.
fn corner_input_radius(execution: &OpticsExecution) -> Option<f32> {
    let mut radius = 0.0_f32;
    for (x, y) in [
        (0usize, 0usize),
        (execution.width - 1, 0),
        (0, execution.height - 1),
        (execution.width - 1, execution.height - 1),
    ] {
        let input_x = execution.input_coordinate_x(x) as f32;
        let input_y = execution.input_coordinate_y(y) as f32;
        radius = radius.max(input_x.hypot(input_y));
    }
    (radius.is_finite() && radius > 0.0).then_some(radius)
}

fn distortion_kernel(model: Option<DistortionModel>, x: f32, y: f32) -> (f32, f32) {
    match model {
        None => (x, y),
        Some(DistortionModel::Poly3 { k1 }) => undist_poly3(x, y, k1),
        Some(DistortionModel::Poly5 { k1, k2 }) => undist_poly5(x, y, k1, k2),
        Some(DistortionModel::Ptlens { a, b, c }) => undist_ptlens(x, y, a, b, c),
    }
}

/// Exact per-channel multiplier at one input coordinate, reproduced from the
/// same kernels `plan.rs` runs: `m_red = s * t_red`, `m_green = s`,
/// `m_blue = s * t_blue` (multiply, never overwrite).
pub(crate) fn knot_multipliers(execution: &OpticsExecution, x: f32, y: f32) -> [f32; 3] {
    let (geometry_x, geometry_y) = distortion_kernel(execution.distortion(), x, y);
    let input_squared = x * x + y * y;
    let scale_factor = if input_squared > 0.0 {
        (geometry_x * x + geometry_y * y) / input_squared
    } else {
        1.0
    };
    let mut multipliers = [scale_factor, scale_factor, scale_factor];
    match execution.tca() {
        None => {}
        Some(TcaModel::Linear { kr, kb }) => {
            multipliers[0] = scale_factor * kr;
            multipliers[2] = scale_factor * kb;
        }
        Some(TcaModel::Poly3 { red, blue }) => {
            let (red_x, red_y, blue_x, blue_y) =
                tca_poly3_reverse(geometry_x, geometry_y, red, blue);
            let geometry_squared = geometry_x * geometry_x + geometry_y * geometry_y;
            if geometry_squared > 0.0 {
                multipliers[0] *= (red_x * geometry_x + red_y * geometry_y) / geometry_squared;
                multipliers[2] *= (blue_x * geometry_x + blue_y * geometry_y) / geometry_squared;
            }
        }
    }
    multipliers
}

/// `true` when the kernel breaks at step 0 for this coordinate (the multiplier
/// is then the identity `m = 1`). Replicates each kernel's first-iteration
/// `|F(rd)| < NEWTON_EPS` arithmetic exactly, including the f32 `rd` build and
/// Poly3's `rd / k1` f32 truncation; `rd == 0` is treated as a break (the
/// kernels return the input there too).
fn step0_break_distortion(model: Option<DistortionModel>, x: f32, y: f32) -> bool {
    let rd = ((x * x + y * y) as f64).sqrt();
    if rd == 0.0 {
        return true;
    }
    let residual = match model {
        None => return false,
        Some(DistortionModel::Poly3 { k1 }) => {
            let inv_k1 = (1.0_f32 / k1) as f64;
            let rd_div_k1 = (rd * inv_k1) as f32 as f64;
            rd * rd * rd + rd * inv_k1 - rd_div_k1
        }
        Some(DistortionModel::Poly5 { k1, k2 }) => {
            let rd_squared = rd * rd;
            rd * (1.0 + f64::from(k1) * rd_squared + f64::from(k2) * rd_squared * rd_squared) - rd
        }
        Some(DistortionModel::Ptlens { a, b, c }) => {
            rd * (f64::from(a) * rd * rd * rd + f64::from(b) * rd * rd + f64::from(c) * rd + 1.0)
                - rd
        }
    };
    residual.abs() < NEWTON_EPS
}

/// Same step-0 break predicate for one `tca_poly3_reverse` channel, evaluated
/// at the post-distortion geometry coordinate the TCA kernel actually sees.
fn step0_break_tca(channel: [f32; 3], geometry_x: f32, geometry_y: f32) -> bool {
    let rd = ((geometry_x * geometry_x + geometry_y * geometry_y) as f64).sqrt();
    if rd == 0.0 {
        return true;
    }
    let [v, c, b] = channel;
    let residual = f64::from(b) * rd * rd * rd + f64::from(c) * rd * rd + f64::from(v) * rd - rd;
    residual.abs() < NEWTON_EPS
}

fn break_predicate(execution: &OpticsExecution, kernel: BreakKernel) -> impl Fn(f64) -> bool + '_ {
    let direction = std::f32::consts::FRAC_1_SQRT_2;
    move |radius: f64| {
        let radius = radius as f32;
        let x = radius * direction;
        let y = radius * direction;
        match kernel {
            BreakKernel::Distortion => step0_break_distortion(execution.distortion(), x, y),
            BreakKernel::TcaRed | BreakKernel::TcaBlue => {
                let (geometry_x, geometry_y) = distortion_kernel(execution.distortion(), x, y);
                match execution.tca() {
                    Some(TcaModel::Poly3 { red, blue }) => step0_break_tca(
                        if kernel == BreakKernel::TcaRed {
                            red
                        } else {
                            blue
                        },
                        geometry_x,
                        geometry_y,
                    ),
                    _ => false,
                }
            }
        }
    }
}

/// Locate the radius where `predicate` flips inside `[low, high]` by
/// deterministic f64 bisection (known-flip endpoints, so no monotonicity is
/// assumed). Returns the boundary on the `!low_state` side.
fn bisect_break(predicate: &impl Fn(f64) -> bool, low: f64, high: f64) -> f64 {
    let low_state = predicate(low);
    let mut low = low;
    let mut high = high;
    for _ in 0..BISECTION_STEPS {
        let middle = (low + high) * 0.5;
        if predicate(middle) == low_state {
            low = middle;
        } else {
            high = middle;
        }
    }
    high
}

/// Run the freeze-boundary and cliff guards over a built table: pin kernel
/// state flips as extra knots, invalidate non-finite tables, and invalidate
/// adjacent jumps that no flip explains.
fn assess(
    execution: &OpticsExecution,
    table: &[[f32; 3]],
    step: f32,
) -> (Vec<PinnedSegment>, bool) {
    let mut valid = true;
    for multipliers in table {
        if !multipliers.iter().all(|value| value.is_finite()) {
            valid = false;
        }
    }

    let mut kernels = Vec::new();
    if execution.distortion().is_some() {
        kernels.push(BreakKernel::Distortion);
    }
    if let Some(TcaModel::Poly3 { .. }) = execution.tca() {
        kernels.push(BreakKernel::TcaRed);
        kernels.push(BreakKernel::TcaBlue);
    }

    // Walk every radial kernel's step-0 break state; a flip between adjacent
    // knots pins the boundary and explains the multiplier jump there.
    let mut flips: Vec<(usize, BreakKernel, f32)> = Vec::new();
    for kernel in kernels {
        let predicate = break_predicate(execution, kernel);
        let mut previous = predicate(0.0);
        for index in 1..table.len() {
            let radius = f64::from(index as f32 * step);
            let current = predicate(radius);
            if current != previous {
                let low = f64::from((index - 1) as f32 * step);
                let break_radius = bisect_break(&predicate, low, radius) as f32;
                flips.push((index - 1, kernel, break_radius));
            }
            previous = current;
        }
    }

    // At most one kernel flip per knot segment; two flips would make the
    // side selection at lookup ambiguous, so refuse the table instead.
    flips.sort_by_key(|(index, kernel, _)| (*index, *kernel as u8));
    let mut step_segments: Vec<PinnedSegment> = Vec::new();
    for (index, _kernel, break_radius) in &flips {
        match step_segments.last() {
            Some(last) if last.index == *index => valid = false,
            _ => step_segments.push(PinnedSegment {
                index: *index,
                break_radius: *break_radius,
            }),
        }
    }

    // Cliff detector: an adjacent jump beyond the limit without an attributed
    // kernel-state flip in the same segment invalidates. Green follows only
    // the distortion kernel; red/blue additionally follow their own channel.
    for index in 0..table.len() - 1 {
        let attributed = |channel: usize| {
            flips.iter().any(|(flip_index, kernel, _)| {
                *flip_index == index
                    && match channel {
                        1 => *kernel == BreakKernel::Distortion,
                        0 => matches!(*kernel, BreakKernel::Distortion | BreakKernel::TcaRed),
                        _ => matches!(*kernel, BreakKernel::Distortion | BreakKernel::TcaBlue),
                    }
            })
        };
        for (channel, (low, high)) in table[index].iter().zip(table[index + 1]).enumerate() {
            if (high - low).abs() > JUMP_LIMIT && !attributed(channel) {
                valid = false;
            }
        }
    }
    (step_segments, valid)
}

#[cfg(test)]
mod tests {
    use rayon::ThreadPoolBuilder;
    use rayon::prelude::*;
    use rohditor_image::{LinearRgbImage, LinearRgbSpace};

    use super::*;
    use crate::resample::cubic_sample;
    use crate::{
        CorrectionComponents, LensCorrectionPlan, OpticsQuery, OpticsService, ProfileMatch,
        ProfileRequest,
    };

    /// Per-pixel parity grid from the review (same size the gates were set at).
    const GRID_WIDTH: usize = 1_024;
    const GRID_HEIGHT: usize = 768;

    struct MatrixProfile {
        name: &'static str,
        make: &'static str,
        model: &'static str,
        clean: &'static str,
        lens_make: &'static str,
        lens_model: &'static str,
        focal: f32,
        /// Resolve via the first automatic-match candidate id: the E 10-18 is
        /// the widest Ptlens on the mount but auto-matches as Ambiguous(2).
        explicit: bool,
        /// Poly3-tele freeze class: also gate against the converged oracle.
        freeze_class: bool,
    }

    /// Sony ILCE-6400 body entry with the Tamron fixture lens at 35 mm.
    fn base_profile(name: &'static str) -> MatrixProfile {
        MatrixProfile {
            name,
            make: "Sony",
            model: "ILCE-6400",
            clean: "Alpha 6400",
            lens_make: "Tamron",
            lens_model: "Tamron 17-70mm F/2.8 Di III-A VC RXD",
            focal: 35.0,
            explicit: false,
            freeze_class: false,
        }
    }

    fn matrix() -> Vec<MatrixProfile> {
        vec![
            MatrixProfile {
                name: "Tamron 17-70 @35 (Ptlens + Poly3 TCA, fixture)",
                ..base_profile("unused")
            },
            MatrixProfile {
                name: "Sony E 24 f/1.8 ZA @24 (Ptlens + Linear TCA)",
                lens_make: "Sony",
                lens_model: "E 24mm f/1.8 ZA",
                focal: 24.0,
                ..base_profile("unused")
            },
            MatrixProfile {
                name: "Sony FE 100-400 @100 (Poly3 + Poly3 TCA)",
                lens_make: "Sony",
                lens_model: "FE 100-400mm f/4.5-5.6 GM OSS",
                focal: 100.0,
                ..base_profile("unused")
            },
            MatrixProfile {
                name: "Canon PowerShot G12 @10 (Poly5 + Poly3 TCA)",
                make: "Canon",
                model: "Canon PowerShot G12",
                clean: "PowerShot G12",
                lens_make: "Canon",
                lens_model: "Canon PowerShot G12 & compatibles (Standard)",
                focal: 10.0,
                ..base_profile("unused")
            },
            MatrixProfile {
                name: "Sony E 10-18 @10 via Explicit (widest Ptlens, Ambiguous(2) automatic)",
                lens_make: "Sony",
                lens_model: "E 10-18mm f/4 OSS",
                focal: 10.0,
                explicit: true,
                ..base_profile("unused")
            },
            MatrixProfile {
                name: "Sony FE 100-400 @400 (scaled bound binds, freeze class)",
                lens_make: "Sony",
                lens_model: "FE 100-400mm f/4.5-5.6 GM OSS",
                focal: 400.0,
                freeze_class: true,
                ..base_profile("unused")
            },
            MatrixProfile {
                name: "Tamron 17-70 @70 (long end, fast path under the knot-pinning guard)",
                focal: 70.0,
                ..base_profile("unused")
            },
        ]
    }

    fn query(profile: &MatrixProfile) -> OpticsQuery {
        OpticsQuery {
            camera_make: profile.make.to_owned(),
            camera_model: profile.model.to_owned(),
            camera_clean_make: profile.make.to_owned(),
            camera_clean_model: profile.clean.to_owned(),
            lens_make: Some(profile.lens_make.to_owned()),
            lens_model: Some(profile.lens_model.to_owned()),
            focal_length_mm: Some(profile.focal),
            aperture_f_number: Some(4.0),
            focus_distance_m: Some(10.0),
        }
    }

    fn resolve(
        service: &OpticsService,
        profile: &MatrixProfile,
        width: usize,
        height: usize,
    ) -> LensCorrectionPlan {
        let request = if profile.explicit {
            let ProfileMatch::Ambiguous(candidates) = service.profile_match(&query(profile)) else {
                panic!("{} must auto-match as ambiguous", profile.name);
            };
            ProfileRequest::Explicit {
                profile_id: candidates[0].id.clone(),
            }
        } else {
            ProfileRequest::Automatic
        };
        service
            .resolve_plan(
                &query(profile),
                request,
                CorrectionComponents {
                    distortion: true,
                    vignetting: false,
                    chromatic_aberration: true,
                },
                width,
                height,
            )
            .unwrap_or_else(|error| panic!("{} must resolve: {error:?}", profile.name))
    }

    /// `B(profile, size) = max(0.15 px, 1.5 * NEWTON_EPS * norm_unscale)`.
    fn coordinate_bound(execution: &OpticsExecution) -> f32 {
        let norm_unscale = execution.normalization().1;
        (0.15_f64.max(1.5 * NEWTON_EPS * norm_unscale)) as f32
    }

    /// Per-pixel input radius exactly as the LUT branch computes it.
    fn branch_inputs(execution: &OpticsExecution, x: usize, y: usize) -> (f32, f32) {
        let (norm_scale, norm_unscale) = execution.normalization();
        let (center_x, center_y) = execution.center();
        let scale = f64::from(execution.scale());
        let center_px_x = center_x * norm_unscale;
        let center_px_y = center_y * norm_unscale;
        let mapped_x = center_px_x + (x as f64 - center_px_x) / scale;
        let mapped_y = center_px_y + (y as f64 - center_px_y) / scale;
        (
            (mapped_x * norm_scale - center_x) as f32,
            (mapped_y * norm_scale - center_y) as f32,
        )
    }

    fn lut_source(
        execution: &OpticsExecution,
        input_x: f32,
        input_y: f32,
        multiplier: f32,
    ) -> (f32, f32) {
        let norm_unscale = execution.normalization().1;
        let (center_x, center_y) = execution.center();
        (
            ((f64::from(input_x * multiplier) + center_x) * norm_unscale) as f32,
            ((f64::from(input_y * multiplier) + center_y) * norm_unscale) as f32,
        )
    }

    /// Converged f64 root for the research oracle (NOT the shipped authority):
    /// solves the same polynomials the Newton kernels invert, without their
    /// step-0 early break or truncation.
    fn oracle_root(mut low: f64, high: f64, value: impl Fn(f64) -> f64) -> Option<f64> {
        // value(low) < 0 <= value(high) is expected; expand if needed.
        let mut high = high;
        while value(high) < 0.0 {
            if high > 1.0e6 {
                return None;
            }
            high *= 2.0;
        }
        for _ in 0..200 {
            let middle = (low + high) * 0.5;
            if value(middle) < 0.0 {
                low = middle;
            } else {
                high = middle;
            }
        }
        Some((low + high) * 0.5)
    }

    fn oracle_multipliers(execution: &OpticsExecution, x: f32, y: f32) -> Option<[f32; 3]> {
        let input_radius = (f64::from(x) * f64::from(x) + f64::from(y) * f64::from(y)).sqrt();
        if input_radius == 0.0 {
            return Some([1.0, 1.0, 1.0]);
        }
        let distortion_scale = match execution.distortion() {
            None => 1.0,
            Some(DistortionModel::Poly3 { k1 }) => {
                let k1 = f64::from(k1);
                oracle_root(0.0, input_radius, |ru| {
                    k1 * ru * ru * ru + ru - input_radius
                })? / input_radius
            }
            Some(DistortionModel::Poly5 { k1, k2 }) => {
                let (k1, k2) = (f64::from(k1), f64::from(k2));
                oracle_root(0.0, input_radius, |ru| {
                    ru * (1.0 + k1 * ru * ru + k2 * ru.powi(4)) - input_radius
                })? / input_radius
            }
            Some(DistortionModel::Ptlens { a, b, c }) => {
                let (a, b, c) = (f64::from(a), f64::from(b), f64::from(c));
                oracle_root(0.0, input_radius, |ru| {
                    ru * (a * ru.powi(3) + b * ru * ru + c * ru + 1.0) - input_radius
                })? / input_radius
            }
        };
        let geometry_radius = distortion_scale * input_radius;
        let tca = match execution.tca() {
            None => [1.0, 1.0, 1.0],
            Some(TcaModel::Linear { kr, kb }) => [f64::from(kr), 1.0, f64::from(kb)],
            Some(TcaModel::Poly3 { red, blue }) => {
                let channel = |coeffs: [f32; 3]| -> Option<f64> {
                    let [v, c, b] = [
                        f64::from(coeffs[0]),
                        f64::from(coeffs[1]),
                        f64::from(coeffs[2]),
                    ];
                    Some(
                        oracle_root(0.0, geometry_radius, |ru| {
                            b * ru.powi(3) + c * ru * ru + v * ru - geometry_radius
                        })? / geometry_radius,
                    )
                };
                [channel(red)?, 1.0, channel(blue)?]
            }
        };
        Some([
            (distortion_scale * tca[0]) as f32,
            distortion_scale as f32,
            (distortion_scale * tca[2]) as f32,
        ])
    }

    #[test]
    fn radial_lut_parity() {
        let service = OpticsService::load_bundled().expect("bundled Lensfun data");
        for profile in matrix() {
            let plan = resolve(&service, &profile, GRID_WIDTH, GRID_HEIGHT);
            let execution = plan.execution();
            let table = RadialMultipliers::build(&execution).expect("radial table");
            assert!(table.valid, "{} must take the fast path", profile.name);
            let bound = coordinate_bound(&execution);

            // Full per-pixel grid: LUT coordinates vs the authority.
            let max_deviation = (0..GRID_HEIGHT)
                .into_par_iter()
                .map(|y| {
                    let mut row_max = 0.0_f32;
                    for x in 0..GRID_WIDTH {
                        let (input_x, input_y) = branch_inputs(&execution, x, y);
                        let multipliers = table.lookup(input_x.hypot(input_y));
                        let authority = execution.map_coordinates(x, y).expect("authority pixel");
                        for channel in 0..3 {
                            let (source_x, source_y) =
                                lut_source(&execution, input_x, input_y, multipliers[channel]);
                            row_max = row_max
                                .max((source_x - authority[channel].0).abs())
                                .max((source_y - authority[channel].1).abs());
                        }
                    }
                    row_max
                })
                .reduce(|| 0.0_f32, f32::max);
            println!(
                "{}: max |delta coord| {max_deviation:.4} px vs bound {bound:.4} px",
                profile.name
            );
            assert!(
                max_deviation <= bound,
                "{}: coordinate deviation {max_deviation:.4} px over formula bound B = max(0.15, 1.5*eps*norm_unscale) = {bound:.4} px",
                profile.name
            );

            if profile.freeze_class {
                // Oracle clause: the LUT must not be worse than the authority
                // against the converged root, beyond the eps plateau slack.
                let slack = (1.5 * NEWTON_EPS * execution.normalization().1) as f32;
                let (authority_vs_oracle, lut_vs_oracle) = (0..GRID_HEIGHT)
                    .into_par_iter()
                    .map(|y| {
                        let (mut authority_row, mut lut_row) = (0.0_f32, 0.0_f32);
                        for x in 0..GRID_WIDTH {
                            let (input_x, input_y) = branch_inputs(&execution, x, y);
                            let Some(oracle) = oracle_multipliers(&execution, input_x, input_y)
                            else {
                                continue;
                            };
                            let multipliers = table.lookup(input_x.hypot(input_y));
                            let authority =
                                execution.map_coordinates(x, y).expect("authority pixel");
                            for channel in 0..3 {
                                let (oracle_x, oracle_y) =
                                    lut_source(&execution, input_x, input_y, oracle[channel]);
                                let (lut_x, lut_y) =
                                    lut_source(&execution, input_x, input_y, multipliers[channel]);
                                authority_row = authority_row
                                    .max((authority[channel].0 - oracle_x).abs())
                                    .max((authority[channel].1 - oracle_y).abs());
                                lut_row = lut_row
                                    .max((lut_x - oracle_x).abs())
                                    .max((lut_y - oracle_y).abs());
                            }
                        }
                        (authority_row, lut_row)
                    })
                    .reduce(
                        || (0.0_f32, 0.0_f32),
                        |(authority_a, lut_a), (authority_b, lut_b)| {
                            (authority_a.max(authority_b), lut_a.max(lut_b))
                        },
                    );
                println!(
                    "{}: authority vs oracle {authority_vs_oracle:.4} px, LUT vs oracle {lut_vs_oracle:.4} px, slack {slack:.4} px",
                    profile.name
                );
                assert!(
                    lut_vs_oracle <= authority_vs_oracle + slack,
                    "{}: LUT is worse than the authority against the converged oracle",
                    profile.name
                );
            }

            // Freeze-boundary knots are present and the pinned segment uses
            // side selection (bit-exact knot values, never a blend).
            let (step, table_values, segments) = (table.step, &table.table, &table.step_segments);
            assert!(
                !segments.is_empty(),
                "{}: expected pinned freeze boundaries",
                profile.name
            );
            for segment in segments {
                let low = segment.index as f32 * step;
                let high = (segment.index + 1) as f32 * step;
                assert!(
                    segment.break_radius > low && segment.break_radius < high,
                    "{}: break radius inside its segment",
                    profile.name
                );
                let below = (low + segment.break_radius) * 0.5;
                let above = (segment.break_radius + high) * 0.5;
                assert_eq!(
                    table.lookup(below),
                    table_values[segment.index],
                    "{}: below the break must be the low knot, not a blend",
                    profile.name
                );
                assert_eq!(
                    table.lookup(above),
                    table_values[segment.index + 1],
                    "{}: above the break must be the high knot, not a blend",
                    profile.name
                );
            }
        }
    }

    #[test]
    fn radial_lut_freeze_knot_matches_newton_epsilon() {
        // Poly3 distortion freezes while |F(rd)| = rd^3 < NEWTON_EPS, so the
        // pinned distortion knot must sit at cbrt(NEWTON_EPS).
        let service = OpticsService::load_bundled().expect("bundled Lensfun data");
        let profile = matrix()
            .into_iter()
            .find(|profile| profile.freeze_class)
            .expect("FE 100-400 @400 entry");
        let plan = resolve(&service, &profile, GRID_WIDTH, GRID_HEIGHT);
        let execution = plan.execution();
        let table = RadialMultipliers::build(&execution).expect("radial table");
        let expected = NEWTON_EPS.cbrt() as f32;
        assert!(
            table
                .step_segments
                .iter()
                .any(|segment| (segment.break_radius - expected).abs() <= table.step),
            "FE 100-400 @400 must pin the Poly3 freeze boundary at cbrt(NEWTON_EPS) \
                 ({expected:.6}), step {}",
            table.step
        );
    }

    #[test]
    fn radial_lut_tripwire() {
        let service = OpticsService::load_bundled().expect("bundled Lensfun data");
        let direction = std::f32::consts::FRAC_1_SQRT_2;
        for profile in matrix() {
            let plan = resolve(&service, &profile, GRID_WIDTH, GRID_HEIGHT);
            let execution = plan.execution();
            let table = RadialMultipliers::build(&execution).expect("radial table");
            assert!(table.valid, "{}: guard tripped", profile.name);
            let mut worst_margin = 0.0_f32;
            for index in 0..table.table.len() {
                let radius = index as f32 * table.step;
                let angled = knot_multipliers(&execution, radius * direction, radius * direction);
                let straight = knot_multipliers(&execution, radius, 0.0);
                for channel in 0..3 {
                    let margin = (angled[channel] - straight[channel]).abs()
                        / angled[channel].abs().max(1e-3);
                    worst_margin = worst_margin.max(margin);
                }
            }
            println!(
                "{}: 0deg/45deg tripwire margin {worst_margin:.3e} (limit {TRIPWIRE_RELATIVE_TOLERANCE:.0e})",
                profile.name
            );
            assert!(
                worst_margin <= TRIPWIRE_RELATIVE_TOLERANCE,
                "{}: multi-angle radiality tripwire violated",
                profile.name
            );
        }
    }

    /// Build a real table, mutate one knot, and re-run the guard.
    fn crafted(execution: &OpticsExecution, mutate: impl Fn(&mut [[f32; 3]])) -> RadialMultipliers {
        let base = RadialMultipliers::build(execution).expect("base table");
        let mut table = base.table.clone();
        mutate(&mut table);
        let (step_segments, valid) = assess(execution, &table, base.step);
        RadialMultipliers {
            step: base.step,
            table,
            step_segments,
            valid,
        }
    }

    /// Authority pixels computed straight from `map_coordinates` +
    /// `resample::cubic_sample` — the untouched pre-#55 code path.
    fn authority_reference(execution: &OpticsExecution, image: &LinearRgbImage<f32>) -> Vec<f32> {
        let row_elements = execution.width * 3;
        let mut reference = vec![0.0_f32; row_elements * execution.height];
        reference
            .par_chunks_mut(row_elements)
            .enumerate()
            .for_each(|(y, row)| {
                for x in 0..execution.width {
                    let coordinates = execution.map_coordinates(x, y).expect("authority pixel");
                    for channel in 0..3 {
                        row[x * 3 + channel] = cubic_sample(
                            image.data(),
                            image.row_stride(),
                            execution.width,
                            execution.height,
                            channel,
                            coordinates[channel].0,
                            coordinates[channel].1,
                        )
                        .expect("authority sample");
                    }
                }
            });
        reference
    }

    struct NeverCancelled;
    impl crate::Cancellation for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    #[test]
    fn radial_lut_fallback() {
        let service = OpticsService::load_bundled().expect("bundled Lensfun data");
        for profile in [
            base_profile("Tamron 17-70 @35 fallback"),
            MatrixProfile {
                name: "FE 100-400 @400 fallback",
                lens_make: "Sony",
                lens_model: "FE 100-400mm f/4.5-5.6 GM OSS",
                focal: 400.0,
                ..base_profile("unused")
            },
        ] {
            let plan = resolve(&service, &profile, GRID_WIDTH, GRID_HEIGHT);
            let execution = plan.execution();
            let pixels = (0..GRID_WIDTH * GRID_HEIGHT * 3)
                .map(|index| ((index % 511) as f32 - 255.0) / 383.0)
                .collect::<Vec<_>>();
            let image = LinearRgbImage::new(
                GRID_WIDTH,
                GRID_HEIGHT,
                GRID_WIDTH * 3,
                LinearRgbSpace::CameraNative,
                pixels,
            )
            .expect("test image");
            let reference = authority_reference(&execution, &image);

            // Non-finite knot component.
            let nonfinite = crafted(&execution, |table| {
                table[600][1] = f32::NAN;
            });
            assert!(!nonfinite.valid, "a NaN knot must invalidate the table");
            let result = execution
                .apply_cancellable_forced(image.clone(), &NeverCancelled, &nonfinite)
                .expect("fallback apply");
            assert_eq!(
                result.image.data(),
                reference.as_slice(),
                "{}: crafted non-finite table must reproduce the authority path bit-for-bit",
                profile.name
            );

            // Unexplained adjacent jump far from every freeze boundary.
            let jumped = crafted(&execution, |table| {
                table[table.len() - 3][2] += 0.5;
            });
            assert!(
                !jumped.valid,
                "{}: an unexplained jump must invalidate the table",
                profile.name
            );
            let result = execution
                .apply_cancellable_forced(image.clone(), &NeverCancelled, &jumped)
                .expect("fallback apply");
            assert_eq!(
                result.image.data(),
                reference.as_slice(),
                "{}: crafted jump table must reproduce the authority path bit-for-bit",
                profile.name
            );

            // The fast path must actually differ from the reference somewhere,
            // or the bit-identity checks above would pass vacuously.
            let fast = execution
                .apply_cancellable(image, &NeverCancelled)
                .expect("fast-path apply");
            assert!(
                fast.image.data() != reference.as_slice(),
                "{}: fast path must not be a no-op on this fixture",
                profile.name
            );
        }
    }

    #[test]
    fn radial_lut_determinism() {
        let service = OpticsService::load_bundled().expect("bundled Lensfun data");
        let profile = base_profile("determinism");
        let plan = resolve(&service, &profile, 640, 480);
        let execution = plan.execution();
        let pixels = (0..640 * 480 * 3)
            .map(|index| 0.05 + (index % 977) as f32 * 0.001)
            .collect::<Vec<_>>();
        let image = LinearRgbImage::new(640, 480, 640 * 3, LinearRgbSpace::CameraNative, pixels)
            .expect("test image");

        let first = execution
            .apply_cancellable(image.clone(), &NeverCancelled)
            .expect("first apply");
        let second = execution
            .apply_cancellable(image.clone(), &NeverCancelled)
            .expect("second apply");
        assert_eq!(first.image.data(), second.image.data());

        let one = ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("single-thread pool");
        let many = ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("multi-thread pool");
        let single = one
            .install(|| execution.apply_cancellable(image.clone(), &NeverCancelled))
            .expect("single-thread apply");
        let multiple = many
            .install(|| execution.apply_cancellable(image, &NeverCancelled))
            .expect("multi-thread apply");
        assert_eq!(single.image.data(), multiple.image.data());

        // The sequential table build is order-independent bit-for-bit.
        let a = RadialMultipliers::build(&execution).expect("table a");
        let b = RadialMultipliers::build(&execution).expect("table b");
        assert_eq!(a.step, b.step);
        assert_eq!(a.step_segments, b.step_segments);
        for (knot_a, knot_b) in a.table.iter().zip(&b.table) {
            for (value_a, value_b) in knot_a.iter().zip(knot_b) {
                assert_eq!(value_a.to_bits(), value_b.to_bits());
            }
        }
    }

    #[test]
    fn radial_lut_output_gate() {
        // Unit-amplitude sawtooth fixture: LUT vs the fallback (authority)
        // output must stay inside max(0.1, 1.2 * B) worst case and 1e-4 at
        // the 99.9 percentile; an absolute 0.1 px bound would fail at tele
        // deviations, so the bound scales with the lens.
        let service = OpticsService::load_bundled().expect("bundled Lensfun data");
        for profile in [
            base_profile("Tamron @35 sawtooth"),
            MatrixProfile {
                name: "FE 100-400 @400 sawtooth",
                lens_make: "Sony",
                lens_model: "FE 100-400mm f/4.5-5.6 GM OSS",
                focal: 400.0,
                ..base_profile("unused")
            },
        ] {
            let plan = resolve(&service, &profile, GRID_WIDTH, GRID_HEIGHT);
            let execution = plan.execution();
            let sawtooth = (0..GRID_HEIGHT)
                .flat_map(|_| {
                    (0..GRID_WIDTH).flat_map(|x| {
                        let value = (x % 64) as f32 / 63.0;
                        [value; 3]
                    })
                })
                .collect::<Vec<_>>();
            let image = LinearRgbImage::new(
                GRID_WIDTH,
                GRID_HEIGHT,
                GRID_WIDTH * 3,
                LinearRgbSpace::CameraNative,
                sawtooth,
            )
            .expect("sawtooth image");
            let fallback = crafted(&execution, |table| {
                table[table.len() - 3][2] += 0.5;
            });
            assert!(!fallback.valid);
            let authority = execution
                .apply_cancellable_forced(image.clone(), &NeverCancelled, &fallback)
                .expect("authority apply");
            let lut = execution
                .apply_cancellable(image, &NeverCancelled)
                .expect("LUT apply");
            let bound = coordinate_bound(&execution);
            let output_bound = 0.1_f32.max(1.2 * bound);
            let mut deltas = authority
                .image
                .data()
                .iter()
                .zip(lut.image.data())
                .map(|(expected, actual)| (expected - actual).abs())
                .collect::<Vec<_>>();
            let max_delta = deltas.iter().copied().fold(0.0_f32, f32::max);
            deltas.sort_by(|a, b| a.total_cmp(b));
            let percentile = deltas[(deltas.len() as f64 * 0.999) as usize];
            println!(
                "{}: sawtooth max |delta linear| {max_delta:.4} vs bound {output_bound:.3}, 99.9p {percentile:.6}",
                profile.name
            );
            assert!(
                max_delta <= output_bound,
                "{}: output delta {max_delta:.4} over gate max(0.1, 1.2*B) = {output_bound:.3}",
                profile.name
            );
            assert!(
                percentile <= 1e-4,
                "{}: 99.9-percentile output delta {percentile:.6} over 1e-4",
                profile.name
            );
        }
    }
}
