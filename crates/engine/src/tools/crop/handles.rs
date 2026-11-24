//! Where a crop rectangle can be grabbed.

use rohditor_edit::NormalizedCropRect;

use crate::view::ImagePlacement;

/// How far from a handle or an edge it can be grabbed, in logical pixels.
const GRAB_DISTANCE: f32 = 12.0;

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

impl CropHandle {
    /// Those drawn on the rectangle's outline.
    pub const OUTLINE: [Self; 8] = [
        Self::NorthWest,
        Self::North,
        Self::NorthEast,
        Self::East,
        Self::SouthEast,
        Self::South,
        Self::SouthWest,
        Self::West,
    ];

    /// Where the handle is. `Move` is the whole rectangle and has no position.
    pub fn position(self, crop: NormalizedCropRect) -> Option<(f64, f64)> {
        let x = match self {
            Self::NorthWest | Self::West | Self::SouthWest => crop.left,
            Self::North | Self::South => (crop.left + crop.right) * 0.5,
            Self::NorthEast | Self::East | Self::SouthEast => crop.right,
            Self::Move => return None,
        };
        let y = match self {
            Self::NorthWest | Self::North | Self::NorthEast => crop.top,
            Self::West | Self::East => (crop.top + crop.bottom) * 0.5,
            Self::SouthWest | Self::South | Self::SouthEast => crop.bottom,
            Self::Move => return None,
        };
        Some((x, y))
    }

    /// What a press at `pointer` grabs: the nearest handle, else an edge, else the rectangle. A
    /// full-frame crop has nowhere to move to.
    pub fn at(photo: ImagePlacement, crop: NormalizedCropRect, pointer: [f32; 2]) -> Option<Self> {
        let distance = |handle: Self| {
            let [x, y] = photo.to_screen(handle.position(crop)?);
            Some((handle, (x - pointer[0]).hypot(y - pointer[1])))
        };
        let [left, top] = photo.to_screen((crop.left, crop.top));
        let [right, bottom] = photo.to_screen((crop.right, crop.bottom));
        let beyond = |value: f32, from: f32, to: f32| (from - value).max(value - to);
        let outside = beyond(pointer[0], left, right).max(beyond(pointer[1], top, bottom));
        let nearest = |distances: &mut dyn Iterator<Item = (Self, f32)>| {
            distances
                .filter(|(_, distance)| *distance <= GRAB_DISTANCE)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(handle, _)| handle)
        };
        let edges = [
            (Self::North, (pointer[1] - top).abs()),
            (Self::East, (pointer[0] - right).abs()),
            (Self::South, (pointer[1] - bottom).abs()),
            (Self::West, (pointer[0] - left).abs()),
        ];
        nearest(&mut Self::OUTLINE.into_iter().filter_map(distance))
            .or_else(|| nearest(&mut edges.into_iter().filter(|_| outside <= GRAB_DISTANCE)))
            .or_else(|| (outside <= 0.0 && !crop.is_full_frame()).then_some(Self::Move))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHOTO: ImagePlacement = ImagePlacement {
        origin: [100.0, 50.0],
        size: [400.0, 200.0],
    };

    #[test]
    fn corners_come_before_edges_and_the_inside() {
        // On screen: 200..400 by 100..200.
        let crop = NormalizedCropRect {
            left: 0.25,
            top: 0.25,
            right: 0.75,
            bottom: 0.75,
        };
        let at = |x, y| CropHandle::at(PHOTO, crop, [x, y]);
        assert_eq!(at(201.0, 101.0), Some(CropHandle::NorthWest));
        assert_eq!(at(408.0, 208.0), Some(CropHandle::SouthEast));
        assert_eq!(at(240.0, 95.0), Some(CropHandle::North));
        assert_eq!(at(405.0, 130.0), Some(CropHandle::East));
        assert_eq!(at(300.0, 150.0), Some(CropHandle::Move));
        assert_eq!(at(440.0, 100.0), None, "in line with an edge, beside it");
        assert_eq!(at(300.0, 60.0), None);
    }

    #[test]
    fn the_nearest_handle_of_a_small_crop_is_grabbed() {
        let crop = NormalizedCropRect {
            left: 0.5,
            top: 0.5,
            right: 0.52,
            bottom: 0.54,
        };
        // 8 by 8 logical pixels from (300, 150).
        let grabbed = CropHandle::at(PHOTO, crop, [309.0, 159.0]);
        assert_eq!(grabbed, Some(CropHandle::SouthEast));
    }

    #[test]
    fn a_full_frame_crop_is_resized_but_not_moved() {
        let at = |x, y| CropHandle::at(PHOTO, NormalizedCropRect::FULL_FRAME, [x, y]);
        assert_eq!(at(300.0, 150.0), None);
        assert_eq!(at(95.0, 45.0), Some(CropHandle::NorthWest));
    }
}
