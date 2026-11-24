//! Moving and resizing a crop rectangle within the photo.

use rohditor_edit::NormalizedCropRect;

use super::CropHandle;

const MINIMUM_NORMALIZED_EDGE: f64 = 0.002;

pub(super) fn translate(rect: NormalizedCropRect, dx: f64, dy: f64) -> NormalizedCropRect {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let left = (rect.left + dx).clamp(0.0, 1.0 - width);
    let top = (rect.top + dy).clamp(0.0, 1.0 - height);
    NormalizedCropRect {
        left,
        top,
        // The sum may round up to just over the photo's edge.
        right: (left + width).min(1.0),
        bottom: (top + height).min(1.0),
    }
}

pub(super) fn resize_free(
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

/// Resize to `ratio` (width over height, both normalized). What lies opposite the handle stays
/// in place: a corner drag keeps the other corner, an edge drag the other edge, where the crop
/// grows to both sides and moves along that edge once it reaches the end of the photo.
pub(super) fn resize_locked(
    rect: NormalizedCropRect,
    handle: CropHandle,
    point: (f64, f64),
    ratio: f64,
) -> NormalizedCropRect {
    use CropHandle::{East, North, NorthEast, NorthWest, South, SouthEast, SouthWest, West};
    let horizontal = match handle {
        NorthWest | West | SouthWest => Anchor::End(rect.right),
        NorthEast | East | SouthEast => Anchor::Start(rect.left),
        North | South | CropHandle::Move => Anchor::Center((rect.left + rect.right) * 0.5),
    };
    let vertical = match handle {
        NorthWest | North | NorthEast => Anchor::End(rect.bottom),
        SouthWest | South | SouthEast => Anchor::Start(rect.top),
        West | East | CropHandle::Move => Anchor::Center((rect.top + rect.bottom) * 0.5),
    };
    // Past the anchor is not a direction to grow in.
    let wanted_width = horizontal.length_to(point.0);
    let wanted_height = vertical.length_to(point.1);
    let width = wanted_width
        .max(wanted_height * ratio)
        .max(MINIMUM_NORMALIZED_EDGE)
        .max(MINIMUM_NORMALIZED_EDGE * ratio)
        .min(horizontal.space())
        .min(vertical.space() * ratio);
    let (left, right) = horizontal.span(width);
    let (top, bottom) = vertical.span(width / ratio);
    NormalizedCropRect {
        left,
        top,
        right,
        bottom,
    }
}

/// What stays in place along one axis.
#[derive(Clone, Copy)]
enum Anchor {
    Start(f64),
    Center(f64),
    End(f64),
}

impl Anchor {
    /// The length that brings the dragged side to `position`.
    fn length_to(self, position: f64) -> f64 {
        match self {
            Self::Start(start) => position - start,
            Self::Center(_) => 0.0,
            Self::End(end) => end - position,
        }
    }

    /// The longest length that fits the photo.
    fn space(self) -> f64 {
        match self {
            Self::Start(start) => 1.0 - start,
            Self::Center(_) => 1.0,
            Self::End(end) => end,
        }
    }

    fn span(self, length: f64) -> (f64, f64) {
        match self {
            Self::Start(start) => (start, (start + length).min(1.0)),
            Self::Center(center) => {
                let start = (center - length * 0.5).min(1.0 - length).max(0.0);
                (start, (start + length).min(1.0))
            }
            Self::End(end) => ((end - length).max(0.0), end),
        }
    }
}

pub(super) fn fit_rect(
    center_x: f64,
    center_y: f64,
    width: f64,
    height: f64,
) -> NormalizedCropRect {
    let scale = (1.0_f64).min(1.0 / width).min(1.0 / height);
    let width = width * scale;
    let height = height * scale;
    let left = (center_x - width * 0.5).clamp(0.0, 1.0 - width);
    let top = (center_y - height * 0.5).clamp(0.0, 1.0 - height);
    NormalizedCropRect {
        left,
        top,
        // The sum may round up to just over the photo's edge.
        right: (left + width).min(1.0),
        bottom: (top + height).min(1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECT: NormalizedCropRect = NormalizedCropRect {
        left: 0.5,
        top: 0.6,
        right: 0.7,
        bottom: 0.8,
    };

    fn assert_rect(actual: NormalizedCropRect, expected: [f64; 4]) {
        let actual = [actual.left, actual.top, actual.right, actual.bottom];
        let close = std::iter::zip(actual, expected).all(|(a, b)| (a - b).abs() < 1.0e-12);
        assert!(close, "{actual:?} is not {expected:?}");
        assert!(actual.iter().all(|edge| (0.0..=1.0).contains(edge)));
    }

    #[test]
    fn a_locked_corner_stops_at_the_photos_edge_and_keeps_the_opposite_corner() {
        let outside = (1.5, 1.5);
        let resize = |handle, point| resize_locked(RECT, handle, point, 1.0);
        assert_rect(resize(CropHandle::SouthEast, outside), [0.5, 0.6, 0.9, 1.0]);
        assert_rect(
            resize(CropHandle::NorthWest, (-1.0, -1.0)),
            [0.0, 0.1, 0.7, 0.8],
        );
        // Past the opposite corner it does not flip and grow again.
        let tiny = resize(CropHandle::SouthEast, (0.1, 0.1));
        assert_rect(tiny, [0.5, 0.6, 0.502, 0.602]);
    }

    #[test]
    fn a_locked_edge_keeps_the_opposite_edge() {
        let east = resize_locked(RECT, CropHandle::East, (1.2, 0.0), 0.8);
        // 0.5 wide and 0.625 high; centered on 0.7 it would end at 1.0125.
        assert_rect(east, [0.5, 0.375, 1.0, 1.0]);
        let north = resize_locked(RECT, CropHandle::North, (0.0, 0.4), 0.5);
        assert_rect(north, [0.5, 0.4, 0.7, 0.8]);
    }
}
