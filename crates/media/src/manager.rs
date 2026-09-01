//! Media orchestration: deduplicated downloads into the content-addressed
//! cache, bounded admission, and cooperative cancellation.

use std::collections::HashMap;
use std::future::Future;
use std::io::{Seek, SeekFrom};
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
use crate::{
    MAX_CONCURRENT_DOWNLOADS, MAX_CONCURRENT_UPLOADS, MAX_OUTGOING_ATTACHMENT_BYTES,
    MEDIA_QUEUE_CAPACITY, MediaError,
};

/// Result of copying a user-selected source into Wasabi-owned durable staging.
/// The filesystem path remains behind the backend boundary; the composer sees
/// only `attachment`.
#[derive(Clone, Debug)]
pub struct StagedUpload {
    pub attachment: wasabi_domain::StagedAttachment,
    pub durable_path: PathBuf,
    pub payload: wasabi_domain::TransferPayload,
}

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
            if m.static_url.is_some() {
                return None;
            }
            (
                m.direct_path.clone()?,
                m.media_key.clone(),
                m.file_sha256.clone()?,
                m.file_enc_sha256.clone(),
                m.file_length.unwrap_or(0),
                MediaType::Image,
            )
        } else if let Some(m) = base.video_message.as_option() {
            if m.static_url.is_some() {
                return None;
            }
            (
                m.direct_path.clone()?,
                m.media_key.clone(),
                m.file_sha256.clone()?,
                m.file_enc_sha256.clone(),
                m.file_length.unwrap_or(0),
                MediaType::Video,
            )
        } else if let Some(m) = base.ptv_message.as_option() {
            if m.static_url.is_some() {
                return None;
            }
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
    upload_queue: Arc<Semaphore>,
    upload_running: Arc<Semaphore>,
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
            .field("upload_running", &MAX_CONCURRENT_UPLOADS)
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
            upload_queue: Arc::new(Semaphore::new(MEDIA_QUEUE_CAPACITY)),
            upload_running: Arc::new(Semaphore::new(MAX_CONCURRENT_UPLOADS)),
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

    /// Copy a selected file into restart-safe Wasabi ownership. No original
    /// path or bytes are exposed by the returned composer projection.
    pub async fn stage_upload(
        &self,
        source: PathBuf,
        transfer: wasabi_domain::TransferId,
        cancel: CancellationToken,
    ) -> Result<StagedUpload, MediaError> {
        if transfer.as_str().is_empty() {
            return Err(MediaError::InvalidInput(
                "transfer identity is empty".to_string(),
            ));
        }
        let (kind, mime_type) = attachment_type(&source);
        let display_name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Attachment".to_string());
        let key = to_hex(&Sha256::digest(transfer.as_str().as_bytes()));
        let durable_path = self.cache.root().join(format!("outgoing-{key}.stage"));
        if tokio::fs::try_exists(&durable_path).await? {
            return Err(MediaError::InvalidInput(
                "transfer identity already has a staged file".to_string(),
            ));
        }
        let destination = durable_path.clone();
        let worker_cancel = cancel.clone();
        let copy = tokio::task::spawn_blocking(move || {
            copy_staged_file(&source, &destination, &worker_cancel)
        });
        let bytes_total = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(MediaError::Cancelled),
            result = copy => result.map_err(|error| MediaError::Io(std::io::Error::other(error.to_string())))??,
        };
        let attachment = wasabi_domain::StagedAttachment {
            transfer,
            kind,
            display_name: display_name.clone(),
            mime_type: mime_type.clone(),
            bytes_total,
        };
        Ok(StagedUpload {
            attachment,
            durable_path,
            payload: wasabi_domain::TransferPayload {
                kind,
                display_name,
                mime_type,
                caption: None,
                voice_note: false,
                duration_seconds: None,
            },
        })
    }

    /// Convert a cached image into a WhatsApp sticker and stage it for upload.
    pub async fn stage_sticker_from_image(
        &self,
        source: PathBuf,
        transfer: wasabi_domain::TransferId,
        cancel: CancellationToken,
    ) -> Result<StagedUpload, MediaError> {
        if transfer.as_str().is_empty() {
            return Err(MediaError::InvalidInput(
                "transfer identity is empty".to_string(),
            ));
        }
        let key = to_hex(&Sha256::digest(transfer.as_str().as_bytes()));
        let durable_path = self.cache.root().join(format!("outgoing-{key}.stage"));
        if tokio::fs::try_exists(&durable_path).await? {
            return Err(MediaError::InvalidInput(
                "transfer identity already has a staged file".to_string(),
            ));
        }
        let destination = durable_path.clone();
        let worker_cancel = cancel.clone();
        let convert = tokio::task::spawn_blocking(move || {
            if worker_cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            crate::sticker::convert_image_to_sticker_file(&source, &destination)
        });
        let bytes_total = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(MediaError::Cancelled),
            result = convert => result.map_err(|error| MediaError::Io(std::io::Error::other(error.to_string())))??,
        };
        Ok(StagedUpload {
            attachment: wasabi_domain::StagedAttachment {
                transfer,
                kind: wasabi_domain::AttachmentKind::Sticker,
                display_name: "sticker.webp".to_string(),
                mime_type: "image/webp".to_string(),
                bytes_total,
            },
            durable_path,
            payload: wasabi_domain::TransferPayload {
                kind: wasabi_domain::AttachmentKind::Sticker,
                display_name: "sticker.webp".to_string(),
                mime_type: "image/webp".to_string(),
                caption: None,
            },
        })
    }

    /// Remove only a Wasabi-owned outgoing stage; arbitrary paths are refused.
    pub async fn discard_staged_upload(&self, path: PathBuf) -> Result<(), MediaError> {
        let owned = path.parent() == Some(self.cache.root())
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("outgoing-") && name.ends_with(".stage"));
        if !owned {
            return Err(MediaError::InvalidInput(
                "refusing to remove a non-staging path".to_string(),
            ));
        }
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(MediaError::Io(error)),
        }
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

    /// Encrypt and upload one Wasabi-owned staged file without holding the
    /// plaintext or ciphertext in memory. The current protocol client is
    /// captured only after admission, so reconnects replace stale clients.
    pub async fn upload(
        &self,
        source: PathBuf,
        kind: wasabi_domain::AttachmentKind,
        cancel: CancellationToken,
    ) -> Result<whatsapp_rust::upload::UploadResponse, MediaError> {
        let metadata = tokio::fs::metadata(&source).await?;
        if !metadata.is_file() {
            return Err(MediaError::InvalidInput(
                "attachment source is not a regular file".to_string(),
            ));
        }
        if metadata.len() == 0 {
            return Err(MediaError::InvalidInput(
                "attachment source is empty".to_string(),
            ));
        }
        if metadata.len() > MAX_OUTGOING_ATTACHMENT_BYTES {
            return Err(MediaError::InvalidInput(
                "attachment exceeds the configured maximum".to_string(),
            ));
        }

        let _admission = self
            .upload_queue
            .try_acquire()
            .map_err(|_| MediaError::Overloaded)?;
        let _slot = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(MediaError::Cancelled),
            slot = self.upload_running.acquire() => slot.map_err(|_| MediaError::Unavailable)?,
        };
        let client = self
            .client_provider
            .client()
            .await
            .ok_or(MediaError::Unavailable)?;
        let media_type = attachment_media_type(kind);
        let encrypted_path = self.cache.staging_tmp();
        let guard = TmpGuard(encrypted_path.clone());
        let encrypt_source = source.clone();
        let encrypt_destination = encrypted_path.clone();
        let encryption = tokio::task::spawn_blocking(move || {
            encrypt_file(&encrypt_source, &encrypt_destination, media_type)
        });
        let info = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(MediaError::Cancelled),
            result = encryption => result
                .map_err(|error| MediaError::Encryption(error.to_string()))??,
        };
        let encrypted_len = tokio::fs::metadata(&encrypted_path).await?.len();
        let upload_source = FileUploadSource {
            path: encrypted_path,
            len: encrypted_len,
        };
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(MediaError::Cancelled),
            result = client.upload_stream(upload_source, info, media_type) =>
                result.map_err(|error| MediaError::Upload(error.to_string()))?,
        };
        drop(guard);
        Ok(response)
    }
}

fn copy_staged_file(
    source: &std::path::Path,
    destination: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<u64, MediaError> {
    use std::io::{Read as _, Write as _};

    let outcome = (|| {
        let source = std::fs::File::open(source)?;
        if !source.metadata()?.is_file() {
            return Err(MediaError::InvalidInput(
                "attachment source is not a regular file".to_string(),
            ));
        }
        let mut source = source;
        let mut staged = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        let mut copied = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if cancel.is_cancelled() {
                return Err(MediaError::Cancelled);
            }
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            copied = copied.saturating_add(read as u64);
            if copied > MAX_OUTGOING_ATTACHMENT_BYTES {
                return Err(MediaError::InvalidInput(
                    "attachment exceeds the configured maximum".to_string(),
                ));
            }
            staged.write_all(&buffer[..read])?;
        }
        if copied == 0 {
            return Err(MediaError::InvalidInput(
                "attachment source is empty".to_string(),
            ));
        }
        staged.sync_all()?;
        Ok(copied)
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    outcome
}

fn attachment_type(path: &std::path::Path) -> (wasabi_domain::AttachmentKind, String) {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    use wasabi_domain::AttachmentKind;
    match extension.as_str() {
        "jpg" | "jpeg" => (AttachmentKind::Image, "image/jpeg".to_string()),
        "png" => (AttachmentKind::Image, "image/png".to_string()),
        "webp" => (AttachmentKind::Image, "image/webp".to_string()),
        "gif" => (AttachmentKind::Image, "image/gif".to_string()),
        "mp4" | "m4v" => (AttachmentKind::Video, "video/mp4".to_string()),
        "mov" => (AttachmentKind::Video, "video/quicktime".to_string()),
        "webm" => (AttachmentKind::Video, "video/webm".to_string()),
        "mp3" => (AttachmentKind::Audio, "audio/mpeg".to_string()),
        "m4a" => (AttachmentKind::Audio, "audio/mp4".to_string()),
        "ogg" | "opus" => (AttachmentKind::Audio, "audio/ogg".to_string()),
        "wav" => (AttachmentKind::Audio, "audio/wav".to_string()),
        "pdf" => (AttachmentKind::Document, "application/pdf".to_string()),
        "txt" => (AttachmentKind::Document, "text/plain".to_string()),
        "csv" => (AttachmentKind::Document, "text/csv".to_string()),
        "zip" => (AttachmentKind::Document, "application/zip".to_string()),
        _ => (
            AttachmentKind::Document,
            "application/octet-stream".to_string(),
        ),
    }
}

#[derive(Clone)]
struct FileUploadSource {
    path: PathBuf,
    len: u64,
}

impl wacore::upload::UploadSource for FileUploadSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn reader_from(&self, offset: u64) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset.min(self.len)))?;
        Ok(Box::new(file))
    }
}

fn encrypt_file(
    source: &std::path::Path,
    destination: &std::path::Path,
    media_type: MediaType,
) -> Result<wacore::upload::EncryptedMediaInfo, MediaError> {
    let mut source = std::fs::File::open(source)?;
    let mut destination = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let info = wacore::upload::encrypt_media_streaming(&mut source, &mut destination, media_type)
        .map_err(|error| MediaError::Encryption(error.to_string()))?;
    destination.sync_all()?;
    Ok(info)
}

fn attachment_media_type(kind: wasabi_domain::AttachmentKind) -> MediaType {
    match kind {
        wasabi_domain::AttachmentKind::Image => MediaType::Image,
        wasabi_domain::AttachmentKind::Video => MediaType::Video,
        wasabi_domain::AttachmentKind::Audio => MediaType::Audio,
        wasabi_domain::AttachmentKind::Document => MediaType::Document,
        wasabi_domain::AttachmentKind::Sticker => MediaType::Sticker,
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
    use waproto::buffa::MessageField;

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

    #[test]
    fn file_encryption_is_streamed_and_preserves_plaintext_hash() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("plain.bin");
        let encrypted = dir.path().join("encrypted.bin");
        let bytes = vec![0x5a; 256 * 1024 + 7];
        std::fs::write(&source, &bytes).expect("seed source");
        let info = encrypt_file(&source, &encrypted, MediaType::Document).expect("encrypt");
        assert_eq!(info.file_length, bytes.len() as u64);
        assert_eq!(
            info.file_sha256.as_slice(),
            Sha256::digest(&bytes).as_slice()
        );
        assert_eq!(
            std::fs::metadata(encrypted)
                .expect("encrypted metadata")
                .len(),
            wacore::upload::encrypted_len(bytes.len()) as u64
        );
    }

    #[test]
    fn attachment_kinds_map_to_protocol_media_classes() {
        assert_eq!(
            attachment_media_type(wasabi_domain::AttachmentKind::Image),
            MediaType::Image
        );
        assert_eq!(
            attachment_media_type(wasabi_domain::AttachmentKind::Video),
            MediaType::Video
        );
        assert_eq!(
            attachment_media_type(wasabi_domain::AttachmentKind::Audio),
            MediaType::Audio
        );
        assert_eq!(
            attachment_media_type(wasabi_domain::AttachmentKind::Document),
            MediaType::Document
        );
        assert_eq!(
            attachment_media_type(wasabi_domain::AttachmentKind::Sticker),
            MediaType::Sticker
        );
    }

    #[test]
    fn durable_staging_copies_exact_bytes_and_cleans_cancelled_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("source.pdf");
        let staged = dir.path().join("outgoing.stage");
        let bytes = vec![0x33; 128 * 1024 + 3];
        std::fs::write(&source, &bytes).expect("seed source");
        let copied =
            copy_staged_file(&source, &staged, &CancellationToken::new()).expect("stage source");
        assert_eq!(copied, bytes.len() as u64);
        assert_eq!(std::fs::read(&staged).expect("read stage"), bytes);

        let cancelled_path = dir.path().join("cancelled.stage");
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            copy_staged_file(&source, &cancelled_path, &cancelled),
            Err(MediaError::Cancelled)
        );
        assert!(!cancelled_path.exists());
    }

    #[test]
    fn attachment_type_is_specific_but_unknown_files_stay_documents() {
        assert_eq!(
            attachment_type(std::path::Path::new("photo.JPEG")),
            (
                wasabi_domain::AttachmentKind::Image,
                "image/jpeg".to_string()
            )
        );
        assert_eq!(
            attachment_type(std::path::Path::new("archive.unknown")),
            (
                wasabi_domain::AttachmentKind::Document,
                "application/octet-stream".to_string()
            )
        );
    }

    fn downloadable_video(static_url: Option<&str>) -> wa::message::VideoMessage {
        wa::message::VideoMessage {
            direct_path: Some("/v/t62.7118-24/media".to_string()),
            static_url: static_url.map(str::to_string),
            media_key: Some(vec![1; 32]),
            file_sha256: Some(vec![2; 32]),
            file_enc_sha256: Some(vec![3; 32]),
            file_length: Some(123),
            ..Default::default()
        }
    }

    #[test]
    fn raw_download_params_refuse_media_that_requires_static_url() {
        let image = wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                direct_path: Some("/v/t62.7118-24/media".to_string()),
                static_url: Some("https://static.cdn.example/media/image".to_string()),
                media_key: Some(vec![1; 32]),
                file_sha256: Some(vec![2; 32]),
                file_enc_sha256: Some(vec![3; 32]),
                file_length: Some(123),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            media_downloadable(&image).is_none(),
            "DownloadParams cannot preserve ImageMessage.static_url"
        );

        let video = wa::Message {
            video_message: MessageField::some(downloadable_video(Some(
                "https://static.cdn.example/media/video",
            ))),
            ..Default::default()
        };
        assert!(
            media_downloadable(&video).is_none(),
            "DownloadParams cannot preserve VideoMessage.static_url"
        );

        let ptv = wa::Message {
            ptv_message: MessageField::some(downloadable_video(Some(
                "https://static.cdn.example/media/ptv",
            ))),
            ..Default::default()
        };
        assert!(
            media_downloadable(&ptv).is_none(),
            "DownloadParams cannot preserve PTV static_url"
        );
    }

    #[test]
    fn raw_download_params_keep_host_routed_video_metadata() {
        let message = wa::Message {
            video_message: MessageField::some(downloadable_video(None)),
            ..Default::default()
        };
        let params = media_downloadable(&message).expect("host-routed media can use raw params");
        assert_eq!(params.direct_path, "/v/t62.7118-24/media");
        assert_eq!(params.media_key.as_deref(), Some(&[1; 32][..]));
        assert_eq!(params.file_sha256, vec![2; 32]);
        assert_eq!(params.file_enc_sha256.as_deref(), Some(&[3; 32][..]));
        assert_eq!(params.file_length, 123);
        assert_eq!(params.media_type, MediaType::Video);
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
