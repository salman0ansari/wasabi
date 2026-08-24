//! Media orchestration: deduplicated downloads into the content-addressed
//! cache, bounded admission, and cooperative cancellation.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, watch};
use tokio_util::sync::CancellationToken;
use waproto::whatsapp as wa;
use whatsapp_rust::client::Client;
use whatsapp_rust::download::{DownloadParams, DownloadWriter, Downloadable, MediaType};
use whatsapp_rust::wacore::proto_helpers::MessageExt;
use whatsapp_rust_chat_store::ChatStore;

use crate::cache::{DiskCache, is_sha_hex, to_hex};
use crate::{MAX_CONCURRENT_DOWNLOADS, MEDIA_QUEUE_CAPACITY, MediaError};

/// Maps a received message onto the upstream raw-params downloader.
///
/// Returns `None` for media that cannot round-trip through CDN params alone —
/// notably newsletter/channel blobs whose [`Downloadable::static_url`] bypasses
/// host construction; those callers must keep the original typed message and
/// pass it directly to [`MediaManager::download`].
pub fn media_downloadable(message: &wa::Message) -> Option<DownloadParams> {
    let base = message.get_base_message();
    let (direct_path, media_key, file_sha256, file_enc_sha256, file_length, media_type) =
        if let Some(m) = base.image_message.as_option() {
            (
                m.direct_path.clone()?,
                m.media_key.clone(),
                m.file_sha256.clone()?,
                m.file_enc_sha256.clone(),
                m.file_length.unwrap_or(0),
                MediaType::Image,
            )
        } else if let Some(m) = base.video_message.as_option() {
            (
                m.direct_path.clone()?,
                m.media_key.clone(),
                m.file_sha256.clone()?,
                m.file_enc_sha256.clone(),
                m.file_length.unwrap_or(0),
                MediaType::Video,
            )
        } else if let Some(m) = base.ptv_message.as_option() {
            (
                m.direct_path.clone()?,
                m.media_key.clone(),
                m.file_sha256.clone()?,
                m.file_enc_sha256.clone(),
                m.file_length.unwrap_or(0),
                MediaType::Video,
            )
        } else if let Some(m) = base.audio_message.as_option() {
            (
                m.direct_path.clone()?,
                m.media_key.clone(),
                m.file_sha256.clone()?,
                m.file_enc_sha256.clone(),
                m.file_length.unwrap_or(0),
                MediaType::Audio,
            )
        } else if let Some(m) = base.document_message.as_option() {
            (
                m.direct_path.clone()?,
                m.media_key.clone(),
                m.file_sha256.clone()?,
                m.file_enc_sha256.clone(),
                m.file_length.unwrap_or(0),
                MediaType::Document,
            )
        } else {
            let m = base.sticker_message.as_option()?;
            (
                m.direct_path.clone()?,
                m.media_key.clone(),
                m.file_sha256.clone()?,
                m.file_enc_sha256.clone(),
                m.file_length.unwrap_or(0),
                MediaType::Sticker,
            )
        };

    Some(DownloadParams {
        direct_path,
        media_key,
        file_sha256,
        file_enc_sha256,
        file_length,
        media_type,
    })
}

/// Streams decrypted plaintext into `inner` while feeding a SHA-256 over every
/// byte written, so verification costs no extra pass over the file.
///
/// The upstream retry loop calls `truncate(0)` before each fresh attempt; a
/// zero-length truncate therefore resets the hash, keeping the digest equal to
/// exactly the bytes of the surviving attempt.
pub(crate) struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W> HashingWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// Consumes the wrapper: sink back plus digest of what survived.
    pub(crate) fn finalize(self) -> (W, [u8; 32]) {
        (self.inner, self.hasher.finalize().into())
    }
}

impl<W: std::io::Write> std::io::Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// Seek position is irrelevant to the digest: plaintext is written strictly
// sequentially, and the finish rewind carries no bytes.
impl<W: std::io::Seek> std::io::Seek for HashingWriter<W> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl<W: DownloadWriter> DownloadWriter for HashingWriter<W> {
    fn truncate(&mut self, len: u64) -> std::io::Result<()> {
        let r = self.inner.truncate(len);
        if len == 0 && r.is_ok() {
            self.hasher = Sha256::new();
        }
        r
    }
}

/// Removes the staging file unless the download was promoted by rename.
struct TmpGuard(PathBuf);

impl Drop for TmpGuard {
    fn drop(&mut self) {
        // One small unlink; a blocking-pool round trip costs more than the
        // syscall it would wrap.
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Clone)]
pub struct MediaManager {
    client_provider: Arc<dyn ClientProvider>,
    chats: Arc<ChatStore>,
    cache: DiskCache,
    /// Admission gate: caps pending-plus-running operations, so saturation
    /// surfaces as `Overloaded` instead of an unbounded wait set.
    queue: Arc<Semaphore>,
    running: Arc<Semaphore>,
    inflight: OpRegistry<PathBuf>,
}

/// Resolves the currently connected protocol client for each operation.
/// Implementations may swap clients across reconnects; no manager rebuild is
/// required and an operation always captures exactly one client snapshot.
#[async_trait::async_trait]
pub trait ClientProvider: Send + Sync {
    async fn client(&self) -> Option<Arc<Client>>;
}

struct FixedClientProvider(Arc<Client>);

#[async_trait::async_trait]
impl ClientProvider for FixedClientProvider {
    async fn client(&self) -> Option<Arc<Client>> {
        Some(Arc::clone(&self.0))
    }
}

impl std::fmt::Debug for MediaManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaManager")
            .field("queue", &MEDIA_QUEUE_CAPACITY)
            .field("running", &MAX_CONCURRENT_DOWNLOADS)
            .finish()
    }
}

impl MediaManager {
    pub fn new(cache: DiskCache, chats: Arc<ChatStore>, client: Arc<Client>) -> Self {
        Self::with_provider(cache, chats, Arc::new(FixedClientProvider(client)))
    }

    pub fn with_provider(
        cache: DiskCache,
        chats: Arc<ChatStore>,
        client_provider: Arc<dyn ClientProvider>,
    ) -> Self {
        Self {
            client_provider,
            chats,
            cache,
            queue: Arc::new(Semaphore::new(MEDIA_QUEUE_CAPACITY)),
            running: Arc::new(Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
            inflight: OpRegistry::new(),
        }
    }

    pub fn cache(&self) -> &DiskCache {
        &self.cache
    }

    /// Local path of previously fetched media, refreshing its recency.
    pub fn cached_path(&self, sha: [u8; 32]) -> Option<PathBuf> {
        self.cache.lookup_touch(&to_hex(&sha))
    }

    /// Downloads (or joins an identical in-flight download of) `media`,
    /// publishing it in the disk cache under its verified SHA-256.
    ///
    /// `expected_sha`, when provided, must match the streamed content or the
    /// attempt is discarded as `Unavailable`. `cancel` retires THIS caller:
    /// co-waiters keep the shared operation alive until they too are gone.
    pub async fn download<M>(
        &self,
        media: M,
        expected_sha: Option<[u8; 32]>,
        mime_hint: Option<String>,
        cancel: CancellationToken,
    ) -> Result<PathBuf, MediaError>
    where
        M: Downloadable + 'static,
    {
        let key = dedupe_key(&media, expected_sha);
        let known_hex = key_as_sha(&key);

        if let Some(hex) = &known_hex
            && let Some(path) = self.cache.lookup_touch(hex)
        {
            return Ok(path);
        }

        // Held across the whole wait so saturation bounds total backlog, not
        // just active network slots.
        let _admission = self
            .queue
            .try_acquire()
            .map_err(|_| MediaError::Overloaded)?;

        let client_provider = Arc::clone(&self.client_provider);
        let chats = self.chats.clone();
        let cache = self.cache.clone();
        let running = self.running.clone();

        self.inflight
            .clone()
            .run(key, cancel, move |op_cancel| async move {
                let _slot = running
                    .acquire()
                    .await
                    .map_err(|_| MediaError::Unavailable)?;

                let client = client_provider
                    .client()
                    .await
                    .ok_or(MediaError::Unavailable)?;

                // A peer may have published while this attempt sat queued.
                if let Some(hex) = &known_hex
                    && let Some(path) = cache.lookup_touch(hex)
                {
                    return Ok(path);
                }

                let tmp = cache.staging_tmp();
                let guard = TmpGuard(tmp.clone());
                let tokio_file = tokio::fs::File::create(&tmp).await?;
                let writer = HashingWriter::new(tokio_file.into_std().await);

                let writer = tokio::select! {
                    biased;
                    _ = op_cancel.cancelled() => return Err(MediaError::Cancelled),
                    written = client.download_to_writer(&media, writer) =>
                        written.map_err(|e| MediaError::Download(e.to_string()))?,
                };

                let (file, digest) = writer.finalize();
                if let Some(expected) = expected_sha
                    && digest != expected
                {
                    // Content addressing means a wrong digest must never reach
                    // its final name; the guard removes the staging copy.
                    return Err(MediaError::Unavailable);
                }
                let size = file.metadata().map(|m| m.len()).ok();
                // Durability boundary: bytes survive a crash only once both the
                // fsync and the rename below complete.
                file.sync_all()?;
                drop(file);

                let final_path = cache.entry_path(&to_hex(&digest));
                cache.commit(tmp, final_path.clone()).await?;
                drop(guard);

                // The ref table indexes blobs; it is not their source of truth
                // on disk, so persistence hiccups must not fail the fetch.
                if let Err(e) = chats
                    .put_media_ref(
                        digest.to_vec(),
                        final_path.display().to_string(),
                        mime_hint,
                        size.and_then(|s| i64::try_from(s).ok()),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "media ref write failed");
                }

                Ok(final_path)
            })
            .await
    }
}

/// Identity for in-flight collapsing: expected/declared hash when available,
/// otherwise a best-effort reference identity until content fixes the name.
fn dedupe_key(media: &dyn Downloadable, expected_sha: Option<[u8; 32]>) -> String {
    if let Some(h) = expected_sha {
        return to_hex(&h);
    }
    if let Some(s) = media.file_sha256()
        && s.len() == 32
    {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(s);
        return to_hex(&arr);
    }
    format!(
        "pending:{}:{}",
        media.direct_path().unwrap_or(""),
        media.file_length().unwrap_or(0)
    )
}

fn key_as_sha(key: &str) -> Option<String> {
    is_sha_hex(key).then(|| key.to_owned())
}

/// Collapses concurrent equal-key operations onto one execution.
///
/// Ownership model: the FIRST requester spawns the worker; the registry keeps
/// it alive independent of any single waiter. When the last waiter detaches
/// before completion the op token fires, so abandoned work stops promptly, and
/// finished entries always remove themselves — the map never retains history.
pub(crate) struct OpRegistry<T> {
    inner: Arc<RegistryInner<T>>,
}

// Manual impl: cloning a registry must not require T: Clone — the shared map
// is type-erased per instantiation, and waiters only exchange T values.
impl<T> Clone for OpRegistry<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct RegistryInner<T> {
    map: Mutex<HashMap<String, Arc<SharedOp<T>>>>,
}

struct SharedOp<T> {
    token: CancellationToken,
    tx: watch::Sender<Option<Result<T, MediaError>>>,
    state: Mutex<OpState>,
}

/// Waiter bookkeeping lives behind its own mutex so the insert-vs-detach race
/// (last waiter leaving exactly as a newcomer arrives) resolves serially.
struct OpState {
    waiters: u32,
    dead: bool,
}

impl<T> OpRegistry<T>
where
    T: Clone + Send + Sync + 'static,
{
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                map: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Runs (or joins) the operation for `key`. `cancel` retires this caller
    /// only; the shared op survives while any waiter remains.
    ///
    /// Takes the registry by value (an Arc clone) so the returned future stays
    /// `'static` for direct spawning.
    pub(crate) async fn run<F, Fut>(
        self,
        key: String,
        cancel: CancellationToken,
        factory: F,
    ) -> Result<T, MediaError>
    where
        F: FnOnce(CancellationToken) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, MediaError>> + Send + 'static,
    {
        // NOTE: returns the shared outcome directly; registry-level failure
        // paths surface as the same MediaError values.
        let (shared, created) = {
            let mut map = unlock(&self.inner.map);
            // A dead shell means its cohort vanished mid-flight; replace it so
            // a newcomer starts fresh work rather than inheriting a cancelled-
            // era outcome.
            if let Some(existing) = map.get(&key)
                && unlock(&existing.state).dead
            {
                map.remove(&key);
            }
            if let Some(existing) = map.get(&key) {
                unlock(&existing.state).waiters += 1;
                (existing.clone(), false)
            } else {
                let (tx, _) = watch::channel(None);
                let op = Arc::new(SharedOp {
                    token: CancellationToken::new(),
                    tx,
                    state: Mutex::new(OpState {
                        waiters: 1,
                        dead: false,
                    }),
                });
                map.insert(key.clone(), op.clone());
                (op, true)
            }
        };

        if created {
            let registry = self.inner.clone();
            let op_key = key.clone();
            let op_shared = shared.clone();
            tokio::spawn(async move {
                let _entry = RegGuard {
                    registry,
                    key: op_key,
                    shared: op_shared.clone(),
                };
                // Panic or forced-drop of this task must still wake waiters,
                // or they would hang on a dead op forever.
                let mut failsafe = FailSafe {
                    tx: Some(op_shared.tx.clone()),
                    fallback: Err(MediaError::Unavailable),
                    pending: true,
                };

                let outcome = tokio::select! {
                    biased;
                    _ = op_shared.token.cancelled() => Err(MediaError::Cancelled),
                    result = factory(op_shared.token.child_token()) => result,
                };
                if let Some(tx) = failsafe.tx.take() {
                    let _ = tx.send_replace(Some(outcome));
                }
                failsafe.pending = false;
            });
        }

        let mut rx = shared.tx.subscribe();
        let outcome: Result<T, MediaError> = loop {
            if let Some(value) = rx.borrow_and_update().clone() {
                break value;
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break Err(MediaError::Cancelled),
                changed = rx.changed() => {
                    if changed.is_err() {
                        break Err(MediaError::Unavailable);
                    }
                }
            }
        };

        // Release the seat only now, so a cancellation racing completion cannot
        // strand the count above zero and leak a live token.
        {
            let map = unlock(&self.inner.map);
            if map
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &shared))
            {
                let mut state = unlock(&shared.state);
                state.waiters = state.waiters.saturating_sub(1);
                if state.waiters == 0 && shared.tx.borrow().is_none() {
                    // Nobody left to consume the result: stop the work and mark
                    // the shell dead so newcomers start fresh instead of joining.
                    state.dead = true;
                    shared.token.cancel();
                }
            }
        }

        outcome
    }

    #[cfg(test)]
    fn inflight_len(&self) -> usize {
        unlock(&self.inner.map).len()
    }
}

// Locks are held only across field updates, never awaits; poisoning cannot
// carry meaningful state here, so recovery-by-value keeps call sites flat.
fn unlock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct RegGuard<T> {
    registry: Arc<RegistryInner<T>>,
    key: String,
    shared: Arc<SharedOp<T>>,
}

impl<T> Drop for RegGuard<T> {
    fn drop(&mut self) {
        let mut map = unlock(&self.registry.map);
        // Ptr-equality guards against a replacement shell owning this key now.
        if map
            .get(&self.key)
            .is_some_and(|current| Arc::ptr_eq(current, &self.shared))
        {
            map.remove(&self.key);
        }
    }
}

/// Panic/forced-drop safety net: a worker that dies without publishing an
/// outcome must still wake its waiters, or they park forever on the watch.
struct FailSafe<T: Clone + Send + 'static> {
    tx: Option<watch::Sender<Option<Result<T, MediaError>>>>,
    fallback: Result<T, MediaError>,
    pending: bool,
}

impl<T: Clone + Send + 'static> Drop for FailSafe<T> {
    fn drop(&mut self) {
        if self.pending
            && let Some(tx) = self.tx.take()
        {
            let _ = tx.send_replace(Some(self.fallback.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::to_hex;
    use sha2::{Digest, Sha256};
    use std::io::Seek;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    const ABC_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn hashing_writer_matches_known_vector() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("abc.bin");
        let mut w = HashingWriter::new(std::fs::File::create(&path).expect("create"));
        use std::io::Write as _;
        w.write_all(b"abc").expect("write");
        let (file, digest) = w.finalize();
        drop(file);
        assert_eq!(std::fs::read(&path).expect("read"), b"abc".to_vec());
        assert_eq!(to_hex(&digest), ABC_SHA);
    }

    #[test]
    fn hashing_writer_resets_on_zero_truncate() {
        // Mirrors the upstream retry contract: a cleared sink hashes as if the
        // discarded attempt never happened.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("reset.bin");
        let mut w = HashingWriter::new(std::fs::File::create(&path).expect("create"));
        use std::io::Write as _;
        w.write_all(b"garbage").expect("first attempt");
        w.truncate(0).expect("truncate");
        w.rewind().expect("rewind");
        w.write_all(b"xyz").expect("retry");
        let (_, digest) = w.finalize();
        assert_eq!(
            to_hex(&digest),
            to_hex(&Sha256::digest(b"xyz")),
            "digest must cover only the surviving attempt"
        );
    }

    type TestOutcome = u64;

    async fn wait_until(len: impl Fn() -> usize, want: usize) {
        for _ in 0..200 {
            if len() == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(len(), want, "registry did not settle in time");
    }

    // multi_thread: the handshake below blocks on a std channel, which would
    // starve a current-thread runtime's spawned workers.
    #[tokio::test(flavor = "multi_thread")]
    async fn registry_collapses_waiters_into_one_operation() {
        let registry: OpRegistry<TestOutcome> = OpRegistry::new();
        let spawned = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let started_tx = Arc::new(started_tx);
        let (release_tx, release_rx) = tokio::sync::mpsc::channel::<()>(16);
        let release_rx = Arc::new(tokio::sync::Mutex::new(release_rx));

        let mut joins = Vec::new();
        for _ in 0..5 {
            let registry = registry.clone();
            let spawned = spawned.clone();
            let started_tx = started_tx.clone();
            let release_rx = release_rx.clone();
            joins.push(tokio::spawn(registry.run(
                "same-key".to_owned(),
                CancellationToken::new(),
                move |_token| async move {
                    spawned.fetch_add(1, Ordering::SeqCst);
                    started_tx.send(()).expect("leader alive");
                    let _ = release_rx.lock().await.recv().await;
                    Ok(42)
                },
            )));
        }
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("exactly-once start handshake");

        drop(release_tx);
        for join in joins {
            assert_eq!(join.await.expect("task"), Ok(42));
        }

        assert_eq!(spawned.load(Ordering::SeqCst), 1);
        wait_until(|| registry.inflight_len(), 0).await;
    }

    #[tokio::test]
    async fn registry_cancels_op_when_last_waiter_leaves() {
        let registry: OpRegistry<TestOutcome> = OpRegistry::new();
        // mpsc(1): oneshot's Sender is neither Clone nor reusable, and the
        // factory fires the handshake from inside the spawned op.
        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<()>(1);
        let token = CancellationToken::new();

        let waiter = {
            let registry = registry.clone();
            let started_tx = started_tx.clone();
            tokio::spawn(registry.run(
                "doomed".to_owned(),
                token.clone(),
                move |op_token| async move {
                    let _ = started_tx.send(()).await;
                    op_token.cancelled().await;
                    Err(MediaError::Cancelled)
                },
            ))
        };

        started_rx.recv().await.expect("started");
        token.cancel();
        assert_eq!(
            waiter.await.expect("task"),
            Err(MediaError::Cancelled),
            "sole waiter retires with its own cancellation"
        );
        wait_until(|| registry.inflight_len(), 0).await;
    }

    #[tokio::test]
    async fn completed_operations_are_never_retained() {
        let registry: OpRegistry<TestOutcome> = OpRegistry::new();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let first = {
            let registry = registry.clone();
            tokio::spawn(registry.run(
                "once".to_owned(),
                CancellationToken::new(),
                move |_token| async move {
                    gate_rx.await.expect("gate open");
                    Ok(7)
                },
            ))
        };
        gate_tx.send(()).ok();
        assert_eq!(first.await.expect("task"), Ok(7));
        assert_eq!(registry.inflight_len(), 0, "entry removed at completion");

        // A follow-up request starts fresh work rather than replaying history.
        let again = {
            let registry = registry.clone();
            tokio::spawn(registry.run(
                "once".to_owned(),
                CancellationToken::new(),
                |_token| async move { Ok(8) },
            ))
        };
        assert_eq!(again.await.expect("task"), Ok(8));
    }
}
