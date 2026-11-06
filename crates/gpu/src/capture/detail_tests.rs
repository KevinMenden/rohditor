//! CPU parity of the GPU detail passes (false colour, noise reduction) on
//! tiles that cross artificial halo edges.
use super::tests::processor;
use super::*;
use rohditor_demosaic::WhiteBalanceGains;
use rohditor_edit::EditRecipe;
use rohditor_image::LinearRgbSpace;

const BALANCE: WhiteBalanceGains = WhiteBalanceGains {
    red: 2.0,
    green: 1.0,
    blue: 1.5,
};

/// Asymmetric noisy image with a luma edge, a colour speck and a blown pixel.
fn noisy_image(width: usize, height: usize) -> LinearRgbImage<f32> {
    let mut state = 0x2545_f491_u32;
    let mut noise = move || {
        // xorshift32 mapped to [-1, 1].
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state as f32 / u32::MAX as f32 * 2.0 - 1.0
    };
    let mut data = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let mut base = 0.15 + 0.5 * x as f32 / width as f32 + 0.1 * y as f32 / height as f32;
            if x > width / 2 {
                base += 0.2;
            }
            let sigma = 0.01 + 0.02 * base.sqrt();
            data.extend_from_slice(&[
                base / BALANCE.red + sigma * noise(),
                base / BALANCE.green + sigma * noise(),
                base / BALANCE.blue + sigma * noise(),
            ]);
        }
    }
    let speck = ((height / 3) * width + width / 3) * 3;
    data[speck] += 0.3;
    data[speck + 2] += 0.3;
    let blown = ((height / 2) * width + 2 * width / 3) * 3;
    data[blown..blown + 3].fill(2.0);
    LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::CameraNative, data)
        .expect("fixture")
}

fn contract(configure: impl FnOnce(&mut EditRecipe)) -> DetailContract {
    let mut recipe = EditRecipe::default();
    configure(&mut recipe);
    DetailContract::new(&recipe, BALANCE, WhiteBalanceGains::identity()).expect("contract")
}

/// Largest difference to the CPU result, relative to its magnitude.
fn compare_to_cpu(name: &str, contract: &DetailContract, maximum_tile_edge: usize) {
    let mut gpu = processor();
    gpu.force_maximum_tile_edge(maximum_tile_edge);
    let input = noisy_image(331, 197);
    let token = CancellationToken::new();
    let mut reference = input.clone();
    contract
        .apply_cpu(&mut reference, &token)
        .expect("CPU detail stage");
    let result = gpu
        .process(&input, contract, 0, &token)
        .expect("GPU detail stage")
        .expect("active stage");
    assert!(result.metrics.tiles > 1, "{name}: tiles cross halo edges");
    let (mut worst, mut changed) = (0.0_f32, 0.0_f32);
    for ((gpu, cpu), original) in result
        .image
        .data()
        .iter()
        .zip(reference.data())
        .zip(input.data())
    {
        worst = worst.max((gpu - cpu).abs() / (1.0 + cpu.abs()));
        changed = changed.max((cpu - original).abs());
    }
    eprintln!("{name}: max error {worst:e}, max change {changed:e}");
    assert!(changed > 1e-3, "{name}: the stage must visibly act");
    assert!(worst <= 1e-4, "{name}: GPU deviates from CPU by {worst:e}");
}

#[test]
#[ignore = "requires Vulkan; CPU parity across halo-expanded tiles"]
fn false_color_matches_cpu() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    for steps in [1, 2, 5] {
        compare_to_cpu(
            &format!("false colour x{steps}"),
            &contract(|r| r.false_color.steps = steps),
            64,
        );
    }
}

#[test]
#[ignore = "requires Vulkan; CPU parity across halo-expanded tiles"]
fn chroma_noise_reduction_matches_cpu() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    compare_to_cpu(
        "chroma NR",
        &contract(|r| r.noise_reduction.chroma = 0.7),
        64,
    );
}

#[test]
#[ignore = "requires Vulkan; CPU parity across halo-expanded tiles"]
fn luminance_noise_reduction_matches_cpu() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    for detail in [0.0, 0.5, 1.0] {
        compare_to_cpu(
            &format!("luma NR detail {detail}"),
            &contract(|r| {
                r.noise_reduction.luminance = 0.7;
                r.noise_reduction.luminance_detail = detail;
            }),
            64,
        );
    }
}

#[test]
#[ignore = "requires Vulkan; CPU parity across halo-expanded tiles"]
fn the_whole_detail_stage_matches_cpu() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let all = contract(|r| {
        r.false_color.steps = 2;
        r.noise_reduction.chroma = 0.6;
        r.noise_reduction.luminance = 0.5;
        r.capture_sharpening.enabled = true;
        r.capture_sharpening.radius = 0.8;
    });
    // Tile sizes that do and do not divide the image.
    for edge in [48, 100] {
        compare_to_cpu(&format!("full stage, tile {edge}"), &all, edge);
    }
}

#[test]
#[ignore = "requires Vulkan; images too small to profile skip noise reduction on both"]
fn images_too_small_to_profile_skip_noise_reduction() {
    let _guard = crate::preview::processor::tests::gpu_test_guard();
    let mut gpu = processor();
    let input = noisy_image(11, 9);
    let token = CancellationToken::new();
    let contract = contract(|r| r.noise_reduction.chroma = 0.9);
    let result = gpu
        .process(&input, &contract, 0, &token)
        .expect("GPU")
        .expect("active");
    assert_eq!(result.image.data(), input.data());
    let mut reference = input.clone();
    contract.apply_cpu(&mut reference, &token).expect("CPU");
    assert_eq!(reference.data(), input.data());
}
