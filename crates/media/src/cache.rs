//! Content-addressed disk cache for decrypted media blobs.
//!
//! Layout: `<root>/<sha256-hex>` for committed entries, `<root>/staging-<n>.tmp`
//! while a download is streaming. A `.tmp` only becomes visible under its final
//! name via rename after the content hash was verified, so anything found under
//! a hash name is complete by construction and never needs re-validation.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use filetime::FileTime;

/// Staging files older than this at boot are orphans from crashed writes;
/// younger ones may belong to a concurrently starting process.
const STALE_TMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Unique staging suffix so two attempts at different hashes (or a retry after
/// a crash that left debris) can never collide on one tmp path.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct DiskCache {
    root: PathBuf,
    quota: Arc<AtomicU64>,
    // Serialize eviction/scan walks; they are pure reads otherwise.
    scan_lock: Arc<Mutex<()>>,
}

impl std::fmt::Debug for DiskCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskCache")
            .field("root", &self.root)
            .field("quota", &self.quota.load(Ordering::Relaxed))
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EntryStat {
    cold: FileTime,
    size: u64,
}

impl DiskCache {
    /// Opens (creating if needed) the cache with [`crate::DEFAULT_DISK_CACHE_QUOTA_BYTES`],
    /// purging stale staging files and evicting down to quota before returning.
    pub async fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::open_with_quota(root, crate::DEFAULT_DISK_CACHE_QUOTA_BYTES).await
    }

    pub async fn open_with_quota(
        root: impl Into<PathBuf>,
        quota: u64,
    ) -> std::io::Result<Self> {
        let root = root.into();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&root)?;
            let cache = Self {
                root,
                quota: Arc::new(AtomicU64::new(quota)),
                scan_lock: Arc::new(Mutex::new(())),
            };
            cache.sweep_stale_staging();
            let _ = cache.evict_to_blocking(quota);
            Ok(cache)
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn quota(&self) -> u64 {
        self.quota.load(Ordering::Relaxed)
    }

    pub fn set_quota(&self, quota: u64) {
        self.quota.store(quota, Ordering::Relaxed);
    }

    /// Path of a committed entry. Pure lookup: no existence check, no touch.
    pub fn open_path(&self, sha_hex: &str) -> Option<PathBuf> {
        if !is_sha_hex(sha_hex) {
            return None;
        }
        let path = self.entry_path(sha_hex);
        std::fs::metadata(&path)
            .ok()
            .filter(|m| m.is_file())
            .map(|_| path)
    }

    /// Cache hit for an in-flight download decision: checks presence and
    /// refreshes the access time that eviction ranks by.
    ///
    /// The touch is best-effort because noatime/relatime mounts legitimately
    /// drop it; eviction falls back to mtime for ranking in that case.
    pub(crate) fn lookup_touch(&self, sha_hex: &str) -> Option<PathBuf> {
        let path = self.open_path(sha_hex)?;
        let now = FileTime::from_system_time(SystemTime::now());
        let _ = filetime::set_file_atime(&path, now);
        Some(path)
    }

    pub(crate) fn entry_path(&self, sha_hex: &str) -> PathBuf {
        self.root.join(sha_hex)
    }

    /// Unique staging path for a streamed attempt. The final name is decided
    /// only by the verified digest, so staging needs no key at all.
    pub(crate) fn staging_tmp(&self) -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.root.join(format!("staging-{n}.tmp"))
    }

    /// Atomic publish: fsync'd tmp file becomes the committed entry. Same-directory
    /// rename, so readers either see nothing or the complete blob.
    pub(crate) async fn commit(&self, tmp: PathBuf, final_path: PathBuf) -> std::io::Result<()> {
        tokio::fs::rename(tmp, final_path).await
    }

    /// Convenience writer used by tests and small blobs: stores bytes under
    /// their hash with the same fsync+rename discipline as streamed writes.
    pub async fn store_bytes(&self, sha_hex: &str, data: &[u8]) -> std::io::Result<PathBuf> {
        use std::io::Write;

        if !is_sha_hex(sha_hex) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "not a sha256 hex name",
            ));
        }
        let tmp = self.staging_tmp();
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        let final_path = self.entry_path(sha_hex);
        self.commit(tmp, final_path.clone()).await?;
        Ok(final_path)
    }

    pub async fn read_entry(&self, sha_hex: &str) -> std::io::Result<Vec<u8>> {
        tokio::fs::read(self.open_path(sha_hex).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "no such cache entry")
        })?)
        .await
    }

    /// Bytes currently held by committed entries. Staging files are excluded:
    /// they are invisible to readers until renamed.
    pub async fn total_bytes(&self) -> std::io::Result<u64> {
        let root = self.root.clone();
        let lock = self.scan_lock.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = lock.lock().ok();
            Ok(scan_entries(&root)?
                .into_iter()
                .map(|(_, s)| s.size)
                .sum())
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?
    }

    /// Evicts coldest entries (by min(atime, mtime)) until usage fits `quota`.
    /// Returns the post-eviction total.
    pub async fn evict_to(&self, quota: u64) -> std::io::Result<u64> {
        let root = self.root.clone();
        let lock = self.scan_lock.clone();
        tokio::task::spawn_blocking(move || evict_root(root, lock, quota))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?
    }

    fn evict_to_blocking(&self, quota: u64) -> std::io::Result<u64> {
        evict_root(self.root.clone(), self.scan_lock.clone(), quota)
    }

    /// Boot-time orphan sweep: staging files have no owner once the writing
    /// task is gone, and age is the only signal distinguishing debris from a
    /// concurrent writer.
    fn sweep_stale_staging(&self) {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return;
        };
        let cutoff = FileTime::from_system_time(
            SystemTime::now()
                .checked_sub(STALE_TMP_AGE)
                .unwrap_or(SystemTime::UNIX_EPOCH),
        );
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || is_sha_hex(&file_name_of(&path)) {
                continue;
            }
            let modified = meta
                .modified()
                .map(|t| FileTime::from_system_time(t))
                .unwrap_or(FileTime::zero());
            if modified < cutoff {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

fn evict_root(
    root: PathBuf,
    lock: Arc<Mutex<()>>,
    quota: u64,
) -> std::io::Result<u64> {
    let _guard = lock.lock().ok();
    let mut entries = scan_entries(&root)?;
    // Coldest first: min(atime, mtime), because noatime mounts make atime
    // useless alone and mtime alone would rank freshly-read old files cold.
    entries.sort_by_key(|(_, s)| s.cold);
    let mut total: u64 = entries.iter().map(|(_, s)| s.size).sum();
    for (path, stat) in &entries {
        if total <= quota {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total = total.saturating_sub(stat.size);
        }
    }
    Ok(total)
}

/// (path, stat) pairs for committed entries only; anything else in the root is
/// staging debris owned by a live download or awaiting the boot sweep.
fn scan_entries(root: &Path) -> std::io::Result<Vec<(PathBuf, EntryStat)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root)?.flatten() {
        let path = entry.path();
        if !is_sha_hex(&file_name_of(&path)) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let atime = meta.accessed().unwrap_or(SystemTime::now());
        let mtime = meta.modified().unwrap_or(SystemTime::now());
        let cold = FileTime::from_system_time(atime.min(mtime));
        out.push((
            path,
            EntryStat {
                cold,
                size: meta.len(),
            },
        ));
    }
    Ok(out)
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned()
}

pub(crate) fn is_sha_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::to_hex;
    use sha2::{Digest, Sha256};

    fn sha_hex(data: &[u8]) -> String {
        to_hex(&Sha256::digest(data))
    }

    #[tokio::test]
    async fn roundtrip_and_atomic_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::open(dir.path()).await.expect("open");
        let data = b"media bytes";
        let stored = cache
            .store_bytes(&sha_hex(data), data)
            .await
            .expect("store");
        let path = cache.open_path(&sha_hex(data)).expect("entry exists");
        assert_eq!(stored, path);
        assert_eq!(
            cache.read_entry(&sha_hex(data)).await.expect("read"),
            data.to_vec()
        );
        // Commit consumed the staging file: only the hash-named entry remains.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .flatten()
            .filter(|e| !is_sha_hex(&file_name_of(&e.path())))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[tokio::test]
    async fn open_path_rejects_non_hex() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::open(dir.path()).await.expect("open");
        assert!(cache.open_path("not-a-hash").is_none());
    }

    #[tokio::test]
    async fn eviction_deletes_coldest_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cache = DiskCache::open_with_quota(dir.path(), 10_000).await.expect("open");
        let names: Vec<String> = (0u8..3)
            .map(|i| sha_hex(format!("blob-{i}").as_bytes()))
            .collect();
        for (i, name) in names.iter().enumerate() {
            cache
                .store_bytes(name, vec![b'x'; 100].as_slice())
                .await
                .expect("store");
            // Distinct, ordered recency regardless of filesystem timestamp
            // resolution: name[0] oldest, name[2] newest.
            let t = FileTime::from_unix_time(1_000_000 + i as i64 * 1_000, 0);
            filetime::set_file_times(cache.entry_path(name), t, t)
                .expect("set times");
        }
        let total = cache.evict_to(250).await.expect("evict");
        assert_eq!(total, 200);
        assert!(cache.open_path(&names[0]).is_none(), "coldest evicted");
        assert!(cache.open_path(&names[1]).is_some());
        assert!(cache.open_path(&names[2]).is_some());

        // Raising the quota evicts nothing further.
        assert_eq!(cache.evict_to(10_000).await.expect("evict"), 200);
    }

    #[tokio::test]
    async fn boot_sweep_removes_only_stale_staging() {
        let dir = tempfile::tempdir().expect("tempdir");
        let stale_tmp = dir.path().join("deadbeef-0.tmp");
        std::fs::write(&stale_tmp, b"partial").expect("write");
        let old = FileTime::from_unix_time(1, 0);
        filetime::set_file_times(&stale_tmp, old, old).expect("age it");
        let fresh_tmp = dir.path().join("cafebabe-1.tmp");
        std::fs::write(&fresh_tmp, b"partial").expect("write");

        DiskCache::open(dir.path()).await.expect("open");

        assert!(!stale_tmp.exists(), "24h-old staging swept");
        assert!(fresh_tmp.exists(), "young staging preserved");
    }
}
