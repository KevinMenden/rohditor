//! Model files and their identity.
//!
//! Models are not in git and not pinned: any ONNX export of the v1 layout can
//! be used. Its identity for caches is the sha256 of its bytes, so swapping the
//! installed model never reuses results from the previous one.

use std::fmt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::NeuralError;

/// `version` of `ml/contract.json` this crate implements (packing, balance,
/// tiling). Models are assumed to be trained against it.
pub const CONTRACT_VERSION: u32 = 1;

/// Identity of a model's numerical behavior: the contract version and the
/// hash of its file. Part of every cache key over neural results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModelId {
    pub contract_version: u32,
    pub sha256: [u8; 32],
}

impl ModelId {
    /// Lowercase hex of [`Self::sha256`].
    #[must_use]
    pub fn sha256_hex(&self) -> String {
        self.sha256.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "contract v{} sha256 {}",
            self.contract_version,
            self.sha256_hex()
        )
    }
}

/// Model bytes and the identity derived from them.
pub struct ModelFile {
    id: ModelId,
    bytes: Vec<u8>,
}

impl ModelFile {
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            id: ModelId {
                contract_version: CONTRACT_VERSION,
                sha256: Sha256::digest(&bytes).into(),
            },
            bytes,
        }
    }

    pub fn read(path: &Path) -> Result<Self, NeuralError> {
        let display = path.display().to_string();
        let bytes = std::fs::read(path).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => NeuralError::ModelMissing {
                path: display.clone(),
            },
            _ => NeuralError::ModelUnreadable {
                path: display.clone(),
                reason: error.to_string(),
            },
        })?;
        Ok(Self::from_bytes(bytes))
    }

    #[must_use]
    pub const fn id(&self) -> ModelId {
        self.id
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for ModelFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelFile")
            .field("id", &self.id)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_is_the_sha256_of_the_bytes() {
        let model = ModelFile::from_bytes(b"abc".to_vec());
        assert_eq!(
            model.id().sha256_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(model.id().contract_version, CONTRACT_VERSION);
        assert_ne!(model.id(), ModelFile::from_bytes(b"abd".to_vec()).id());
    }

    #[test]
    fn a_missing_file_is_reported_as_missing() {
        let error = ModelFile::read(Path::new("/nonexistent/model.onnx")).expect_err("no file");
        assert!(matches!(error, NeuralError::ModelMissing { .. }));
    }
}
