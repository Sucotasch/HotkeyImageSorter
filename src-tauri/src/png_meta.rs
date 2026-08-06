//! PNG text-chunk preservation. AI tools (AUTOMATIC1111/Forge, ComfyUI, InvokeAI)
//! store generation metadata — prompts, workflows, sd-metadata — in PNG tEXt,
//! zTXt or iTXt chunks, typically written *after* the IDAT data. The `image`
//! crate drops all text chunks when it re-encodes a PNG, so rotate/crop would
//! silently destroy AI prompts. This module extracts those chunks before an
//! edit and re-injects them into the re-encoded file.

use std::fs::File;
use std::io::BufReader;
use std::io::Write;

/// The original PNG chunk type of a text chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PngTextKind {
    /// tEXt — Latin-1, uncompressed.
    Text,
    /// zTXt — Latin-1, compressed.
    ZText,
    /// iTXt — UTF-8, optional compression.
    IText,
}

/// A normalized text chunk: keyword + value, regardless of the original type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PngTextChunk {
    pub kind: PngTextKind,
    pub keyword: String,
    pub text: String,
}

/// Reads every text chunk from a PNG file, wherever it appears in the stream
/// (before, inside or after the IDAT data). Pure metadata pass: the pixel data
/// is skipped, not decoded.
pub fn extract_text_chunks(path: &str) -> Result<Vec<PngTextChunk>, String> {
    let file = File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let decoder = png::Decoder::new(BufReader::new(file));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    // `finish` reads the remainder of the stream (including the trailing
    // text chunks written by A1111/ComfyUI) without decoding pixels.
    reader.finish().map_err(|e| e.to_string())?;
    let info = reader.info();

    let mut out = Vec::new();
    for c in &info.uncompressed_latin1_text {
        out.push(PngTextChunk {
            kind: PngTextKind::Text,
            keyword: c.keyword.clone(),
            text: c.text.clone(),
        });
    }
    for c in &info.compressed_latin1_text {
        out.push(PngTextChunk {
            kind: PngTextKind::ZText,
            keyword: c.keyword.clone(),
            text: c.get_text().unwrap_or_default(),
        });
    }
    for c in &info.utf8_text {
        out.push(PngTextChunk {
            kind: PngTextKind::IText,
            keyword: c.keyword.clone(),
            text: c.get_text().unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Writes a PNG with the given RGBA8 pixels and re-injects the text chunks.
/// Chunks are written after the IDAT data (before IEND) — the exact position
/// A1111/ComfyUI use, so any reader that understands their metadata will still
/// find it. Latin-1-capable chunks keep their original type (zTXt stays
/// compressed); anything non-Latin-1 is promoted to iTXt (UTF-8) so no prompt
/// is lost to a charset limitation.
pub fn write_png_with_text(
    path: &str,
    width: u32,
    height: u32,
    rgba8: &[u8],
    chunks: &[PngTextChunk],
) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("create {path}: {e}"))?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba8).map_err(|e| e.to_string())?;

    for c in chunks {
        write_text_chunk(&mut writer, c)?;
    }
    writer.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// Writes a single text chunk after the image data. Chooses tEXt/zTXt when the
/// value fits Latin-1 (keeps A1111 compatibility and keeps big workflows
/// compressed), otherwise falls back to iTXt so UTF-8 content survives.
fn write_text_chunk<W: Write>(writer: &mut png::Writer<W>, c: &PngTextChunk) -> Result<(), String> {
    let latin1 = c.text.chars().all(|ch| (ch as u32) <= 0xFF);
    match c.kind {
        PngTextKind::Text if latin1 => writer
            .write_text_chunk(&png::text_metadata::TEXtChunk::new(
                c.keyword.clone(),
                c.text.clone(),
            ))
            .map_err(|e| e.to_string()),
        PngTextKind::ZText if latin1 => writer
            .write_text_chunk(&png::text_metadata::ZTXtChunk::new(
                c.keyword.clone(),
                c.text.clone(),
            ))
            .map_err(|e| e.to_string()),
        _ => writer
            .write_text_chunk(&png::text_metadata::ITXtChunk::new(
                c.keyword.clone(),
                c.text.clone(),
            ))
            .map_err(|e| e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use png::{BitDepth, ColorType};

    /// Builds a tiny PNG with one tEXt chunk (`parameters`, as A1111 writes)
    /// placed AFTER the IDAT data, exactly like AI tools do.
    fn synth_png_with_trailing_text(keyword: &str, text: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let w = &mut std::io::Cursor::new(&mut bytes);
            let mut enc = png::Encoder::new(w, 4, 4);
            enc.set_color(ColorType::Rgba);
            enc.set_depth(BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&[0u8; 4 * 4 * 4]).unwrap();
            // Text chunk written after image data, before finish():
            writer
                .write_text_chunk(&png::text_metadata::TEXtChunk::new(
                    keyword.to_string(),
                    text.to_string(),
                ))
                .unwrap();
            writer.finish().unwrap();
        }
        bytes
    }

    #[test]
    fn extracts_trailing_text_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a1111.png");
        std::fs::write(
            &path,
            synth_png_with_trailing_text("parameters", "masterpiece, dog"),
        )
        .unwrap();

        let chunks = extract_text_chunks(path.to_str().unwrap()).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].keyword, "parameters");
        assert_eq!(chunks[0].text, "masterpiece, dog");
    }

    #[test]
    fn roundtrip_keeps_ascii_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.png");
        std::fs::write(
            &src,
            synth_png_with_trailing_text("workflow", "{\"nodes\":[1]}"),
        )
        .unwrap();

        let chunks = extract_text_chunks(src.to_str().unwrap()).unwrap();
        let out = dir.path().join("out.png");
        write_png_with_text(out.to_str().unwrap(), 4, 4, &[128u8; 4 * 4 * 4], &chunks).unwrap();

        let got = extract_text_chunks(out.to_str().unwrap()).unwrap();
        assert_eq!(got, chunks, "prompt must survive the round trip");
    }

    #[test]
    fn roundtrip_keeps_unicode_prompt() {
        // Russian/emoji prompt is not Latin-1: must be preserved via iTXt
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.png");
        let prompt = "красивый кот 🐱";
        // Unicode doesn't fit Latin-1 tEXt — write the source chunk as iTXt.
        let mut bytes = Vec::new();
        {
            let w = &mut std::io::Cursor::new(&mut bytes);
            let mut enc = png::Encoder::new(w, 4, 4);
            enc.set_color(ColorType::Rgba);
            enc.set_depth(BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&[0u8; 64]).unwrap();
            writer
                .write_text_chunk(&png::text_metadata::ITXtChunk::new(
                    "parameters".to_string(),
                    prompt.to_string(),
                ))
                .unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(&src, &bytes).unwrap();

        let chunks = extract_text_chunks(src.to_str().unwrap()).unwrap();
        assert_eq!(chunks[0].text, prompt);

        let out = dir.path().join("out.png");
        write_png_with_text(out.to_str().unwrap(), 4, 4, &[1u8; 64], &chunks).unwrap();

        let got = extract_text_chunks(out.to_str().unwrap()).unwrap();
        assert_eq!(got[0].text, prompt, "unicode prompt must survive");
    }

    #[test]
    fn no_chunks_on_plain_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.png");
        {
            let file = File::create(&path).unwrap();
            let mut enc = png::Encoder::new(file, 2, 2);
            enc.set_color(ColorType::Rgba);
            enc.set_depth(BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&[0u8; 16]).unwrap();
            writer.finish().unwrap();
        }
        assert!(extract_text_chunks(path.to_str().unwrap())
            .unwrap()
            .is_empty());
    }
}
