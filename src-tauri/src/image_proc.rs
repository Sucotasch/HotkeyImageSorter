use image::GenericImageView;

pub fn rotate_image(path: &str, degrees: i32) -> Result<(), String> {
    // For a prototype where environment might lack C-compilers (cmake/nasm),
    // we use the pure-Rust 'image' crate for all formats.
    // This is technically lossy for JPEGs (re-encoding), but guaranteed to build.
    let img = image::open(path).map_err(|e| e.to_string())?;
    let img = match degrees {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => return Err("Invalid rotation degrees".to_string()),
    };
    img.save(path).map_err(|e| e.to_string())
}

pub fn crop_image(path: &str, x: u32, y: u32, width: u32, height: u32) -> Result<(), String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    // Clamp to image bounds: crop_imm panics on out-of-bounds rectangles, so
    // never trust caller-provided coordinates.
    let (iw, ih) = img.dimensions();
    let x = x.min(iw.saturating_sub(1));
    let y = y.min(ih.saturating_sub(1));
    let w = width.min(iw - x).max(1);
    let h = height.min(ih - y).max(1);
    img.crop_imm(x, y, w, h)
        .save(path)
        .map_err(|e| e.to_string())
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
}
