//! Storage layout and tuning knobs.
//!
//! Defaults encode (synchronous=FULL) and the single-writer policy
//! (pool_size=1). Every deviation is an explicit, benchmark-backed decision.

use std::path::{Path, PathBuf};

/// Where everything on disk lives for one process.
#[derive(Clone, Debug)]
pub struct StorageLayout {
    pub root: PathBuf,
}

impl StorageLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn account_dir(&self, account: u64) -> PathBuf {
        self.root.join("accounts").join(account.to_string())
    }

    pub fn account_db(&self, account: u64) -> PathBuf {
        self.account_dir(account).join("store.sqlite3")
    }

    pub fn media_cache(&self) -> PathBuf {
        self.root.join("media-cache")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Create the directory skeleton with owner-only permissions where the
    /// platform supports them.
    pub fn ensure_dirs(&self, account: Option<u64>) -> std::io::Result<()> {
        fs_create_private(&self.root)?;
        fs_create_private(&self.media_cache())?;
        fs_create_private(&self.logs())?;
        if let Some(a) = account {
            let dir = self.account_dir(a);
            fs_create_private(&dir)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn fs_create_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)?;
    // 0700: owner-only. Best-effort hardening; not a security boundary claim.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    Ok(())
}

#[cfg(not(unix))]
fn fs_create_private(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

/// Tuning applied to the shared per-account database.
///
/// `pool_size` drives both pool size and write serialization — keep at 1: two
/// deferred read-write transactions deadlock on upgrade, and busy_timeout
/// cannot break it. Read concurrency comes from `read_pool_size` only.
#[derive(Clone, Debug)]
pub struct StoreTuning {
    pub read_pool_size: u32,
    pub synchronous_full: bool,
    pub cache_size_kib: u32,
    pub busy_timeout_secs: u64,
}

impl Default for StoreTuning {
    fn default() -> Self {
        Self {
            read_pool_size: 2,
            synchronous_full: true,
            cache_size_kib: 512,
            busy_timeout_secs: 30,
        }
    }
}
