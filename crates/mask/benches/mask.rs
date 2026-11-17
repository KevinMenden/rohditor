//! Mask evaluation cost per layer at fit-preview, 24 MP and 48 MP sizes.
//!
//! Rows are evaluated in parallel into a per-thread row buffer, the way a pipeline stage
//! consumes a mask (no full-resolution alpha plane is ever allocated).
//!
//! The `brush-N` cases are what painting produces: N strokes of a 5 % radius, each simplified
//! to about a dozen points. A brush layer is prepared again on every render, so `mask_prepare`
//! at fit size is the cost every preview of a painted photo pays.
//!
//! The `luminance` and `color-3` cases evaluate a range component on a synthetic reference
//! (the cost of measuring the reference from pixels is the pipeline's, not this crate's).

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rayon::prelude::*;
use rohditor_edit::{
    BrushMask, BrushStroke, ColorRange, ColorSample, LinearGradient, LocalSettings, LuminanceRange,
    MaskComponent, MaskLayer, MaskMode, MaskPoint, MaskShape, RadialGradient,
};
use rohditor_mask::{MaskFrame, PreparedMask, ReferenceRow};

const FRAMES: [(&str, usize, usize); 3] = [
    ("fit-2560x1707", 2_560, 1_707),
    ("24MP", 6_000, 4_000),
    ("48MP", 8_000, 6_000),
];

fn point(x: f32, y: f32) -> MaskPoint {
    MaskPoint { x, y }
}

fn layer(shape: MaskShape) -> MaskLayer {
    MaskLayer {
        id: 0,
        name: String::new(),
        enabled: true,
        opacity: 0.8,
        components: vec![MaskComponent {
            mode: MaskMode::Add,
            invert: false,
            shape,
        }],
        settings: LocalSettings::default(),
    }
}

/// 50 wavy strokes of 200 points each, scattered over the frame.
fn brush() -> MaskShape {
    let strokes = (0..50)
        .map(|s| {
            let phase = s as f32 * 0.7;
            let (x0, y0) = (
                0.1 + 0.8 * ((s * 37) % 50) as f32 / 50.0,
                0.1 + 0.8 * ((s * 11) % 50) as f32 / 50.0,
            );
            BrushStroke {
                points: (0..200)
                    .map(|i| {
                        let t = i as f32 / 199.0;
                        point(
                            x0 + 0.25 * (t - 0.5) * phase.cos(),
                            y0 + 0.25 * (t - 0.5) * phase.sin() + 0.03 * (t * 12.0 + phase).sin(),
                        )
                    })
                    .collect(),
                radius: 0.02,
                feather: 0.5,
                flow: 0.6,
                erase: s % 10 == 9,
            }
        })
        .collect();
    MaskShape::Brush(BrushMask { strokes })
}

/// `count` strokes as the Masks tool records them: a 5 % brush, a dozen points per stroke
/// (after simplification), scattered over the frame, one in twelve erasing.
fn painted(count: usize) -> MaskShape {
    let strokes = (0..count)
        .map(|s| {
            let (x0, y0) = (
                0.1 + 0.8 * ((s * 37) % 101) as f32 / 101.0,
                0.1 + 0.8 * ((s * 11) % 103) as f32 / 103.0,
            );
            let phase = s as f32 * 0.7;
            BrushStroke {
                points: (0..12)
                    .map(|i| {
                        let t = i as f32 / 11.0 - 0.5;
                        point(
                            x0 + 0.2 * t * phase.cos(),
                            y0 + 0.2 * t * phase.sin() + 0.02 * (t * 9.0 + phase).sin(),
                        )
                    })
                    .collect(),
                radius: 0.05,
                feather: 0.5,
                flow: 0.8,
                erase: s % 12 == 11,
            }
        })
        .collect();
    MaskShape::Brush(BrushMask { strokes })
}

fn cases() -> Vec<(&'static str, MaskLayer)> {
    vec![
        (
            "linear",
            layer(MaskShape::Linear(LinearGradient {
                start: point(0.2, 0.3),
                end: point(0.7, 0.8),
            })),
        ),
        (
            "radial",
            layer(MaskShape::Radial(RadialGradient {
                center: point(0.5, 0.5),
                radius_x: 0.4,
                radius_y: 0.25,
                angle_degrees: 30.0,
                feather: 0.5,
            })),
        ),
        (
            "luminance",
            layer(MaskShape::Luminance(LuminanceRange {
                low: 0.3,
                high: 0.8,
                feather_low: 0.1,
                feather_high: 0.1,
            })),
        ),
        (
            "color-3",
            layer(MaskShape::Color(ColorRange {
                samples: (0..3)
                    .map(|i| ColorSample {
                        lightness: 0.4 + 0.15 * i as f32,
                        a: 0.1 - 0.1 * i as f32,
                        b: 0.05 * i as f32,
                    })
                    .collect(),
                tolerance: 0.4,
            })),
        ),
        ("brush-50x200", layer(brush())),
        ("brush-10", layer(painted(10))),
        ("brush-100", layer(painted(100))),
        ("brush-500", layer(painted(500))),
    ]
}

/// A per-thread row of output alpha plus a synthetic reference (a luminance ramp and a matching
/// OkLab sweep) for masks with range components.
struct RowBuffers {
    alpha: Vec<f32>,
    luminance: Vec<f32>,
    oklab: Vec<[f32; 3]>,
}

impl RowBuffers {
    fn new(width: usize) -> Self {
        let ramp = |x: usize| x as f32 / width as f32;
        Self {
            alpha: vec![0.0; width],
            luminance: (0..width).map(ramp).collect(),
            oklab: (0..width)
                .map(|x| [ramp(x), 0.1 - 0.2 * ramp(x), 0.1 * ramp(x)])
                .collect(),
        }
    }
}

/// Evaluates every row of the frame, as a pipeline stage would, and returns a checksum.
fn evaluate_all(mask: &PreparedMask, frame: MaskFrame) -> f32 {
    (0..frame.height)
        .into_par_iter()
        .map_init(
            || RowBuffers::new(frame.width),
            |buffers, y| {
                let reference = ReferenceRow {
                    luminance: &buffers.luminance,
                    oklab: &buffers.oklab,
                };
                mask.evaluate_row(y, 0..frame.width, reference, &mut buffers.alpha);
                buffers.alpha[frame.width / 2]
            },
        )
        .sum()
}

fn bench_evaluate(c: &mut Criterion) {
    let mut group = c.benchmark_group("mask_evaluate");
    group.sample_size(10);
    for (frame_name, width, height) in FRAMES {
        let frame = MaskFrame { width, height };
        group.throughput(Throughput::Elements((width * height) as u64));
        for (name, layer) in cases() {
            let mask = PreparedMask::prepare(&layer, frame).expect("prepare");
            group.bench_with_input(BenchmarkId::new(name, frame_name), &mask, |b, mask| {
                b.iter(|| black_box(evaluate_all(mask, frame)));
            });
        }
    }
    group.finish();
}

fn bench_prepare(c: &mut Criterion) {
    let mut group = c.benchmark_group("mask_prepare");
    group.sample_size(10);
    for (frame_name, width, height) in FRAMES {
        let frame = MaskFrame { width, height };
        for (name, layer) in cases() {
            group.bench_with_input(BenchmarkId::new(name, frame_name), &layer, |b, layer| {
                b.iter(|| black_box(PreparedMask::prepare(layer, frame).expect("prepare")));
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_evaluate, bench_prepare);
criterion_main!(benches);
