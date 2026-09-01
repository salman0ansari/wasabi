//! Bounded thumbnail decoding: a byte-budgeted LRU over scaled results plus a
//! small decode worker pool, so a burst of gallery requests can neither pin
//! unbounded memory nor monopolize CPU.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use tokio::sync::Semaphore;

use crate::{DECODED_IMAGE_CACHE_BUDGET_BYTES, MAX_DECODE_WORKERS, MediaError};

/// Cache identity: content hash when the file lives under its hash name (the
/// common case inside the media cache), otherwise a path+metadata fingerprint
/// so edited files cannot serve stale decodes.
fn cache_key(path: &Path, max_dim: u32) -> String {
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        && crate::cache::is_sha_hex(stem)
    {
        return format!("{stem}:{max_dim}");
    }
    let meta = std::fs::metadata(path).ok();
    let fingerprint = (
        path.as_os_str(),
        meta.as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos()),
        meta.as_ref().map(|m| m.len()),
    );
    use std::hash::{Hash, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    fingerprint.hash(&mut hasher);
    format!("derived-{:016x}:{max_dim}", hasher.finish())
}

struct LruSlot {
    value: Arc<Vec<u8>>,
    bytes: u64,
    stamp: u64,
}

#[derive(Clone)]
pub struct ThumbnailService {
    /// Decode is heavy CPU: the pool caps concurrent decodes instead of
    /// letting every request hit the blocking pool at once.
    decode_slots: Arc<Semaphore>,
    inner: Arc<Mutex<LruState>>,
}

#[derive(Default)]
struct LruState {
    map: HashMap<String, LruSlot>,
    used: u64,
    budget: u64,
    clock: u64,
}

impl std::fmt::Debug for ThumbnailService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbnailService")
            .field("budget", &self.inner.lock().map(|s| s.budget))
            .finish()
    }
}

impl ThumbnailService {
    pub fn new() -> Self {
        Self::with_budget(DECODED_IMAGE_CACHE_BUDGET_BYTES)
    }

    pub fn with_budget(budget: u64) -> Self {
        Self {
            decode_slots: Arc::new(Semaphore::new(MAX_DECODE_WORKERS)),
            inner: Arc::new(Mutex::new(LruState {
                budget,
                ..LruState::default()
            })),
        }
    }

    /// Scaled thumbnail bytes (PNG, or JPEG for photographic sources), served
    /// from the byte-budgeted LRU when warm. `max_dim` bounds the longer edge.
    pub async fn thumb(&self, path: &Path, max_dim: u32) -> Result<Arc<Vec<u8>>, MediaError> {
        if max_dim == 0 {
            return Err(MediaError::InvalidInput("max_dim must be positive".into()));
        }
        let key = cache_key(path, max_dim);

        if let Some(hit) = self.lookup(&key) {
            return Ok(hit);
        }

        // Double-checked after the permit wait: an earlier request may have
        // published this exact thumbnail while we were queued.
        let _slot = self
            .decode_slots
            .acquire()
            .await
            .map_err(|_| MediaError::Unavailable)?;
        if let Some(hit) = self.lookup(&key) {
            return Ok(hit);
        }

        let path = path.to_owned();
        let rendered = tokio::task::spawn_blocking(move || render_thumbnail(&path, max_dim))
            .await
            .map_err(|e| MediaError::Decode(e.to_string()))??;

        self.insert(key, Arc::clone(&rendered));
        Ok(rendered)
    }

    /// Decode an animated WebP on the same CPU-bounded worker pool as still
    /// thumbnails. Failure leaves the caller on the still-image path.
    pub async fn animated_webp(
        &self,
        path: &Path,
    ) -> Result<Arc<crate::animation::DecodedAnimation>, MediaError> {
        let _slot = self
            .decode_slots
            .acquire()
            .await
            .map_err(|_| MediaError::Unavailable)?;
        let path = path.to_owned();
        let decoded =
            tokio::task::spawn_blocking(move || crate::animation::decode_webp_animation(&path))
                .await
                .map_err(|error| MediaError::Decode(error.to_string()))??;
        Ok(Arc::new(decoded))
    }

    fn lookup(&self, key: &str) -> Option<Arc<Vec<u8>>> {
        let mut state = lock(&self.inner);
        // Bump the clock before taking the map borrow; recency is what
        // eviction ranks by.
        state.clock += 1;
        let stamp = state.clock;
        let slot = state.map.get_mut(key)?;
        slot.stamp = stamp;
        Some(Arc::clone(&slot.value))
    }

    fn insert(&self, key: String, value: Arc<Vec<u8>>) {
        let mut state = lock(&self.inner);
        // Approximate residency with decoded-pixel cost rather than encoded
        // size: that is the memory pressure thumbnails actually impose.
        let bytes = encoded_pixel_cost(&value);
        while state.used + bytes > state.budget && !state.map.is_empty() {
            let Some(oldest) = state
                .map
                .iter()
                .min_by_key(|(_, s)| s.stamp)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(slot) = state.map.remove(&oldest) {
                state.used = state.used.saturating_sub(slot.bytes);
            }
        }
        if bytes > state.budget {
            // Single oversized render: serve it, but do not let it evict
            // everything else or wedge the budget permanently.
            return;
        }
        state.clock += 1;
        let stamp = state.clock;
        state.used += bytes;
        state.map.insert(
            key,
            LruSlot {
                value,
                bytes,
                stamp,
            },
        );
    }

    #[cfg(test)]
    fn cached_stats(&self) -> (usize, u64) {
        let state = lock(&self.inner);
        (state.map.len(), state.used)
    }
}

impl Default for ThumbnailService {
    fn default() -> Self {
        Self::new()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Resident-cost estimate for a cached render. PNG/JPEG payloads do not carry
/// their dimensions cheaply post-encode, so decode them back once — renders
/// are tiny relative to originals, making this a one-off negligible cost.
fn encoded_pixel_cost(encoded: &[u8]) -> u64 {
    image::ImageReader::new(Cursor::new(encoded))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .map(|(w, h)| u64::from(w) * u64::from(h) * 4)
        .unwrap_or(encoded.len() as u64)
}

/// Center-crop to square and encode a 640×640 JPEG for an own-profile photo.
/// Does not log or retain the source bytes.
pub fn prepare_own_profile_picture(bytes: &[u8]) -> Result<Vec<u8>, MediaError> {
    if bytes.is_empty() {
        return Err(MediaError::InvalidInput("empty image".into()));
    }
    let source =
        image::load_from_memory(bytes).map_err(|error| MediaError::Decode(error.to_string()))?;
    let rgb = source.to_rgb8();
    let (width, height) = rgb.dimensions();
    if width == 0 || height == 0 {
        return Err(MediaError::InvalidInput("empty image".into()));
    }
    let side = width.min(height);
    let x = (width - side) / 2;
    let y = (height - side) / 2;
    let cropped = image::imageops::crop_imm(&rgb, x, y, side, side).to_image();
    let resized = image::imageops::resize(
        &cropped,
        crate::OWN_PROFILE_PICTURE_EDGE,
        crate::OWN_PROFILE_PICTURE_EDGE,
        image::imageops::FilterType::Triangle,
    );
    let mut out = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    image::DynamicImage::ImageRgb8(resized)
        .write_with_encoder(encoder)
        .map_err(|error| MediaError::Decode(error.to_string()))?;
    Ok(out.into_inner())
}

fn render_thumbnail(path: &Path, max_dim: u32) -> Result<Arc<Vec<u8>>, MediaError> {
    // Format is sniffed from the file header before decode consumes the
    // reader; DynamicImage does not retain it.
    let format = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(MediaError::Io)?
        .format();
    let source = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(MediaError::Io)?
        .decode()
        .map_err(|e| MediaError::Decode(e.to_string()))?;

    let thumb = source.thumbnail(max_dim, max_dim);
    let mut out = Cursor::new(Vec::new());
    match format {
        // Photographic input re-encodes as JPEG to keep thumbnail payloads
        // small; everything else stays lossless PNG.
        Some(image::ImageFormat::Jpeg) => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
            thumb
                .to_rgb8()
                .write_with_encoder(encoder)
                .map_err(|e| MediaError::Decode(e.to_string()))?;
        }
        _ => {
            thumb
                .write_with_encoder(image::codecs::png::PngEncoder::new(&mut out))
                .map_err(|e| MediaError::Decode(e.to_string()))?;
        }
    }
    Ok(Arc::new(out.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Write as _;

    fn write_png(dir: &Path, pixels: u32, color: [u8; 3]) -> std::path::PathBuf {
        let img = image::RgbImage::from_fn(pixels * 2, pixels, |_, _| image::Rgb(color));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_with_encoder(image::codecs::png::PngEncoder::new(&mut buf))
            .expect("encode fixture");
        let name = crate::cache::to_hex(&Sha256::digest(buf.get_ref()));
        let path = dir.join(name);
        std::fs::File::create(&path)
            .and_then(|mut f| f.write_all(buf.get_ref()))
            .expect("write fixture");
        path
    }

    #[test]
    fn own_profile_picture_is_square_jpeg() {
        let img = image::RgbImage::from_fn(80, 40, |_, _| image::Rgb([10, 20, 30]));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_with_encoder(image::codecs::png::PngEncoder::new(&mut buf))
            .expect("encode fixture");
        let jpeg = prepare_own_profile_picture(buf.get_ref()).expect("prepare");
        let decoded = image::load_from_memory(&jpeg).expect("decode jpeg");
        assert_eq!(
            (decoded.width(), decoded.height()),
            (
                crate::OWN_PROFILE_PICTURE_EDGE,
                crate::OWN_PROFILE_PICTURE_EDGE
            )
        );
        assert!(image::guess_format(&jpeg).ok() == Some(image::ImageFormat::Jpeg));
    }

    #[tokio::test]
    async fn scales_within_max_dim_preserving_ratio() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_png(dir.path(), 100, [10, 20, 30]);
        let service = ThumbnailService::new();

        let bytes = service.thumb(&path, 40).await.expect("thumb");

        let decoded = image::load_from_memory(&bytes).expect("decode result");
        assert_eq!((decoded.width(), decoded.height()), (40, 20));
    }

    #[tokio::test]
    async fn lru_byte_accounting_evicts_coldest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths: Vec<_> = (0..4)
            .map(|i| write_png(dir.path(), 60 + i as u32, [i as u8, 0, 255]))
            .collect();
        // Every scaled render lands at 50x25, so each slot costs 50*25*4 =
        // 5_000 accounted bytes; a 11_000 budget holds exactly two.
        let service = ThumbnailService::with_budget(11_000);

        for p in &paths {
            service.thumb(p, 50).await.expect("render");
        }
        let (len, used) = service.cached_stats();
        assert_eq!(len, 2, "budget caps resident slots");
        assert!(used <= 11_000);

        // The coldest entry re-renders on demand after eviction.
        let first_again = service.thumb(&paths[0], 50).await.expect("re-render");
        let dims = {
            let img = image::load_from_memory(&first_again).expect("decode");
            (img.width(), img.height())
        };
        assert_eq!(dims, (50, 25));
    }

    #[tokio::test]
    async fn zero_max_dim_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_png(dir.path(), 8, [0, 0, 0]);
        let err = ThumbnailService::new()
            .thumb(&path, 0)
            .await
            .expect_err("rejected");
        assert!(matches!(err, MediaError::InvalidInput(_)));
    }
}
