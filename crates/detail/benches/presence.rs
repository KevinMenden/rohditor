use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};
use rohditor_detail::{NeverCancelled, Presence, apply_presence};
use rohditor_image::{LinearRgbImage, LinearRgbSpace};

const REC2020_LUMA: [f32; 3] = [0.2627, 0.6780, 0.0593];

/// Blocks of varying brightness with a little pseudo-noise: edges and flat areas, as in a photo.
fn image(width: usize, height: usize) -> LinearRgbImage<f32> {
    let mut data = Vec::with_capacity(width * height * 3);
    for i in 0..width * height {
        let (x, y) = (i % width, i / width);
        let base = 0.03 + 0.6 * (((x / 97) * 31 + (y / 61) * 17) % 7) as f32 / 6.0;
        for c in 0..3 {
            let noise = 0.01 * (((i * 3 + c) * 7919 % 1021) as f32 / 1021.0 - 0.5);
            data.push((base * (1.0 - 0.15 * c as f32) + noise).max(0.0));
        }
    }
    LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::Rec2020D65, data)
        .expect("benchmark image")
}

fn bench_presence(c: &mut Criterion) {
    let mut group = c.benchmark_group("presence");
    group.sample_size(10);
    // The interactive preview size and a full 24 MP export.
    for (name, width, height) in [("preview", 2_560, 1_707), ("24MP", 6_000, 4_000)] {
        group.throughput(Throughput::Elements((width * height) as u64));
        let input = image(width, height);
        for (control, presence) in [
            (
                "texture",
                Presence {
                    texture: 0.5,
                    ..Presence::default()
                },
            ),
            (
                "clarity",
                Presence {
                    clarity: 0.5,
                    ..Presence::default()
                },
            ),
            (
                "dehaze",
                Presence {
                    dehaze: 0.5,
                    ..Presence::default()
                },
            ),
            (
                "add-haze",
                Presence {
                    dehaze: -0.5,
                    ..Presence::default()
                },
            ),
        ] {
            group.bench_with_input(BenchmarkId::new(name, control), &input, |b, input| {
                b.iter_batched(
                    || input.clone(),
                    |mut image| {
                        apply_presence(
                            &mut image,
                            black_box(presence),
                            REC2020_LUMA,
                            &NeverCancelled,
                        )
                        .expect("presence");
                        image
                    },
                    criterion::BatchSize::LargeInput,
                );
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_presence);
criterion_main!(benches);
