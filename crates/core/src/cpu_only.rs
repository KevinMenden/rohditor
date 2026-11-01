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
    if recipe.noise_reduction.is_active() {
        return Some("noise reduction runs on the CPU");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_active_noise_reduction_forces_the_cpu() {
        let mut recipe = EditRecipe::default();
        assert_eq!(cpu_only_stage_reason(&recipe), None);
        recipe.noise_reduction.chroma = 0.3;
        assert!(cpu_only_stage_reason(&recipe).is_some());
    }
}
