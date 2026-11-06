//! The reference that luminance and colour range components select on.
//!
//! A range component reads what a pixel looks like *after the global exposure and the base
//! rendering, but before the Light and Color controls and before any local term*. That keeps a
//! range stable while the user moves the contrast slider, the HSL mixer or another mask. The
//! adjustment stage owns the order of its operations, so it passes in the function that turns a
//! pixel as it enters the stage into that reference pixel; this module only measures it row by
//! row into the plain slices `rohditor-mask` expects.

use rohditor_color::{rec2020_encoded_luminance, rec2020_to_oklab};
use rohditor_mask::{ReferenceNeeds, ReferenceRow};

/// Reference values of one row, reused across rows by a worker thread.
#[derive(Debug, Default)]
pub(crate) struct ReferenceRows {
    luminance: Vec<f32>,
    oklab: Vec<[f32; 3]>,
}

impl ReferenceRows {
    /// Measures the pixels of a row (`pixels` is interleaved linear Rec.2020, `width` pixels).
    /// `to_reference` turns a pixel into its reference pixel in place. Only the parts named by
    /// `needs` are measured.
    pub(crate) fn measure(
        &mut self,
        needs: ReferenceNeeds,
        pixels: &[f32],
        to_reference: impl Fn(&mut [f32; 3]),
    ) {
        let pixels = pixels.as_chunks::<3>().0;
        self.luminance.clear();
        self.oklab.clear();
        for pixel in pixels {
            let mut reference = *pixel;
            to_reference(&mut reference);
            if needs.luminance {
                self.luminance.push(rec2020_encoded_luminance(reference));
            }
            if needs.oklab {
                self.oklab.push(rec2020_to_oklab(reference));
            }
        }
    }

    /// The last measured row, in the form the masks take.
    pub(crate) fn row(&self) -> ReferenceRow<'_> {
        ReferenceRow {
            luminance: &self.luminance,
            oklab: &self.oklab,
        }
    }
}

/// Bytes of reference scratch one worker thread needs for a row of `width` pixels.
pub(crate) fn row_bytes(needs: ReferenceNeeds, width: usize) -> usize {
    let per_pixel = if needs.luminance { size_of::<f32>() } else { 0 }
        + if needs.oklab {
            size_of::<[f32; 3]>()
        } else {
            0
        };
    width.saturating_mul(per_pixel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_only_the_requested_parts_of_the_transformed_pixel() {
        // Pixel values double on the way to the reference: the reference is what is measured.
        let pixels = [0.1, 0.1, 0.1, 0.4, 0.4, 0.4];
        let double = |pixel: &mut [f32; 3]| pixel.iter_mut().for_each(|v| *v *= 2.0);
        let mut rows = ReferenceRows::default();

        rows.measure(
            ReferenceNeeds {
                luminance: true,
                oklab: false,
            },
            &pixels,
            double,
        );
        let row = rows.row();
        assert_eq!(row.luminance.len(), 2);
        assert!(row.oklab.is_empty());
        assert_eq!(row.luminance[0], rec2020_encoded_luminance([0.2; 3]));
        assert_eq!(row.luminance[1], rec2020_encoded_luminance([0.8; 3]));

        rows.measure(
            ReferenceNeeds {
                luminance: false,
                oklab: true,
            },
            &pixels,
            double,
        );
        let row = rows.row();
        assert!(row.luminance.is_empty());
        assert_eq!(
            row.oklab,
            [rec2020_to_oklab([0.2; 3]), rec2020_to_oklab([0.8; 3])]
        );
    }

    #[test]
    fn scratch_size_follows_the_needs() {
        let both = ReferenceNeeds {
            luminance: true,
            oklab: true,
        };
        assert_eq!(row_bytes(both, 10), 10 * (4 + 12));
        assert_eq!(row_bytes(ReferenceNeeds::default(), 10), 0);
    }
}
