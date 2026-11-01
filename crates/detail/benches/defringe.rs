use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rohditor_detail::{FringeColour, FringeRemoval, NeverCancelled, NoiseProfile, remove_fringes};
use rohditor_image::{LinearRgbImage, LinearRgbSpace};

/// Vertical dark/bright stripes of `period` pixels with 3 px purple/green
/// fringes at every edge and a little pseudo-noise. A long period means few
/// edges (typical photo); a short one is the worst case.
fn image(width: usize, height: usize, period: usize) -> LinearRgbImage<f32> {
    let half = period / 2;
    let mut data = Vec::with_capacity(width * height * 3);
    for i in 0..width * height {
        let (x, _) = (i % width, i / width);
        let phase = x % period;
        let dark = phase < half;
        let to_edge = (phase % half).min(half - 1 - phase % half);
        let base = if dark { 0.03 } else { 0.9 };
        let offset = match (to_edge < 3, dark) {
            (true, true) => [0.25, -0.02, 0.25],
            (true, false) => [-0.12, 0.0, -0.12],
            _ => [0.0; 3],
        };
        for (c, o) in offset.into_iter().enumerate() {
            let noise = 0.004 * (((i * 3 + c) * 7919 % 1021) as f32 / 1021.0 - 0.5);
            data.push((base + o + noise).max(0.0));
        }
    }
    LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::CameraNative, data)
        .expect("benchmark image")
}

fn bench_defringe(c: &mut Criterion) {
    let mut group = c.benchmark_group("defringe");
    group.sample_size(10);
    let params = FringeRemoval {
        purple: FringeColour {
            amount: 1.0,
            hue: [270.0, 330.0],
        },
        green: FringeColour {
            amount: 1.0,
            hue: [75.0, 150.0],
        },
        balance: [2.0, 1.0, 1.5],
        to_rec2020: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        ceilings: [1.0; 3],
        // The scene's grain: uniform +-0.002, i.e. sigma about 0.0012. The
        // pipeline passes the profile it estimates from the image.
        noise: Some(NoiseProfile {
            a: 0.0,
            b: 1.4e-6,
            bins: 4,
        }),
    };
    for (name, width, height) in [("24MP", 6_000, 4_000), ("48MP", 8_000, 6_000)] {
        group.throughput(Throughput::Elements((width * height) as u64));
        for (scene, period) in [("sparse-edges", 1_000), ("dense-edges", 64)] {
            let input = image(width, height, period);
            group.bench_with_input(BenchmarkId::new(name, scene), &input, |b, input| {
                b.iter_batched(
                    || input.clone(),
                    |mut image| remove_fringes(black_box(&mut image), params, &NeverCancelled),
                    criterion::BatchSize::LargeInput,
                );
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_defringe);
criterion_main!(benches);
