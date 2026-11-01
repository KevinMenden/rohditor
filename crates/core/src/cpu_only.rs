//! Stages without a GPU implementation.
//!
//! This is the one list GPU executors and frontends consult. A recipe that
//! needs a CPU-only stage is routed to the CPU pipeline *by choice*: it is not
//! a GPU failure, so it must never tear down the GPU or switch it off for the
//! rest of the session. Add a stage here when it lands on the CPU first, and
//! remove it when its GPU port does (e.g. #78 for noise reduction).

use rohditor_edit::EditRecipe;

/// Why `recipe` has to run on the CPU, or `None` if every stage has a GPU path.
#[must_use]
pub fn cpu_only_stage_reason(recipe: &EditRecipe) -> Option<&'static str> {
    if recipe.false_color.is_active() {
        return Some("false-colour suppression runs on the CPU");
    }
    if recipe.defringe.is_active() {
        return Some("defringe runs on the CPU");
    }
    if recipe.noise_reduction.is_active() {
        return Some("noise reduction runs on the CPU");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_active_detail_steps_force_the_cpu() {
        let mut recipe = EditRecipe::default();
        assert_eq!(cpu_only_stage_reason(&recipe), None);
        recipe.false_color.steps = 1;
        assert!(cpu_only_stage_reason(&recipe).is_some());
        recipe.false_color.steps = 0;
        recipe.noise_reduction.chroma = 0.3;
        assert!(cpu_only_stage_reason(&recipe).is_some());
        recipe.noise_reduction.chroma = 0.0;
        recipe.noise_reduction.luminance = 0.3;
        assert!(cpu_only_stage_reason(&recipe).is_some());
        recipe.noise_reduction.luminance = 0.0;
        recipe.defringe.green_amount = 0.2;
        assert!(cpu_only_stage_reason(&recipe).is_some());
        recipe.defringe.green_amount = 0.0;
        // The texture setting alone does no work.
        recipe.noise_reduction.luminance = 0.0;
        recipe.noise_reduction.luminance_detail = 0.9;
        assert_eq!(cpu_only_stage_reason(&recipe), None);
    }
}
