use crate::{jpeg_meta, png_meta};
use image::{GenericImageView, ImageEncoder};

/// Re-encodes the image at `path` with `op` applied, preserving metadata that
/// the `image` crate would drop:
///   - JPEG: raw APPn/COM segments (EXIF, XMP, ICC, comments, AI prompts) are
///     extracted before the edit and spliced back in after re-encoding.
///   - PNG: tEXt/zTXt/iTXt chunks (A1111/ComfyUI prompts & workflows) are
///     extracted and re-injected.
///   - other formats (AVIF/WebP): plain re-encode, as before.
fn edit_image(
    path: &str,
    op: impl Fn(image::DynamicImage) -> image::DynamicImage,
) -> Result<(), String> {
    let fmt = image::ImageFormat::from_path(path).map_err(|e| e.to_string())?;
    match fmt {
        image::ImageFormat::Jpeg => {
            let original = std::fs::read(path).map_err(|e| e.to_string())?;
            let segments = jpeg_meta::extract_segments(&original);
            let img = image::load_from_memory(&original).map_err(|e| e.to_string())?;
            let edited = op(img);

            // Re-encode at high quality into memory, then splice the original
            // APPn/COM segments back in (the encoder drops them). JPEG has no
            // alpha channel, so drop to RGB8 first.
            let rgb = edited.to_rgb8();
            let (w, h) = rgb.dimensions();
            let mut encoded: Vec<u8> = Vec::new();
            {
                let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 95);
                encoder
                    .write_image(rgb.as_raw(), w, h, image::ExtendedColorType::Rgb8)
                    .map_err(|e| e.to_string())?;
            }
            let merged = jpeg_meta::merge_missing_segments(&encoded, &segments);
            std::fs::write(path, merged).map_err(|e| e.to_string())
        }
        image::ImageFormat::Png => {
            let chunks = png_meta::extract_text_chunks(path)?;
            let img = image::open(path).map_err(|e| e.to_string())?;
            let edited = op(img);
            let rgba = edited.to_rgba8();
            let (w, h) = rgba.dimensions();
            png_meta::write_png_with_text(path, w, h, rgba.as_raw(), &chunks)
        }
        _ => {
            let img = image::open(path).map_err(|e| e.to_string())?;
            op(img).save(path).map_err(|e| e.to_string())
        }
    }
}

pub fn rotate_image(path: &str, degrees: i32) -> Result<(), String> {
    if !matches!(degrees, 90 | 180 | 270) {
        return Err("Invalid rotation degrees".to_string());
    }
    edit_image(path, |img| match degrees {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    })
}

pub fn crop_image(path: &str, x: u32, y: u32, width: u32, height: u32) -> Result<(), String> {
    edit_image(path, |img| {
        // Clamp to image bounds: crop_imm panics on out-of-bounds rectangles,
        // so never trust caller-provided coordinates.
        let (iw, ih) = img.dimensions();
        let x = x.min(iw.saturating_sub(1));
        let y = y.min(ih.saturating_sub(1));
        let w = width.min(iw - x).max(1);
        let h = height.min(ih - y).max(1);
        img.crop_imm(x, y, w, h)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a small test image to the given format and return its path.
    fn write_fixture(
        dir: &std::path::Path,
        name: &str,
        format: image::ImageFormat,
    ) -> std::path::PathBuf {
        let path = dir.join(name);
        let img = image::RgbaImage::from_pixel(32, 24, image::Rgba([200u8, 50, 30, 255]));
        img.save_with_format(&path, format).unwrap();
        path
    }

    #[test]
    fn avif_roundtrip_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), "test.avif", image::ImageFormat::Avif);
        assert!(path.exists());

        rotate_image(path.to_str().unwrap(), 90).unwrap();
        let img = image::open(&path).unwrap();
        // 32x24 rotated 90deg => 24x32. AVIF encoding may round the size,
        // so assert the orientation flip rather than the exact size.
        let (w, h) = img.dimensions();
        assert!(w < h, "expected portrait after 90deg rotate, got {w}x{h}");
    }

    #[test]
    fn avif_roundtrip_crop() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), "test.avif", image::ImageFormat::Avif);

        crop_image(path.to_str().unwrap(), 4, 4, 16, 12).unwrap();
        let img = image::open(&path).unwrap();
        let (w, h) = img.dimensions();
        assert!(w <= 32 && h <= 24, "crop escaped image bounds: {w}x{h}");
        assert!(w >= 16 && h >= 12, "crop too small: {w}x{h}");
    }

    #[test]
    fn webp_roundtrip_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), "test.webp", image::ImageFormat::WebP);

        rotate_image(path.to_str().unwrap(), 270).unwrap();
        let img = image::open(&path).unwrap();
        let (w, h) = img.dimensions();
        assert!(w < h, "expected portrait after 270deg rotate, got {w}x{h}");
    }

    #[test]
    fn png_roundtrip_smoke() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), "test.png", image::ImageFormat::Png);

        rotate_image(path.to_str().unwrap(), 180).unwrap();
        crop_image(path.to_str().unwrap(), 0, 0, 10, 10).unwrap();
        assert!(path.exists());
    }

    /// Builds a JPEG with a fake EXIF APP1 segment and a COM comment, then
    /// checks rotate keeps both.
    #[test]
    fn jpeg_rotate_keeps_exif_and_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.jpg");

        // Create the fixture: encode image, then splice in an APP1 + COM pair.
        {
            let img = image::RgbImage::from_pixel(32, 24, image::Rgb([10, 20, 30]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let exif = jpeg_meta::JpegSegment {
            marker: 0xE1,
            data: b"Exif\0fake-orientation".to_vec(),
        };
        let com = jpeg_meta::JpegSegment {
            marker: 0xFE,
            data: b"a1111 prompt here".to_vec(),
        };
        let with_meta = jpeg_meta::merge_missing_segments(&bytes, &[exif, com]);
        std::fs::write(&path, &with_meta).unwrap();

        rotate_image(path.to_str().unwrap(), 90).unwrap();

        let after = std::fs::read(&path).unwrap();
        let segs = jpeg_meta::extract_segments(&after);
        let exif_ok = segs
            .iter()
            .any(|s| s.marker == 0xE1 && s.data.starts_with(b"Exif\0"));
        let com_ok = segs
            .iter()
            .any(|s| s.marker == 0xFE && s.data.starts_with(b"a1111"));
        assert!(exif_ok, "EXIF segment lost after rotate");
        assert!(com_ok, "COM comment lost after rotate");
    }
}
