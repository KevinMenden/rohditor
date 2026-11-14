use criterion::{
    BatchSize, BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main,
};
use rohditor_highlight::{ChannelDetectionLevels, OpposedOptions, reconstruct_opposed};
use rohditor_image::{BayerPattern, CfaColor, MosaicImage};

/// Where the clipped samples of a fixture are.
#[derive(Clone, Copy)]
enum Layout {
    /// Nothing clipped: only the detection scan runs.
    Clean,
    /// One isolated clipped sample every ~1000 samples.
    Sparse,
    /// Every sample clipped: one region spanning the frame, no border.
    AllClipped,
    /// A clipped green cell on a 5-cell lattice: the densest layout of
    /// separate regions (worst case for per-region bookkeeping).
    TinyRegions,
    /// One huge blown-green disc with a shoulder (worst case for a single
    /// region's border statistics).
    Disc,
}

#[derive(Clone, Copy)]
struct Fixture {
    name: &'static str,
    width: usize,
    height: usize,
    row_stride: usize,
    layout: Layout,
}

/// Background of the layouts that need near-clip border cells to exist.
const SHOULDER_BASE: f32 = 0.6;

fn sample_value(fixture: Fixture, x: usize, y: usize) -> f32 {
    let is_green = BayerPattern::Rggb.color_at(x, y) == CfaColor::Green;
    match fixture.layout {
        Layout::Clean => 0.35,
        Layout::Sparse => {
            if (x + y * fixture.width).is_multiple_of(997) {
                1.0
            } else {
                0.35
            }
        }
        Layout::AllClipped => 1.0,
        Layout::TinyRegions => {
            if is_green && (x / 2) % 5 == 2 && (y / 2) % 5 == 2 {
                1.0
            } else {
                SHOULDER_BASE
            }
        }
        Layout::Disc => {
            let dx = x as f32 - fixture.width as f32 / 2.0;
            let dy = y as f32 - fixture.height as f32 / 2.0;
            let distance = (dx * dx + dy * dy).sqrt();
            let edge = fixture.height as f32 / 3.0;
            if !is_green {
                SHOULDER_BASE
            } else if distance <= edge {
                1.0
            } else {
                // Linear shoulder back to the background over 200 samples.
                (1.0 - (distance - edge) / 200.0 * (1.0 - SHOULDER_BASE)).max(SHOULDER_BASE)
            }
        }
    }
}

fn make_mosaic(fixture: Fixture) -> MosaicImage<f32> {
    let mut data = vec![0.0_f32; fixture.row_stride * fixture.height];
    for y in 0..fixture.height {
        for x in 0..fixture.width {
            data[y * fixture.row_stride + x] = sample_value(fixture, x, y);
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

fn bench_opposed(c: &mut Criterion) {
    let options = OpposedOptions {
        detection_levels: ChannelDetectionLevels {
            red: 0.9,
            green: 0.9,
            blue: 0.9,
        },
        neutral_ceiling: None,
    };
    let fixtures = [
        Fixture {
            name: "6000x4000_tight_none",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            layout: Layout::Clean,
        },
        Fixture {
            name: "6000x4000_tight_sparse",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            layout: Layout::Sparse,
        },
        Fixture {
            name: "6000x4000_tight_large",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            layout: Layout::AllClipped,
        },
        Fixture {
            name: "6000x4000_many_tiny_regions",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            layout: Layout::TinyRegions,
        },
        Fixture {
            name: "6000x4000_one_huge_region",
            width: 6_000,
            height: 4_000,
            row_stride: 6_000,
            layout: Layout::Disc,
        },
        Fixture {
            name: "37x23_padded_sparse",
            width: 37,
            height: 23,
            row_stride: 41,
            layout: Layout::Sparse,
        },
        Fixture {
            name: "37x23_padded_large",
            width: 37,
            height: 23,
            row_stride: 41,
            layout: Layout::AllClipped,
        },
    ];

    let mut group = c.benchmark_group("opposed");
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
                    |mosaic| black_box(reconstruct_opposed(mosaic, options).expect("opposed")),
                    BatchSize::LargeInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_opposed);
criterion_main!(benches);
