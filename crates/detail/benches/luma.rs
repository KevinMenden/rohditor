//! Luminance noise reduction on its own and together with chroma (the
//! target for the pair is at most 1 s of CPU time at 24 MP).
use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rohditor_detail::{
    ChromaDenoise, LumaDenoise, NeverCancelled, denoise_chroma, denoise_luma, estimate_luma_noise,
};
use rohditor_image::{LinearRgbImage, LinearRgbSpace};

const BALANCE: [f32; 3] = [2.0, 1.0, 1.5];

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

fn bench_luma(c: &mut Criterion) {
    let mut group = c.benchmark_group("luma_denoise");
    group.sample_size(10);
    for (name, width, height) in [("24MP", 6_000, 4_000), ("48MP", 8_000, 6_000)] {
        let input = image(width, height);
        group.throughput(Throughput::Elements((width * height) as u64));
        // Profile, then luminance filter.
        group.bench_with_input(BenchmarkId::new("luma", name), &input, |b, input| {
            b.iter_batched(
                || input.clone(),
                |mut image| {
                    let noise = estimate_luma_noise(&image, BALANCE, &NeverCancelled)
                        .expect("profile")
                        .expect("large enough");
                    denoise_luma(
                        black_box(&mut image),
                        LumaDenoise {
                            strength: 0.5,
                            detail: 0.5,
                            balance: BALANCE,
                            ceilings: [1.0; 3],
                            noise,
                        },
                        &NeverCancelled,
                    )
                },
                criterion::BatchSize::LargeInput,
            );
        });
        // Profile once, chroma, then luminance: what the detail stage runs.
        group.bench_with_input(BenchmarkId::new("chroma+luma", name), &input, |b, input| {
            b.iter_batched(
                || input.clone(),
                |mut image| {
                    let noise = estimate_luma_noise(&image, BALANCE, &NeverCancelled)
                        .expect("profile")
                        .expect("large enough");
                    denoise_chroma(
                        black_box(&mut image),
                        ChromaDenoise {
                            strength: 0.5,
                            balance: BALANCE,
                            ceilings: [1.0; 3],
                            luma_noise: noise,
                        },
                        &NeverCancelled,
                    )
                    .expect("chroma");
                    denoise_luma(
                        &mut image,
                        LumaDenoise {
                            strength: 0.5,
                            detail: 0.5,
                            balance: BALANCE,
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

criterion_group!(benches, bench_luma);
criterion_main!(benches);
