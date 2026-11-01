//! Stages without a GPU implementation.
//!
//! This is the one list GPU executors and frontends consult. A recipe that
//! needs a CPU-only stage is routed to the CPU pipeline *by choice*: it is not
//! a GPU failure, so it must never tear down the GPU or switch it off for the
//! rest of the session. Add a stage here when it lands on the CPU first, and
//! remove it when its GPU port does (e.g. #78 for defringe).

use rohditor_edit::EditRecipe;

/// Why `recipe` has to run on the CPU, or `None` if every stage has a GPU path.
#[must_use]
pub fn cpu_only_stage_reason(recipe: &EditRecipe) -> Option<&'static str> {
    if recipe.defringe.is_active() {
        return Some("defringe runs on the CPU");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_active_defringe_forces_the_cpu() {
        let mut recipe = EditRecipe::default();
        assert_eq!(cpu_only_stage_reason(&recipe), None);
        // The detail stage has a GPU port.
        recipe.false_color.steps = 1;
        recipe.noise_reduction.chroma = 0.3;
        recipe.noise_reduction.luminance = 0.3;
        assert_eq!(cpu_only_stage_reason(&recipe), None);
        recipe.defringe.green_amount = 0.2;
        assert!(cpu_only_stage_reason(&recipe).is_some());
        recipe.defringe.green_amount = 0.0;
        assert_eq!(cpu_only_stage_reason(&recipe), None);
    }
}
