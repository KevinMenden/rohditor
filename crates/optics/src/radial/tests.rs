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
                        let Some(oracle) = oracle_multipliers(&execution, input_x, input_y) else {
                            continue;
                        };
                        let multipliers = table.lookup(input_x.hypot(input_y));
                        let authority = execution.map_coordinates(x, y).expect("authority pixel");
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
