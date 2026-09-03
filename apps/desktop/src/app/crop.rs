use rohditor_edit::NormalizedCropRect;

use crate::ui::crop::CropHandle;

const MINIMUM_NORMALIZED_EDGE: f64 = 0.002;

/// The aspect constraint chosen by the crop authoring UI. It is intentionally
/// not stored in the recipe: only the resulting rectangle changes pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum CropAspect {
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
    pub(crate) const ALL: [Self; 8] = [
        Self::Free,
        Self::Original,
        Self::Square,
        Self::ThreeTwo,
        Self::FourThree,
        Self::FourFive,
        Self::FiveSeven,
        Self::SixteenNine,
    ];

    pub(crate) const fn label(self) -> &'static str {
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

/// UI-only crop state. The committed recipe is never mutated until Apply.
#[derive(Debug, Clone)]
pub(crate) struct CropToolSession {
    original: Option<NormalizedCropRect>,
    draft: NormalizedCropRect,
    aspect: CropAspect,
    locked: bool,
    portrait: bool,
    full_width: usize,
    full_height: usize,
    full_frame_ready: bool,
    drag: Option<CropDrag>,
}

impl CropToolSession {
    pub(crate) fn new(
        committed: Option<NormalizedCropRect>,
        full_width: usize,
        full_height: usize,
    ) -> Self {
        Self {
            original: committed,
            draft: committed.unwrap_or(NormalizedCropRect::FULL_FRAME),
            aspect: CropAspect::Free,
            locked: false,
            portrait: false,
            full_width,
            full_height,
            full_frame_ready: false,
            drag: None,
        }
    }

    pub(crate) const fn draft(&self) -> NormalizedCropRect {
        self.draft
    }

    pub(crate) const fn aspect(&self) -> CropAspect {
        self.aspect
    }

    pub(crate) const fn locked(&self) -> bool {
        self.locked
    }

    pub(crate) const fn portrait(&self) -> bool {
        self.portrait
    }

    pub(crate) const fn full_frame_ready(&self) -> bool {
        self.full_frame_ready
    }

    pub(crate) fn set_full_dimensions(&mut self, width: usize, height: usize) {
        self.full_width = width.max(1);
        self.full_height = height.max(1);
        self.constrain_draft();
        self.full_frame_ready = true;
    }

    pub(crate) fn set_aspect(&mut self, aspect: CropAspect) {
        self.aspect = aspect;
        self.locked = aspect != CropAspect::Free;
        self.constrain_draft();
    }

    pub(crate) fn set_locked(&mut self, locked: bool) {
        self.locked = locked;
        self.constrain_draft();
    }

    pub(crate) fn toggle_orientation(&mut self) {
        self.portrait = !self.portrait;
        self.constrain_draft();
    }

    pub(crate) fn reset(&mut self) {
        self.draft = NormalizedCropRect::FULL_FRAME;
        self.drag = None;
    }

    /// The recipe value to commit. Full frame is canonicalized to `None`.
    #[must_use]
    pub(crate) fn committed_crop(&self) -> Option<NormalizedCropRect> {
        self.draft.canonicalized()
    }

    #[must_use]
    pub(crate) fn is_modified(&self) -> bool {
        self.committed_crop() != self.original
    }

    pub(crate) fn output_dimensions(&self) -> (usize, usize) {
        let width = ((self.draft.right - self.draft.left) * self.full_width as f64).round();
        let height = ((self.draft.bottom - self.draft.top) * self.full_height as f64).round();
        (width.max(1.0) as usize, height.max(1.0) as usize)
    }

    pub(crate) fn begin_drag(&mut self, handle: CropHandle, point: (f64, f64)) {
        self.drag = Some(CropDrag {
            handle,
            start: point,
            rect: self.draft,
        });
    }

    pub(crate) fn drag_to(&mut self, point: (f64, f64)) {
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

    pub(crate) fn finish_drag(&mut self) {
        self.drag = None;
    }

    pub(crate) fn active_handle(&self) -> Option<CropHandle> {
        self.drag.map(|drag| drag.handle)
    }

    fn normalized_ratio(&self) -> Option<f64> {
        self.aspect
            .ratio(
                self.full_width as f64 / self.full_height as f64,
                self.portrait,
            )
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
    use super::{CropAspect, CropToolSession};
    use crate::ui::crop::CropHandle;

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
}
