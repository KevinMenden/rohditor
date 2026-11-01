//! Stages without a GPU implementation.
//!
//! This is the one list GPU executors and frontends consult. A recipe that
//! needs a CPU-only stage is routed to the CPU pipeline *by choice*: it is not
//! a GPU failure, so it must never tear down the GPU or switch it off for the
//! rest of the session. Add a stage here when it lands on the CPU first, and
//! remove it when its GPU port does.

use rohditor_edit::EditRecipe;

/// Why `recipe` has to run on the CPU, or `None` if every stage has a GPU path.
/// Every stage has one today; a stage that lands on the CPU first is listed
/// here until its port.
#[must_use]
pub fn cpu_only_stage_reason(_recipe: &EditRecipe) -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stage_has_a_gpu_path() {
        let mut recipe = EditRecipe::default();
        recipe.false_color.steps = 1;
        recipe.noise_reduction.chroma = 0.3;
        recipe.defringe.green_amount = 0.2;
        assert_eq!(cpu_only_stage_reason(&recipe), None);
    }
}
