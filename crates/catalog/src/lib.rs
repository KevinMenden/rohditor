//! UI-independent photo catalog primitives.
//!
//! The catalog deliberately works from embedded RAW previews. It never needs
//! to decode a sensor buffer just to make an image browseable.

mod cache;
mod scanner;
mod thumbnail;

pub use cache::{CacheError, ThumbnailCache, ThumbnailCacheKey};
pub use scanner::{CatalogEntry, SUPPORTED_EXTENSIONS, ScanError, scan_folder};
pub use thumbnail::{
    DEFAULT_THUMBNAIL_JPEG_QUALITY, DEFAULT_THUMBNAIL_LONG_EDGE, PlaceholderReason, Thumbnail,
    ThumbnailError, ThumbnailGenerator, ThumbnailOptions, ThumbnailOptionsError, ThumbnailOutcome,
};
