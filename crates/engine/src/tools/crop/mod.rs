//! The Crop tool: the rectangle being drafted, and how gestures and ratios change it.

mod aspect;
mod handles;
mod resize;

use rohditor_edit::NormalizedCropRect;

pub use aspect::CropAspect;
pub use handles::CropHandle;
use resize::{fit_rect, resize_free, resize_locked, translate};

#[derive(Debug, Clone, Copy)]
struct CropDrag {
    handle: CropHandle,
    start: (f64, f64),
    rect: NormalizedCropRect,
}

/// Crop authoring state. The committed recipe is never mutated until the
/// tool is left with a commit (see `ActiveTool::leave`).
#[derive(Debug, Clone)]
pub struct CropToolSession {
    /// The recipe's crop.
    committed: Option<NormalizedCropRect>,
    draft: NormalizedCropRect,
    /// The draft as last drawn by hand. Ratios are fitted into it, so that trying one after the
    /// other does not shrink the draft.
    drawn: NormalizedCropRect,
    aspect: CropAspect,
    /// The ratio locked while the aspect is `Free`, in pixels and before it was flipped.
    locked_ratio: Option<f64>,
    portrait: bool,
    full_width: usize,
    full_height: usize,
    drag: Option<CropDrag>,
}

impl CropToolSession {
    /// `full_width` and `full_height` are the photo's size without the crop.
    pub fn new(
        committed: Option<NormalizedCropRect>,
        full_width: usize,
        full_height: usize,
    ) -> Self {
        Self {
            committed,
            draft: committed.unwrap_or(NormalizedCropRect::FULL_FRAME),
            drawn: committed.unwrap_or(NormalizedCropRect::FULL_FRAME),
            aspect: CropAspect::Free,
            locked_ratio: None,
            portrait: false,
            full_width: full_width.max(1),
            full_height: full_height.max(1),
            drag: None,
        }
    }

    pub const fn draft(&self) -> NormalizedCropRect {
        self.draft
    }

    pub const fn aspect(&self) -> CropAspect {
        self.aspect
    }

    /// Whether the draft is constrained to a ratio: a preset's, or the one it was locked at.
    pub fn locked(&self) -> bool {
        self.aspect != CropAspect::Free || self.locked_ratio.is_some()
    }

    /// Whether the ratio is flipped.
    pub const fn portrait(&self) -> bool {
        self.portrait
    }

    /// Choose a ratio and lock it. `Free` unlocks and keeps the draft.
    pub fn set_aspect(&mut self, aspect: CropAspect) {
        // Flipped ratios stay flipped from one preset to the next.
        let portrait = self.portrait;
        self.clear_ratio();
        if aspect == CropAspect::Free {
            return;
        }
        self.aspect = aspect;
        self.portrait = portrait;
        self.constrain_draft();
    }

    /// Lock the draft's current ratio, or stop constraining it.
    pub fn set_locked(&mut self, locked: bool) {
        if locked == self.locked() {
            return;
        }
        self.clear_ratio();
        if locked {
            // Both edges have a minimum length, so this is a finite ratio.
            let pixel_ratio = ((self.draft.right - self.draft.left) * self.full_width as f64)
                / ((self.draft.bottom - self.draft.top) * self.full_height as f64);
            self.locked_ratio = Some(pixel_ratio);
        }
    }

    /// Swap the sides of the ratio. Without one there is nothing to flip.
    pub fn flip_ratio(&mut self) {
        if self.locked() {
            self.portrait = !self.portrait;
            self.constrain_draft();
        }
    }

    /// Back to the whole photo, at any ratio.
    pub fn reset(&mut self) {
        self.set_rect(NormalizedCropRect::FULL_FRAME);
        self.clear_ratio();
    }

    fn set_rect(&mut self, rect: NormalizedCropRect) {
        self.draft = rect;
        self.drawn = rect;
        self.drag = None;
    }

    fn clear_ratio(&mut self) {
        self.aspect = CropAspect::Free;
        self.locked_ratio = None;
        self.portrait = false;
    }

    /// The recipe value to commit: `None` if the draft keeps every pixel, as it may lie a
    /// rounding error inside the photo after fitting a ratio.
    #[must_use]
    pub fn committed_crop(&self) -> Option<NormalizedCropRect> {
        (self.output_dimensions() != (self.full_width, self.full_height)).then_some(self.draft)
    }

    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.committed_crop() != self.committed
    }

    /// Drop the changes to the draft. Returns whether there were any.
    pub fn discard_changes(&mut self) -> bool {
        let modified = self.is_modified();
        self.load(self.committed);
        modified
    }

    /// Follow a recipe whose crop was changed by something else than this tool.
    pub fn sync(&mut self, committed: Option<NormalizedCropRect>) {
        if committed != self.committed {
            self.load(committed);
        }
    }

    fn load(&mut self, committed: Option<NormalizedCropRect>) {
        self.committed = committed;
        self.set_rect(committed.unwrap_or(NormalizedCropRect::FULL_FRAME));
        // A ratio that the loaded crop may not have.
        self.clear_ratio();
    }

    /// The size of the cropped photo: its edges fall on the same pixels as when it is rendered.
    pub fn output_dimensions(&self) -> (usize, usize) {
        let length = |from: f64, to: f64, full: usize| {
            let full = full as f64;
            ((to * full).round() - (from * full).round()).max(1.0) as usize
        };
        (
            length(self.draft.left, self.draft.right, self.full_width),
            length(self.draft.top, self.draft.bottom, self.full_height),
        )
    }

    pub fn begin_drag(&mut self, handle: CropHandle, point: (f64, f64)) {
        self.drag = Some(CropDrag {
            handle,
            start: point,
            rect: self.draft,
        });
    }

    /// `point` may lie outside the photo. What was grabbed keeps its distance to the pointer, so
    /// that grabbing a handle beside its center does not move it.
    pub fn drag_to(&mut self, point: (f64, f64)) {
        let Some(drag) = self.drag else {
            return;
        };
        let dx = point.0 - drag.start.0;
        let dy = point.1 - drag.start.1;
        let Some((x, y)) = drag.handle.position(drag.rect) else {
            self.draft = translate(drag.rect, dx, dy);
            self.drawn = self.draft;
            return;
        };
        let point = (x + dx, y + dy);
        self.draft = match self.normalized_ratio() {
            Some(ratio) => resize_locked(drag.rect, drag.handle, point, ratio),
            None => resize_free(drag.rect, drag.handle, point),
        };
        self.drawn = self.draft;
    }

    pub fn finish_drag(&mut self) {
        self.drag = None;
    }

    pub fn active_handle(&self) -> Option<CropHandle> {
        self.drag.map(|drag| drag.handle)
    }

    fn normalized_ratio(&self) -> Option<f64> {
        // Pixel-space ratio first, exactly like the preset path: presets are
        // flipped in pixel space inside `CropAspect::ratio`, a Free-locked
        // session flips its captured base form here, and both then share the
        // single normalized conversion.
        let pixel_ratio = match self.aspect {
            CropAspect::Free => self
                .locked_ratio
                .map(|ratio| if self.portrait { 1.0 / ratio } else { ratio }),
            _ => self.aspect.ratio(
                self.full_width as f64 / self.full_height as f64,
                self.portrait,
            ),
        };
        pixel_ratio
            .map(|pixel_ratio| pixel_ratio * self.full_height as f64 / self.full_width as f64)
    }

    fn constrain_draft(&mut self) {
        let Some(ratio) = self.normalized_ratio() else {
            return;
        };
        let center_x = (self.drawn.left + self.drawn.right) * 0.5;
        let center_y = (self.drawn.top + self.drawn.bottom) * 0.5;
        let mut width = self.drawn.right - self.drawn.left;
        let mut height = self.drawn.bottom - self.drawn.top;
        if width / height > ratio {
            width = height * ratio;
        } else {
            height = width / ratio;
        }
        self.draft = fit_rect(center_x, center_y, width, height);
    }
}

#[cfg(test)]
mod tests {
    use super::{CropAspect, CropHandle, CropToolSession};

    /// The draft's ratio in pixel coordinates: `(Δx·W)/(Δy·H)`.
    fn pixel_ratio(session: &CropToolSession, full_width: usize, full_height: usize) -> f64 {
        let rect = session.draft();
        ((rect.right - rect.left) * full_width as f64)
            / ((rect.bottom - rect.top) * full_height as f64)
    }

    /// 6000×4000 session, free-resized on SouthEast to a 3000×1600 draft
    /// (pixel ratio 1.875), the fixture shared by the capture tests.
    fn session_resized_to_1875() -> CropToolSession {
        let mut session = CropToolSession::new(None, 6000, 4000);
        session.begin_drag(CropHandle::SouthEast, (1.0, 1.0));
        session.drag_to((0.5, 0.4));
        session.finish_drag();
        session
    }

    #[test]
    fn moving_and_free_resizing_stay_in_bounds_without_inversion() {
        let mut session = CropToolSession::new(None, 6000, 4000);
        session.begin_drag(CropHandle::NorthWest, (0.0, 0.0));
        session.drag_to((0.9, 0.9));
        session.finish_drag();
        let crop = session.draft();
        assert!(crop.left < crop.right && crop.top < crop.bottom);
        session.begin_drag(CropHandle::Move, (crop.left, crop.top));
        session.drag_to((-2.0, 3.0));
        session.finish_drag();
        let crop = session.draft();
        assert!(crop.left >= 0.0 && crop.top >= 0.0 && crop.right <= 1.0 && crop.bottom <= 1.0);
    }

    #[test]
    fn locked_ratio_is_preserved_in_pixel_coordinates() {
        let mut session = CropToolSession::new(None, 6000, 4000);
        session.set_aspect(CropAspect::ThreeTwo);
        session.begin_drag(CropHandle::NorthWest, (0.0, 0.0));
        session.drag_to((0.35, 0.25));
        session.finish_drag();
        let crop = session.draft();
        let ratio = (crop.right - crop.left) * 6000.0 / ((crop.bottom - crop.top) * 4000.0);
        assert!((ratio - 1.5).abs() < 1.0e-9);
    }

    #[test]
    fn reset_canonicalizes_to_a_neutral_recipe_crop() {
        let mut session = CropToolSession::new(None, 20, 10);
        session.set_aspect(CropAspect::Square);
        assert!(session.committed_crop().is_some());
        session.reset();
        assert_eq!(session.committed_crop(), None);
    }

    #[test]
    fn the_draft_follows_the_recipe_only_when_its_crop_changed() {
        let mut session = session_resized_to_1875();
        let draft = session.draft();
        session.sync(None);
        assert_eq!(session.draft(), draft, "an edit elsewhere keeps the draft");
        session.sync(Some(draft));
        assert!(!session.is_modified());
        session.set_aspect(CropAspect::Square);
        assert!(session.discard_changes());
        assert_eq!((session.draft(), session.locked()), (draft, false));
        assert!(!session.discard_changes());
    }

    #[test]
    fn the_size_is_that_of_the_rendered_crop() {
        let crop = rohditor_edit::NormalizedCropRect {
            left: 0.25,
            top: 0.1,
            right: 0.75,
            bottom: 0.9,
        };
        // 2.5 to 7.5 and 0.7 to 6.3 pixels.
        let session = CropToolSession::new(Some(crop), 10, 7);
        let rendered = rohditor_core::OutputGeometry::new(
            10,
            7,
            rohditor_image::Orientation::Normal,
            Some(crop),
        )
        .expect("valid crop");
        assert_eq!(session.output_dimensions(), rendered.output_dimensions());
        assert_eq!(session.output_dimensions(), (5, 5));
    }

    #[test]
    fn lock_on_free_preserves_draft_pixel_ratio_corner() {
        let mut session = session_resized_to_1875();
        let before = session.draft();
        session.set_locked(true);
        session.begin_drag(CropHandle::NorthWest, (before.left, before.top));
        session.drag_to((0.1, 0.2));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.875).abs() < 1.0e-9);
        assert_ne!(session.draft(), before);
    }

    #[test]
    fn lock_on_free_preserves_ratio_for_edge_handles() {
        // Width-driven edge: East.
        let mut session = session_resized_to_1875();
        session.set_locked(true);
        session.begin_drag(CropHandle::East, (0.5, 0.2));
        session.drag_to((0.8, 0.5));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.875).abs() < 1.0e-9);
        // Height-driven edge: South, on a fresh session.
        let mut session = session_resized_to_1875();
        session.set_locked(true);
        session.begin_drag(CropHandle::South, (0.25, 0.4));
        session.drag_to((0.25, 0.7));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.875).abs() < 1.0e-9);
    }

    #[test]
    fn lock_on_free_works_on_portrait_image() {
        // The capture is not landscape-biased: a physically portrait image.
        let mut session = CropToolSession::new(None, 4000, 6000);
        session.begin_drag(CropHandle::SouthEast, (1.0, 1.0));
        session.drag_to((0.4, 0.5));
        session.finish_drag();
        session.set_locked(true);
        session.begin_drag(CropHandle::NorthWest, (0.0, 0.0));
        session.drag_to((0.1, 0.1));
        session.finish_drag();
        assert!((pixel_ratio(&session, 4000, 6000) - 8.0 / 15.0).abs() < 1.0e-9);
    }

    #[test]
    fn captured_ratio_flips() {
        // Orientation flips happen in pixel space, as with presets. Storing
        // the capture in normalized units would yield 1.2 here, not 0.5333.
        let mut session = session_resized_to_1875();
        session.set_locked(true);
        session.flip_ratio();
        let flipped = 1.0 / 1.875;
        assert!((pixel_ratio(&session, 6000, 4000) - flipped).abs() < 1.0e-9);
        session.begin_drag(CropHandle::NorthWest, (session.draft().left, 0.0));
        session.drag_to((0.05, 0.05));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - flipped).abs() < 1.0e-9);
        session.flip_ratio();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.875).abs() < 1.0e-9);
    }

    #[test]
    fn selecting_free_unlocks_and_keeps_draft() {
        let mut session = CropToolSession::new(None, 6000, 4000);
        session.set_aspect(CropAspect::ThreeTwo);
        session.begin_drag(CropHandle::NorthWest, (0.0, 0.0));
        session.drag_to((0.2, 0.2));
        session.finish_drag();
        let before = session.draft();
        session.set_aspect(CropAspect::Free);
        assert!(!session.locked());
        assert_eq!(session.draft(), before);
        session.begin_drag(CropHandle::SouthEast, (1.0, 1.0));
        session.drag_to((0.6, 0.4));
        session.finish_drag();
        let ratio = pixel_ratio(&session, 6000, 4000);
        assert!((ratio - 1.5).abs() > 1.0e-9);
    }

    #[test]
    fn preset_after_free_capture_wins() {
        let mut session = session_resized_to_1875();
        session.set_locked(true);
        session.set_aspect(CropAspect::Square);
        assert!(session.locked());
        let draft = session.draft();
        session.begin_drag(CropHandle::NorthWest, (draft.left, draft.top));
        session.drag_to((0.05, 0.05));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.0).abs() < 1.0e-9);
    }

    #[test]
    fn reticking_lock_recaptures_current_draft() {
        let mut session = session_resized_to_1875();
        session.set_locked(true);
        session.set_locked(false);
        session.begin_drag(CropHandle::NorthWest, (0.0, 0.0));
        session.drag_to((0.2, 0.05));
        session.finish_drag();
        // Untouched the ratio drifted off 1.875; reticking pins the new one.
        session.set_locked(true);
        let draft = session.draft();
        session.begin_drag(CropHandle::NorthWest, (draft.left, draft.top));
        session.drag_to((0.1, 0.02));
        session.finish_drag();
        let ratio = pixel_ratio(&session, 6000, 4000);
        // 1800x1400 px = (0.5-0.2)*6000 x (0.4-0.05)*4000 captured at retick -> 1800/1400 = 9/7
        assert!((ratio - 9.0 / 7.0).abs() < 1.0e-9);
        assert!((ratio - 1.875).abs() > 1.0e-9);
    }

    #[test]
    fn reselecting_free_unlocks_captured_lock() {
        // `set_aspect` runs unconditionally: re-clicking the selected Free
        // preset also unlocks a captured lock.
        let mut session = session_resized_to_1875();
        session.set_locked(true);
        let before = session.draft();
        session.set_aspect(CropAspect::Free);
        assert!(!session.locked());
        assert_eq!(session.draft(), before);
    }

    #[test]
    fn lock_on_full_frame_locks_image_aspect() {
        let mut session = CropToolSession::new(None, 6000, 4000);
        session.set_locked(true);
        session.begin_drag(CropHandle::NorthWest, (0.0, 0.0));
        session.drag_to((0.25, 0.15));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.5).abs() < 1.0e-9);
    }

    #[test]
    fn locked_preset_edge_and_orientation_paths() {
        let mut session = CropToolSession::new(None, 6000, 4000);
        session.set_aspect(CropAspect::ThreeTwo);
        session.begin_drag(CropHandle::East, (1.0, 0.5));
        session.drag_to((0.7, 0.5));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.5).abs() < 1.0e-9);

        let mut session = CropToolSession::new(None, 6000, 4000);
        session.set_aspect(CropAspect::Square);
        session.begin_drag(CropHandle::South, (0.5, 1.0));
        session.drag_to((0.5, 0.6));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.0).abs() < 1.0e-9);

        let mut session = CropToolSession::new(None, 6000, 4000);
        session.set_aspect(CropAspect::Original);
        session.flip_ratio();
        assert!((pixel_ratio(&session, 6000, 4000) - 2.0 / 3.0).abs() < 1.0e-9);
    }

    #[test]
    fn trying_ratios_does_not_shrink_the_crop() {
        let mut session = session_resized_to_1875();
        session.set_aspect(CropAspect::Square);
        let square = session.draft();
        session.set_aspect(CropAspect::SixteenNine);
        session.flip_ratio();
        session.flip_ratio();
        session.set_aspect(CropAspect::Square);
        assert_eq!(session.draft(), square);
        assert_eq!(session.output_dimensions(), (1600, 1600));
        // A crop drawn at a ratio is what the next one is fitted into.
        session.begin_drag(CropHandle::SouthEast, (square.right, square.bottom));
        session.drag_to((square.right - 0.1, square.bottom - 0.15));
        session.finish_drag();
        session.set_aspect(CropAspect::Original);
        assert_eq!(session.output_dimensions(), (1000, 666));
    }

    #[test]
    fn unlocking_keeps_the_crop_and_resetting_clears_the_ratio() {
        let mut session = CropToolSession::new(None, 6000, 4000);
        session.set_aspect(CropAspect::FourFive);
        session.flip_ratio();
        let draft = session.draft();
        session.set_locked(false);
        assert_eq!(
            (session.draft(), session.aspect()),
            (draft, CropAspect::Free)
        );
        session.flip_ratio();
        assert_eq!(session.draft(), draft, "there is no ratio to flip");
        session.set_locked(true);
        assert_eq!(session.draft(), draft);
        session.reset();
        assert!(!session.locked() && !session.portrait() && !session.is_modified());
        assert_eq!(session.output_dimensions(), (6000, 4000));
    }

    #[test]
    fn fitting_the_photos_own_ratio_is_no_crop() {
        for (width, height) in [(6000, 4000), (6048, 4024), (4672, 7008), (6001, 3999)] {
            let mut session = CropToolSession::new(None, width, height);
            session.set_aspect(CropAspect::Original);
            assert_eq!(session.committed_crop(), None, "{width} × {height}");
        }
    }
}
