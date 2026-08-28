//! Editor-domain and CPU-pipeline types.
//!
//! The processing implementation begins after the RAW decoder gate passes.

/// Schema version reserved for the first non-destructive edit recipe.
pub const EDIT_RECIPE_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::EDIT_RECIPE_SCHEMA_VERSION;

    #[test]
    fn first_recipe_schema_is_version_one() {
        assert_eq!(EDIT_RECIPE_SCHEMA_VERSION, 1);
    }
}
