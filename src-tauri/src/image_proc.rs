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
