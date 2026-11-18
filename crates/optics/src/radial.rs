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
//! A pinned break whose *own radius* is bit-ambiguous also refuses the
//! table. Poly3 truncates `rd / k1` to f32 inside its step-0 residual
//! (`distortion.rs:42`), so the per-pixel break decision carries truncation
//! noise of `ulp(rd / k1)` — around the break the bisection (which saw one
//! noise realization on the 45 deg ray) can disagree with the per-pixel
//! predicate inside a radius band. Pixels in that band land on the wrong
//! side of the jump, and the resulting coordinate error scales with
//! `norm_unscale` (measured 7.4414 px for the FE 100-400 @400 entry at the
//! 6048x4024 production grid, far over its `B` of 1.5351 px).
//! [`freeze_ambiguity_band`] bounds that band from kernel constants only
//! (no per-image sampling) and the build refuses any table whose band
//! exceeds [`AMBIGUOUS_FREEZE_BAND_ULPS`] ulps of the break radius. Exact
//! f64-arithmetic kernels (Ptlens, Poly5, TCA-Poly3) band at pure input
//! rounding and stay valid; every bundled Poly3-distortion freeze boundary
//! measured so far exceeds the limit and falls back bit-exact.
//!
//! ## Parity bound (formula-bound, stated per profile and size)
//!
//! `B(profile, size) = max(0.15 px, 1.5 * NEWTON_EPS * norm_unscale)` with
//! `norm_unscale = crop * diag_px * real_focal / hypot(36, 24)`. The plateau
//! term is lens- and size-dependent: ~0.005 px at the ultra-wide end, ~0.09 px
//! at the 35 mm full-size fixture, ~0.18 px at the 400 mm gate size. `B`
//! scales with the image diagonal: the 400 mm entry measures
//! `B = 1.5351 px` on the 6048x4024 production grid.
//! There is no universal pixel bound. Comparison basis is LUT vs AUTHORITY
//! (the shipped Newton kernels; the converged f64 root is a research
//! oracle). On the Poly3-tele freeze class the authority itself sits ~1.3 px
//! off the converged root — and that class is exactly what the
//! freeze-ambiguity guard now refuses, so any table the guard accepts is
//! gated against an authority that is itself close to the root.

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

/// Upper bound, in ulps of the kernel radius, on how far the same step-0
/// predicate at the same nominal radius can disagree between the bisection
/// ray and a pixel: each path builds `(x, y)` and `x * x + y * y` through
/// its own f32 roundings (a few ulps combined, doubled for safety).
const FREEZE_RADIUS_NOISE_ULPS: f32 = 8.0;

/// A pinned freeze boundary whose per-pixel decision band (see
/// [`freeze_ambiguity_band`]) exceeds this many ulps of the break radius is
/// bit-ambiguous: no f32 break radius reproduces the per-pixel step-0
/// decisions inside the band, so the whole table is refused rather than
/// risking frame-inconsistent freeze sides. Exact-f64 kernels band at
/// [`FREEZE_RADIUS_NOISE_ULPS`]; measured bundled Poly3 boundaries band at
/// ~100–3,000 ulps (FE 100-400 @400: ~100; @100: ~2,900).
const AMBIGUOUS_FREEZE_BAND_ULPS: f32 = 16.0;

/// One radial kernel whose step-0 Newton break can flip across the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Why a table build was refused. Every reason routes the whole apply to the
/// untouched authority path; the remap never mixes table and authority
/// inside one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableGuard {
    /// A knot multiplier is non-finite (kernel NaN returns).
    NonFinite,
    /// The 0 deg / 45 deg radiality tripwire fired at some knot.
    Tripwire,
    /// Two kernel freeze flips landed in one knot segment, so the side
    /// selection at lookup would be ambiguous.
    BreakCollision,
    /// A pinned break radius is bit-ambiguous beyond
    /// [`AMBIGUOUS_FREEZE_BAND_ULPS`] ulps (see [`freeze_ambiguity_band`]).
    FreezeAmbiguity,
    /// An adjacent multiplier jump no kernel-state flip explains.
    UnexplainedJump,
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
    /// `Some` when a guard rule refused the table; the remap then keeps the
    /// authority path byte-for-byte. `None` means the LUT may be used.
    pub(crate) guard: Option<TableGuard>,
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
        let (step_segments, mut guard) = assess(execution, &table, step);
        if guard.is_none() && !tripwire_ok {
            guard = Some(TableGuard::Tripwire);
        }
        Some(Self {
            step,
            table,
            step_segments,
            guard,
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
/// state flips as extra knots, refuse non-finite tables, ambiguous freeze
/// boundaries, and adjacent jumps that no flip explains. The first violated
/// rule names the refusal.
fn assess(
    execution: &OpticsExecution,
    table: &[[f32; 3]],
    step: f32,
) -> (Vec<PinnedSegment>, Option<TableGuard>) {
    let mut guard: Option<TableGuard> = None;
    for multipliers in table {
        if !multipliers.iter().all(|value| value.is_finite()) {
            guard.get_or_insert(TableGuard::NonFinite);
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
            Some(last) if last.index == *index => {
                guard.get_or_insert(TableGuard::BreakCollision);
            }
            _ => step_segments.push(PinnedSegment {
                index: *index,
                break_radius: *break_radius,
            }),
        }
    }

    // Freeze-ambiguity guard: a break whose per-pixel decision band is wide
    // relative to its own f32 representation cannot be honoured by the
    // side-selection lookup, whatever break radius the bisection reports.
    if guard.is_none() {
        for (_index, kernel, break_radius) in &flips {
            let band = freeze_ambiguity_band(execution, *kernel, *break_radius);
            let limit = AMBIGUOUS_FREEZE_BAND_ULPS * ulp32(*break_radius);
            if band.is_nan() || band > limit {
                guard = Some(TableGuard::FreezeAmbiguity);
                break;
            }
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
                guard.get_or_insert(TableGuard::UnexplainedJump);
            }
        }
    }
    (step_segments, guard)
}

/// The ulp of a non-negative f32, taken from the wider side of a binade
/// boundary so the bound stays conservative at powers of two.
fn ulp32(value: f32) -> f32 {
    (value - value.next_down()).abs()
}

/// Conservative band of per-pixel input radii around a pinned break radius
/// inside which the kernel's own step-0 decision can differ from the
/// decision the bisection saw on the 45 deg ray. Built from kernel
/// constants only — never from sampling the image grid:
///
/// * Kernel-radius rounding: the pixel path and the ray path feed the
///   predicate through different f32 roundings of `(x, y)`, `x * x + y * y`
///   and the f64 `sqrt`, so the radius the residual sees differs by up to
///   [`FREEZE_RADIUS_NOISE_ULPS`] ulps.
/// * Poly3's truncated term: the kernel truncates `rd / k1` to f32
///   (`distortion.rs:42`), so two evaluations of its step-0 residual differ
///   by up to `ulp(rd / k1)` beyond radius rounding. Dividing by the smooth
///   residual slope converts it to a radius band. Ptlens, Poly5 and
///   TCA-Poly3 run their Newton arithmetic exactly in f64 from f32 inputs,
///   so they carry no truncation term.
///
/// TCA channels see the post-distortion geometry radius; their band is
/// mapped back to input radii through the local geometry-radius slope.
/// Non-finite results (zero slope or zero scale — tangential or degenerate
/// crossings) become NaN or infinity and the caller refuses the table.
fn freeze_ambiguity_band(
    execution: &OpticsExecution,
    kernel: BreakKernel,
    break_radius: f32,
) -> f32 {
    let direction = std::f32::consts::FRAC_1_SQRT_2;
    match kernel {
        BreakKernel::Distortion => {
            let x = break_radius * direction;
            let y = break_radius * direction;
            let radius = ((x * x + y * y) as f64).sqrt();
            let (slope, truncation) = match execution.distortion() {
                Some(DistortionModel::Poly3 { k1 }) => (
                    3.0 * radius * radius,
                    f64::from(ulp32((radius * f64::from(1.0_f32 / k1)) as f32)),
                ),
                Some(DistortionModel::Poly5 { k1, k2 }) => (
                    (3.0 * f64::from(k1) * radius * radius + 5.0 * f64::from(k2) * radius.powi(4))
                        .abs(),
                    0.0,
                ),
                Some(DistortionModel::Ptlens { a, b, c }) => (
                    (4.0 * f64::from(a) * radius.powi(3)
                        + 3.0 * f64::from(b) * radius * radius
                        + 2.0 * f64::from(c) * radius)
                        .abs(),
                    0.0,
                ),
                // The predicate never flips for a `None` kernel, so this
                // arm is unreachable; zero slope refuses if it ever runs.
                None => (0.0, 0.0),
            };
            radius_band_to_f32(radius_band(radius, slope, truncation))
        }
        BreakKernel::TcaRed | BreakKernel::TcaBlue => {
            let x = break_radius * direction;
            let (geometry_x, geometry_y) = distortion_kernel(execution.distortion(), x, x);
            let geometry_radius =
                ((geometry_x * geometry_x + geometry_y * geometry_y) as f64).sqrt();
            let Some(TcaModel::Poly3 { red, blue }) = execution.tca() else {
                return f32::INFINITY;
            };
            let [v, c, b] = if kernel == BreakKernel::TcaRed {
                red
            } else {
                blue
            };
            let slope = (3.0 * f64::from(b) * geometry_radius * geometry_radius
                + 2.0 * f64::from(c) * geometry_radius
                + f64::from(v)
                - 1.0)
                .abs();
            // d(geometry radius) / d(input radius) around the break. The
            // geometry map is continuous to within the freeze jump here
            // (multiplier jumps are <= 3.4e-3), so a central difference over
            // a fraction of the knot step tracks the local slope closely.
            let h = f64::from(break_radius) * 1e-4;
            let high = (f64::from(break_radius) + h) as f32 * direction;
            let low = (f64::from(break_radius) - h) as f32 * direction;
            let (high_x, high_y) = distortion_kernel(execution.distortion(), high, high);
            let (low_x, low_y) = distortion_kernel(execution.distortion(), low, low);
            let high_radius = ((high_x * high_x + high_y * high_y) as f64).sqrt();
            let low_radius = ((low_x * low_x + low_y * low_y) as f64).sqrt();
            let scale = ((high_radius - low_radius) / (2.0 * h)).abs();
            radius_band_to_f32(radius_band(geometry_radius, slope, 0.0) / scale)
        }
    }
}

/// Radius-space band from the two noise sources at a kernel radius: input
/// rounding (`FREEZE_RADIUS_NOISE_ULPS` ulps) plus a residual-space
/// truncation noise divided by the smooth residual slope.
fn radius_band(kernel_radius: f64, slope: f64, truncation: f64) -> f64 {
    let rounding = f64::from(FREEZE_RADIUS_NOISE_ULPS) * f64::from(ulp32(kernel_radius as f32));
    rounding + truncation / slope
}

/// Saturating narrowing for band values; anything past `f32` range maps to
/// infinity, and NaN (from a degenerate slope/scale division) stays NaN —
/// the caller refuses both.
fn radius_band_to_f32(band: f64) -> f32 {
    if band > f64::from(f32::MAX) {
        f32::INFINITY
    } else {
        band as f32
    }
}

#[cfg(test)]
mod tests;
