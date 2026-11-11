//! Gain-map computation and CPU inverse at 24 MP and 48 MP (the export sizes).

#![allow(clippy::unwrap_used)]

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rohditor_gainmap::{GainMapParams, LinearRgbView, apply_gain_map, compute_gain_map};

const FRAMES: [(&str, usize, usize); 2] = [("24MP", 6_000, 4_000), ("48MP", 8_000, 6_000)];

/// A diagonal ramp: cheap to build, exercises the whole boost range.
fn frame(width: usize, height: usize, scale: f32) -> Vec<f32> {
    let mut pixels = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let v = (x + y) as f32 / (width + height) as f32 * scale;
            pixels.extend_from_slice(&[v, v * 0.9, v * 0.8]);
        }
    }
    pixels
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("gainmap");
    group.sample_size(10);
    let params = GainMapParams::default();
    for (name, width, height) in FRAMES {
        let sdr = frame(width, height, 1.0);
        let hdr = frame(width, height, 6.0);
        let sdr_view = LinearRgbView::new(width, height, &sdr).unwrap();
        let hdr_view = LinearRgbView::new(width, height, &hdr).unwrap();
        group.throughput(Throughput::Elements((width * height) as u64));
        group.bench_function(BenchmarkId::new("compute", name), |b| {
            b.iter(|| {
                black_box(compute_gain_map(&sdr_view, &hdr_view, &params, &|| false).unwrap())
            });
        });
        let (map, metadata) = compute_gain_map(&sdr_view, &hdr_view, &params, &|| false).unwrap();
        group.bench_function(BenchmarkId::new("apply", name), |b| {
            b.iter(|| black_box(apply_gain_map(&sdr_view, &map, &metadata, 3.0).unwrap()));
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
