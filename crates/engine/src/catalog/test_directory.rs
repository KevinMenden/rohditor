//! A scratch folder for the catalog tests.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub(super) struct TestDirectory(PathBuf);

impl TestDirectory {
    pub fn new() -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "rohditor-engine-catalog-{}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create catalog test directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn create_arw(&self, name: &str) -> PathBuf {
        let path = self.path().join(name);
        File::create(&path).expect("create catalog test file");
        path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
