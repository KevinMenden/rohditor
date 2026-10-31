use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rohditor_detail::{NeverCancelled, Plane, atrous_decompose, edge_avoiding_decompose};

fn plane(width: usize, height: usize) -> Plane {
    let data = (0..width * height)
        .map(|i| ((i * 7919) % 1021) as f32 / 1021.0)
        .collect();
    Plane::new(width, height, data).expect("benchmark plane")
}

fn bench_atrous(c: &mut Criterion) {
    let mut group = c.benchmark_group("atrous_5_levels");
    group.sample_size(10);
    for (name, width, height) in [("24MP", 6_000, 4_000), ("48MP", 8_000, 6_000)] {
        let input = plane(width, height);
        group.throughput(Throughput::Elements((width * height) as u64));
        group.bench_with_input(BenchmarkId::new("plain", name), &input, |b, input| {
            b.iter(|| atrous_decompose(black_box(input), 5, &NeverCancelled));
        });
        group.bench_with_input(
            BenchmarkId::new("edge_avoiding", name),
            &input,
            |b, input| {
                b.iter(|| {
                    edge_avoiding_decompose(black_box(input), input, 5, 0.1, &NeverCancelled)
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_atrous);
criterion_main!(benches);
