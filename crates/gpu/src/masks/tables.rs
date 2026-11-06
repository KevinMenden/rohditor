//! Packs the active mask layers of a recipe into the shared adjustment tables.
//!
//! `masks.wgsl` documents the word layout and repeats its constants; a test keeps them equal.

use rohditor_edit::{
    BrushMask, EditRecipe, LIGHT_TONE_LUT_SIZE, LightAdjustments, LightToneLut, MAX_COLOR_SAMPLES,
    MAX_MASK_COMPONENTS, MAX_MASK_LAYERS, MaskLayer, MaskMode, MaskShape,
};
use rohditor_mask::{MaskFrame, ResolvedShape, resolve_component};

use crate::GpuPreviewError;

const LAYERS_OFFSET: usize = LIGHT_TONE_LUT_SIZE * (1 + MAX_MASK_LAYERS);
const LAYER_WORDS: usize = 8;
const COMPONENTS_OFFSET: usize = LAYERS_OFFSET + MAX_MASK_LAYERS * LAYER_WORDS;
/// Parameter words come after the shape, combine mode and invert words.
const PARAMETERS: usize = 3;
/// The largest shape is a colour range: count, weight, two radii, then the samples.
const COMPONENT_WORDS: usize = PARAMETERS + 4 + MAX_COLOR_SAMPLES * 3;
const TABLE_WORDS: usize =
    COMPONENTS_OFFSET + MAX_MASK_LAYERS * MAX_MASK_COMPONENTS * COMPONENT_WORDS;

/// Size of the fixed adjustment table buffer. Fixing it at the recipe limits means no per-render
/// allocation, and the bytes are part of the processor's own reservation.
pub(crate) const TABLE_BYTES: u64 = (TABLE_WORDS * size_of::<f32>()) as u64;

/// First word of the uniform words [`parameter_words`] fills.
pub(crate) const PARAMETER_WORD: usize = 144;
/// Word of the uniform's `coverage_origin`.
pub(crate) const COVERAGE_ORIGIN_WORD: usize = PARAMETER_WORD + 2;

fn active_layers(recipe: &EditRecipe) -> impl Iterator<Item = &MaskLayer> {
    recipe.local.layers.iter().filter(|layer| layer.is_active())
}

/// The uniform words for this recipe: layer count, reference needs, coverage origin (zero until a
/// band sets it) and the global exposure.
pub(crate) fn parameter_words(recipe: &EditRecipe) -> [u32; 5] {
    let mut count = 0;
    let mut needs = 0;
    for layer in active_layers(recipe) {
        count += 1;
        for component in &layer.components {
            needs |= match component.shape {
                MaskShape::Luminance(_) => 1,
                MaskShape::Color(_) => 2,
                _ => 0,
            };
        }
    }
    [count, needs, 0, 0, recipe.light.exposure_ev.to_bits()]
}

/// The Light LUT of a layer that has tone sliders.
struct LayerTone {
    word: usize,
    lut: Box<LightToneLut>,
}

/// One render's mask tables, ready for upload, and the brushes they read as coverage planes.
pub(crate) struct MaskTables<'recipe> {
    /// From [`LAYERS_OFFSET`], up to the last used component record.
    records: Vec<f32>,
    tones: Vec<LayerTone>,
    pub(crate) brushes: Vec<&'recipe BrushMask>,
}

impl<'recipe> MaskTables<'recipe> {
    /// Resolves the active layers of `recipe` for a developed frame of `frame` pixels.
    pub(crate) fn resolve(
        recipe: &'recipe EditRecipe,
        frame: (u32, u32),
    ) -> Result<Self, GpuPreviewError> {
        let frame = MaskFrame {
            width: frame.0 as usize,
            height: frame.1 as usize,
        };
        let mut tables = Self {
            records: Vec::new(),
            tones: Vec::new(),
            brushes: Vec::new(),
        };
        let count = active_layers(recipe).count();
        if count == 0 {
            return Ok(tables);
        }
        tables.records = vec![
            0.0;
            (COMPONENTS_OFFSET - LAYERS_OFFSET)
                + count * MAX_MASK_COMPONENTS * COMPONENT_WORDS
        ];
        for (index, layer) in active_layers(recipe).enumerate() {
            tables.pack_layer(index, layer, frame)?;
        }
        Ok(tables)
    }

    fn pack_layer(
        &mut self,
        index: usize,
        layer: &'recipe MaskLayer,
        frame: MaskFrame,
    ) -> Result<(), GpuPreviewError> {
        let settings = &layer.settings;
        let has_tone = settings.contrast != 0.0
            || settings.highlights != 0.0
            || settings.shadows != 0.0
            || settings.whites != 0.0
            || settings.blacks != 0.0;
        if has_tone {
            self.tones.push(LayerTone {
                word: LIGHT_TONE_LUT_SIZE * (1 + index),
                lut: Box::new(LightToneLut::new(&LightAdjustments {
                    contrast: settings.contrast,
                    highlights: settings.highlights,
                    shadows: settings.shadows,
                    whites: settings.whites,
                    blacks: settings.blacks,
                    ..LightAdjustments::default()
                })),
            });
        }
        let record = index * LAYER_WORDS;
        self.records[record..record + 7].copy_from_slice(&[
            layer.opacity.clamp(0.0, 1.0),
            settings.exposure_ev,
            settings.temperature,
            settings.tint,
            1.0 + settings.saturation,
            f32::from(u8::from(has_tone)),
            layer.components.len() as f32,
        ]);
        for (position, component) in layer.components.iter().enumerate() {
            let resolved = resolve_component(component, frame).map_err(|error| {
                GpuPreviewError::InvalidInput {
                    reason: error.to_string(),
                }
            })?;
            let start = (COMPONENTS_OFFSET - LAYERS_OFFSET)
                + (index * MAX_MASK_COMPONENTS + position) * COMPONENT_WORDS;
            let record = &mut self.records[start..start + COMPONENT_WORDS];
            record[1] = match resolved.mode {
                MaskMode::Add => 0.0,
                MaskMode::Subtract => 1.0,
                MaskMode::Intersect => 2.0,
            };
            record[2] = f32::from(u8::from(resolved.invert));
            let (shape, parameters) = record.split_at_mut(PARAMETERS);
            shape[0] = pack_shape(&resolved.shape, self.brushes.len(), parameters);
            if let MaskShape::Brush(brush) = &component.shape {
                self.brushes.push(brush);
            }
        }
        Ok(())
    }

    /// Writes the tables into the shared buffer. A later render overwrites them; the layer count
    /// in the uniform decides how much of them the shader reads.
    pub(crate) fn upload(&self, queue: &wgpu::Queue, buffer: &wgpu::Buffer) {
        if !self.records.is_empty() {
            queue.write_buffer(
                buffer,
                (LAYERS_OFFSET * size_of::<f32>()) as u64,
                bytemuck::cast_slice(&self.records),
            );
        }
        for tone in &self.tones {
            queue.write_buffer(
                buffer,
                (tone.word * size_of::<f32>()) as u64,
                bytemuck::cast_slice(tone.lut.values()),
            );
        }
    }
}

/// Writes a shape's parameters and returns its shape code. `brush_plane` is the plane this shape
/// reads if it is a brush.
fn pack_shape(shape: &ResolvedShape, brush_plane: usize, out: &mut [f32]) -> f32 {
    let mut write = |values: &[f32]| out[..values.len()].copy_from_slice(values);
    match shape {
        ResolvedShape::Linear { start, direction } => {
            write(&[start[0], start[1], direction[0], direction[1]]);
            0.0
        }
        ResolvedShape::Radial {
            center,
            cos,
            sin,
            inv_radius_x,
            inv_radius_y,
            feather,
        } => {
            write(&[
                center[0],
                center[1],
                *cos,
                *sin,
                *inv_radius_x,
                *inv_radius_y,
                *feather,
            ]);
            1.0
        }
        ResolvedShape::Brush => {
            write(&[brush_plane as f32]);
            2.0
        }
        ResolvedShape::Luminance {
            low,
            high,
            feather_low,
            feather_high,
        } => {
            write(&[*low, *high, *feather_low, *feather_high]);
            3.0
        }
        ResolvedShape::Color {
            samples,
            lightness_weight,
            inner,
            outer,
        } => {
            write(&[samples.len() as f32, *lightness_weight, *inner, *outer]);
            out[4..4 + samples.len() * 3].copy_from_slice(samples.as_flattened());
            4.0
        }
    }
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{
        ColorRange, ColorSample, LinearGradient, LocalSettings, LuminanceRange, MaskComponent,
        MaskPoint,
    };

    use super::*;

    #[test]
    fn shader_layout_constants_match_the_rust_layout() {
        let shader = include_str!("masks.wgsl");
        for constant in [
            format!("MASK_LAYERS_OFFSET: u32 = {LAYERS_OFFSET}u"),
            format!("MASK_LAYER_WORDS: u32 = {LAYER_WORDS}u"),
            format!("MASK_COMPONENTS_OFFSET: u32 = {COMPONENTS_OFFSET}u"),
            format!("MASK_COMPONENT_WORDS: u32 = {COMPONENT_WORDS}u"),
            format!("MASK_COMPONENTS_PER_LAYER: u32 = {MAX_MASK_COMPONENTS}u"),
            format!("MASK_MAX_LAYERS: u32 = {MAX_MASK_LAYERS}u"),
            format!("MASK_PARAMETERS: u32 = {PARAMETERS}u"),
        ] {
            assert!(shader.contains(&constant), "masks.wgsl lacks {constant}");
        }
    }

    fn layer(enabled: bool, components: Vec<MaskComponent>) -> MaskLayer {
        MaskLayer {
            id: 0,
            name: String::new(),
            enabled,
            opacity: 0.5,
            components,
            settings: LocalSettings {
                exposure_ev: 1.0,
                saturation: -0.25,
                ..LocalSettings::default()
            },
        }
    }

    fn component(mode: MaskMode, shape: MaskShape) -> MaskComponent {
        MaskComponent {
            mode,
            invert: mode == MaskMode::Subtract,
            shape,
        }
    }

    fn recipe() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 0.5;
        // The disabled layer takes no slot.
        recipe.local.layers = vec![
            layer(false, Vec::new()),
            layer(
                true,
                vec![
                    component(
                        MaskMode::Add,
                        MaskShape::Linear(LinearGradient {
                            start: MaskPoint { x: 0.0, y: 0.5 },
                            end: MaskPoint { x: 1.0, y: 0.5 },
                        }),
                    ),
                    component(MaskMode::Subtract, MaskShape::Brush(BrushMask::default())),
                    component(
                        MaskMode::Intersect,
                        MaskShape::Luminance(LuminanceRange {
                            low: 0.25,
                            high: 0.75,
                            feather_low: 0.0,
                            feather_high: 0.0,
                        }),
                    ),
                ],
            ),
            layer(
                true,
                vec![component(
                    MaskMode::Add,
                    MaskShape::Color(ColorRange {
                        samples: vec![ColorSample {
                            lightness: 0.5,
                            a: 0.1,
                            b: -0.1,
                        }],
                        tolerance: 0.5,
                    }),
                )],
            ),
        ];
        recipe
    }

    #[test]
    fn packs_active_layers_into_consecutive_slots_with_their_components() {
        let recipe = recipe();
        let tables = MaskTables::resolve(&recipe, (100, 50)).expect("test fixture");
        assert_eq!(tables.brushes.len(), 1);
        assert_eq!(
            &tables.records[..7],
            &[0.5, 1.0, 0.0, 0.0, 0.75, 0.0, 3.0],
            "first active layer takes slot 0"
        );
        assert_eq!(tables.records[LAYER_WORDS + 6], 1.0);

        let component = |layer: usize, position: usize| {
            let start = (COMPONENTS_OFFSET - LAYERS_OFFSET)
                + (layer * MAX_MASK_COMPONENTS + position) * COMPONENT_WORDS;
            &tables.records[start..start + COMPONENT_WORDS]
        };
        // Linear, in pixel space: 100 pixels long, so `direction` is 1 / 100.
        assert_eq!(component(0, 0)[..3], [0.0, 0.0, 0.0]);
        assert_eq!(component(0, 0)[3..7], [0.0, 25.0, 0.01, 0.0]);
        // The brush reads plane 0 and carries its mode and invert.
        assert_eq!(component(0, 1)[..4], [2.0, 1.0, 1.0, 0.0]);
        assert_eq!(component(0, 2)[..7], [3.0, 2.0, 0.0, 0.25, 0.75, 0.0, 0.0]);
        // Colour samples follow the count, weight and radii.
        let color = component(1, 0);
        assert_eq!(color[0], 4.0);
        assert_eq!(color[3], 1.0);
        assert_eq!(color[7..10], [0.5, 0.1, -0.1]);
    }

    #[test]
    fn uniform_words_count_active_layers_and_reference_needs() {
        let words = parameter_words(&recipe());
        assert_eq!(words, [2, 3, 0, 0, 0.5_f32.to_bits()]);
        assert_eq!(parameter_words(&EditRecipe::default())[..2], [0, 0]);
    }

    #[test]
    fn a_tone_slider_gives_its_layer_slot_a_lut() {
        let mut recipe = recipe();
        recipe.local.layers[2].settings.contrast = 0.5;
        let tables = MaskTables::resolve(&recipe, (100, 50)).expect("test fixture");
        assert_eq!(tables.tones.len(), 1);
        assert_eq!(tables.tones[0].word, LIGHT_TONE_LUT_SIZE * 2);
        assert_eq!(tables.records[LAYER_WORDS + 5], 1.0);
    }

    #[test]
    fn a_recipe_without_active_layers_uploads_nothing() {
        let recipe = EditRecipe::default();
        let tables = MaskTables::resolve(&recipe, (100, 50)).expect("test fixture");
        assert!(tables.records.is_empty() && tables.tones.is_empty());
    }
}
