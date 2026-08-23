//! Shared test fixtures. Never linked into production binaries.

use std::path::PathBuf;

/// A private temp directory rooted per test; cleaned on drop.
pub struct TestDir {
    _guard: tempfile::TempDir,
    path: PathBuf,
}

impl TestDir {
    pub fn new(label: &str) -> Self {
        let guard = tempfile::Builder::new()
            .prefix(&format!("wasabi-{label}-"))
            .tempdir()
            .expect("tempdir");
        let path = guard.path().to_path_buf();
        Self {
            _guard: guard,
            path,
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}
