use rohditor_edit::{CameraProfileSelection, HighlightAdjustments, RenderingProfileSelection};

use super::{AdjustmentRange, DocumentPanelModel};
use crate::ui::widgets;

/// Which sections differ from a fresh photo's defaults. The Detail and Optics
/// settings answer this themselves, so they are not repeated here.
pub(super) struct SectionChanges {
    pub(super) tone: bool,
    pub(super) white_balance: bool,
    pub(super) saturation: bool,
    pub(super) tone_curve: bool,
    pub(super) highlights: bool,
    pub(super) profile: bool,
    pub(super) color_mixer: bool,
    pub(super) grading: bool,
}

impl SectionChanges {
    pub(super) fn of(document: &DocumentPanelModel) -> Self {
        let values = &document.values;
        let ranges = &document.ranges;
        let any_moved = |entries: &[(f32, AdjustmentRange)]| {
            entries
                .iter()
                .any(|(value, range)| !widgets::approximately_equal(*value, range.neutral))
        };
        let highlight_defaults = HighlightAdjustments::default();
        Self {
            tone: any_moved(&[
                (values.exposure, ranges.exposure),
                (values.contrast, ranges.contrast),
                (values.highlights, ranges.highlights),
                (values.shadows, ranges.shadows),
                (values.whites, ranges.whites),
                (values.blacks, ranges.blacks),
            ]),
            white_balance: document.white_balance_changed,
            saturation: any_moved(&[
                (values.saturation, ranges.saturation),
                (values.vibrance, ranges.vibrance),
            ]),
            tone_curve: any_moved(&[
                (values.tone_curve_shadows, ranges.tone_curve),
                (values.tone_curve_darks, ranges.tone_curve),
                (values.tone_curve_lights, ranges.tone_curve),
                (values.tone_curve_highlights, ranges.tone_curve),
            ]),
            highlights: values.highlight_method != highlight_defaults.method
                || !widgets::approximately_equal(
                    values.highlight_threshold,
                    highlight_defaults.clip.threshold,
                ),
            profile: document.camera_profile != CameraProfileSelection::Automatic
                || document.rendering_profile != RenderingProfileSelection::MATCHED,
            color_mixer: values.hsl.iter().any(|channel| {
                channel
                    .iter()
                    .zip([ranges.hsl_hue, ranges.hsl_saturation, ranges.hsl_luminance])
                    .any(|(value, range)| !widgets::approximately_equal(*value, range.neutral))
            }),
            grading: values
                .grading
                .iter()
                .flatten()
                .any(|value| !widgets::approximately_equal(*value, ranges.grading.neutral)),
        }
    }
}
