use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rohditor_detail::{FalseColor, NeverCancelled, suppress_false_color};
use rohditor_image::{LinearRgbImage, LinearRgbSpace};

/// Smooth gradient plus deterministic pseudo-noise, so the medians see
/// realistic (unsorted) windows.
fn image(width: usize, height: usize) -> LinearRgbImage<f32> {
    let data = (0..width * height * 3)
        .map(|i| {
            let p = i / 3;
            let signal = 0.1 + 0.8 * (p % width) as f32 / width as f32;
            signal + 0.02 * (((i * 7919) % 1021) as f32 / 1021.0 - 0.5)
        })
        .collect();
    LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::CameraNative, data)
        .expect("benchmark image")
}

fn bench_false_color(c: &mut Criterion) {
    let mut group = c.benchmark_group("false_color");
    group.sample_size(10);
    for (name, width, height) in [("24MP", 6_000, 4_000), ("48MP", 8_000, 6_000)] {
        let input = image(width, height);
        group.throughput(Throughput::Elements((width * height) as u64));
        // Steps 1 and 2 give the fixed cost (colour split and write-back)
        // and the cost of each further median pass.
        for steps in [1_u8, 2, 5] {
            group.bench_with_input(
                BenchmarkId::new(name, format!("{steps}-steps")),
                &input,
                |b, input| {
                    b.iter_batched(
                        || input.clone(),
                        |mut image| {
                            suppress_false_color(
                                black_box(&mut image),
                                FalseColor {
                                    steps,
                                    balance: [2.0, 1.0, 1.5],
                                    ceilings: [1.0; 3],
                                },
                                &NeverCancelled,
                            )
                        },
                        criterion::BatchSize::LargeInput,
                    );
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_false_color);
criterion_main!(benches);
