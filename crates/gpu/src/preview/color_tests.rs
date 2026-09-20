//! Color parity on asymmetric signed/HDR fixtures and a single resident source.
use super::tests::{
    gpu_control_matrix, gpu_test_guard, gpu_test_processor, neutral_recipe, synthetic_frame,
};
use super::*;
use rohditor_core::{CpuPipeline, PreviewOptions, apply_adjustments, render_display_srgb8};
use rohditor_image::LinearRgbImage;
use rohditor_raw::{RawDecoder, RawlerDecoder};

#[test]
fn color_uniform_layout_preserves_all_controls_and_shared_constants() {
    let mut recipe = neutral_recipe();
    for (index, channel) in recipe.color.hsl.channels.iter_mut().enumerate() {
        channel.hue = index as f32 / 8.0;
        channel.saturation = -0.5;
        channel.luminance = 0.25;
    }
    recipe.color.grading.shadows = [0.1, 0.2, 0.3];
    recipe.color.grading.midtones = [-0.1, -0.2, -0.3];
    recipe.color.grading.highlights = [0.4, 0.5, 0.6];
    let words = build_parameters(
        (13, 7),
        Orientation::Normal,
        OutputGeometry::new(13, 7, Orientation::Normal, None)
            .expect("color parity fixture should succeed"),
        &recipe,
        OutputPolicy::ClipToSrgb,
        WhiteBalanceGains::identity(),
        Matrix3::identity(),
    );
    for index in 0..8 {
        assert_eq!(
            words[52 + index * 4..56 + index * 4],
            [
                index as f32 / 8.0,
                -0.5,
                0.25,
                rohditor_core::HSL_CHANNEL_CENTERS[index]
            ]
            .map(f32::to_bits)
        );
    }
    assert_eq!(words[84..88], [0.1, 0.2, 0.3, 0.0].map(f32::to_bits));
    assert_eq!(words[88..92], [-0.1, -0.2, -0.3, 0.0].map(f32::to_bits));
    assert_eq!(words[92..96], [0.4, 0.5, 0.6, 0.0].map(f32::to_bits));
    assert_eq!(
        words[96..100],
        [
            1.0,
            1.0,
            rohditor_core::HSL_HUE_SHIFT_PER_FULL_VALUE,
            f32::EPSILON
        ]
        .map(f32::to_bits)
    );
}

fn color_recipes() -> Vec<EditRecipe> {
    let mut recipes = vec![neutral_recipe()];
    for band in 0..8 {
        for control in 0..3 {
            for value in [-1.0, 1.0] {
                let mut recipe = neutral_recipe();
                let channel = &mut recipe.color.hsl.channels[band];
                match control {
                    0 => channel.hue = value,
                    1 => channel.saturation = value,
                    _ => channel.luminance = value,
                }
                recipes.push(recipe);
            }
        }
    }
    for range in 0..3 {
        for channel in 0..3 {
            for value in [-1.0, 1.0] {
                let mut recipe = neutral_recipe();
                let grading = &mut recipe.color.grading;
                match range {
                    0 => grading.shadows[channel] = value,
                    1 => grading.midtones[channel] = value,
                    _ => grading.highlights[channel] = value,
                }
                recipes.push(recipe);
            }
        }
    }
    for value in [-1.0, 1.0] {
        let mut recipe = neutral_recipe();
        for channel in &mut recipe.color.hsl.channels {
            channel.hue = value;
            channel.saturation = value;
            channel.luminance = value;
        }
        recipes.push(recipe);
    }
    // Include combined Light/rendering/color cases; WB is exercised by the
    // camera-native control-matrix tests, not this identity-transform fixture.
    for (_, mut recipe) in gpu_control_matrix() {
        recipe.color.white_balance = neutral_recipe().color.white_balance;
        recipes.push(recipe);
    }
    recipes.push(neutral_recipe()); // reset after active edits
    recipes
}

fn color_fixture() -> Vec<f32> {
    let mut pixels = vec![
        [0.0; 3],
        [0.5; 3],
        [0.5001, 0.5, 0.5],
        [1.0e-8, 0.0, 0.0],
        [1.0e-6, 2.0e-6, 0.0],
        [-0.1, 0.0, 0.3],
        [-2.0, -1.0, -0.5],
        [4.0, 0.5, 0.1],
        [16.0, 8.0, -2.0],
        [-0.678, 0.2627, 0.0],
        [0.001, -0.0004, 0.0],
    ];
    // Band centers and halfway colors, plus both sides of red wraparound.
    let mut hues = Vec::new();
    for (index, center) in rohditor_core::HSL_CHANNEL_CENTERS.into_iter().enumerate() {
        let next = rohditor_core::HSL_CHANNEL_CENTERS
            .get(index + 1)
            .copied()
            .unwrap_or(1.0);
        hues.extend([center, (center + next) * 0.5]);
    }
    hues.extend([0.0001, 0.9999]);
    for hue in hues {
        // Fully saturated hue wheel expressed independently as triangular waves.
        let rgb = [0.0, 4.0, 2.0]
            .map(|phase| (((hue * 6.0 + phase) % 6.0 - 3.0).abs() - 1.0).clamp(0.0, 1.0));
        pixels.push(rgb.map(|v| v * 0.75));
        pixels.push(rgb.map(|v| v * 4.0 - 0.25));
    }
    (0..13 * 7)
        .flat_map(|index| pixels[index % pixels.len()])
        .collect()
}

#[test]
#[ignore = "requires a hardware Vulkan adapter; checks linear and display color parity"]
fn gpu_hsl_grading_matches_cpu_and_reuses_resident_source() {
    let _guard = gpu_test_guard();
    let Some(processor) = gpu_test_processor() else {
        return;
    };
    let recipe = neutral_recipe();
    let base = CpuPipeline::default()
        .prepare_preview_base(
            &synthetic_frame(Orientation::Normal),
            &recipe,
            PreviewOptions::default(),
        )
        .expect("color parity fixture should succeed");
    let input = color_fixture();
    let mut upload =
        GpuPreviewUpload::from_demosaiced_base(&base).expect("color parity fixture should succeed");
    upload.width = 13;
    upload.height = 7;
    upload.texels = pack_rgba32f(&input, 13, 7, 39, &CancellationToken::new())
        .expect("color parity fixture should succeed");
    let source = processor
        .upload_prepared(upload)
        .expect("color parity fixture should succeed");
    let mut reusable = None;
    let mut maximum_linear_error = 0.0_f32;
    let mut maximum_code_error = 0;
    for (index, recipe) in color_recipes().iter().enumerate() {
        assert!(
            source.matches_recipe(recipe),
            "recipe {index} should reuse source"
        );
        let mut cpu = LinearRgbImage::new(13, 7, 39, LinearRgbSpace::Rec2020D65, input.clone())
            .expect("color parity fixture should succeed");
        apply_adjustments(&mut cpu, recipe).expect("color parity fixture should succeed");
        for policy in [OutputPolicy::ClipToSrgb, OutputPolicy::ChromaCompressToSrgb] {
            let reused = reusable.is_some();
            let frame = processor
                .render(&source, recipe, policy, reusable.take())
                .expect("color parity fixture should succeed");
            assert_eq!(frame.textures_reused(), reused);
            let linear = read_working(&processor, &frame);
            for (sample, (&actual, &expected)) in linear.iter().zip(cpu.data()).enumerate() {
                // Lossless source upload isolates shader math; RGBA16F output contributes
                // up to about 0.1% relative rounding (including device truncation).
                let error = (actual - expected).abs();
                maximum_linear_error = maximum_linear_error.max(error);
                assert!(
                    actual.is_finite() && error <= 2.0e-5 + 0.0015 * expected.abs(),
                    "recipe {index} sample {sample}: GPU {actual}, CPU {expected}, error {error}"
                );
            }
            let gpu = processor
                .readback_display(&frame)
                .expect("color parity fixture should succeed");
            {
                let display = render_display_srgb8(&cpu, Orientation::Normal, policy)
                    .expect("CPU display reference should render");
                for (cpu, gpu) in display.data().chunks_exact(3).zip(gpu.rgba.chunks_exact(4)) {
                    for channel in 0..3 {
                        let error = cpu[channel].abs_diff(gpu[channel]);
                        maximum_code_error = maximum_code_error.max(error);
                        assert!(
                            error <= 2,
                            "recipe {index}, {policy:?}: CPU {cpu:?}, GPU {gpu:?}"
                        );
                    }
                }
            }
            reusable = Some(frame);
        }
    }
    eprintln!(
        "HSL/grading 13x7 parity: max linear error={maximum_linear_error}, max encoded error={maximum_code_error} codes; one source upload"
    );
}

fn read_working(processor: &GpuPreviewProcessor, frame: &GpuPreviewFrame) -> Vec<f32> {
    let (width, height) = frame.source_dimensions;
    let row_bytes = (width * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = processor.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("test-only working-linear readback"),
        size: u64::from(row_bytes) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = processor.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: frame
                ._working_texture
                .as_ref()
                .expect("ordinary preview frames retain a working texture"),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: Some(height),
            },
        },
        extent((width, height)),
    );
    processor.queue.submit([encoder.finish()]);
    let (sender, receiver) = mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender
                .send(result)
                .expect("color parity fixture should succeed")
        });
    processor
        .wait_for_queue()
        .expect("color parity fixture should succeed");
    receiver
        .recv()
        .expect("color parity fixture should succeed")
        .expect("color parity fixture should succeed");
    let bytes = buffer.slice(..).get_mapped_range();
    let mut rgb = Vec::new();
    for row in bytes.chunks_exact(row_bytes as usize) {
        for pixel in row[..width as usize * 8].chunks_exact(8) {
            for channel in pixel[..6].chunks_exact(2) {
                rgb.push(f16::from_bits(u16::from_le_bytes([channel[0], channel[1]])).to_f32());
            }
        }
    }
    drop(bytes);
    buffer.unmap();
    rgb
}

#[test]
#[ignore = "requires private Sony corpus and hardware Vulkan; writes temporary visual comparisons"]
fn private_hsl_grading_corpus_parity_and_timings() {
    let _guard = gpu_test_guard();
    let Some(processor) = gpu_test_processor() else {
        return;
    };
    let corpus =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/private");
    let artifacts = std::env::temp_dir().join(format!("rohditor-gpu-color-{}", std::process::id()));
    std::fs::create_dir_all(&artifacts).expect("comparison directory should be writable");
    let pipeline = CpuPipeline::default();
    let baseline = neutral_recipe();
    let controls: Vec<_> = gpu_control_matrix()
        .into_iter()
        .filter(|(label, _)| matches!(*label, "neutral" | "hsl" | "grading" | "combined_supported"))
        .map(|(label, mut recipe)| {
            recipe.color.white_balance = baseline.color.white_balance;
            (label, recipe)
        })
        .collect();
    eprintln!(
        "Color qualification: {} / {} / {}",
        processor.capabilities.adapter_name,
        processor.capabilities.driver,
        processor.capabilities.driver_info
    );
    for name in [
        "DSC00851", "DSC01166", "DSC02382", "DSC03270", "DSC03687", "DSC03821",
    ] {
        let raw = RawlerDecoder::default()
            .decode(&corpus.join(format!("{name}.ARW")))
            .expect("private corpus sample is required");
        let base = pipeline
            .prepare_preview_base(&raw, &baseline, PreviewOptions::default())
            .expect("private base should develop");
        let source = processor
            .upload_base(&base)
            .expect("private source should upload");
        let mut reusable = None;
        for (label, recipe) in &controls {
            let mut samples = Vec::new();
            // Warm each recipe before measuring. Queue completion is not
            // slider-to-presentation latency and includes submission overhead.
            for iteration in 0..12 {
                let frame = processor
                    .render(&source, recipe, OutputPolicy::ClipToSrgb, reusable.take())
                    .expect("resident color edit should render");
                processor.wait_for_queue().expect("GPU work should finish");
                if iteration >= 2 {
                    samples.push(
                        frame
                            .queue_completion_time()
                            .expect("queue callback should finish"),
                    );
                }
                reusable = Some(frame);
            }
            samples.sort_unstable();
            let mut cpu_samples = Vec::new();
            for _ in 0..5 {
                let started = Instant::now();
                let result = pipeline
                    .render_preview_from_base(&base, recipe, OutputPolicy::ClipToSrgb)
                    .expect("CPU color reference should render");
                std::hint::black_box(result);
                cpu_samples.push(started.elapsed());
            }
            cpu_samples.sort_unstable();
            eprintln!(
                "{name} {label}: {}x{}, warm GPU queue median={:.3} ms, max={:.3} ms; CPU render median={:.3} ms; resident={:.1} MiB",
                base.image().width(),
                base.image().height(),
                samples[5].as_secs_f64() * 1000.0,
                samples[9].as_secs_f64() * 1000.0,
                cpu_samples[2].as_secs_f64() * 1000.0,
                (source.estimated_bytes()
                    + reusable.as_ref().expect("rendered frame").estimated_bytes())
                    as f64
                    / 1_048_576.0
            );
            for policy in [OutputPolicy::ClipToSrgb, OutputPolicy::ChromaCompressToSrgb] {
                let frame = processor
                    .render(&source, recipe, policy, reusable.take())
                    .expect("policy should render");
                let cpu = pipeline
                    .render_preview_from_base(&base, recipe, policy)
                    .expect("CPU reference should render")
                    .image;
                let gpu = processor
                    .readback_display(&frame)
                    .expect("diagnostic readback should finish");
                let gpu_rgb: Vec<u8> = gpu
                    .rgba
                    .chunks_exact(4)
                    .flat_map(|p| p[..3].iter().copied())
                    .collect();
                let max_error = cpu
                    .data()
                    .iter()
                    .zip(&gpu_rgb)
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap_or(0);
                assert!(
                    max_error <= 2,
                    "{name} {label} {policy:?}: {max_error} codes"
                );
                eprintln!("{name} {label} {policy:?}: max parity error={max_error} codes");
                if *label == "combined_supported" {
                    // Side-by-side CPU/GPU, lossless PPM for manual inspection.
                    let mut ppm =
                        format!("P6\n{} {}\n255\n", cpu.width() * 2, cpu.height()).into_bytes();
                    for (cpu_row, gpu_row) in cpu
                        .data()
                        .chunks_exact(cpu.width() * 3)
                        .zip(gpu_rgb.chunks_exact(cpu.width() * 3))
                    {
                        ppm.extend_from_slice(cpu_row);
                        ppm.extend_from_slice(gpu_row);
                    }
                    std::fs::write(artifacts.join(format!("{name}-{policy:?}.ppm")), ppm)
                        .expect("comparison artifact should write");
                }
                reusable = Some(frame);
            }
        }
    }
    eprintln!(
        "CPU left / GPU right comparison artifacts: {}",
        artifacts.display()
    );
}
