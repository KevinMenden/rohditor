//! Noise-free analytic ground-truth images. With no sensor noise, error
//! against these measures reconstruction of structure only.

use rohditor_image::{LinearRgbImage, LinearRgbSpace};

use crate::ReferenceError;

#[derive(Debug, Clone, Copy)]
pub enum Fixture {
    /// Chirp whose spatial frequency grows with radius, per-channel phase.
    ZonePlate,
    /// A slightly rotated, softly blurred colour edge.
    SlantedEdge,
    /// A soft-edged disc with a different colour inside.
    Disc,
}

impl Fixture {
    pub const ALL: [Self; 3] = [Self::ZonePlate, Self::SlantedEdge, Self::Disc];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ZonePlate => "zone plate",
            Self::SlantedEdge => "slanted edge",
            Self::Disc => "disc",
        }
    }

    fn value(self, x: f32, y: f32, channel: usize, (cx, cy): (f32, f32)) -> f32 {
        let (dx, dy) = (x - cx, y - cy);
        match self {
            Self::ZonePlate => {
                // Instantaneous frequency reaches ~0.4 cycles/pixel at 512 px radius.
                let k = std::f32::consts::PI * 0.4 / 512.0;
                let phase = channel as f32 * 0.9;
                0.5 + 0.35 * (k * (dx * dx + dy * dy) + phase).cos()
            }
            Self::SlantedEdge => {
                let distance = dx * 0.087 + dy * 0.996; // edge ~5 degrees off horizontal
                let t = smoothstep(-1.5, 1.5, distance);
                let (low, high) = [(0.15, 0.80), (0.20, 0.55), (0.70, 0.10)][channel];
                low + (high - low) * t
            }
            Self::Disc => {
                let t = smoothstep(-1.2, 1.2, (dx * dx + dy * dy).sqrt() - 300.0);
                let (inside, outside) = [(0.8, 0.2), (0.3, 0.6), (0.1, 0.7)][channel];
                inside + (outside - inside) * t
            }
        }
    }

    /// Render a `width` x `height` linear RGB image of this fixture.
    pub fn render(
        self,
        width: usize,
        height: usize,
    ) -> Result<LinearRgbImage<f32>, ReferenceError> {
        let centre = (width as f32 / 2.0, height as f32 / 2.0);
        let mut data = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                for channel in 0..3 {
                    data.push(self.value(x as f32, y as f32, channel, centre));
                }
            }
        }
        Ok(LinearRgbImage::new(
            width,
            height,
            width * 3,
            LinearRgbSpace::CameraNative,
            data,
        )?)
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
