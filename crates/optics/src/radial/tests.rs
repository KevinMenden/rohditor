use rayon::ThreadPoolBuilder;
use rayon::prelude::*;
use rohditor_image::{LinearRgbImage, LinearRgbSpace};

use super::*;
use crate::resample::cubic_sample;
use crate::{CorrectionComponents, LensCorrectionPlan, OpticsQuery, OpticsService, ProfileRequest};

/// Per-pixel parity grid from the review (same size the gates were set at).
const GRID_WIDTH: usize = 1_024;
const GRID_HEIGHT: usize = 768;

/// A6400-native production geometry.
const FULL_SIZE_WIDTH: usize = 6_048;
const FULL_SIZE_HEIGHT: usize = 4_024;

struct MatrixProfile {
    name: &'static str,
    make: &'static str,
    model: &'static str,
    clean: &'static str,
    lens_make: &'static str,
    lens_model: &'static str,
    focal: f32,
    /// Poly3-tele freeze class: also gate against the converged oracle.
    freeze_class: bool,
    /// `false` when the freeze-ambiguity guard refuses the table and the
    /// apply must run the authority path instead.
    expects_lut: bool,
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
        freeze_class: false,
        expects_lut: true,
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
            expects_lut: false,
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
            name: "Sony E 10-18 @10 (widest Ptlens, calibrated for two sensor sizes)",
            lens_make: "Sony",
            lens_model: "E 10-18mm f/4 OSS",
            focal: 10.0,
            ..base_profile("unused")
        },
        MatrixProfile {
            name: "Sony FE 100-400 @400 (scaled bound binds, freeze class)",
            lens_make: "Sony",
            lens_model: "FE 100-400mm f/4.5-5.6 GM OSS",
            focal: 400.0,
            freeze_class: true,
            expects_lut: false,
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
    service
        .resolve_plan(
            &query(profile),
            ProfileRequest::Automatic,
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

/// Fast-path entries must carry no guard reason; freeze-ambiguity entries
/// must be refused by that guard and nothing else.
fn assert_expected_path(profile: &MatrixProfile, table: &RadialMultipliers) {
    if profile.expects_lut {
        assert!(
            table.guard.is_none(),
            "{} must take the fast path, guard: {:?}",
            profile.name,
            table.guard
        );
    } else {
        assert_eq!(
            table.guard,
            Some(TableGuard::FreezeAmbiguity),
            "{} must be refused by the freeze-ambiguity guard",
            profile.name
        );
    }
}

/// Per-pixel input radius exactly as the LUT branch computes it.
fn branch_inputs(execution: &OpticsExecution, x: usize, y: usize) -> (f32, f32) {
    (
        execution.input_coordinate_x(x) as f32,
        execution.input_coordinate_y(y) as f32,
    )
}

fn lut_source(
    execution: &OpticsExecution,
    input_x: f32,
    input_y: f32,
    multiplier: f32,
) -> (f32, f32) {
    execution.source_coordinate(input_x * multiplier, input_y * multiplier)
}

/// Max LUT-vs-authority coordinate deviation over the full per-pixel
/// grid — coordinates only, no image allocation.
fn max_coordinate_deviation(
    execution: &OpticsExecution,
    table: &RadialMultipliers,
    width: usize,
    height: usize,
) -> f32 {
    (0..height)
        .into_par_iter()
        .map(|y| {
            let mut row_max = 0.0_f32;
            for x in 0..width {
                let (input_x, input_y) = branch_inputs(execution, x, y);
                let multipliers = table.lookup(input_x.hypot(input_y));
                let authority = execution.map_coordinates(x, y).expect("authority pixel");
                for channel in 0..3 {
                    let (source_x, source_y) =
                        lut_source(execution, input_x, input_y, multipliers[channel]);
                    row_max = row_max
                        .max((source_x - authority[channel].0).abs())
                        .max((source_y - authority[channel].1).abs());
                }
            }
            row_max
        })
        .reduce(|| 0.0_f32, f32::max)
}

#[test]
fn radial_lut_parity() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data");
    for profile in matrix() {
        let plan = resolve(&service, &profile, GRID_WIDTH, GRID_HEIGHT);
        let execution = plan.execution();
        let table = RadialMultipliers::build(&execution).expect("radial table");
        assert_expected_path(&profile, &table);
        let bound = coordinate_bound(&execution);

        if !profile.expects_lut {
            // The guard-refused table is never used by the apply; its
            // (potentially boundary-straddling) lookup deviation is not a
            // shipped coordinate. Bit-exact fallback identity is gated in
            // `radial_lut_fallback`; skip the LUT deviation asserts.
            println!(
                "{}: guard-refused table ({:?}), authority path, bound {:.4} px not applied",
                profile.name,
                table.guard.expect("refusal names its reason"),
                bound
            );
        } else {
            let max_deviation =
                max_coordinate_deviation(&execution, &table, GRID_WIDTH, GRID_HEIGHT);
            println!(
                "{}: max |delta coord| {max_deviation:.4} px vs bound {bound:.4} px",
                profile.name
            );
            assert!(
                max_deviation <= bound,
                "{}: coordinate deviation {max_deviation:.4} px over formula bound B = max(0.15, 1.5*eps*norm_unscale) = {bound:.4} px",
                profile.name
            );
        }

        // Freeze-boundary knots are present and the pinned segment uses
        // side selection (bit-exact knot values, never a blend) — the
        // lookup mechanism stays sound even on a guard-refused table; the
        // guard decides whether the apply is allowed to consult it.
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

/// Full-size geometry variant of `radial_lut_parity`: the fixture lens and
/// the long-lens profile at the A6400-native 6048x4024 grid, where the
/// plateau term of `B` is ~5.7x its value at the 1024x768 gate size.
/// The fixture must keep its LUT table (its benchmarked performance claims
/// stand); the Poly3-tele entry must be refused by the freeze-ambiguity
/// guard and its apply must run the untouched authority path bit-for-bit.
#[test]
#[ignore = "walks the full-frame 6048x4024 coordinate grid for two profiles (~24.3M pixels each) and applies one 24 MP image through the fallback; run with --ignored to check the accepted deviation against B at production scale"]
fn radial_lut_parity_full_size() {
    const FULL_WIDTH: usize = FULL_SIZE_WIDTH;
    const FULL_HEIGHT: usize = FULL_SIZE_HEIGHT;
    let service = OpticsService::load_bundled().expect("bundled Lensfun data");
    let fixtures = matrix()
        .into_iter()
        .filter(|profile| profile.focal == 35.0 || profile.freeze_class)
        .collect::<Vec<_>>();
    assert_eq!(fixtures.len(), 2, "fixture plus the 400 mm freeze class");
    for profile in fixtures {
        let plan = resolve(&service, &profile, FULL_WIDTH, FULL_HEIGHT);
        let execution = plan.execution();
        let table = RadialMultipliers::build(&execution).expect("full-size radial table");
        println!(
            "{} @ {FULL_WIDTH}x{FULL_HEIGHT}: guard {:?}, {} pinned segment(s)",
            profile.name,
            table.guard,
            table.step_segments.len()
        );
        assert_expected_path(&profile, &table);
        let bound = coordinate_bound(&execution);

        if profile.expects_lut {
            let max_deviation =
                max_coordinate_deviation(&execution, &table, FULL_WIDTH, FULL_HEIGHT);
            println!(
                "{} @ {FULL_WIDTH}x{FULL_HEIGHT}: max |delta coord| {max_deviation:.4} px vs bound {bound:.4} px",
                profile.name
            );
            assert!(
                max_deviation <= bound,
                "{}: full-size coordinate deviation {max_deviation:.4} px over formula bound B = max(0.15, 1.5*eps*norm_unscale) = {bound:.4} px",
                profile.name
            );
            continue;
        }

        // Refused entry: the unbisected ambiguity must be demonstrably
        // unsafe at this geometry — the table's lookup disagrees with the
        // per-pixel authority on the freeze band and the worst deviation
        // exceeds B — so the guard refusal, not a tolerated deviation, is
        // what ships. (The disagreement count counts per-channel samples
        // beyond 1e-3 px: interpolation slack below that stays invisible.)
        let break_radius = table
            .step_segments
            .first()
            .expect("one distortion freeze boundary")
            .break_radius;
        let (max_deviation, disagreements) = (0..FULL_HEIGHT)
            .into_par_iter()
            .map(|y| {
                let input_y = execution.input_coordinate_y(y) as f32;
                let (mut row_dev, mut row_disagree) = (0.0_f32, 0usize);
                for x in 0..FULL_WIDTH {
                    let input_x = execution.input_coordinate_x(x) as f32;
                    let multipliers = table.lookup(input_x.hypot(input_y));
                    let authority = execution.map_coordinates(x, y).expect("authority pixel");
                    for channel in 0..3 {
                        let (source_x, source_y) =
                            lut_source(&execution, input_x, input_y, multipliers[channel]);
                        let deviation = (source_x - authority[channel].0)
                            .abs()
                            .max((source_y - authority[channel].1).abs());
                        row_disagree += usize::from(deviation > 1e-3);
                        row_dev = row_dev.max(deviation);
                    }
                }
                (row_dev, row_disagree)
            })
            .reduce(
                || (0.0_f32, 0usize),
                |(dev_a, dis_a), (dev_b, dis_b)| (dev_a.max(dev_b), dis_a + dis_b),
            );
        println!(
            "{} @ {FULL_WIDTH}x{FULL_HEIGHT}: unguarded lookup max |delta coord| {max_deviation:.4} px vs bound {bound:.4} px, {} channel samples > 1e-3 px around break {:.6}",
            profile.name, disagreements, break_radius
        );
        assert!(
            disagreements > 0 && max_deviation > bound,
            "{}: guard must refuse a table whose production-scale lookup deviates {max_deviation:.4} px over B = {bound:.4} px on {} band samples",
            profile.name,
            disagreements
        );

        // The apply with the guard-refused table is bit-identical to the
        // untouched authority code on the worst region: every pixel within
        // two knot steps of the break circle.
        let image = LinearRgbImage::new(
            FULL_WIDTH,
            FULL_HEIGHT,
            FULL_WIDTH * 3,
            LinearRgbSpace::CameraNative,
            (0..FULL_WIDTH * FULL_HEIGHT * 3)
                .map(|index| 0.05 + (index % 977) as f32 * 0.001)
                .collect::<Vec<_>>(),
        )
        .expect("full-size test image");
        let applied = execution
            .apply_cancellable(image.clone(), &NeverCancelled)
            .expect("guard-refused apply runs the authority path");
        let row_stride = image.row_stride();
        let ring = table.step * 2.0;
        let mut checked = 0usize;
        for y in 0..FULL_HEIGHT {
            let input_y = execution.input_coordinate_y(y) as f32;
            for x in 0..FULL_WIDTH {
                let input_x = execution.input_coordinate_x(x) as f32;
                if (input_x.hypot(input_y) - break_radius).abs() > ring {
                    continue;
                }
                let coordinates = execution.map_coordinates(x, y).expect("authority pixel");
                for (channel, (coordinate_x, coordinate_y)) in coordinates.iter().enumerate() {
                    let expected = cubic_sample(
                        image.data(),
                        row_stride,
                        FULL_WIDTH,
                        FULL_HEIGHT,
                        channel,
                        *coordinate_x,
                        *coordinate_y,
                    )
                    .expect("authority sample");
                    assert_eq!(
                        applied.image.data()[y * FULL_WIDTH * 3 + x * 3 + channel],
                        expected,
                        "{}: fallback output at ({x}, {y}) channel {channel} must be bit-exact authority",
                        profile.name
                    );
                    checked += 1;
                }
            }
        }
        println!(
            "{} @ {FULL_WIDTH}x{FULL_HEIGHT}: {} ring pixels around the break verified bit-exact against the authority",
            profile.name, checked
        );
        assert!(checked > 0, "ring band must contain pixels");
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

/// The freeze-ambiguity guard: Poly3's f32-truncated `rd / k1` term makes
/// the per-pixel step-0 decision differ from the bisected break inside a
/// radius band far beyond `AMBIGUOUS_FREEZE_BAND_ULPS`, so every Poly3
/// freeze boundary must refuse its table — at gate size and at production
/// size alike (the band is a kernel constant, not a grid coincidence). The
/// exact-f64 fixture (Ptlens + Poly3 TCA) must keep its table at both
/// sizes, including the 6048x4024 geometry its benchmarked perf was taken
/// on. Builds only — the grids are never walked.
#[test]
fn radial_lut_freeze_ambiguity_guard() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data");
    for profile in matrix() {
        for (width, height) in [
            (GRID_WIDTH, GRID_HEIGHT),
            (FULL_SIZE_WIDTH, FULL_SIZE_HEIGHT),
        ] {
            let plan = resolve(&service, &profile, width, height);
            let execution = plan.execution();
            let table = RadialMultipliers::build(&execution).expect("radial table");
            println!(
                "{} @{}x{}: guard {:?}, segments {}",
                profile.name,
                width,
                height,
                table.guard,
                table
                    .step_segments
                    .iter()
                    .map(|segment| format!("{:.6}", segment.break_radius))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            assert_expected_path(&profile, &table);
        }
    }
}

#[test]
fn radial_lut_tripwire() {
    let service = OpticsService::load_bundled().expect("bundled Lensfun data");
    let direction = std::f32::consts::FRAC_1_SQRT_2;
    for profile in matrix() {
        let plan = resolve(&service, &profile, GRID_WIDTH, GRID_HEIGHT);
        let execution = plan.execution();
        let table = RadialMultipliers::build(&execution).expect("radial table");
        assert_expected_path(&profile, &table);
        let mut worst_margin = 0.0_f32;
        for index in 0..table.table.len() {
            let radius = index as f32 * table.step;
            let angled = knot_multipliers(&execution, radius * direction, radius * direction);
            let straight = knot_multipliers(&execution, radius, 0.0);
            for channel in 0..3 {
                let margin =
                    (angled[channel] - straight[channel]).abs() / angled[channel].abs().max(1e-3);
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
    let (step_segments, guard) = assess(execution, &table, base.step);
    RadialMultipliers {
        step: base.step,
        table,
        step_segments,
        guard,
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
            expects_lut: false,
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
        assert!(
            nonfinite.guard.is_some(),
            "{}: a NaN knot must invalidate the table",
            profile.name
        );
        if profile.expects_lut {
            assert_eq!(nonfinite.guard, Some(TableGuard::NonFinite));
        }
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
            jumped.guard.is_some(),
            "{}: an unexplained jump must invalidate the table",
            profile.name
        );
        if profile.expects_lut {
            assert_eq!(jumped.guard, Some(TableGuard::UnexplainedJump));
        }
        let result = execution
            .apply_cancellable_forced(image.clone(), &NeverCancelled, &jumped)
            .expect("fallback apply");
        assert_eq!(
            result.image.data(),
            reference.as_slice(),
            "{}: crafted jump table must reproduce the authority path bit-for-bit",
            profile.name
        );

        // The natural build decides per profile: a fast path that must
        // actually differ from the reference (or the bit-identity checks
        // above would pass vacuously), or — for guard-refused entries —
        // bit-exact equality with the authority on the natural table.
        let fast = execution
            .apply_cancellable(image, &NeverCancelled)
            .expect("apply");
        if profile.expects_lut {
            assert!(
                fast.image.data() != reference.as_slice(),
                "{}: fast path must not be a no-op on this fixture",
                profile.name
            );
        } else {
            assert_eq!(
                fast.image.data(),
                reference.as_slice(),
                "{}: naturally guard-refused table must run the authority path bit-for-bit",
                profile.name
            );
        }
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
        assert!(fallback.guard.is_some());
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
