use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use image::GenericImageView;
use rohditor_raw::SourceIdentity;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{CatalogEntry, Thumbnail, ThumbnailOptions};

const CACHE_VERSION: u32 = 1;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// Errors while locating or writing the thumbnail cache.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("could not determine the default Rohditor cache directory")]
    NoDefaultDirectory,

    #[error("could not access thumbnail cache path {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not encode thumbnail cache metadata {path}: {source}")]
    MetadataEncode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("thumbnail dimensions are invalid for cache entry: {width}x{height}")]
    InvalidDimensions { width: u32, height: u32 },

    #[error("thumbnail bytes do not decode to their declared dimensions {width}x{height}")]
    InvalidThumbnail { width: u32, height: u32 },
}

/// A stable filename-safe identity for one thumbnail variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThumbnailCacheKey(String);

impl ThumbnailCacheKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Persistent thumbnail storage.
#[derive(Debug, Clone)]
pub struct ThumbnailCache {
    root: PathBuf,
}

impl ThumbnailCache {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Locate the cache below `XDG_CACHE_HOME`, or `$HOME/.cache` when the XDG
    /// variable is absent or empty.
    pub fn from_default() -> Result<Self, CacheError> {
        let root = std::env::var_os("XDG_CACHE_HOME")
            .filter(|directory| !directory.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
        root.map(|root| Self::new(root.join("rohditor").join("thumbnails")))
            .ok_or(CacheError::NoDefaultDirectory)
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Calculate the cache identity for an entry and a thumbnail variant.
    #[must_use]
    pub fn key_for(&self, entry: &CatalogEntry, options: ThumbnailOptions) -> ThumbnailCacheKey {
        cache_key(entry.path(), entry.source_identity(), options)
    }

    /// Load a valid cached thumbnail, treating stale or corrupt entries as
    /// cache misses so callers can regenerate them.
    pub fn load(
        &self,
        entry: &CatalogEntry,
        options: ThumbnailOptions,
    ) -> Result<Option<Thumbnail>, CacheError> {
        let key = self.key_for(entry, options);
        let metadata_path = self.metadata_path(&key);
        let metadata_bytes = match fs::read(&metadata_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(CacheError::Io {
                    path: metadata_path,
                    source,
                });
            }
        };
        let metadata: CacheMetadata = match serde_json::from_slice(&metadata_bytes) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(None),
        };
        if !metadata.matches(entry.source_identity(), options) {
            return Ok(None);
        }
        if metadata.width == 0 || metadata.height == 0 {
            return Ok(None);
        }

        let image_path = self.image_path(&key);
        let bytes = match fs::read(&image_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(CacheError::Io {
                    path: image_path,
                    source,
                });
            }
        };
        let decoded = match image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg) {
            Ok(decoded) => decoded,
            Err(_) => return Ok(None),
        };
        if decoded.dimensions() != (metadata.width, metadata.height) {
            return Ok(None);
        }
        Ok(Some(Thumbnail::new(metadata.width, metadata.height, bytes)))
    }

    /// Store a thumbnail using sibling temporary files and atomic renames.
    pub fn store(
        &self,
        entry: &CatalogEntry,
        options: ThumbnailOptions,
        thumbnail: &Thumbnail,
    ) -> Result<(), CacheError> {
        if thumbnail.width() == 0 || thumbnail.height() == 0 {
            return Err(CacheError::InvalidDimensions {
                width: thumbnail.width(),
                height: thumbnail.height(),
            });
        }
        let decoded =
            image::load_from_memory_with_format(thumbnail.bytes(), image::ImageFormat::Jpeg)
                .map_err(|_| CacheError::InvalidThumbnail {
                    width: thumbnail.width(),
                    height: thumbnail.height(),
                })?;
        if decoded.dimensions() != (thumbnail.width(), thumbnail.height()) {
            return Err(CacheError::InvalidThumbnail {
                width: thumbnail.width(),
                height: thumbnail.height(),
            });
        }

        fs::create_dir_all(&self.root).map_err(|source| CacheError::Io {
            path: self.root.clone(),
            source,
        })?;
        let key = self.key_for(entry, options);
        let image_path = self.image_path(&key);
        write_atomically(&image_path, thumbnail.bytes())?;

        let metadata = CacheMetadata {
            version: CACHE_VERSION,
            source_identity: entry.source_identity(),
            max_long_edge: options.max_long_edge(),
            jpeg_quality: options.jpeg_quality(),
            width: thumbnail.width(),
            height: thumbnail.height(),
        };
        let metadata_path = self.metadata_path(&key);
        let metadata_bytes =
            serde_json::to_vec(&metadata).map_err(|source| CacheError::MetadataEncode {
                path: metadata_path.clone(),
                source,
            })?;
        write_atomically(&metadata_path, &metadata_bytes)
    }

    fn image_path(&self, key: &ThumbnailCacheKey) -> PathBuf {
        self.root.join(format!("{}.jpg", key.as_str()))
    }

    fn metadata_path(&self, key: &ThumbnailCacheKey) -> PathBuf {
        self.root.join(format!("{}.json", key.as_str()))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheMetadata {
    version: u32,
    source_identity: SourceIdentity,
    max_long_edge: u32,
    jpeg_quality: u8,
    width: u32,
    height: u32,
}

impl CacheMetadata {
    fn matches(&self, identity: SourceIdentity, options: ThumbnailOptions) -> bool {
        self.version == CACHE_VERSION
            && self.source_identity == identity
            && self.max_long_edge == options.max_long_edge()
            && self.jpeg_quality == options.jpeg_quality()
    }
}

fn cache_key(
    path: &Path,
    identity: SourceIdentity,
    options: ThumbnailOptions,
) -> ThumbnailCacheKey {
    let mut hash = 0xcbf29ce484222325_u64;
    hash_bytes(&mut hash, b"rohditor-catalog-thumbnail-v1\0");
    hash_bytes(&mut hash, path.to_string_lossy().as_bytes());
    hash_bytes(&mut hash, &[0]);
    hash_bytes(&mut hash, &identity.size_bytes.to_le_bytes());
    hash_optional_u128(&mut hash, identity.modified_unix_nanos);
    hash_optional_u64(&mut hash, identity.filesystem_device);
    hash_optional_u64(&mut hash, identity.filesystem_inode);
    hash_bytes(&mut hash, &options.max_long_edge().to_le_bytes());
    hash_bytes(&mut hash, &[options.jpeg_quality()]);
    ThumbnailCacheKey(format!("{hash:016x}"))
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

fn hash_optional_u128(hash: &mut u64, value: Option<u128>) {
    match value {
        Some(value) => {
            hash_bytes(hash, &[1]);
            hash_bytes(hash, &value.to_le_bytes());
        }
        None => hash_bytes(hash, &[0]),
    }
}

fn hash_optional_u64(hash: &mut u64, value: Option<u64>) {
    match value {
        Some(value) => {
            hash_bytes(hash, &[1]);
            hash_bytes(hash, &value.to_le_bytes());
        }
        None => hash_bytes(hash, &[0]),
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), CacheError> {
    let Some(parent) = path.parent() else {
        return Err(CacheError::Io {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "cache path has no parent"),
        });
    };
    let file_name = path.file_name().map_or_else(
        || "thumbnail-cache-entry".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|source| CacheError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use image::{Rgb, RgbImage};

    use crate::scan_folder;

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rohditor-catalog-cache-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create cache test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn entry(&self, size_bytes: u64) -> CatalogEntry {
            let path = self.path().join("image.ARW");
            let file = File::create(&path).expect("create cache source file");
            file.set_len(size_bytes).expect("set cache source size");
            scan_folder(self.path())
                .expect("scan cache source directory")
                .into_iter()
                .next()
                .expect("find cache source entry")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn thumbnail() -> Thumbnail {
        let image = RgbImage::from_pixel(3, 2, Rgb([10, 20, 30]));
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85)
            .encode_image(&image)
            .expect("encode cache thumbnail");
        Thumbnail::new(3, 2, bytes)
    }

    #[test]
    fn cache_round_trip_uses_the_source_identity() {
        let directory = TestDirectory::new();
        let cache = ThumbnailCache::new(directory.path().join("thumbs"));
        let entry = directory.entry(10);
        let options = ThumbnailOptions::default();
        let expected = thumbnail();

        assert_eq!(cache.load(&entry, options).expect("cache miss"), None);
        cache
            .store(&entry, options, &expected)
            .expect("store thumbnail");
        let loaded = cache
            .load(&entry, options)
            .expect("load thumbnail")
            .expect("cached thumbnail");
        assert_eq!(loaded, expected);

        let changed = directory.entry(11);
        assert_eq!(
            cache.load(&changed, options).expect("changed cache miss"),
            None
        );
    }

    #[test]
    fn cache_options_are_part_of_the_key() {
        let directory = TestDirectory::new();
        let cache = ThumbnailCache::new(directory.path().join("thumbs"));
        let entry = directory.entry(10);
        let default_options = ThumbnailOptions::default();
        let other_options = ThumbnailOptions::new(256, 90).expect("valid options");

        assert_ne!(
            cache.key_for(&entry, default_options),
            cache.key_for(&entry, other_options)
        );
    }

    #[test]
    fn malformed_or_corrupt_entries_are_cache_misses() {
        let directory = TestDirectory::new();
        let cache = ThumbnailCache::new(directory.path().join("thumbs"));
        let entry = directory.entry(10);
        let options = ThumbnailOptions::default();
        fs::create_dir_all(cache.root()).expect("create cache directory");
        let key = cache.key_for(&entry, options);
        fs::write(cache.metadata_path(&key), b"not json").expect("write bad metadata");
        assert_eq!(
            cache.load(&entry, options).expect("bad metadata miss"),
            None
        );

        cache
            .store(&entry, options, &thumbnail())
            .expect("store valid thumbnail");
        fs::write(cache.image_path(&key), b"not an image").expect("write bad image");
        assert_eq!(cache.load(&entry, options).expect("bad image miss"), None);
    }

    #[test]
    fn invalid_thumbnail_is_rejected_before_writing() {
        let directory = TestDirectory::new();
        let cache = ThumbnailCache::new(directory.path().join("thumbs"));
        let entry = directory.entry(10);
        let invalid = Thumbnail::new(3, 2, vec![1, 2, 3]);

        assert!(matches!(
            cache.store(&entry, ThumbnailOptions::default(), &invalid),
            Err(CacheError::InvalidThumbnail { .. })
        ));
    }
}
