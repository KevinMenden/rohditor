use rohditor_edit::NormalizedCropRect;

const MINIMUM_NORMALIZED_EDGE: f64 = 0.002;

/// Which part of the crop rectangle a pointer gesture grabbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropHandle {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    Move,
}

/// The aspect constraint chosen by the crop authoring UI. It is intentionally
/// not stored in the recipe: only the resulting rectangle changes pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CropAspect {
    #[default]
    Free,
    Original,
    Square,
    ThreeTwo,
    FourThree,
    FourFive,
    FiveSeven,
    SixteenNine,
}

impl CropAspect {
    pub const ALL: [Self; 8] = [
        Self::Free,
        Self::Original,
        Self::Square,
        Self::ThreeTwo,
        Self::FourThree,
        Self::FourFive,
        Self::FiveSeven,
        Self::SixteenNine,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Original => "Original",
            Self::Square => "1:1",
            Self::ThreeTwo => "3:2",
            Self::FourThree => "4:3",
            Self::FourFive => "4:5",
            Self::FiveSeven => "5:7",
            Self::SixteenNine => "16:9",
        }
    }

    const fn ratio(self, original_ratio: f64, portrait: bool) -> Option<f64> {
        let landscape = match self {
            Self::Free => return None,
            Self::Original => original_ratio,
            Self::Square => 1.0,
            Self::ThreeTwo => 3.0 / 2.0,
            Self::FourThree => 4.0 / 3.0,
            Self::FourFive => 4.0 / 5.0,
            Self::FiveSeven => 5.0 / 7.0,
            Self::SixteenNine => 16.0 / 9.0,
        };
        Some(if portrait { 1.0 / landscape } else { landscape })
    }
}

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
    original: Option<NormalizedCropRect>,
    draft: NormalizedCropRect,
    aspect: CropAspect,
    locked: bool,
    /// Ratio captured when the lock is ticked while the preset is `Free`,
    /// stored in pixel units in landscape base form (the effective ratio is
    /// `1.0 / locked_ratio` while portrait), mirroring how `CropAspect::ratio`
    /// keeps preset ratios in pixel space before the normalized conversion.
    /// Preset-locked drags never consult this.
    locked_ratio: Option<f64>,
    portrait: bool,
    full_width: usize,
    full_height: usize,
    full_frame_ready: bool,
    drag: Option<CropDrag>,
}

impl CropToolSession {
    pub fn new(
        committed: Option<NormalizedCropRect>,
        full_width: usize,
        full_height: usize,
    ) -> Self {
        Self {
            original: committed,
            draft: committed.unwrap_or(NormalizedCropRect::FULL_FRAME),
            aspect: CropAspect::Free,
            locked: false,
            locked_ratio: None,
            portrait: false,
            full_width,
            full_height,
            full_frame_ready: false,
            drag: None,
        }
    }

    pub const fn draft(&self) -> NormalizedCropRect {
        self.draft
    }

    pub const fn aspect(&self) -> CropAspect {
        self.aspect
    }

    pub const fn locked(&self) -> bool {
        self.locked
    }

    pub const fn portrait(&self) -> bool {
        self.portrait
    }

    pub const fn full_frame_ready(&self) -> bool {
        self.full_frame_ready
    }

    pub fn set_full_dimensions(&mut self, width: usize, height: usize) {
        self.full_width = width.max(1);
        self.full_height = height.max(1);
        self.constrain_draft();
        self.full_frame_ready = true;
    }

    pub fn set_aspect(&mut self, aspect: CropAspect) {
        self.aspect = aspect;
        self.locked = aspect != CropAspect::Free;
        // The ratio derives from the preset from here on; a Free switch also
        // unlocks, so the preset row and the checkbox can never disagree.
        self.locked_ratio = None;
        self.constrain_draft();
    }

    /// Ticking the lock on the `Free` preset captures the draft's current
    /// ratio (portrait-aware, so the effective ratio equals the draft ratio
    /// at tick time and the capture causes no snap); unticking clears it.
    pub fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
        self.locked_ratio = if locked && self.aspect == CropAspect::Free {
            // Dragged rects keep both edges >= MINIMUM_NORMALIZED_EDGE and presets
            // are fit-to-frame, so this division can't produce inf/NaN.
            let pixel_ratio = ((self.draft.right - self.draft.left) * self.full_width as f64)
                / ((self.draft.bottom - self.draft.top) * self.full_height as f64);
            Some(if self.portrait {
                1.0 / pixel_ratio
            } else {
                pixel_ratio
            })
        } else {
            None
        };
        self.constrain_draft();
    }

    pub fn toggle_orientation(&mut self) {
        self.portrait = !self.portrait;
        self.constrain_draft();
    }

    pub fn reset(&mut self) {
        self.draft = NormalizedCropRect::FULL_FRAME;
        self.drag = None;
    }

    /// The recipe value to commit. Full frame is canonicalized to `None`.
    #[must_use]
    pub fn committed_crop(&self) -> Option<NormalizedCropRect> {
        self.draft.canonicalized()
    }

    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.committed_crop() != self.original
    }

    pub fn output_dimensions(&self) -> (usize, usize) {
        let width = ((self.draft.right - self.draft.left) * self.full_width as f64).round();
        let height = ((self.draft.bottom - self.draft.top) * self.full_height as f64).round();
        (width.max(1.0) as usize, height.max(1.0) as usize)
    }

    pub fn begin_drag(&mut self, handle: CropHandle, point: (f64, f64)) {
        self.drag = Some(CropDrag {
            handle,
            start: point,
            rect: self.draft,
        });
    }

    pub fn drag_to(&mut self, point: (f64, f64)) {
        let Some(drag) = self.drag else {
            return;
        };
        let dx = point.0 - drag.start.0;
        let dy = point.1 - drag.start.1;
        self.draft = if drag.handle == CropHandle::Move {
            translate(drag.rect, dx, dy)
        } else if self.locked {
            resize_locked(drag.rect, drag.handle, point, self.normalized_ratio())
        } else {
            resize_free(drag.rect, drag.handle, point)
        };
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
        let center_x = (self.draft.left + self.draft.right) * 0.5;
        let center_y = (self.draft.top + self.draft.bottom) * 0.5;
        let mut width = self.draft.right - self.draft.left;
        let mut height = self.draft.bottom - self.draft.top;
        if width / height > ratio {
            width = height * ratio;
        } else {
            height = width / ratio;
        }
        self.draft = fit_rect(center_x, center_y, width, height);
    }
}

fn translate(rect: NormalizedCropRect, dx: f64, dy: f64) -> NormalizedCropRect {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let left = (rect.left + dx).clamp(0.0, 1.0 - width);
    let top = (rect.top + dy).clamp(0.0, 1.0 - height);
    NormalizedCropRect {
        left,
        top,
        right: left + width,
        bottom: top + height,
    }
}

fn resize_free(
    rect: NormalizedCropRect,
    handle: CropHandle,
    point: (f64, f64),
) -> NormalizedCropRect {
    let mut next = rect;
    let x = point.0.clamp(0.0, 1.0);
    let y = point.1.clamp(0.0, 1.0);
    match handle {
        CropHandle::NorthWest | CropHandle::West | CropHandle::SouthWest => {
            next.left = x.min(rect.right - MINIMUM_NORMALIZED_EDGE)
        }
        CropHandle::NorthEast | CropHandle::East | CropHandle::SouthEast => {
            next.right = x.max(rect.left + MINIMUM_NORMALIZED_EDGE)
        }
        CropHandle::North | CropHandle::South | CropHandle::Move => {}
    }
    match handle {
        CropHandle::NorthWest | CropHandle::North | CropHandle::NorthEast => {
            next.top = y.min(rect.bottom - MINIMUM_NORMALIZED_EDGE)
        }
        CropHandle::SouthWest | CropHandle::South | CropHandle::SouthEast => {
            next.bottom = y.max(rect.top + MINIMUM_NORMALIZED_EDGE)
        }
        CropHandle::West | CropHandle::East | CropHandle::Move => {}
    }
    next
}

fn resize_locked(
    rect: NormalizedCropRect,
    handle: CropHandle,
    point: (f64, f64),
    ratio: Option<f64>,
) -> NormalizedCropRect {
    let Some(ratio) = ratio else {
        return resize_free(rect, handle, point);
    };
    if handle == CropHandle::Move {
        return translate(rect, point.0 - rect.left, point.1 - rect.top);
    }
    let (anchor_x, anchor_y) = match handle {
        CropHandle::NorthWest => (rect.right, rect.bottom),
        CropHandle::NorthEast => (rect.left, rect.bottom),
        CropHandle::SouthWest => (rect.right, rect.top),
        CropHandle::SouthEast => (rect.left, rect.top),
        CropHandle::West => (rect.right, (rect.top + rect.bottom) * 0.5),
        CropHandle::East => (rect.left, (rect.top + rect.bottom) * 0.5),
        CropHandle::North => ((rect.left + rect.right) * 0.5, rect.bottom),
        CropHandle::South => ((rect.left + rect.right) * 0.5, rect.top),
        CropHandle::Move => unreachable!(),
    };
    let requested_width = (point.0 - anchor_x).abs();
    let requested_height = (point.1 - anchor_y).abs();
    let width = match handle {
        CropHandle::West | CropHandle::East => requested_width,
        CropHandle::North | CropHandle::South => requested_height * ratio,
        _ => requested_width.max(requested_height * ratio),
    }
    .max(MINIMUM_NORMALIZED_EDGE);
    let height = (width / ratio).max(MINIMUM_NORMALIZED_EDGE);
    let center_x = match handle {
        CropHandle::West | CropHandle::East | CropHandle::North | CropHandle::South => anchor_x,
        CropHandle::NorthWest | CropHandle::SouthWest => anchor_x - width * 0.5,
        CropHandle::NorthEast | CropHandle::SouthEast => anchor_x + width * 0.5,
        CropHandle::Move => unreachable!(),
    };
    let center_y = match handle {
        CropHandle::North | CropHandle::South | CropHandle::West | CropHandle::East => anchor_y,
        CropHandle::NorthWest | CropHandle::NorthEast => anchor_y - height * 0.5,
        CropHandle::SouthWest | CropHandle::SouthEast => anchor_y + height * 0.5,
        CropHandle::Move => unreachable!(),
    };
    fit_rect(center_x, center_y, width, height)
}

fn fit_rect(center_x: f64, center_y: f64, width: f64, height: f64) -> NormalizedCropRect {
    let scale = (1.0_f64).min(1.0 / width).min(1.0 / height);
    let width = width * scale;
    let height = height * scale;
    let left = (center_x - width * 0.5).clamp(0.0, 1.0 - width);
    let top = (center_y - height * 0.5).clamp(0.0, 1.0 - height);
    NormalizedCropRect {
        left,
        top,
        right: left + width,
        bottom: top + height,
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
    fn captured_ratio_flips_with_orientation() {
        // Orientation flips happen in pixel space, as with presets. Storing
        // the capture in normalized units would yield 1.2 here, not 0.5333.
        let mut session = session_resized_to_1875();
        session.set_locked(true);
        session.toggle_orientation();
        let flipped = 1.0 / 1.875;
        assert!((pixel_ratio(&session, 6000, 4000) - flipped).abs() < 1.0e-9);
        session.begin_drag(CropHandle::NorthWest, (session.draft().left, 0.0));
        session.drag_to((0.05, 0.05));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - flipped).abs() < 1.0e-9);
        session.toggle_orientation();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.875).abs() < 1.0e-9);
    }

    #[test]
    fn lock_captured_while_portrait_keeps_screen_ratio() {
        let mut session = CropToolSession::new(None, 6000, 4000);
        session.toggle_orientation();
        session.begin_drag(CropHandle::SouthEast, (1.0, 1.0));
        session.drag_to((0.5, 0.4));
        session.finish_drag();
        // The portrait capture arm stores 1/1.875 so the effective ratio
        // stays the on-screen 1.875: locking causes no snap.
        session.set_locked(true);
        session.begin_drag(CropHandle::NorthWest, (0.0, 0.0));
        session.drag_to((0.1, 0.2));
        session.finish_drag();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.875).abs() < 1.0e-9);
        session.toggle_orientation();
        assert!((pixel_ratio(&session, 6000, 4000) - 1.0 / 1.875).abs() < 1.0e-9);
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
        session.toggle_orientation();
        assert!((pixel_ratio(&session, 6000, 4000) - 2.0 / 3.0).abs() < 1.0e-9);
    }
}
