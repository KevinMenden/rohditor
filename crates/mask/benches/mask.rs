//! Mask evaluation cost per layer at fit-preview, 24 MP and 48 MP sizes.
//!
//! Rows are evaluated in parallel into a per-thread row buffer, the way a pipeline stage
//! consumes a mask (no full-resolution alpha plane is ever allocated).

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rayon::prelude::*;
use rohditor_edit::{
    BrushMask, BrushStroke, LinearGradient, LocalSettings, MaskComponent, MaskLayer, MaskMode,
    MaskPoint, MaskShape, RadialGradient,
};
use rohditor_mask::{MaskFrame, PreparedMask};

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

fn cases() -> [(&'static str, MaskLayer); 3] {
    [
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
        ("brush-50x200", layer(brush())),
    ]
}

/// Evaluates every row of the frame, as a pipeline stage would, and returns a checksum.
fn evaluate_all(mask: &PreparedMask, frame: MaskFrame) -> f32 {
    (0..frame.height)
        .into_par_iter()
        .map_init(
            || vec![0.0f32; frame.width],
            |row, y| {
                mask.evaluate_row(y, 0..frame.width, row);
                row[frame.width / 2]
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
