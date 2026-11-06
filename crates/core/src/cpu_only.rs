//! Stages without a GPU implementation.
//!
//! This is the one list GPU executors and frontends consult. A recipe that
//! needs a CPU-only stage is routed to the CPU pipeline *by choice*: it is not
//! a GPU failure, so it must never tear down the GPU or switch it off for the
//! rest of the session. Add a stage here when it lands on the CPU first, and
//! remove it when its GPU port does.

use rohditor_edit::{EditRecipe, MaskShape};

/// Most brush components the GPU colour pass takes. Each is a coverage plane as large as the
/// fit preview (17 MiB reserved at the default size, with its upload staging), so many more
/// could exhaust the GPU budget, which ends GPU rendering for the session. They render on the
/// CPU instead.
const GPU_MAX_BRUSH_COMPONENTS: usize = 16;

/// Why `recipe` has to run on the CPU, or `None` if every stage has a GPU path.
/// A stage that lands on the CPU first is listed here until its port.
#[must_use]
pub fn cpu_only_stage_reason(recipe: &EditRecipe) -> Option<&'static str> {
    let brushes = recipe
        .local
        .layers
        .iter()
        .filter(|layer| layer.is_active())
        .flat_map(|layer| &layer.components)
        .filter(|component| matches!(component.shape, MaskShape::Brush(_)))
        .count();
    (brushes > GPU_MAX_BRUSH_COMPONENTS)
        .then_some("more than 16 brush components are not available on the GPU")
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{BrushMask, LocalSettings, MaskComponent, MaskLayer, MaskMode};

    use super::*;

    #[test]
    fn global_stages_all_have_a_gpu_path() {
        let mut recipe = EditRecipe::default();
        recipe.false_color.steps = 1;
        recipe.noise_reduction.chroma = 0.3;
        recipe.defringe.green_amount = 0.2;
        assert_eq!(cpu_only_stage_reason(&recipe), None);
    }

    fn recipe_with_brushes(count: usize) -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.local.layers.push(MaskLayer {
            id: 0,
            name: "Painted".to_owned(),
            enabled: true,
            opacity: 1.0,
            components: vec![
                MaskComponent {
                    mode: MaskMode::Add,
                    invert: false,
                    shape: MaskShape::Brush(BrushMask::default()),
                };
                count
            ],
            settings: LocalSettings {
                exposure_ev: 0.5,
                ..LocalSettings::default()
            },
        });
        recipe
    }

    #[test]
    fn masks_have_a_gpu_path_until_brushes_would_exhaust_it() {
        let mut recipe = recipe_with_brushes(GPU_MAX_BRUSH_COMPONENTS);
        assert_eq!(cpu_only_stage_reason(&recipe), None);
        recipe
            .local
            .layers
            .push(recipe_with_brushes(1).local.layers.remove(0));
        assert!(cpu_only_stage_reason(&recipe).is_some());
        // Only active layers need planes.
        recipe.local.layers[1].enabled = false;
        assert_eq!(cpu_only_stage_reason(&recipe), None);
    }
}
