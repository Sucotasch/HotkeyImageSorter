use crate::{exif_writer, jpeg_meta, png_meta};
use image::{GenericImageView, ImageEncoder};

/// The result of converting one PNG to JPG.
#[derive(Debug)]
pub enum PngToJpgOutcome {
    /// The JPG was written successfully, with sizes before/after.
    Converted { in_size: u64, out_size: u64 },
    /// `prompt`/`workflow` together exceed the JPEG EXIF limit — no JPG is
    /// written at all (a JPG without the workflow is a data-loss trap).
    TooLarge,
}

/// Detects the image format of `path`. `.jif` files are the JPEG Interchange
/// Format (byte-identical to JPEG) but the `image` crate doesn't recognise the
/// extension, so map it explicitly to `Jpeg`.
fn detect_format(path: &str) -> Result<image::ImageFormat, String> {
    if std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jif"))
        .unwrap_or(false)
    {
        return Ok(image::ImageFormat::Jpeg);
    }
    image::ImageFormat::from_path(path).map_err(|e| e.to_string())
}

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
    let fmt = detect_format(path)?;
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

/// Attempts a truly lossless JPEG rotation via libjpeg-turbo (no re-encoding).
/// Returns `Ok(true)` if the lossless path was used, `Ok(false)` if the image
/// cannot be transformed losslessly (dimensions not MCU-aligned, so the output
/// would get ugly strips) and the caller should fall back to re-encoding, or
/// `Err` on unexpected I/O failures.
fn try_lossless_jpeg_rotate(path: &str, degrees: i32) -> Result<bool, String> {
    if !matches!(degrees, 90 | 180 | 270) {
        return Err("Invalid rotation degrees".to_string());
    }
    let original = std::fs::read(path).map_err(|e| e.to_string())?;
    let op = match degrees {
        90 => turbojpeg::TransformOp::Rot90,
        180 => turbojpeg::TransformOp::Rot180,
        270 => turbojpeg::TransformOp::Rot270,
        _ => unreachable!(),
    };
    let mut transform = turbojpeg::Transform::op(op);
    // Refuse imperfect transforms (partial MCU blocks on the edges): the
    // caller re-encodes instead, so the output never has strip artifacts.
    transform.perfect = true;

    let rotated = match turbojpeg::transform(&transform, &original) {
        Ok(buf) => buf,
        Err(_) => return Ok(false),
    };
    let bytes: &[u8] = rotated.as_ref();
    // turbojpeg already preserves EXIF/ICC/COM markers by default
    // (`copy_none: false`); merge_missing_segments is an idempotent safety net.
    let segments = jpeg_meta::extract_segments(&original);
    let merged = jpeg_meta::merge_missing_segments(bytes, &segments);
    std::fs::write(path, merged).map_err(|e| e.to_string())?;
    Ok(true)
}

pub fn rotate_image(path: &str, degrees: i32) -> Result<(), String> {
    if !matches!(degrees, 90 | 180 | 270) {
        return Err("Invalid rotation degrees".to_string());
    }
    // JPEGs get the lossless libjpeg-turbo path when possible; otherwise we
    // fall back to the metadata-preserving re-encode.
    if detect_format(path)
        .map(|f| f == image::ImageFormat::Jpeg)
        .unwrap_or(false)
        && try_lossless_jpeg_rotate(path, degrees)?
    {
        return Ok(());
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

/// Composites RGBA onto a white background (JPEG has no alpha channel).
fn flatten_on_white(rgba: &image::RgbaImage) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for p in rgba.pixels() {
        let [r, g, b, a] = p.0;
        if a == 255 {
            rgb.extend_from_slice(&[r, g, b]);
        } else {
            let af = f32::from(a) / 255.0;
            let inv = 1.0 - af;
            let mix = |c: u8| (f32::from(c) * af + 255.0 * inv).round() as u8;
            rgb.push(mix(r));
            rgb.push(mix(g));
            rgb.push(mix(b));
        }
    }
    rgb
}

/// Encodes RGB8 via libjpeg-turbo: 4:4:4 (no chroma subsampling — preserves
/// the fine colored detail AI images are full of), huffman-optimized.
fn compress_rgb(rgb: &[u8], w: usize, h: usize, quality: i32) -> Result<Vec<u8>, String> {
    let mut comp = turbojpeg::Compressor::new().map_err(|e| e.to_string())?;
    comp.set_quality(quality).map_err(|e| e.to_string())?;
    comp.set_subsamp(turbojpeg::Subsamp::None) // 4:4:4
        .map_err(|e| e.to_string())?;
    comp.set_optimize(true).map_err(|e| e.to_string())?;
    let img = turbojpeg::Image {
        pixels: rgb,
        width: w,
        height: h,
        pitch: w * 3,
        format: turbojpeg::PixelFormat::RGB,
    };
    comp.compress_to_vec(img).map_err(|e| e.to_string())
}

/// Converts a PNG to a JPG next to `dst`, embedding ComfyUI metadata the way
/// ComfyUI's SaveImage does: `prompt` → EXIF Make, `workflow` → EXIF
/// ImageDescription, A1111 `parameters` → COM segment. Alpha is flattened
/// onto white (JPEG has no alpha).
///
/// When the ComfyUI metadata cannot fit the JPEG EXIF limit, no JPG is
/// written at all (`PngToJpgOutcome::TooLarge`) — a JPG without the workflow
/// is a data-loss trap, so the caller reports and skips the file.
pub fn png_to_jpg(src: &str, dst: &str, quality: i32) -> Result<PngToJpgOutcome, String> {
    let chunks = png_meta::extract_text_chunks(src)?;
    let mut prompt = None;
    let mut workflow = None;
    let mut parameters = None;
    for c in &chunks {
        match c.keyword.as_str() {
            "prompt" => prompt = Some(c.text.clone()),
            "workflow" => workflow = Some(c.text.clone()),
            "parameters" => parameters = Some(c.text.clone()),
            _ => {}
        }
    }

    // --- pixels ---
    let img = image::open(src).map_err(|e| format!("decode {src}: {e}"))?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let rgba = img.to_rgba8();
    let rgb = flatten_on_white(&rgba);
    let jpeg = compress_rgb(&rgb, w, h, quality)?;

    // --- metadata: does it fit? ---
    let exif = match exif_writer::build_exif_app1(prompt.as_deref(), workflow.as_deref()) {
        Ok(Some(e)) => Some(e),
        Ok(None) => None,
        Err(_) => {
            return Ok(PngToJpgOutcome::TooLarge);
        }
    };

    let mut segments: Vec<jpeg_meta::JpegSegment> = Vec::new();
    if let Some(e) = exif {
        segments.push(jpeg_meta::JpegSegment {
            marker: 0xE1,
            data: e,
        });
    }
    // A1111 `parameters` -> COM segment (no 64 KB limit, multiple allowed).
    if let Some(p) = &parameters {
        if p.len() < 65000 {
            segments.push(jpeg_meta::JpegSegment {
                marker: 0xFE,
                data: p.as_bytes().to_vec(),
            });
        }
    }

    let out = jpeg_meta::merge_missing_segments(&jpeg, &segments);
    std::fs::write(dst, out).map_err(|e| format!("write {dst}: {e}"))?;

    Ok(PngToJpgOutcome::Converted {
        in_size: std::fs::metadata(src).map(|m| m.len()).unwrap_or(0),
        out_size: std::fs::metadata(dst).map(|m| m.len()).unwrap_or(0),
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
    /// Lossless rotation works when dimensions are MCU-aligned (multiples of
    /// 16 for 4:2:0 subsampling). Verify the output stays a valid JPEG, keeps
    /// markers and reports `Ok(true)` (i.e. the lossless path was taken).
    #[test]
    fn jpeg_lossless_rotate_on_mcu_aligned_image() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcu.jpg");
        {
            let img = image::RgbImage::from_pixel(64, 48, image::Rgb([200, 100, 50]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let exif = jpeg_meta::JpegSegment {
            marker: 0xE1,
            data: b"Exif\0fake-orientation".to_vec(),
        };
        let with_meta = jpeg_meta::merge_missing_segments(&bytes, std::slice::from_ref(&exif));
        std::fs::write(&path, &with_meta).unwrap();

        assert!(try_lossless_jpeg_rotate(path.to_str().unwrap(), 90).unwrap());

        let after = std::fs::read(&path).unwrap();
        let segs = jpeg_meta::extract_segments(&after);
        assert!(
            segs.iter()
                .any(|s| s.marker == 0xE1 && s.data.starts_with(b"Exif\0")),
            "EXIF lost in lossless rotate"
        );
        // 64x48 rotated 90deg => 48x64
        let img = image::load_from_memory(&after).unwrap();
        let (w, h) = img.dimensions();
        assert_eq!((w, h), (48, 64), "lossless rotate produced wrong size");
    }

    /// Non-MCU-aligned JPEGs must fall back to re-encode (Ok(false)) rather
    /// than producing a strip artifact.
    #[test]
    fn jpeg_lossless_rotate_falls_back_when_imperfect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("odd.jpg");
        {
            let img = image::RgbImage::from_pixel(17, 9, image::Rgb([10, 20, 30]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        // 17x9 is not divisible by the 16x16 MCU size → imperfect transform
        assert!(!try_lossless_jpeg_rotate(path.to_str().unwrap(), 90).unwrap());
    }

    /// Builds a tiny PNG with the given text chunks written AFTER the IDAT
    /// data (the position A1111/ComfyUI use).
    fn synth_png_with_text(chunks: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let w = &mut std::io::Cursor::new(&mut bytes);
            let mut enc = png::Encoder::new(w, 4, 4);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&[0u8; 4 * 4 * 4]).unwrap();
            for (kw, text) in chunks {
                writer
                    .write_text_chunk(&png::text_metadata::TEXtChunk::new(
                        kw.to_string(),
                        text.to_string(),
                    ))
                    .unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    /// Reads Make/ImageDescription from a JPEG the way metadata.rs does.
    fn exif_make_desc(path: &str) -> (Option<String>, Option<String>) {
        use std::io::BufReader;
        let file = std::fs::File::open(path).unwrap();
        let exif = exif::Reader::new()
            .read_from_container(&mut BufReader::new(file))
            .unwrap();
        let mut make = None;
        let mut desc = None;
        for f in exif.fields() {
            if f.tag == exif::Tag::Make {
                if let exif::Value::Ascii(parts) = &f.value {
                    make = Some(String::from_utf8_lossy(&parts[0]).into_owned());
                }
            }
            if f.tag == exif::Tag::ImageDescription {
                if let exif::Value::Ascii(parts) = &f.value {
                    desc = Some(String::from_utf8_lossy(&parts[0]).into_owned());
                }
            }
        }
        (make, desc)
    }

    #[test]
    fn png_to_jpg_preserves_prompt_and_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("comfy.png");
        let prompt =
            r#"{"3":{"inputs":{"text":"masterpiece, dog"},"class_type":"CLIPTextEncode"}}"#;
        let workflow = r#"{"id":"abc","nodes":[]}"#;
        std::fs::write(
            &src,
            synth_png_with_text(&[("prompt", prompt), ("workflow", workflow)]),
        )
        .unwrap();
        let dst = dir.path().join("out.jpg");

        let rep = png_to_jpg(src.to_str().unwrap(), dst.to_str().unwrap(), 90).unwrap();
        assert!(
            matches!(rep, PngToJpgOutcome::Converted { .. }),
            "small metadata must fit"
        );

        let (make, desc) = exif_make_desc(dst.to_str().unwrap());
        assert_eq!(make.as_deref(), Some(&format!("Prompt: {prompt}")[..]));
        assert_eq!(desc.as_deref(), Some(&format!("Workflow: {workflow}")[..]));

        // Output must be a valid, decodable JPEG of the right size
        let img = image::open(&dst).unwrap();
        assert_eq!(img.dimensions(), (4, 4));
    }

    #[test]
    fn png_to_jpg_skips_when_metadata_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("big.png");
        std::fs::write(
            &src,
            synth_png_with_text(&[
                ("prompt", &"p".repeat(50_000)),
                ("workflow", &"w".repeat(50_000)),
            ]),
        )
        .unwrap();
        let dst = dir.path().join("out.jpg");

        let rep = png_to_jpg(src.to_str().unwrap(), dst.to_str().unwrap(), 90).unwrap();
        assert!(matches!(rep, PngToJpgOutcome::TooLarge));
        assert!(!dst.exists(), "must NOT write a metadata-less JPG");
    }

    #[test]
    fn png_to_jpg_writes_parameters_to_com() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a1111.png");
        let params = "masterpiece, dog\nNegative prompt: blurry\nSteps: 20, Sampler: Euler a";
        std::fs::write(&src, synth_png_with_text(&[("parameters", params)])).unwrap();
        let dst = dir.path().join("out.jpg");

        png_to_jpg(src.to_str().unwrap(), dst.to_str().unwrap(), 90).unwrap();
        let bytes = std::fs::read(&dst).unwrap();
        assert!(
            bytes.windows(params.len()).any(|w| w == params.as_bytes()),
            "parameters text must be present in a COM segment"
        );
    }

    #[test]
    fn png_to_jpg_flattens_alpha_on_white() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("alpha.png");
        // Fully transparent pixels must become white in the JPG.
        std::fs::write(&src, synth_png_with_text(&[])).unwrap();
        let dst = dir.path().join("out.jpg");

        png_to_jpg(src.to_str().unwrap(), dst.to_str().unwrap(), 90).unwrap();
        let img = image::open(&dst).unwrap().to_rgb8();
        for p in img.pixels() {
            assert!(
                p.0[0] >= 250 && p.0[1] >= 250 && p.0[2] >= 250,
                "got {:?}",
                p.0
            );
        }
    }

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
