//! The copies of a CPU preview base that mask layers with detail settings blend in.
//!
//! The expensive part of a copy, its reconstruction, is cached per detail setting and independent
//! of the masks: dragging one layer's detail slider costs one reconstruction, editing a mask's
//! shape none, and a white-balance change only repeats the colour conversion.

use std::time::Instant;

use rohditor_core::{
    CancellationToken, CpuPipeline, LocalDetailImage, PipelineError, PreviewOptions, StageTimings,
    local_detail_variants,
};
use rohditor_edit::EditRecipe;

use super::{PreviewJob, cache_invariant};
use crate::preview_cache::{PreviewCache, PreviewCacheKeys};

/// The developed copies `job.recipe` needs, reusing cached reconstructions.
pub(super) fn prepare_images(
    job: &PreviewJob,
    options: PreviewOptions,
    cancellation: &CancellationToken,
    pipeline: &CpuPipeline,
    preview_cache: &mut PreviewCache,
    timings: &mut StageTimings,
) -> Result<Vec<LocalDetailImage>, PipelineError> {
    let started = Instant::now();
    let variants: Vec<EditRecipe> = local_detail_variants(&job.recipe)
        .into_iter()
        .map(|settings| job.recipe.with_detail_settings(settings))
        .collect();
    let keys: Vec<PreviewCacheKeys> = variants
        .iter()
        .map(|variant| {
            PreviewCacheKeys::new_for_pipeline(
                job.ticket.document_id,
                &job.frame,
                variant,
                options,
                pipeline,
            )
        })
        .collect();
    let mut images = Vec::with_capacity(variants.len());
    for (variant, variant_keys) in variants.iter().zip(&keys) {
        if preview_cache.local_detail(variant_keys).is_none() {
            let preview = preview_cache.prepare_local_detail(
                pipeline,
                &job.frame,
                variant,
                options,
                variant_keys,
                cancellation,
            )?;
            preview_cache.insert_local_detail(variant_keys, preview, &keys);
        }
        let reconstructed = preview_cache.local_detail(variant_keys).ok_or_else(|| {
            cache_invariant("a local detail copy was unavailable after preparing")
        })?;
        let base = pipeline.prepare_preview_base_from_reconstruction_cancellable(
            reconstructed,
            variant,
            cancellation,
        )?;
        images.push(LocalDetailImage::new(base));
    }
    let elapsed = started.elapsed();
    timings.local_detail += elapsed;
    timings.total += elapsed;
    Ok(images)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use rohditor_core::HighlightDiagnostics;
    use rohditor_edit::{HighlightMethod, MAX_LOCAL_DETAIL_VARIANTS};
    use rohditor_edit::{
        LinearGradient, LocalDetail, LocalSettings, MaskComponent, MaskLayer, MaskMode, MaskPoint,
        MaskShape, WhiteBalance,
    };
    use rohditor_raw::RawFrame;

    use super::super::tests::fake_frame;
    use super::super::{PreviewBackend, PreviewResolution, develop_preview};
    use super::*;
    use crate::document::PreviewTicket;
    use crate::preview_cache::PreviewCacheHits;

    fn layer(id: u32, end_x: f32, sharpening: f32) -> MaskLayer {
        MaskLayer {
            id,
            name: String::new(),
            enabled: true,
            opacity: 1.0,
            components: vec![MaskComponent {
                mode: MaskMode::Add,
                invert: false,
                shape: MaskShape::Linear(LinearGradient {
                    start: MaskPoint { x: 0.1, y: 0.5 },
                    end: MaskPoint { x: end_x, y: 0.5 },
                }),
            }],
            settings: LocalSettings {
                detail: LocalDetail {
                    sharpening,
                    ..LocalDetail::default()
                },
                ..LocalSettings::default()
            },
        }
    }

    /// Textured, so that sharpening changes pixels (the worker fixture is flat and tiny).
    fn textured_frame() -> RawFrame {
        let (width, height) = (64, 48);
        let mut frame = fake_frame();
        frame.info.width = width;
        frame.info.height = height;
        frame.row_stride = width;
        frame.mosaic = (0..width * height)
            .map(|index| {
                let (x, y) = (index % width, index / width);
                if (x / 3 + y / 5) % 2 == 0 {
                    12_000
                } else {
                    36_000
                }
            })
            .collect();
        frame
    }

    fn recipe(layers: Vec<MaskLayer>) -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.raw.highlights.method = HighlightMethod::Off;
        recipe.local.layers = layers;
        recipe
    }

    struct Harness {
        frame: Arc<RawFrame>,
        cache: PreviewCache,
        revision: u64,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                frame: Arc::new(textured_frame()),
                cache: PreviewCache::default(),
                revision: 0,
            }
        }

        fn keys(&self, recipe: &EditRecipe) -> PreviewCacheKeys {
            PreviewCacheKeys::new(1, &self.frame, recipe, PreviewOptions::default())
        }

        /// Whether the reconstruction for this sharpening offset is cached.
        fn holds_copy(&self, sharpening: f32) -> bool {
            let plain = recipe(Vec::new());
            let settings = plain.detail_settings().with_local(LocalDetail {
                sharpening,
                ..LocalDetail::default()
            });
            self.cache
                .local_detail(&self.keys(&plain.with_detail_settings(settings)))
                .is_some()
        }

        fn develop(&mut self, recipe: EditRecipe) -> (PreviewCacheHits, Vec<u8>, Duration) {
            self.revision += 1;
            let options = PreviewOptions::default();
            let job = PreviewJob {
                ticket: PreviewTicket {
                    document_id: 1,
                    revision: self.revision,
                    sequence: self.revision,
                },
                frame: Arc::clone(&self.frame),
                recipe,
                options,
                backend: PreviewBackend::Cpu,
                resolution: PreviewResolution::Fit,
            };
            let keys = self.keys(&job.recipe);
            let hits = self.cache.prepare(&keys, &self.frame);
            let (image, _, diagnostics) = develop_preview(
                &job,
                options,
                &keys,
                hits,
                &CancellationToken::new(),
                &CpuPipeline::default(),
                &mut self.cache,
            )
            .expect("preview develops");
            assert!(matches!(
                diagnostics.highlight_diagnostics,
                HighlightDiagnostics::Off
            ));
            // The status bar shows the total, so it must include developing the copies.
            assert!(diagnostics.timings.total >= diagnostics.timings.local_detail);
            (
                hits,
                image.data().to_vec(),
                diagnostics.timings.local_detail,
            )
        }
    }

    #[test]
    fn a_shape_edit_only_repeats_the_blend() {
        let mut harness = Harness::new();
        let (_, first, _) = harness.develop(recipe(vec![layer(0, 0.5, 0.5)]));
        let (hits, moved, local_detail) = harness.develop(recipe(vec![layer(0, 0.9, 0.5)]));
        assert!(hits.reconstructed && hits.demosaiced && !hits.adjusted);
        assert_eq!(local_detail, Duration::ZERO);
        assert_ne!(first, moved, "the wider mask must show");
    }

    #[test]
    fn a_detail_slider_keeps_the_base_reconstruction_and_the_other_layers_copy() {
        let mut harness = Harness::new();
        let mut layers = vec![layer(0, 0.5, 0.5), layer(1, 0.5, 0.25)];
        harness.develop(recipe(layers.clone()));
        // A long drag on one layer makes more values than the cache has room for.
        for step in 0..=MAX_LOCAL_DETAIL_VARIANTS * 2 {
            layers[1].settings.detail.sharpening = 0.3 + 0.05 * step as f32;
            let (hits, ..) = harness.develop(recipe(layers.clone()));
            assert!(hits.reconstructed && !hits.demosaiced);
            assert!(harness.holds_copy(0.5), "step {step} evicted a needed copy");
        }
        assert!(!harness.holds_copy(0.25), "old values must be evicted");
    }

    #[test]
    fn switching_a_layer_off_and_on_does_not_develop_the_copy_again() {
        let mut harness = Harness::new();
        let mut layers = vec![layer(0, 0.5, 0.5)];
        let (_, on, _) = harness.develop(recipe(layers.clone()));
        layers[0].enabled = false;
        let (_, off, _) = harness.develop(recipe(layers.clone()));
        assert_ne!(on, off);
        assert!(harness.holds_copy(0.5));
        layers[0].enabled = true;
        let (_, again, _) = harness.develop(recipe(layers));
        assert_eq!(on, again);
    }

    #[test]
    fn white_balance_reuses_the_reconstruction_of_a_copy() {
        let mut harness = Harness::new();
        let mut edited = recipe(vec![layer(0, 0.5, 0.5)]);
        let (_, before, _) = harness.develop(edited.clone());
        edited.color.white_balance = WhiteBalance::ManualMultipliers {
            red: 1.2,
            green: 1.0,
            blue: 0.9,
        };
        let (hits, after, _) = harness.develop(edited);
        assert!(hits.reconstructed && !hits.demosaiced);
        assert!(harness.holds_copy(0.5));
        assert_ne!(before, after);
    }

    #[test]
    fn copies_count_towards_the_resident_bytes() {
        let mut harness = Harness::new();
        harness.develop(recipe(Vec::new()));
        let without = harness.cache.resident_bytes();
        harness.develop(recipe(vec![layer(0, 0.5, 0.5)]));
        assert!(harness.cache.resident_bytes() > without);
    }
}
