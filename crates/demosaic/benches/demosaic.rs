use std::fmt::Display;
use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use rohditor_demosaic::{DemosaicAlgorithm, WhiteBalanceGains, demosaic};
use rohditor_image::{BayerPattern, MosaicImage};

const PREVIEW_WIDTH: usize = 2_560;
const PREVIEW_HEIGHT: usize = 1_703;

fn benchmark_demosaic(criterion: &mut Criterion) {
    for (name, width, height) in [
        ("preview_2560x1703", PREVIEW_WIDTH, PREVIEW_HEIGHT),
        ("full_a6400_6000x4000", 6_000, 4_000),
    ] {
        let mosaic = representative_mosaic(width, height);
        let mut group = criterion.benchmark_group(name);
        group.throughput(Throughput::Elements((width * height) as u64));
        for (algorithm_name, algorithm) in [
            ("bilinear", DemosaicAlgorithm::Bilinear),
            ("mhc", DemosaicAlgorithm::MalvarHeCutler),
        ] {
            group.bench_function(algorithm_name, |bencher| {
                bencher.iter(|| {
                    must(demosaic(
                        black_box(&mosaic),
                        WhiteBalanceGains::identity(),
                        algorithm,
                    ))
                });
            });
        }
        group.finish();
    }
}

fn representative_mosaic(width: usize, height: usize) -> MosaicImage<f32> {
    let samples = width * height;
    let data = (0..samples)
        .map(|index| (index % 1_024) as f32 / 1_023.0)
        .collect();
    must(MosaicImage::new(
        width,
        height,
        width,
        BayerPattern::Rggb,
        data,
    ))
}

fn must<T, E: Display>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("benchmark fixture failed: {error}"),
    }
}

criterion_group!(benches, benchmark_demosaic);
criterion_main!(benches);
