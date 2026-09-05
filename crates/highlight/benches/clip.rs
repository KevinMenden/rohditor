use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use rohditor_highlight::{ChannelClipLevels, clip};
use rohditor_image::{BayerPattern, MosaicImage};

#[derive(Clone, Copy)]
struct Fixture {
    name: &'static str,
    width: usize,
    height: usize,
    row_stride: usize,
    sparse: bool,
    clipped: bool,
}

fn make_mosaic(fixture: Fixture) -> MosaicImage<f32> {
    let mut data = vec![0.35_f32; fixture.row_stride * fixture.height];
    for y in 0..fixture.height {
        for x in 0..fixture.width {
            let should_clip =
                fixture.clipped || (fixture.sparse && (x + y * fixture.width).is_multiple_of(997));
            if should_clip {
                data[y * fixture.row_stride + x] = 1.4;
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

fn bench_clip(c: &mut Criterion) {
    let levels = ChannelClipLevels {
        red: 0.9,
        green: 0.95,
        blue: 1.0,
    };
    let fixtures = [
        Fixture {
            name: "6000x4000_tight_none",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            sparse: false,
            clipped: false,
        },
        Fixture {
            name: "6000x4000_tight_sparse",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            sparse: true,
            clipped: false,
        },
        Fixture {
            name: "6000x4000_tight_large",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            sparse: false,
            clipped: true,
        },
        Fixture {
            name: "37x23_padded_none",
            width: 37,
            height: 23,
            row_stride: 41,
            sparse: false,
            clipped: false,
        },
        Fixture {
            name: "37x23_padded_sparse",
            width: 37,
            height: 23,
            row_stride: 41,
            sparse: true,
            clipped: false,
        },
        Fixture {
            name: "37x23_padded_large",
            width: 37,
            height: 23,
            row_stride: 41,
            sparse: false,
            clipped: true,
        },
    ];

    let mut group = c.benchmark_group("clip");
    for fixture in fixtures {
        group.throughput(Throughput::Elements(
            (fixture.width * fixture.height) as u64,
        ));
        group.bench_with_input(
            BenchmarkId::new("fused_in_place", fixture.name),
            &fixture,
            |bencher, fixture| {
                bencher.iter_batched(
                    || make_mosaic(*fixture),
                    |mosaic| black_box(clip(mosaic, levels).expect("clip")),
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_clip);
criterion_main!(benches);
