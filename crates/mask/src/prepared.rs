//! One mask layer resolved to pixel space, evaluated row by row.

use std::ops::Range;

use rohditor_edit::{MaskComponent, MaskLayer, MaskMode, MaskShape};

use crate::brush::PreparedBrush;
use crate::color_range::PreparedColor;
use crate::linear::PreparedLinear;
use crate::luminance::PreparedLuminance;
use crate::radial::PreparedRadial;
use crate::reference::{ReferenceNeeds, ReferenceRow};
use crate::{MaskError, MaskFrame};

/// A [`MaskLayer`] resolved to pixel space for one frame size. Build it once per render and
/// evaluate rows from any number of threads (`&self` only).
///
/// Only the geometry (`components`) and `opacity` are used. Whether the layer is `enabled` or
/// has neutral settings is the pipeline's decision, made before preparing it.
///
/// # Combining components
///
/// Components apply in order, starting from 0: a component's alpha `c` is first replaced by
/// `1 - c` if the component is `invert`ed, then combined with the running alpha `acc`:
///
/// | mode        | result              |
/// |-------------|---------------------|
/// | `Add`       | `max(acc, c)`       |
/// | `Subtract`  | `acc * (1 - c)`     |
/// | `Intersect` | `acc * c`           |
///
/// The layer's `opacity` multiplies the final result.
///
/// # Range components
///
/// Luminance and colour ranges select on what the pixels look like, not where they are. Their
/// values come from the [`ReferenceRow`] passed to [`evaluate_row`](Self::evaluate_row); ask
/// [`reference_needs`](Self::reference_needs) which parts to measure. They combine like any
/// other component and are typically `Intersect`.
#[derive(Debug, Clone)]
pub struct PreparedMask {
    frame: MaskFrame,
    opacity: f32,
    components: Vec<PreparedComponent>,
    needs: ReferenceNeeds,
}

#[derive(Debug, Clone)]
struct PreparedComponent {
    mode: MaskMode,
    invert: bool,
    shape: PreparedShape,
}

#[derive(Debug, Clone)]
enum PreparedShape {
    Linear(PreparedLinear),
    Radial(PreparedRadial),
    Brush(PreparedBrush),
    Luminance(PreparedLuminance),
    Color(PreparedColor),
}

impl PreparedShape {
    fn reference_needs(&self) -> ReferenceNeeds {
        ReferenceNeeds {
            luminance: matches!(self, Self::Luminance(_)),
            oklab: matches!(self, Self::Color(_)),
        }
    }
}

impl PreparedMask {
    /// Resolves `layer` for a frame of the given size.
    ///
    /// Expects a layer that passed `LocalAdjustments::validate`; values that would break the
    /// maths (zero-length gradients, non-finite points) are reported as
    /// [`MaskError::Degenerate`] rather than producing NaN alpha.
    pub fn prepare(layer: &MaskLayer, frame: MaskFrame) -> Result<Self, MaskError> {
        if frame.width == 0 || frame.height == 0 {
            return Err(MaskError::EmptyFrame {
                width: frame.width,
                height: frame.height,
            });
        }
        if !layer.opacity.is_finite() {
            return Err(MaskError::Degenerate {
                shape: "layer",
                reason: "opacity must be finite",
            });
        }
        let components: Vec<PreparedComponent> = layer
            .components
            .iter()
            .map(|component| PreparedComponent::prepare(component, frame))
            .collect::<Result<_, _>>()?;
        let needs = components
            .iter()
            .fold(ReferenceNeeds::default(), |needs, component| {
                needs.union(component.shape.reference_needs())
            });
        Ok(Self {
            frame,
            opacity: layer.opacity.clamp(0.0, 1.0),
            components,
            needs,
        })
    }

    /// The reference values this mask reads. When [`ReferenceNeeds::any`] is false, pass
    /// [`ReferenceRow::NONE`] to `evaluate_row`.
    #[must_use]
    pub const fn reference_needs(&self) -> ReferenceNeeds {
        self.needs
    }

    /// Writes the layer's alpha (opacity applied) for pixels `x_range` of row `y` into `out`.
    ///
    /// `reference` holds the reference values of those same pixels, but only the parts named by
    /// [`reference_needs`](Self::reference_needs).
    ///
    /// # Panics
    ///
    /// If `y` or `x_range` lies outside the frame, `out.len() != x_range.len()`, or a part of
    /// `reference` the mask reads does not have one value per pixel.
    pub fn evaluate_row(
        &self,
        y: usize,
        x_range: Range<usize>,
        reference: ReferenceRow<'_>,
        out: &mut [f32],
    ) {
        assert!(y < self.frame.height, "row {y} outside the mask frame");
        assert!(
            x_range.start <= x_range.end && x_range.end <= self.frame.width,
            "columns {x_range:?} outside the mask frame"
        );
        assert_eq!(
            out.len(),
            x_range.len(),
            "output must match the column range"
        );
        assert!(
            !self.needs.luminance || reference.luminance.len() == out.len(),
            "luminance reference must match the column range"
        );
        assert!(
            !self.needs.oklab || reference.oklab.len() == out.len(),
            "OkLab reference must match the column range"
        );

        let mut scratch = RowScratch::default();
        // A leading Add component writes straight into `out` (max(0, c) == c); anything else
        // starts from an all-zero accumulator.
        let writes_directly = self
            .components
            .first()
            .is_some_and(|component| component.mode == MaskMode::Add);
        if !writes_directly {
            out.fill(0.0);
        }
        for (index, component) in self.components.iter().enumerate() {
            if index == 0 && writes_directly {
                component.evaluate_row(y, x_range.start, reference, out, &mut scratch.keep);
                continue;
            }
            scratch.component.resize(out.len(), 0.0);
            component.evaluate_row(
                y,
                x_range.start,
                reference,
                &mut scratch.component,
                &mut scratch.keep,
            );
            combine(component.mode, out, &scratch.component);
        }
        if self.opacity != 1.0 {
            for alpha in out.iter_mut() {
                *alpha *= self.opacity;
            }
        }
    }

    /// Bytes held by this prepared mask, for the pipeline's working-set check.
    #[must_use]
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of_val(self.components.as_slice())
            + self
                .components
                .iter()
                .map(|component| match &component.shape {
                    PreparedShape::Brush(brush) => brush.bytes(),
                    PreparedShape::Color(color) => color.bytes(),
                    _ => 0,
                })
                .sum::<usize>()
    }
}

/// Buffers `evaluate_row` reuses across its components. Empty until a component needs them.
#[derive(Default)]
struct RowScratch {
    /// Alpha of the component being combined into the output.
    component: Vec<f32>,
    /// Brush stroke working row.
    keep: Vec<f32>,
}

impl PreparedComponent {
    fn prepare(component: &MaskComponent, frame: MaskFrame) -> Result<Self, MaskError> {
        let shape = match &component.shape {
            MaskShape::Linear(gradient) => {
                PreparedShape::Linear(PreparedLinear::prepare(gradient, frame)?)
            }
            MaskShape::Radial(radial) => {
                PreparedShape::Radial(PreparedRadial::prepare(radial, frame)?)
            }
            MaskShape::Brush(brush) => PreparedShape::Brush(PreparedBrush::prepare(brush, frame)?),
            MaskShape::Luminance(range) => {
                PreparedShape::Luminance(PreparedLuminance::prepare(range)?)
            }
            MaskShape::Color(range) => PreparedShape::Color(PreparedColor::prepare(range)?),
        };
        Ok(Self {
            mode: component.mode,
            invert: component.invert,
            shape,
        })
    }

    /// The component's own alpha (inversion applied, combine mode not).
    fn evaluate_row(
        &self,
        y: usize,
        x_start: usize,
        reference: ReferenceRow<'_>,
        out: &mut [f32],
        keep: &mut Vec<f32>,
    ) {
        match &self.shape {
            PreparedShape::Linear(linear) => linear.evaluate_row(y, x_start, out),
            PreparedShape::Radial(radial) => radial.evaluate_row(y, x_start, out),
            PreparedShape::Brush(brush) => brush.evaluate_row(y, x_start, out, keep),
            PreparedShape::Luminance(range) => range.evaluate_row(reference.luminance, out),
            PreparedShape::Color(range) => range.evaluate_row(reference.oklab, out),
        }
        if self.invert {
            for alpha in out.iter_mut() {
                *alpha = 1.0 - *alpha;
            }
        }
    }
}

/// Folds component alpha `component` into the running alpha `acc`.
fn combine(mode: MaskMode, acc: &mut [f32], component: &[f32]) {
    let pairs = acc.iter_mut().zip(component);
    match mode {
        MaskMode::Add => pairs.for_each(|(acc, c)| *acc = acc.max(*c)),
        MaskMode::Subtract => pairs.for_each(|(acc, c)| *acc *= 1.0 - c),
        MaskMode::Intersect => pairs.for_each(|(acc, c)| *acc *= c),
    }
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{
        ColorRange, ColorSample, LinearGradient, LocalSettings, LuminanceRange, MaskPoint,
        RadialGradient,
    };

    use super::*;
    use crate::test_support::{FRAME, plane_of};

    fn point(x: f32, y: f32) -> MaskPoint {
        MaskPoint { x, y }
    }

    /// Left half of the frame (alpha 1 left of x = 0.25, 0 right of 0.75, falling between).
    fn left() -> MaskShape {
        MaskShape::Linear(LinearGradient {
            start: point(0.25, 0.5),
            end: point(0.75, 0.5),
        })
    }

    /// Top-to-bottom gradient, 1 above y = 0.25.
    fn top() -> MaskShape {
        MaskShape::Linear(LinearGradient {
            start: point(0.5, 0.25),
            end: point(0.5, 0.75),
        })
    }

    fn component(mode: MaskMode, invert: bool, shape: MaskShape) -> MaskComponent {
        MaskComponent {
            mode,
            invert,
            shape,
        }
    }

    fn layer(opacity: f32, components: Vec<MaskComponent>) -> MaskLayer {
        MaskLayer {
            id: 0,
            name: String::new(),
            enabled: true,
            opacity,
            components,
            settings: LocalSettings::default(),
        }
    }

    fn evaluate(layer: &MaskLayer) -> Vec<f32> {
        let mask = PreparedMask::prepare(layer, FRAME).unwrap();
        plane_of(FRAME, |y, x, out| {
            mask.evaluate_row(y, x..x + out.len(), ReferenceRow::NONE, out)
        })
    }

    fn single(shape: MaskShape) -> Vec<f32> {
        evaluate(&layer(1.0, vec![component(MaskMode::Add, false, shape)]))
    }

    #[test]
    fn combine_modes_follow_the_truth_table() {
        let (a, b) = (single(left()), single(top()));
        type Expected = fn(f32, f32) -> f32;
        let cases: [(MaskMode, bool, Expected); 6] = [
            (MaskMode::Add, false, |a, b| a.max(b)),
            (MaskMode::Add, true, |a, b| a.max(1.0 - b)),
            (MaskMode::Subtract, false, |a, b| a * (1.0 - b)),
            (MaskMode::Subtract, true, |a, b| a * b),
            (MaskMode::Intersect, false, |a, b| a * b),
            (MaskMode::Intersect, true, |a, b| a * (1.0 - b)),
        ];
        for (mode, invert, expected) in cases {
            let both = evaluate(&layer(
                1.0,
                vec![
                    component(MaskMode::Add, false, left()),
                    component(mode, invert, top()),
                ],
            ));
            for ((got, a), b) in both.iter().zip(&a).zip(&b) {
                assert!((got - expected(*a, *b)).abs() < 1e-6, "{mode:?} {invert}");
            }
        }
    }

    #[test]
    fn a_hard_edged_truth_table_covers_all_four_corners() {
        // Two hard circles overlapping partly: check each region of the Venn diagram.
        let circle = |x: f32| {
            MaskShape::Radial(RadialGradient {
                center: point(x, 0.5),
                radius_x: 0.2,
                radius_y: 0.2,
                angle_degrees: 0.0,
                feather: 0.0,
            })
        };
        let region = |mode, invert| {
            evaluate(&layer(
                1.0,
                vec![
                    component(MaskMode::Add, false, circle(0.4)),
                    component(mode, invert, circle(0.6)),
                ],
            ))
        };
        // Pixels: only in A (x=11), in both (x=18), only in B (x=26), in neither (x=2).
        let at = |plane: &[f32], x: usize| plane[11 * FRAME.width + x];
        let union = region(MaskMode::Add, false);
        assert_eq!([2, 11, 18, 26].map(|x| at(&union, x)), [0.0, 1.0, 1.0, 1.0]);
        let difference = region(MaskMode::Subtract, false);
        assert_eq!(
            [2, 11, 18, 26].map(|x| at(&difference, x)),
            [0.0, 1.0, 0.0, 0.0]
        );
        let intersection = region(MaskMode::Intersect, false);
        assert_eq!(
            [2, 11, 18, 26].map(|x| at(&intersection, x)),
            [0.0, 0.0, 1.0, 0.0]
        );
        let outside_b = region(MaskMode::Intersect, true);
        assert_eq!(
            [2, 11, 18, 26].map(|x| at(&outside_b, x)),
            [0.0, 1.0, 0.0, 0.0]
        );
    }

    #[test]
    fn invert_gives_one_minus_alpha_and_opacity_scales_the_result() {
        let plain = single(left());
        let inverted = evaluate(&layer(1.0, vec![component(MaskMode::Add, true, left())]));
        let faded = evaluate(&layer(0.4, vec![component(MaskMode::Add, false, left())]));
        for ((p, i), f) in plain.iter().zip(&inverted).zip(&faded) {
            assert!((p + i - 1.0).abs() < 1e-6);
            assert!((p * 0.4 - f).abs() < 1e-6);
        }
    }

    #[test]
    fn an_empty_layer_is_all_zero_and_row_pieces_match_whole_rows() {
        assert!(evaluate(&layer(1.0, vec![])).iter().all(|a| *a == 0.0));

        let two = layer(
            0.8,
            vec![
                component(MaskMode::Add, false, left()),
                component(MaskMode::Subtract, true, top()),
            ],
        );
        let whole = evaluate(&two);
        let mask = PreparedMask::prepare(&two, FRAME).unwrap();
        let mut piece = vec![0.0; 11];
        mask.evaluate_row(9, 7..18, ReferenceRow::NONE, &mut piece);
        assert_eq!(piece, whole[9 * FRAME.width + 7..9 * FRAME.width + 18]);
    }

    /// A luminance reference that grows with x: 0 at the left edge, 1 at the right.
    fn luminance_ramp() -> Vec<f32> {
        (0..FRAME.width)
            .map(|x| x as f32 / (FRAME.width - 1) as f32)
            .collect()
    }

    fn evaluate_with_luminance(layer: &MaskLayer, luminance: &[f32]) -> Vec<f32> {
        let mask = PreparedMask::prepare(layer, FRAME).unwrap();
        plane_of(FRAME, |y, x, out| {
            let reference = ReferenceRow {
                luminance: &luminance[x..x + out.len()],
                oklab: &[],
            };
            mask.evaluate_row(y, x..x + out.len(), reference, out)
        })
    }

    #[test]
    fn a_gradient_intersected_with_a_luminance_range_is_their_product() {
        let range = MaskShape::Luminance(LuminanceRange {
            low: 0.4,
            high: 0.9,
            feather_low: 0.2,
            feather_high: 0.0,
        });
        let both = layer(
            1.0,
            vec![
                component(MaskMode::Add, false, top()),
                component(MaskMode::Intersect, false, range.clone()),
            ],
        );
        let ramp = luminance_ramp();
        let gradient = single(top());
        let range_only = evaluate_with_luminance(
            &layer(1.0, vec![component(MaskMode::Add, false, range)]),
            &ramp,
        );
        let combined = evaluate_with_luminance(&both, &ramp);
        for ((got, g), r) in combined.iter().zip(&gradient).zip(&range_only) {
            assert!((got - g * r).abs() < 1e-6);
        }
        // The range really restricts: some pixels the gradient fully covers are cut away, and
        // some stay.
        assert!(
            combined
                .iter()
                .zip(&gradient)
                .any(|(c, g)| *g == 1.0 && *c == 0.0)
        );
        assert!(
            combined
                .iter()
                .zip(&gradient)
                .any(|(c, g)| *g == 1.0 && *c == 1.0)
        );
    }

    /// Reference values of pixels `xs` of row `y`, computed from the absolute position so that
    /// every pixel has its own value and a row offset shows as a different result.
    fn reference_of(y: usize, xs: Range<usize>) -> (Vec<f32>, Vec<[f32; 3]>) {
        let luminance: Vec<f32> = xs
            .clone()
            .map(|x| ((x * 7 + y * 13) % 17) as f32 / 16.0)
            .collect();
        let oklab = xs
            .zip(&luminance)
            .map(|(x, lum)| [*lum, 0.1 - 0.2 * lum, 0.05 * (x % 5) as f32])
            .collect();
        (luminance, oklab)
    }

    fn evaluate_span(mask: &PreparedMask, y: usize, xs: Range<usize>) -> Vec<f32> {
        let (luminance, oklab) = reference_of(y, xs.clone());
        let mut out = vec![0.0; xs.len()];
        mask.evaluate_row(
            y,
            xs,
            ReferenceRow {
                luminance: &luminance,
                oklab: &oklab,
            },
            &mut out,
        );
        out
    }

    #[test]
    fn a_sub_range_uses_the_reference_of_its_own_pixels() {
        // A position-dependent component and both kinds of range, on the asymmetric frame, so
        // an offset mixed up between the geometry and the reference changes the result.
        let range_layer = layer(
            0.9,
            vec![
                component(MaskMode::Add, false, left()),
                component(
                    MaskMode::Intersect,
                    false,
                    MaskShape::Luminance(LuminanceRange {
                        low: 0.2,
                        high: 0.8,
                        feather_low: 0.1,
                        feather_high: 0.1,
                    }),
                ),
                component(
                    MaskMode::Intersect,
                    true,
                    MaskShape::Color(ColorRange {
                        samples: vec![
                            ColorSample {
                                lightness: 0.2,
                                a: 0.0,
                                b: 0.05,
                            },
                            ColorSample {
                                lightness: 0.9,
                                a: -0.1,
                                b: 0.1,
                            },
                        ],
                        tolerance: 0.2,
                    }),
                ),
            ],
        );
        let mask = PreparedMask::prepare(&range_layer, FRAME).expect("valid layer");
        for y in [0, 9, FRAME.height - 1] {
            let whole = evaluate_span(&mask, y, 0..FRAME.width);
            // The row is neither empty nor flat, so the comparison means something.
            assert!(whole.iter().any(|alpha| *alpha > 0.0));
            assert!(whole.windows(2).any(|pair| pair[0] != pair[1]));
            for xs in [7..18, 30..FRAME.width, 0..1, FRAME.width - 1..FRAME.width] {
                assert_eq!(
                    evaluate_span(&mask, y, xs.clone()),
                    whole[xs.clone()],
                    "row {y}, columns {xs:?}"
                );
            }
        }
    }

    #[test]
    #[should_panic(expected = "luminance reference must match")]
    fn a_full_width_luminance_reference_for_a_sub_range_is_rejected() {
        let mask = PreparedMask::prepare(
            &layer(
                1.0,
                vec![component(
                    MaskMode::Add,
                    false,
                    MaskShape::Luminance(LuminanceRange::default()),
                )],
            ),
            FRAME,
        )
        .expect("valid layer");
        // The reference of the whole row, not of the columns asked for: it must not be read
        // from column 0 as if it were aligned.
        let (luminance, oklab) = reference_of(3, 0..FRAME.width);
        let reference = ReferenceRow {
            luminance: &luminance,
            oklab: &oklab,
        };
        mask.evaluate_row(3, 7..18, reference, &mut [0.0; 11]);
    }

    #[test]
    #[should_panic(expected = "OkLab reference must match")]
    fn a_full_width_color_reference_for_a_sub_range_is_rejected() {
        let mask = PreparedMask::prepare(
            &layer(
                1.0,
                vec![component(
                    MaskMode::Add,
                    false,
                    MaskShape::Color(ColorRange::default()),
                )],
            ),
            FRAME,
        )
        .expect("valid layer");
        let (luminance, oklab) = reference_of(3, 0..FRAME.width);
        let reference = ReferenceRow {
            luminance: &luminance,
            oklab: &oklab,
        };
        mask.evaluate_row(3, 7..18, reference, &mut [0.0; 11]);
    }

    #[test]
    fn masks_report_which_reference_they_read() {
        let needs = |shapes: Vec<MaskShape>| {
            let components = shapes
                .into_iter()
                .map(|shape| component(MaskMode::Add, false, shape))
                .collect();
            PreparedMask::prepare(&layer(1.0, components), FRAME)
                .unwrap()
                .reference_needs()
        };
        let luminance = || MaskShape::Luminance(LuminanceRange::default());
        let color = || MaskShape::Color(ColorRange::default());
        assert!(!needs(vec![left(), top()]).any());
        assert_eq!(
            needs(vec![left(), luminance()]),
            ReferenceNeeds {
                luminance: true,
                oklab: false
            }
        );
        assert_eq!(
            needs(vec![color()]),
            ReferenceNeeds {
                luminance: false,
                oklab: true
            }
        );
        assert_eq!(
            needs(vec![luminance(), color()]),
            ReferenceNeeds {
                luminance: true,
                oklab: true
            }
        );
    }

    #[test]
    #[should_panic(expected = "luminance reference must match")]
    fn a_missing_reference_is_a_programming_error() {
        let mask = PreparedMask::prepare(
            &layer(
                1.0,
                vec![component(
                    MaskMode::Add,
                    false,
                    MaskShape::Luminance(LuminanceRange::default()),
                )],
            ),
            FRAME,
        )
        .unwrap();
        mask.evaluate_row(0, 0..4, ReferenceRow::NONE, &mut [0.0; 4]);
    }

    #[test]
    fn rejects_empty_frames_and_reports_brush_memory() {
        let one = layer(1.0, vec![component(MaskMode::Add, false, left())]);
        assert!(matches!(
            PreparedMask::prepare(
                &one,
                MaskFrame {
                    width: 0,
                    height: 5
                }
            ),
            Err(MaskError::EmptyFrame { .. })
        ));
        let mask = PreparedMask::prepare(&one, FRAME).unwrap();
        assert!(mask.bytes() >= std::mem::size_of::<PreparedMask>());
    }
}
