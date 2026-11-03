//! Stages without a GPU implementation.
//!
//! This is the one list GPU executors and frontends consult. A recipe that
//! needs a CPU-only stage is routed to the CPU pipeline *by choice*: it is not
//! a GPU failure, so it must never tear down the GPU or switch it off for the
//! rest of the session. Add a stage here when it lands on the CPU first, and
//! remove it when its GPU port does.

use rohditor_edit::EditRecipe;

/// Why `recipe` has to run on the CPU, or `None` if every stage has a GPU path.
/// A stage that lands on the CPU first is listed here until its port.
#[must_use]
pub fn cpu_only_stage_reason(recipe: &EditRecipe) -> Option<&'static str> {
    // The GPU colour pass does not evaluate masks yet (see #88).
    if recipe.local.is_active() {
        return Some("local adjustments (masks) are not available on the GPU yet");
    }
    None
}

#[cfg(test)]
mod tests {
    use rohditor_edit::{
        LocalSettings, MaskComponent, MaskLayer, MaskMode, MaskPoint, MaskShape, RadialGradient,
    };

    use super::*;

    #[test]
    fn global_stages_all_have_a_gpu_path() {
        let mut recipe = EditRecipe::default();
        recipe.false_color.steps = 1;
        recipe.noise_reduction.chroma = 0.3;
        recipe.defringe.green_amount = 0.2;
        assert_eq!(cpu_only_stage_reason(&recipe), None);
    }

    fn recipe_with_mask() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.local.layers.push(MaskLayer {
            id: 0,
            name: "Sky".to_owned(),
            enabled: true,
            opacity: 1.0,
            components: vec![MaskComponent {
                mode: MaskMode::Add,
                invert: false,
                shape: MaskShape::Radial(RadialGradient {
                    center: MaskPoint { x: 0.5, y: 0.5 },
                    radius_x: 0.3,
                    radius_y: 0.3,
                    angle_degrees: 0.0,
                    feather: 0.5,
                }),
            }],
            settings: LocalSettings {
                exposure_ev: 0.5,
                ..LocalSettings::default()
            },
        });
        recipe
    }

    #[test]
    fn active_masks_route_to_the_cpu_and_inactive_ones_do_not() {
        let mut recipe = recipe_with_mask();
        assert!(cpu_only_stage_reason(&recipe).is_some());
        recipe.local.layers[0].enabled = false;
        assert_eq!(cpu_only_stage_reason(&recipe), None);
        recipe.local.layers[0].enabled = true;
        recipe.local.layers[0].settings = LocalSettings::default();
        assert_eq!(cpu_only_stage_reason(&recipe), None);
    }
}
