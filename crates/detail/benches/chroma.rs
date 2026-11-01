use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rohditor_detail::{ChromaDenoise, NeverCancelled, denoise_chroma, estimate_noise_profiles};
use rohditor_image::{LinearRgbImage, LinearRgbSpace};

/// Smooth gradient plus deterministic pseudo-noise, so the noise estimator
/// and the shrinkage see realistic work.
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

fn bench_chroma(c: &mut Criterion) {
    let mut group = c.benchmark_group("chroma_denoise");
    group.sample_size(10);
    for (name, width, height) in [("24MP", 6_000, 4_000), ("48MP", 8_000, 6_000)] {
        let input = image(width, height);
        group.throughput(Throughput::Elements((width * height) as u64));
        group.bench_with_input(BenchmarkId::from_parameter(name), &input, |b, input| {
            b.iter_batched(
                || input.clone(),
                |mut image| {
                    let noise = estimate_noise_profiles(&image, [2.0, 1.0, 1.5], &NeverCancelled)
                        .expect("profile")
                        .expect("large enough");
                    denoise_chroma(
                        black_box(&mut image),
                        ChromaDenoise {
                            strength: 0.5,
                            balance: [2.0, 1.0, 1.5],
                            ceilings: [1.0; 3],
                            noise,
                        },
                        &NeverCancelled,
                    )
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_chroma);
criterion_main!(benches);
