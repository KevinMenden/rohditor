//! Noise reduction filters. Each works on interleaved RGB images and leaves
//! the parts of the signal it is not responsible for untouched.

mod chroma;

pub use chroma::{ChromaDenoise, chroma_scratch_bytes, denoise_chroma};
