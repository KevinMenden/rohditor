//! Toolkit-neutral zoom and pan state of the image viewport.
//!
//! Geometry is plain `f32` pairs (`[x, y]`) in logical pixels, so any frontend
//! can drive it from its own input events. Positions are relative to the
//! viewport's center: a `pan` of `[0, 0]` shows the image centered.

use crate::preview::PreviewSource;

/// Smallest scale [`fit_scale`] returns, so a tiny viewport never collapses the image.
const MIN_FIT_SCALE: f32 = 0.01;
/// Zoom limits of the wheel/pinch gesture.
const ZOOM_RANGE: (f32, f32) = (0.03, 16.0);
/// Zoom change per scroll unit (exponential).
const ZOOM_PER_SCROLL: f32 = 0.002;
/// How long the zoom percentage stays visible after a change, in seconds.
const ZOOM_FEEDBACK_SECONDS: f64 = 0.8;

type Pair = [f32; 2];

/// Scale at which an image of `image` pixels fits entirely inside `viewport`.
pub fn fit_scale(viewport: Pair, image: Pair) -> f32 {
    if image[0] <= 0.0 || image[1] <= 0.0 {
        return 1.0;
    }
    (viewport[0] / image[0])
        .min(viewport[1] / image[1])
        .max(MIN_FIT_SCALE)
}

/// Whether the viewport fits the image or shows it at an explicit zoom, plus
/// the pan offset of the image center from the viewport center.
#[derive(Debug)]
pub struct ViewState {
    fit: bool,
    zoom: f32,
    pan: Pair,
    zoom_feedback_until: f64,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            fit: true,
            zoom: 1.0,
            pan: [0.0, 0.0],
            zoom_feedback_until: 0.0,
        }
    }
}

impl ViewState {
    /// Fit the whole image into the viewport. `now` is the frontend clock in
    /// seconds, used only to time the zoom feedback.
    pub fn fit(&mut self, now: f64) {
        self.fit = true;
        self.pan = [0.0, 0.0];
        self.show_zoom_feedback(now);
    }

    /// Show one image pixel per logical pixel, centered.
    pub fn actual_size(&mut self, now: f64) {
        self.fit = false;
        self.zoom = 1.0;
        self.pan = [0.0, 0.0];
        self.show_zoom_feedback(now);
    }

    /// A source-scale frame arrived: enter 1:1 unless one was already shown, so
    /// a refresh keeps the user's zoom and pan.
    pub fn source_scale_frame_ready(&mut self, previous_source: Option<PreviewSource>, now: f64) {
        if !previous_source.is_some_and(PreviewSource::is_source_scale) {
            self.actual_size(now);
        }
    }

    pub const fn is_fit(&self) -> bool {
        self.fit
    }

    pub fn is_actual_size(&self) -> bool {
        !self.fit && (self.zoom - 1.0).abs() <= f32::EPSILON * 4.0
    }

    pub fn zoom_label(&self) -> String {
        if self.fit {
            "FIT".to_owned()
        } else if self.is_actual_size() {
            "SOURCE 100%".to_owned()
        } else {
            format!("{:.0}%", self.zoom * 100.0)
        }
    }

    /// The scale to draw at: the fit scale in fit mode, the explicit zoom otherwise.
    pub const fn scale(&self, fit_scale: f32) -> f32 {
        if self.fit { fit_scale } else { self.zoom }
    }

    /// Offset of the image center from the viewport center.
    pub const fn pan(&self) -> Pair {
        self.pan
    }

    /// Whether the transient zoom percentage should still be shown at `now`.
    pub fn zoom_feedback_active(&self, now: f64) -> bool {
        now < self.zoom_feedback_until
    }

    /// Drag the image by `delta`. Leaves fit mode at the current fit scale.
    pub fn pan_by(&mut self, fit_scale: f32, viewport_size: Pair, image_size: Pair, delta: Pair) {
        if self.fit {
            self.fit = false;
            self.zoom = fit_scale;
        }
        self.pan[0] += delta[0];
        self.pan[1] += delta[1];
        self.clamp_pan(viewport_size, image_size);
    }

    /// Zoom by a scroll amount, keeping the image point under `pointer` fixed.
    /// `pointer` is relative to the viewport center; it anchors the zoom only
    /// while it is over the image, otherwise the image center stays put.
    pub fn zoom_by(
        &mut self,
        fit_scale: f32,
        viewport_size: Pair,
        image_size: Pair,
        pointer: Option<Pair>,
        scroll_delta: f32,
        now: f64,
    ) {
        let old_scale = self.scale(fit_scale);
        let old_center = self.pan;
        let half = [
            image_size[0] * old_scale * 0.5,
            image_size[1] * old_scale * 0.5,
        ];
        let anchor = pointer
            .filter(|p| {
                (p[0] - old_center[0]).abs() <= half[0] && (p[1] - old_center[1]).abs() <= half[1]
            })
            .unwrap_or(old_center);
        self.fit = false;
        self.zoom =
            (old_scale * (scroll_delta * ZOOM_PER_SCROLL).exp()).clamp(ZOOM_RANGE.0, ZOOM_RANGE.1);

        // Keep the same image-space point below the pointer as the scale changes.
        // `pan` moves the displayed image center relative to the viewport center.
        let scale_ratio = self.zoom / old_scale;
        self.pan[0] += (anchor[0] - old_center[0]) * (1.0 - scale_ratio);
        self.pan[1] += (anchor[1] - old_center[1]) * (1.0 - scale_ratio);
        self.clamp_pan(viewport_size, image_size);
        self.show_zoom_feedback(now);
    }

    fn show_zoom_feedback(&mut self, now: f64) {
        self.zoom_feedback_until = now + ZOOM_FEEDBACK_SECONDS;
    }

    fn clamp_pan(&mut self, viewport_size: Pair, image_size: Pair) {
        // Keep at least half of the smaller extent overlapping on each axis. These
        // limits remain continuous when an image dimension crosses the viewport.
        for axis in 0..2 {
            let displayed = image_size[axis] * self.zoom;
            let limit = displayed.max(viewport_size[axis]) * 0.5;
            self.pan[axis] = self.pan[axis].clamp(-limit, limit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(fit: bool, zoom: f32, pan: Pair) -> ViewState {
        ViewState {
            fit,
            zoom,
            pan,
            zoom_feedback_until: 0.0,
        }
    }

    /// The image-space offset (in image pixels) under a viewport-relative pointer.
    fn image_space_offset(state: &ViewState, pointer: Pair) -> Pair {
        [
            (pointer[0] - state.pan[0]) / state.zoom,
            (pointer[1] - state.pan[1]) / state.zoom,
        ]
    }

    fn assert_close(expected: Pair, actual: Pair) {
        let distance = (expected[0] - actual[0]).hypot(expected[1] - actual[1]);
        assert!(distance < 0.0001, "{expected:?} != {actual:?}");
    }

    #[test]
    fn fit_scale_respects_both_axes_and_never_reaches_zero() {
        assert_eq!(fit_scale([800.0, 600.0], [1600.0, 900.0]), 0.5);
        assert_eq!(fit_scale([600.0, 800.0], [900.0, 1600.0]), 0.5);
        assert_eq!(fit_scale([0.0, 0.0], [100.0, 100.0]), 0.01);
        assert_eq!(fit_scale([800.0, 600.0], [0.0, 100.0]), 1.0);
    }

    #[test]
    fn view_commands_reset_pan_and_report_their_mode() {
        let mut view = state(false, 2.0, [40.0, -12.0]);
        view.fit(3.0);
        assert!(view.is_fit());
        assert_eq!(view.pan(), [0.0, 0.0]);
        assert_eq!(view.zoom_label(), "FIT");
        view.actual_size(4.0);
        assert!(!view.is_fit());
        assert_eq!(view.zoom_label(), "SOURCE 100%");
    }

    #[test]
    fn source_scale_frame_resets_zoom_on_entry_and_preserves_it_on_refresh() {
        let mut view = state(false, 0.4, [28.0, -16.0]);

        view.source_scale_frame_ready(Some(PreviewSource::HighQualityCpu), 1.0);
        assert_eq!(view.zoom, 1.0);
        assert_eq!(view.pan, [0.0, 0.0]);

        view.zoom = 0.35;
        view.pan = [-42.0, 19.0];
        view.source_scale_frame_ready(Some(PreviewSource::OneToOneGpu), 2.0);
        assert_eq!(view.zoom, 0.35);
        assert_eq!(view.pan, [-42.0, 19.0]);
    }

    #[test]
    fn pan_and_wheel_zoom_leave_fit_mode_and_anchor_to_the_pointer() {
        let mut view = ViewState::default();
        let viewport = [800.0, 600.0];
        let image = [1_000.0, 1_000.0];
        view.pan_by(0.5, viewport, image, [12.0, -8.0]);
        assert!(!view.is_fit());
        assert_eq!(view.zoom, 0.5);
        assert_eq!(view.pan, [12.0, -8.0]);

        view.zoom_by(0.5, viewport, image, Some([0.0, 0.0]), 120.0, 5.0);
        assert!(view.zoom > 0.5);
        let grow = view.zoom / 0.5;
        assert_close([12.0 * grow, -8.0 * grow], view.pan);
        assert!(view.zoom_feedback_active(5.5));
        assert!(!view.zoom_feedback_active(5.9));
    }

    #[test]
    fn wheel_zoom_keeps_corner_image_points_under_the_pointer() {
        let viewport = [900.0, 500.0];
        let image = [2_400.0, 1_600.0];
        let corners = [
            [-438.0, -238.0],
            [438.0, -238.0],
            [-438.0, 238.0],
            [438.0, 238.0],
        ];

        for pointer in corners {
            let mut view = state(false, 0.8, [20.0, -15.0]);
            let before = image_space_offset(&view, pointer);

            view.zoom_by(0.4, viewport, image, Some(pointer), 160.0, 1.0);

            assert_close(before, image_space_offset(&view, pointer));
        }
    }

    #[test]
    fn repeated_zoom_in_and_out_restores_scale_and_pan() {
        let viewport = [900.0, 500.0];
        let image = [2_400.0, 1_600.0];
        let pointer = [-410.0, -190.0];
        let mut view = state(false, 0.9, [50.0, -30.0]);

        for _ in 0..8 {
            view.zoom_by(0.4, viewport, image, Some(pointer), 120.0, 1.0);
            view.zoom_by(0.4, viewport, image, Some(pointer), -120.0, 1.0);
        }

        assert!((view.zoom - 0.9).abs() < 0.0001);
        assert_close([50.0, -30.0], view.pan);
    }

    #[test]
    fn zoom_across_viewport_dimensions_keeps_the_pointer_anchor_stable() {
        let viewport = [900.0, 500.0];
        let image = [1_600.0, 900.0];
        let pointer = [-200.0, -70.0];
        let mut view = state(false, 0.45, [0.0, 0.0]);
        let original_offset = image_space_offset(&view, pointer);

        assert!(image[0] * view.zoom < viewport[0]);
        assert!(image[1] * view.zoom < viewport[1]);
        for _ in 0..4 {
            view.zoom_by(0.45, viewport, image, Some(pointer), 60.0, 1.0);
            assert_close(original_offset, image_space_offset(&view, pointer));
        }
        assert!(image[0] * view.zoom > viewport[0]);
        assert!(image[1] * view.zoom > viewport[1]);

        for _ in 0..4 {
            view.zoom_by(0.45, viewport, image, Some(pointer), -60.0, 1.0);
            assert_close(original_offset, image_space_offset(&view, pointer));
        }
        assert!((view.zoom - 0.45).abs() < 0.0001);
        assert_close([0.0, 0.0], view.pan);
    }

    #[test]
    fn wheel_over_empty_canvas_keeps_the_image_center_stable() {
        let mut view = state(false, 1.0, [40.0, -20.0]);

        view.zoom_by(
            1.0,
            [900.0, 500.0],
            [400.0, 200.0],
            Some([-400.0, -200.0]),
            120.0,
            1.0,
        );

        assert_close([40.0, -20.0], view.pan);
    }

    #[test]
    fn wheel_zoom_from_fit_mode_starts_at_the_fit_scale() {
        let mut view = ViewState::default();
        assert!(view.is_fit());

        view.zoom_by(0.25, [800.0, 600.0], [2_000.0, 1_000.0], None, 120.0, 1.0);

        assert!(!view.is_fit());
        assert_eq!(view.zoom, (0.25 * (120.0_f32 * ZOOM_PER_SCROLL).exp()));
        assert_close([0.0, 0.0], view.pan);
    }

    #[test]
    fn a_pointer_exactly_on_the_image_edge_still_anchors_the_zoom() {
        // The image spans x in [-100, 100] around the viewport center.
        let mut on_edge = state(false, 1.0, [0.0, 0.0]);
        on_edge.zoom_by(
            1.0,
            [900.0, 500.0],
            [200.0, 100.0],
            Some([100.0, 0.0]),
            120.0,
            1.0,
        );
        assert!(
            on_edge.pan[0] < 0.0,
            "the edge point stays under the pointer"
        );

        let mut outside = state(false, 1.0, [0.0, 0.0]);
        outside.zoom_by(
            1.0,
            [900.0, 500.0],
            [200.0, 100.0],
            Some([100.5, 0.0]),
            120.0,
            1.0,
        );
        assert_eq!(
            outside.pan,
            [0.0, 0.0],
            "outside the image the center anchors"
        );
    }

    #[test]
    fn pan_bounds_keep_half_of_the_smaller_extent_overlapping_the_viewport() {
        let viewport = [900.0, 500.0];

        let mut small_image = ViewState::default();
        small_image.pan_by(1.0, viewport, [100.0, 200.0], [1_000.0, -1_000.0]);
        assert_eq!(small_image.pan, [450.0, -250.0]);

        let mut large_image = ViewState::default();
        large_image.pan_by(1.0, viewport, [1_800.0, 1_200.0], [1_000.0, -1_000.0]);
        assert_eq!(large_image.pan, [900.0, -600.0]);
    }
}
