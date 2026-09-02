use thiserror::Error;

use rohditor_edit::EditError;
use rohditor_demosaic::DemosaicError;
use rohditor_image::ImageError;

/// Errors from validation or execution of Rohditor's CPU image pipeline.
#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("image processing was cancelled because a newer preview superseded it")]
    Cancelled,

    #[error("invalid image dimensions {width}x{height} with row stride {row_stride}: {reason}")]
    InvalidDimensions {
        width: usize,
        height: usize,
        row_stride: usize,
        reason: String,
    },

    #[error("could not allocate {elements} image elements")]
    Allocation { elements: usize },

    #[error(
        "refusing an estimated {estimated_bytes}-byte CPU image working set; configured maximum is {max_bytes} bytes"
    )]
    WorkingSetLimit {
        estimated_bytes: usize,
        max_bytes: usize,
    },

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

    #[error("{stage} received a non-finite sample at ({x}, {y})")]
    NonFiniteImageData {
        stage: &'static str,
        x: usize,
        y: usize,
    },

    #[error("image stage requires {expected}, but received {actual}")]
    WrongImageState {
        expected: &'static str,
        actual: &'static str,
    },
}

impl From<ImageError> for PipelineError {
    fn from(error: ImageError) -> Self {
        match error {
            ImageError::InvalidDimensions {
                width,
                height,
                row_stride,
                reason,
            } => Self::InvalidDimensions {
                width,
                height,
                row_stride,
                reason,
            },
            ImageError::Allocation { elements } => Self::Allocation { elements },
            ImageError::UnsupportedCfa {
                name,
                width,
                height,
            } => Self::UnsupportedCfa {
                name,
                width,
                height,
            },
        }
    }
}

impl From<EditError> for PipelineError {
    fn from(error: EditError) -> Self {
        Self::InvalidRecipe {
            field: error.field,
            reason: error.reason,
        }
    }
}

impl From<DemosaicError> for PipelineError {
    fn from(error: DemosaicError) -> Self {
        match error {
            DemosaicError::Cancelled => Self::Cancelled,
            DemosaicError::InvalidGains { reason } => Self::InvalidMetadata {
                field: "as_shot_white_balance",
                reason: reason.to_owned(),
            },
            DemosaicError::InvalidDimensions {
                width,
                height,
                row_stride,
                reason,
            } => Self::InvalidDimensions {
                width,
                height,
                row_stride,
                reason,
            },
            DemosaicError::NonFiniteImageData { stage, x, y } => {
                Self::NonFiniteImageData { stage, x, y }
            }
            DemosaicError::Image(error) => error.into(),
        }
    }
}
