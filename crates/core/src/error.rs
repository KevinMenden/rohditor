use thiserror::Error;

/// Errors from validation or execution of Rohditor's CPU image pipeline.
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("invalid image dimensions {width}x{height} with row stride {row_stride}: {reason}")]
    InvalidDimensions {
        width: usize,
        height: usize,
        row_stride: usize,
        reason: String,
    },

    #[error("could not allocate {elements} image elements")]
    Allocation { elements: usize },

    #[error("unsupported CFA pattern {name} ({width}x{height})")]
    UnsupportedCfa {
        name: String,
        width: usize,
        height: usize,
    },

    #[error("invalid or missing RAW metadata field {field}: {reason}")]
    InvalidMetadata { field: &'static str, reason: String },

    #[error("invalid edit recipe field {field}: {reason}")]
    InvalidRecipe { field: &'static str, reason: String },

    #[error("image stage requires {expected}, but received {actual}")]
    WrongImageState {
        expected: &'static str,
        actual: &'static str,
    },
}
