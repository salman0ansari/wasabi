//! Convert a local image into a WhatsApp-shaped sticker: WebP, max 512×512,
//! alpha preserved when present, no caption.

use std::io::Cursor;
use std::path::Path;

use image::codecs::webp::WebPEncoder;
use image::{DynamicImage, ExtendedColorType};

use crate::MediaError;

/// Longer-edge bound for outgoing stickers.
pub const STICKER_MAX_EDGE: u32 = 512;

pub fn convert_image_to_sticker(path: &Path) -> Result<Vec<u8>, MediaError> {
    let source = image::ImageReader::open(path)
        .and_then(|reader| reader.with_guessed_format())
        .map_err(MediaError::Io)?
        .decode()
        .map_err(|error| MediaError::Decode(error.to_string()))?;
    if source.width() == 0 || source.height() == 0 {
        return Err(MediaError::Decode("image has empty dimensions".into()));
    }

    let resized = if source.width() > STICKER_MAX_EDGE || source.height() > STICKER_MAX_EDGE {
        source.thumbnail(STICKER_MAX_EDGE, STICKER_MAX_EDGE)
    } else {
        source
    };
    let width = resized.width();
    let height = resized.height();
    if width == 0 || height == 0 || width > STICKER_MAX_EDGE || height > STICKER_MAX_EDGE {
        return Err(MediaError::Decode("sticker resize failed".into()));
    }

    let mut out = Cursor::new(Vec::new());
    let encoder = WebPEncoder::new_lossless(&mut out);
    if resized.color().has_alpha() {
        encoder
            .encode(
                resized.to_rgba8().as_raw(),
                width,
                height,
                ExtendedColorType::Rgba8,
            )
            .map_err(|error| MediaError::Decode(error.to_string()))?;
    } else {
        encoder
            .encode(
                resized.to_rgb8().as_raw(),
                width,
                height,
                ExtendedColorType::Rgb8,
            )
            .map_err(|error| MediaError::Decode(error.to_string()))?;
    }
    let bytes = out.into_inner();
    if bytes.is_empty() {
        return Err(MediaError::Decode(
            "sticker encoder produced no bytes".into(),
        ));
    }
    Ok(bytes)
}

pub fn convert_image_to_sticker_file(source: &Path, destination: &Path) -> Result<u64, MediaError> {
    let bytes = convert_image_to_sticker(source)?;
    use std::io::Write as _;
    let result = (|| -> Result<u64, std::io::Error> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(bytes.len() as u64)
    })();
    match result {
        Ok(len) => Ok(len),
        Err(error) => {
            let _ = std::fs::remove_file(destination);
            Err(MediaError::Io(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use std::io::Write as _;

    fn write_png(dir: &Path, width: u32, height: u32, alpha: bool) -> std::path::PathBuf {
        let mut buf = Cursor::new(Vec::new());
        if alpha {
            let img = image::RgbaImage::from_fn(width, height, |x, y| {
                image::Rgba([x as u8, y as u8, 80, if x < width / 2 { 255 } else { 0 }])
            });
            image::DynamicImage::ImageRgba8(img)
                .write_with_encoder(PngEncoder::new(&mut buf))
                .expect("encode png");
        } else {
            let img =
                image::RgbImage::from_fn(width, height, |x, y| image::Rgb([x as u8, y as u8, 80]));
            image::DynamicImage::ImageRgb8(img)
                .write_with_encoder(PngEncoder::new(&mut buf))
                .expect("encode png");
        }
        let path = dir.join(format!("{width}x{height}.png"));
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(buf.get_ref()))
            .expect("write png");
        path
    }

    fn write_jpeg(dir: &Path, width: u32, height: u32) -> std::path::PathBuf {
        let img =
            image::RgbImage::from_fn(width, height, |x, y| image::Rgb([x as u8, 40, y as u8]));
        let mut buf = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_with_encoder(JpegEncoder::new_with_quality(&mut buf, 85))
            .expect("encode jpeg");
        let path = dir.join(format!("{width}x{height}.jpg"));
        std::fs::File::create(&path)
            .and_then(|mut file| file.write_all(buf.get_ref()))
            .expect("write jpeg");
        path
    }

    fn assert_sticker_bounds(bytes: &[u8]) {
        assert!(!bytes.is_empty(), "webp must be non-empty");
        let decoded = image::load_from_memory(bytes).expect("decode sticker webp");
        assert!(decoded.width() <= STICKER_MAX_EDGE);
        assert!(decoded.height() <= STICKER_MAX_EDGE);
        assert!(decoded.width() > 0 && decoded.height() > 0);
    }

    #[test]
    fn png_fixture_becomes_webp_within_512() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = write_png(dir.path(), 800, 600, false);
        let bytes = convert_image_to_sticker(&source).expect("convert png");
        assert_sticker_bounds(&bytes);
        let decoded = image::load_from_memory(&bytes).expect("decode");
        assert_eq!((decoded.width(), decoded.height()), (512, 384));
    }

    #[test]
    fn jpeg_fixture_becomes_webp_within_512() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = write_jpeg(dir.path(), 640, 480);
        let bytes = convert_image_to_sticker(&source).expect("convert jpeg");
        assert_sticker_bounds(&bytes);
    }

    #[test]
    fn small_png_is_not_upscaled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = write_png(dir.path(), 120, 80, false);
        let bytes = convert_image_to_sticker(&source).expect("convert small png");
        let decoded = image::load_from_memory(&bytes).expect("decode");
        assert_eq!((decoded.width(), decoded.height()), (120, 80));
    }

    #[test]
    fn alpha_png_keeps_alpha() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = write_png(dir.path(), 64, 64, true);
        let bytes = convert_image_to_sticker(&source).expect("convert alpha png");
        let decoded = image::load_from_memory(&bytes).expect("decode");
        assert!(decoded.color().has_alpha());
    }
}
