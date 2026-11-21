use rohditor_image::ImageError;
use thiserror::Error;

/// Failures of neural reconstruction and model loading.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NeuralError {
    #[error("neural demosaicing was cancelled")]
    Cancelled,

    #[error("the neural demosaic model {path} is not installed")]
    ModelMissing { path: String },

    #[error("could not read the neural demosaic model {path}: {reason}")]
    ModelUnreadable { path: String, reason: String },

    #[error("invalid neural demosaic input: {reason}")]
    InvalidInput { reason: String },

    #[error("the neural demosaic model returned {actual:?}, expected {expected:?}")]
    UnexpectedOutputShape {
        expected: [usize; 3],
        actual: Vec<usize>,
    },

    #[error("neural inference failed: {reason}")]
    Runtime { reason: String },

    #[error(transparent)]
    Image(#[from] ImageError),
}

impl NeuralError {
    pub(crate) fn invalid_input(reason: impl Into<String>) -> Self {
        Self::InvalidInput {
            reason: reason.into(),
        }
    }
}
