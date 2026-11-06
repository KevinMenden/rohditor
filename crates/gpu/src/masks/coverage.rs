//! Brush coverage planes for the colour pass.
//!
//! The planes hold `R16Uint` alpha, which needs no optional device feature and resolves alpha
//! about thirty times finer than a half float does near 1. There is one plane per brush
//! component, because components combine in order with whatever surrounds them.

use rayon::prelude::*;
use rohditor_core::OutputGeometry;
use rohditor_edit::BrushMask;
use rohditor_mask::{BrushCoverage, MaskFrame};

use crate::GpuPreviewError;
use crate::memory::Reservation;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Uint;
const BYTES_PER_TEXEL: u64 = 2;

/// A rectangle of the developed frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl FrameRect {
    /// The developed-frame rectangle that `rows` output rows from `first_row` read. Orientation
    /// and crop are axis aligned, so two opposite corners decide it; on a rotated image output
    /// rows are source columns.
    fn of_output_rows(geometry: OutputGeometry, first_row: u32, rows: u32) -> Self {
        debug_assert!(rows > 0, "a band always has rows");
        let width = geometry.output_dimensions().0;
        let a = geometry.source_coordinate_in_bounds(0, first_row as usize);
        let b = geometry.source_coordinate_in_bounds(width - 1, (first_row + rows - 1) as usize);
        Self {
            x: a.0.min(b.0) as u32,
            y: a.1.min(b.1) as u32,
            width: (a.0.abs_diff(b.0) + 1) as u32,
            height: (a.1.abs_diff(b.1) + 1) as u32,
        }
    }

    fn texels(self) -> usize {
        self.width as usize * self.height as usize
    }
}

/// Rasterizes the rectangle of `brush` into `out`, one row per chunk.
fn rasterize(brush: &BrushCoverage, rect: FrameRect, out: &mut [u16]) {
    out.par_chunks_mut(rect.width as usize)
        .enumerate()
        .for_each_init(
            || (vec![0.0_f32; rect.width as usize], Vec::new()),
            |(row, keep), (y, plane_row)| {
                brush.evaluate_row(rect.y as usize + y, rect.x as usize, row, keep);
                for (texel, alpha) in plane_row.iter_mut().zip(row.iter()) {
                    *texel = (alpha.clamp(0.0, 1.0) * 65535.0).round() as u16;
                }
            },
        );
}

fn prepare(brush: &BrushMask, frame: MaskFrame) -> Result<BrushCoverage, GpuPreviewError> {
    BrushCoverage::prepare(brush, frame).map_err(|error| GpuPreviewError::InvalidInput {
        reason: error.to_string(),
    })
}

fn mask_frame((width, height): (u32, u32)) -> MaskFrame {
    MaskFrame {
        width: width as usize,
        height: height as usize,
    }
}

/// The texture array and its reservation, which also counts the upload staging.
struct Planes {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    count: u32,
    memory: Reservation,
}

impl Planes {
    fn new(
        device: &wgpu::Device,
        size: (u32, u32),
        count: u32,
        budget: u64,
    ) -> Result<Self, GpuPreviewError> {
        let bytes = u64::from(size.0) * u64::from(size.1) * u64::from(count) * BYTES_PER_TEXEL;
        let memory = Reservation::try_new(bytes * 2, budget)?;
        let texture = create_texture(device, size, count);
        let view = array_view(&texture);
        Ok(Self {
            texture,
            view,
            size,
            count,
            memory,
        })
    }

    fn plane_bytes(&self) -> u64 {
        u64::from(self.size.0) * u64::from(self.size.1) * BYTES_PER_TEXEL
    }

    fn bytes(&self) -> u64 {
        self.plane_bytes() * u64::from(self.count)
    }

    /// What is held now: the planes plus upload staging, until [`FitCoverage`] settles it.
    const fn reserved_bytes(&self) -> u64 {
        self.memory.bytes()
    }

    fn write(&self, queue: &wgpu::Queue, plane: u32, rect: FrameRect, texels: &[u16]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: plane,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(rect.width * BYTES_PER_TEXEL as u32),
                rows_per_image: Some(rect.height),
            },
            wgpu::Extent3d {
                width: rect.width,
                height: rect.height,
                depth_or_array_layers: 1,
            },
        );
    }
}

fn create_texture(device: &wgpu::Device, size: (u32, u32), count: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rohditor brush coverage planes"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: count,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn array_view(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

/// What the shader's coverage binding sees for a recipe without brushes.
pub(crate) fn empty_view(device: &wgpu::Device) -> wgpu::TextureView {
    array_view(&create_texture(device, (1, 1), 1))
}

/// Full-frame planes kept between fit-preview renders. Moving a slider changes no stroke, so only
/// a plane whose brush changed is rasterized and uploaded again.
#[derive(Default)]
pub(crate) struct FitCoverage(Option<FitPlanes>);

struct FitPlanes {
    planes: Planes,
    brushes: Vec<BrushMask>,
}

impl FitCoverage {
    /// Brings the planes up to date with `brushes` and returns the view to bind, if any.
    pub(crate) fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        brushes: &[&BrushMask],
        frame: (u32, u32),
        budget: u64,
    ) -> Result<Option<wgpu::TextureView>, GpuPreviewError> {
        if brushes.is_empty() {
            self.0 = None;
            return Ok(None);
        }
        let count = brushes.len() as u32;
        let reusable = self
            .0
            .as_ref()
            .is_some_and(|fit| fit.planes.size == frame && fit.planes.count == count);
        if !reusable {
            // Release the old planes first: the budget must not have to fit both.
            self.0 = None;
            self.0 = Some(FitPlanes {
                planes: Planes::new(device, frame, count, budget)?,
                brushes: Vec::new(),
            });
        }
        let fit = self.0.as_mut().expect("the planes were just ensured");
        let stale: Vec<usize> = (0..brushes.len())
            .filter(|&plane| fit.brushes.get(plane) != Some(brushes[plane]))
            .collect();
        // Painting rewrites one plane per move, so one plane of staging stays reserved. Nothing
        // uploaded before this render is still waiting for its staging by now.
        let settled = fit.planes.bytes() + fit.planes.plane_bytes();
        fit.planes
            .memory
            .release_to(settled.min(fit.planes.reserved_bytes()));
        if !stale.is_empty() {
            // Prepare everything first: a failure must not leave a plane holding strokes that
            // `fit.brushes` does not list, which a later render would take for up to date.
            let prepared = stale
                .iter()
                .map(|&plane| prepare(brushes[plane], mask_frame(frame)))
                .collect::<Result<Vec<_>, _>>()?;
            let rect = FrameRect {
                x: 0,
                y: 0,
                width: frame.0,
                height: frame.1,
            };
            let mut texels = vec![0_u16; rect.texels()];
            for (&plane, brush) in stale.iter().zip(&prepared) {
                rasterize(brush, rect, &mut texels);
                fit.planes.write(queue, plane as u32, rect, &texels);
            }
            fit.brushes = brushes.iter().map(|brush| (*brush).clone()).collect();
        }
        Ok(Some(fit.planes.view.clone()))
    }
}

/// Planes for one band of output rows at a time, for full-resolution work.
pub(crate) struct BandCoverage {
    geometry: OutputGeometry,
    brushes: Vec<BrushCoverage>,
    planes: Option<Planes>,
    /// One plane of the largest band; reused across bands and brushes.
    texels: Vec<u16>,
    uploaded_bytes: u64,
}

impl BandCoverage {
    /// Reserves planes for bands of up to `max_rows` output rows of `geometry`. Nothing is
    /// allocated for a recipe without brushes.
    pub(crate) fn new(
        device: &wgpu::Device,
        brushes: &[&BrushMask],
        frame: (u32, u32),
        geometry: OutputGeometry,
        max_rows: u32,
        budget: u64,
    ) -> Result<Self, GpuPreviewError> {
        let mut coverage = Self {
            geometry,
            brushes: Vec::new(),
            planes: None,
            texels: Vec::new(),
            uploaded_bytes: 0,
        };
        if brushes.is_empty() {
            return Ok(coverage);
        }
        let largest = FrameRect::of_output_rows(
            geometry,
            0,
            max_rows.min(geometry.output_dimensions().1 as u32),
        );
        coverage.planes = Some(Planes::new(
            device,
            (largest.width, largest.height),
            brushes.len() as u32,
            budget,
        )?);
        coverage.brushes = brushes
            .iter()
            .map(|brush| prepare(brush, mask_frame(frame)))
            .collect::<Result<_, _>>()?;
        coverage.texels = vec![0; largest.texels()];
        Ok(coverage)
    }

    /// The view to bind, if this recipe has brushes.
    pub(crate) fn view(&self) -> Option<&wgpu::TextureView> {
        self.planes.as_ref().map(|planes| &planes.view)
    }

    /// Rasterizes and uploads the frame rectangle of these output rows and points the shader at
    /// it through the uniform. Queue writes take effect before the next submission, so the
    /// caller records its dispatch for these rows after this.
    pub(crate) fn write_rows(
        &mut self,
        queue: &wgpu::Queue,
        parameters: &wgpu::Buffer,
        first_row: u32,
        rows: u32,
    ) {
        let Some(planes) = &self.planes else {
            return;
        };
        let rect = FrameRect::of_output_rows(self.geometry, first_row, rows);
        let texels = &mut self.texels[..rect.texels()];
        for (plane, brush) in self.brushes.iter().enumerate() {
            rasterize(brush, rect, texels);
            planes.write(queue, plane as u32, rect, texels);
        }
        self.uploaded_bytes += self.brushes.len() as u64 * rect.texels() as u64 * BYTES_PER_TEXEL;
        queue.write_buffer(
            parameters,
            (super::tables::COVERAGE_ORIGIN_WORD * size_of::<u32>()) as u64,
            bytemuck::cast_slice(&[rect.x, rect.y]),
        );
    }

    /// GPU bytes reserved, including upload staging.
    pub(crate) fn reserved_bytes(&self) -> u64 {
        self.planes.as_ref().map_or(0, |planes| planes.bytes() * 2)
    }

    /// CPU bytes held: the placed dabs and one plane of scratch.
    pub(crate) fn cpu_bytes(&self) -> u64 {
        let dabs: usize = self.brushes.iter().map(BrushCoverage::bytes).sum();
        (dabs + self.texels.len() * size_of::<u16>()) as u64
    }

    /// Coverage bytes uploaded so far.
    pub(crate) const fn uploaded_bytes(&self) -> u64 {
        self.uploaded_bytes
    }
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{BrushStroke, MaskPoint, NormalizedCropRect};
    use rohditor_image::Orientation;

    use super::*;

    #[test]
    fn output_rows_map_to_source_rows_or_columns_by_orientation() {
        let crop = NormalizedCropRect {
            left: 0.1,
            top: 0.2,
            right: 0.9,
            bottom: 0.8,
        };
        let plain =
            OutputGeometry::new(100, 50, Orientation::Normal, Some(crop)).expect("test fixture");
        // Crop (10, 10) .. (90, 40) in the frame: rows keep their order, offset by the crop.
        assert_eq!(
            FrameRect::of_output_rows(plain, 5, 7),
            FrameRect {
                x: 10,
                y: 15,
                width: 80,
                height: 7
            }
        );

        // Rotating by 90 degrees turns output rows into source columns.
        let rotated =
            OutputGeometry::new(100, 50, Orientation::Rotate90, None).expect("test fixture");
        let rect = FrameRect::of_output_rows(rotated, 3, 4);
        assert_eq!((rect.width, rect.height), (4, 50));
        assert_eq!(rect.y, 0);
        // Every pixel of those output rows falls inside the rectangle.
        for y in 3..7 {
            for x in 0..50 {
                let (sx, sy) = rotated.source_coordinate_in_bounds(x, y);
                assert!(
                    (rect.x as usize..(rect.x + rect.width) as usize).contains(&sx)
                        && (rect.y as usize..(rect.y + rect.height) as usize).contains(&sy),
                    "output ({x}, {y}) reads ({sx}, {sy}), outside {rect:?}"
                );
            }
        }
    }

    #[test]
    fn rasterizing_a_rectangle_matches_the_same_part_of_the_full_frame() {
        let frame = MaskFrame {
            width: 64,
            height: 48,
        };
        let brush = BrushCoverage::prepare(
            &BrushMask {
                strokes: vec![BrushStroke {
                    points: vec![MaskPoint { x: 0.2, y: 0.3 }, MaskPoint { x: 0.8, y: 0.7 }],
                    radius: 0.1,
                    feather: 0.5,
                    flow: 0.8,
                    erase: false,
                }],
            },
            frame,
        )
        .expect("test fixture");
        let full = FrameRect {
            x: 0,
            y: 0,
            width: 64,
            height: 48,
        };
        let mut whole = vec![0; full.texels()];
        rasterize(&brush, full, &mut whole);
        // Flow builds up towards 1 along the stroke without reaching it.
        assert!(
            *whole.iter().max().expect("test fixture") > 65000,
            "the stroke is painted"
        );
        assert_eq!(whole[0], 0, "the corner is unpainted");

        let part = FrameRect {
            x: 10,
            y: 20,
            width: 30,
            height: 9,
        };
        let mut cut = vec![0; part.texels()];
        rasterize(&brush, part, &mut cut);
        for y in 0..9 {
            let row = (20 + y) * 64 + 10;
            assert_eq!(&cut[y * 30..(y + 1) * 30], &whole[row..row + 30], "row {y}");
        }
    }
}
