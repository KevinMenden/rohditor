//! Worker jobs that read the developed image for the Masks tool.
//!
//! Luminance and colour ranges select on what the image looks like, so two things the Masks tool
//! shows need pixels: the colour under the picker, and the overlay of a layer that has a range
//! component. Both measure the *reference* (after the global exposure and base rendering, see
//! `rohditor_core::sample_color_range`) on the cached preview base, so they cost one base
//! preparation at most, and after that only the sampling.

use std::sync::{Arc, mpsc};

use rohditor_core::{
    CancellationToken, CpuPipeline, DemosaicedBase, PreviewOptions, render_mask_overlay_from_image,
    sample_color_range,
};
use rohditor_edit::EditRecipe;
use rohditor_raw::RawFrame;

use super::{
    PreviewBackend, PreviewJob, PreviewResolution, WorkerEvent, ensure_preview_base, send_event,
};
use crate::document::PreviewTicket;
use crate::preview_cache::{PreviewCache, PreviewCacheKeys};
use crate::tools::ColorPick;
use crate::wake::WakeFn;

/// The colour under a click of the colour-range picker.
#[derive(Debug)]
pub struct ColorSampleJob {
    pub(super) ticket: PreviewTicket,
    pub(super) frame: Arc<RawFrame>,
    pub(super) recipe: EditRecipe,
    /// Position `0..1` over the image as shown.
    pub(super) position: (f64, f64),
    /// Echoed back with the sample, so the click's meaning survives the round trip.
    pub(super) pick: ColorPick,
    pub(super) options: PreviewOptions,
}

/// The overlay of one mask layer, measured on the developed image.
#[derive(Debug)]
pub struct MaskOverlayJob {
    pub(super) ticket: PreviewTicket,
    pub(super) frame: Arc<RawFrame>,
    pub(super) recipe: EditRecipe,
    pub(super) layer: u32,
    pub(super) options: PreviewOptions,
}

pub(super) fn process_color_sample(
    job: ColorSampleJob,
    sender: &mpsc::Sender<WorkerEvent>,
    wake: &WakeFn,
    pipeline: &CpuPipeline,
    preview_cache: &mut PreviewCache,
) {
    let result = with_base(
        job.ticket,
        &job.frame,
        &job.recipe,
        job.options,
        pipeline,
        preview_cache,
        |base| {
            sample_color_range(
                base.image(),
                base.source_orientation(),
                &job.recipe,
                job.position,
            )
            .ok_or_else(|| "There is no color to sample there".to_owned())
        },
    );
    let event = match result {
        Ok(sample) => WorkerEvent::ColorRangeSampleReady {
            ticket: job.ticket,
            pick: job.pick,
            sample,
        },
        Err(message) => WorkerEvent::ColorRangeSampleFailed {
            ticket: job.ticket,
            message,
        },
    };
    send_event(sender, wake, event);
}

pub(super) fn process_mask_overlay(
    job: MaskOverlayJob,
    sender: &mpsc::Sender<WorkerEvent>,
    wake: &WakeFn,
    pipeline: &CpuPipeline,
    preview_cache: &mut PreviewCache,
) {
    let result = with_base(
        job.ticket,
        &job.frame,
        &job.recipe,
        job.options,
        pipeline,
        preview_cache,
        |base| {
            render_mask_overlay_from_image(
                base.image(),
                base.source_orientation(),
                &job.recipe,
                job.layer,
            )
            .map_err(|error| format!("Could not render the mask overlay: {error}"))?
            .ok_or_else(|| "The mask no longer exists".to_owned())
        },
    );
    let event = match result {
        Ok(overlay) => WorkerEvent::MaskOverlayReady {
            ticket: job.ticket,
            layer: job.layer,
            overlay: Box::new(overlay),
        },
        Err(message) => WorkerEvent::MaskOverlayFailed {
            ticket: job.ticket,
            layer: job.layer,
            message,
        },
    };
    send_event(sender, wake, event);
}

/// Runs `read` on the preview base for `recipe`, preparing it first if the cache lacks it.
fn with_base<T>(
    ticket: PreviewTicket,
    frame: &Arc<RawFrame>,
    recipe: &EditRecipe,
    options: PreviewOptions,
    pipeline: &CpuPipeline,
    preview_cache: &mut PreviewCache,
    read: impl FnOnce(&DemosaicedBase) -> Result<T, String>,
) -> Result<T, String> {
    recipe
        .validate()
        .map_err(|error| format!("The current edit recipe is invalid: {error}"))?;
    let keys =
        PreviewCacheKeys::new_for_pipeline(ticket.document_id, frame, recipe, options, pipeline);
    let cache_hits = preview_cache.prepare(&keys, frame);
    let preview_job = PreviewJob {
        ticket,
        frame: Arc::clone(frame),
        recipe: recipe.clone(),
        options,
        backend: PreviewBackend::Cpu,
        resolution: PreviewResolution::Fit,
    };
    ensure_preview_base(
        &preview_job,
        options,
        &keys,
        cache_hits,
        &CancellationToken::new(),
        pipeline,
        preview_cache,
    )
    .map_err(|error| format!("Could not prepare the developed image: {error}"))?;
    let base = preview_cache
        .demosaiced(&keys)
        .ok_or_else(|| "The developed image was unavailable".to_owned())?;
    read(base)
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{
        ColorSample, HighlightMethod, LocalSettings, LuminanceRange, MaskComponent, MaskLayer,
        MaskMode, MaskShape,
    };

    use super::super::tests::fake_frame;
    use super::*;

    const LAYER: u32 = 5;

    fn recipe_with_range(shape: MaskShape) -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.raw.highlights.method = HighlightMethod::Off;
        recipe.local.layers.push(MaskLayer {
            id: LAYER,
            name: "Range".to_owned(),
            enabled: true,
            opacity: 1.0,
            components: vec![MaskComponent {
                mode: MaskMode::Add,
                invert: false,
                shape,
            }],
            settings: LocalSettings {
                exposure_ev: 1.0,
                ..LocalSettings::default()
            },
        });
        recipe
    }

    fn ticket() -> PreviewTicket {
        PreviewTicket {
            document_id: 21,
            revision: 4,
            sequence: 4,
        }
    }

    fn whole_range() -> MaskShape {
        MaskShape::Luminance(LuminanceRange {
            low: 0.0,
            high: 1.0,
            feather_low: 0.0,
            feather_high: 0.0,
        })
    }

    #[test]
    fn the_overlay_of_a_range_layer_is_measured_on_the_cached_base() {
        let frame = Arc::new(fake_frame());
        let recipe = recipe_with_range(whole_range());
        let mut cache = PreviewCache::default();
        let (sender, receiver) = mpsc::channel();
        let job = |layer| MaskOverlayJob {
            ticket: ticket(),
            frame: Arc::clone(&frame),
            recipe: recipe.clone(),
            layer,
            options: PreviewOptions::default(),
        };

        process_mask_overlay(
            job(LAYER),
            &sender,
            &crate::wake::noop(),
            &CpuPipeline::default(),
            &mut cache,
        );
        let WorkerEvent::MaskOverlayReady {
            ticket: got,
            layer,
            overlay,
        } = receiver.try_recv().expect("an event")
        else {
            panic!("expected the overlay");
        };
        assert_eq!((got, layer), (ticket(), LAYER));
        // Every pixel is inside the whole tonal range.
        assert!(!overlay.data().is_empty());
        assert!(overlay.data().iter().all(|alpha| *alpha == 255));

        // A layer the recipe lacks is reported, not drawn.
        process_mask_overlay(
            job(99),
            &sender,
            &crate::wake::noop(),
            &CpuPipeline::default(),
            &mut cache,
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerEvent::MaskOverlayFailed { layer: 99, .. })
        ));
    }

    #[test]
    fn the_picker_returns_the_colour_with_the_click_it_belongs_to() {
        let frame = Arc::new(fake_frame());
        let recipe = recipe_with_range(whole_range());
        let mut cache = PreviewCache::default();
        let (sender, receiver) = mpsc::channel();
        let sample_at = |position, pick| ColorSampleJob {
            ticket: ticket(),
            frame: Arc::clone(&frame),
            recipe: recipe.clone(),
            position,
            pick,
            options: PreviewOptions::default(),
        };

        process_color_sample(
            sample_at((0.5, 0.5), ColorPick::Add),
            &sender,
            &crate::wake::noop(),
            &CpuPipeline::default(),
            &mut cache,
        );
        let WorkerEvent::ColorRangeSampleReady { pick, sample, .. } =
            receiver.try_recv().expect("an event")
        else {
            panic!("expected a sample");
        };
        assert_eq!(pick, ColorPick::Add);
        let ColorSample { lightness, a, b } = sample;
        assert!(lightness > 0.0 && lightness <= 1.0, "{sample:?}");
        assert!(a.is_finite() && b.is_finite());

        process_color_sample(
            sample_at((1.5, 0.5), ColorPick::Replace),
            &sender,
            &crate::wake::noop(),
            &CpuPipeline::default(),
            &mut cache,
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(WorkerEvent::ColorRangeSampleFailed { .. })
        ));
    }
}
