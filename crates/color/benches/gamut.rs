use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rohditor_color::compress_linear_srgb_chroma;

fn gamut_mapping(c: &mut Criterion) {
    let cases = [
        ("in-gamut", [0.18, 0.32, 0.67]),
        ("sparse-out-of-gamut", [1.02, 0.30, 0.12]),
        ("saturated", [2.0, -0.4, 0.8]),
    ];
    let mut group = c.benchmark_group("chroma-compress-pixel");
    group.throughput(Throughput::Elements(1));
    for (name, rgb) in cases {
        group.bench_with_input(BenchmarkId::from_parameter(name), &rgb, |bencher, rgb| {
            bencher.iter(|| compress_linear_srgb_chroma(black_box(*rgb)));
        });
    }
    group.finish();
}

criterion_group!(benches, gamut_mapping);
criterion_main!(benches);
