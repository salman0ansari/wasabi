//! CPU-bounded animated WebP decode. The `image` crate's still-image path is
//! first-frame-only; this walks `WebPDecoder` animation frames instead.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, DynamicImage, Rgba};

use crate::MediaError;

/// Reject oversized sticker/animation payloads before allocating frames.
const MAX_ANIMATION_BYTES: u64 = 4 * 1024 * 1024;
const MAX_ANIMATION_FRAMES: usize = 48;
const MAX_ANIMATION_EDGE: u32 = 512;
const MIN_FRAME_DELAY: Duration = Duration::from_millis(30);

/// One decoded animation frame as PNG so the desktop view can paint without
/// re-decoding WebP on the UI thread.
#[derive(Clone, Debug)]
pub struct AnimationFrame {
    pub png: Arc<Vec<u8>>,
    pub delay: Duration,
}

/// CPU-decoded animated WebP. Empty or single-frame results are not produced;
/// those stay on the still-image path.
#[derive(Clone, Debug)]
pub struct DecodedAnimation {
    pub width: u32,
    pub height: u32,
    pub frames: Arc<Vec<AnimationFrame>>,
}

/// Play an animated sticker only when the payload is marked animated, the
/// verified cache has the file, and the user has not asked to reduce motion.
pub fn play_animated_sticker(animated: bool, cache_hit: bool, reduce_motion: bool) -> bool {
    animated && cache_hit && !reduce_motion
}

pub fn decode_webp_animation(path: &Path) -> Result<DecodedAnimation, MediaError> {
    let bytes = std::fs::read(path)?;
    if bytes.is_empty() {
        return Err(MediaError::Decode("animation source is empty".into()));
    }
    if bytes.len() as u64 > MAX_ANIMATION_BYTES {
        return Err(MediaError::Decode(
            "animation exceeds the decode budget".into(),
        ));
    }

    let mut decoder = WebPDecoder::new(Cursor::new(bytes.as_slice()))
        .map_err(|error| MediaError::Decode(error.to_string()))?;
    if !decoder.has_animation() {
        return Err(MediaError::Decode("webp is not animated".into()));
    }
    let _ = decoder.set_background_color(Rgba([0, 0, 0, 0]));

    let mut width = 0;
    let mut height = 0;
    let mut frames = Vec::new();
    for frame in decoder.into_frames() {
        let frame = frame.map_err(|error| MediaError::Decode(error.to_string()))?;
        let delay = Duration::from(frame.delay()).max(MIN_FRAME_DELAY);
        let mut buffer = frame.into_buffer();
        if buffer.width() > MAX_ANIMATION_EDGE || buffer.height() > MAX_ANIMATION_EDGE {
            let scaled =
                DynamicImage::ImageRgba8(buffer).thumbnail(MAX_ANIMATION_EDGE, MAX_ANIMATION_EDGE);
            buffer = scaled.into_rgba8();
        }
        width = buffer.width();
        height = buffer.height();
        let mut png = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(buffer)
            .write_with_encoder(PngEncoder::new(&mut png))
            .map_err(|error| MediaError::Decode(error.to_string()))?;
        frames.push(AnimationFrame {
            png: Arc::new(png.into_inner()),
            delay,
        });
        if frames.len() > MAX_ANIMATION_FRAMES {
            return Err(MediaError::Decode(
                "animation exceeds the frame budget".into(),
            ));
        }
    }
    if frames.len() < 2 {
        return Err(MediaError::Decode("webp is not animated".into()));
    }
    Ok(DecodedAnimation {
        width,
        height,
        frames: Arc::new(frames),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sticker::{STICKER_MAX_EDGE, convert_image_to_sticker};
    use image::codecs::png::PngEncoder;
    use std::io::Write as _;

    fn write_png(dir: &Path, width: u32, height: u32) -> std::path::PathBuf {
        let img = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([x as u8, y as u8, 40, 255])
        });
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_with_encoder(PngEncoder::new(&mut buf))
            .expect("encode fixture");
        let path = dir.join("still.png");
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(buf.get_ref()))
            .expect("write fixture");
        path
    }

    #[test]
    fn reduce_motion_forces_a_still_frame() {
        assert!(play_animated_sticker(true, true, false));
        assert!(!play_animated_sticker(true, true, true));
        assert!(!play_animated_sticker(true, false, false));
        assert!(!play_animated_sticker(false, true, false));
    }

    #[test]
    fn still_webp_is_not_treated_as_animation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let png = write_png(dir.path(), 32, 32);
        let webp = convert_image_to_sticker(&png).expect("sticker webp");
        assert!(webp.len() > 8);
        let path = dir.path().join("still.webp");
        std::fs::write(&path, webp).expect("write webp");
        let err = decode_webp_animation(&path).expect_err("still webp");
        assert!(matches!(err, MediaError::Decode(_)));
        let _ = STICKER_MAX_EDGE;
    }
}
