use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use rohditor_highlight::{ChannelDetectionLevels, LocalRatioOptions, reconstruct_local_ratios};
use rohditor_image::{BayerPattern, MosaicImage};

#[derive(Clone, Copy)]
struct Fixture {
    name: &'static str,
    width: usize,
    height: usize,
    row_stride: usize,
    sparse: bool,
    unsupported: bool,
}

fn make_mosaic(fixture: Fixture) -> MosaicImage<f32> {
    let mut data = vec![0.35_f32; fixture.row_stride * fixture.height];
    for y in 0..fixture.height {
        for x in 0..fixture.width {
            let index = y * fixture.row_stride + x;
            let clipped = if fixture.unsupported {
                (x / 2, y / 2) == (fixture.width / 4, fixture.height / 4)
            } else {
                fixture.sparse && (x + y * fixture.width).is_multiple_of(997)
            };
            if clipped {
                data[index] = 1.0;
            }
        }
    }
    MosaicImage::new(
        fixture.width,
        fixture.height,
        fixture.row_stride,
        BayerPattern::Rggb,
        data,
    )
    .expect("benchmark fixture")
}

fn bench_local_ratios(c: &mut Criterion) {
    let options = LocalRatioOptions {
        detection_levels: ChannelDetectionLevels {
            red: 0.9,
            green: 0.9,
            blue: 0.9,
        },
    };
    let fixtures = [
        Fixture {
            name: "6000x4000_tight_none",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            sparse: false,
            unsupported: false,
        },
        Fixture {
            name: "6000x4000_tight_sparse",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            sparse: true,
            unsupported: false,
        },
        Fixture {
            name: "37x23_padded_sparse",
            width: 37,
            height: 23,
            row_stride: 41,
            sparse: true,
            unsupported: false,
        },
        Fixture {
            name: "37x23_padded_unsupported",
            width: 37,
            height: 23,
            row_stride: 41,
            sparse: false,
            unsupported: true,
        },
    ];

    let mut group = c.benchmark_group("local_ratios");
    for fixture in fixtures {
        group.throughput(Throughput::Elements(
            (fixture.width * fixture.height) as u64,
        ));
        group.bench_with_input(
            BenchmarkId::new("reconstruct", fixture.name),
            &fixture,
            |bencher, fixture| {
                bencher.iter_batched(
                    || make_mosaic(*fixture),
                    |mosaic| {
                        black_box(reconstruct_local_ratios(mosaic, options).expect("local ratios"))
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_local_ratios);
criterion_main!(benches);
