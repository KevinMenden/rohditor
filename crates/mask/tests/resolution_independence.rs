//! The same layer at a quarter of the resolution must match the full-resolution result.
//!
//! Each low-resolution pixel covers a 4x4 block of full-resolution pixels with the same centre,
//! so the block average of the full-resolution plane is what the low-resolution pixel should see.

use rohditor_edit::{
    BrushMask, BrushStroke, LinearGradient, LocalSettings, MaskComponent, MaskLayer, MaskMode,
    MaskPoint, MaskShape, RadialGradient,
};
use rohditor_mask::{MaskFrame, PreparedMask};

const SCALE: usize = 4;
const LOW: MaskFrame = MaskFrame {
    width: 37,
    height: 23,
};
const HIGH: MaskFrame = MaskFrame {
    width: LOW.width * SCALE,
    height: LOW.height * SCALE,
};

fn point(x: f32, y: f32) -> MaskPoint {
    MaskPoint { x, y }
}

fn layer(components: Vec<(MaskMode, bool, MaskShape)>) -> MaskLayer {
    MaskLayer {
        id: 0,
        name: String::new(),
        enabled: true,
        opacity: 0.9,
        components: components
            .into_iter()
            .map(|(mode, invert, shape)| MaskComponent {
                mode,
                invert,
                shape,
            })
            .collect(),
        settings: LocalSettings::default(),
    }
}

fn plane(layer: &MaskLayer, frame: MaskFrame) -> Vec<f32> {
    let mask = PreparedMask::prepare(layer, frame).expect("prepare");
    let mut plane = vec![0.0; frame.width * frame.height];
    for (y, row) in plane.chunks_exact_mut(frame.width).enumerate() {
        mask.evaluate_row(y, 0..frame.width, row);
    }
    plane
}

/// Largest difference between each low-resolution pixel and the mean of its full-res block.
fn max_difference(layer: &MaskLayer) -> f32 {
    let low = plane(layer, LOW);
    let high = plane(layer, HIGH);
    let mut worst = 0.0f32;
    for y in 0..LOW.height {
        for x in 0..LOW.width {
            let mut sum = 0.0;
            for by in 0..SCALE {
                for bx in 0..SCALE {
                    sum += high[(y * SCALE + by) * HIGH.width + x * SCALE + bx];
                }
            }
            let mean = sum / (SCALE * SCALE) as f32;
            worst = worst.max((low[y * LOW.width + x] - mean).abs());
        }
    }
    worst
}

#[test]
fn linear_gradient_is_resolution_independent() {
    let layer = layer(vec![(
        MaskMode::Add,
        false,
        MaskShape::Linear(LinearGradient {
            start: point(0.15, 0.2),
            end: point(0.8, 0.9),
        }),
    )]);
    assert!(max_difference(&layer) < 0.01);
}

#[test]
fn radial_gradient_is_resolution_independent() {
    let layer = layer(vec![(
        MaskMode::Add,
        false,
        MaskShape::Radial(RadialGradient {
            center: point(0.45, 0.55),
            radius_x: 0.35,
            radius_y: 0.2,
            angle_degrees: 25.0,
            feather: 0.6,
        }),
    )]);
    assert!(max_difference(&layer) < 0.02);
}

#[test]
fn brush_is_resolution_independent() {
    let layer = layer(vec![(
        MaskMode::Add,
        false,
        MaskShape::Brush(BrushMask {
            strokes: vec![BrushStroke {
                points: vec![point(0.2, 0.3), point(0.5, 0.7), point(0.85, 0.4)],
                radius: 0.12,
                feather: 0.6,
                flow: 0.5,
                erase: false,
            }],
        }),
    )]);
    assert!(max_difference(&layer) < 0.03);
}
