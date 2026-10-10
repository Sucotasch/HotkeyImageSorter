//! Image metadata extraction for the Ctrl+Q viewer: EXIF, PNG text chunks
//! (AI prompts/workflows), JPEG comments, and heuristic detection/parsing of
//! AI-generation metadata (AUTOMATIC1111, ComfyUI, InvokeAI).

use crate::models::{AiPromptInfo, ImageMetadata, MetadataEntry};
use crate::{exif_writer, jpeg_meta, png_meta};
use std::io::BufReader;

/// Collects everything the frontend needs to render the metadata modal.
pub fn get_metadata(path: &str) -> Result<ImageMetadata, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("stat {path}: {e}"))?;
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());

    let format = image::ImageFormat::from_path(path)
        .map(|f| format_extension(&f))
        .unwrap_or_else(|_| {
            // `.jif` is the JPEG Interchange Format — byte-identical to JPEG —
            // but the `image` crate doesn't recognise the extension.
            let ext = extension(path);
            if ext.eq_ignore_ascii_case("jif") {
                "jpeg".to_string()
            } else {
                ext.to_string()
            }
        });

    let exif = read_exif(path)?;
    let text_chunks = png_meta::extract_text_chunks(path).unwrap_or_default();
    // PNG/WebP/AVIF have no JPEG COM segments — running the JPEG walker over
    // their bytes finds coincidental 0xFF 0xFE sequences and surfaces garbage
    // as "JPEG comments", so only JPEG-family files are scanned.
    let mut comments = if matches!(format.as_str(), "jpg" | "jpeg") {
        read_jpeg_comments(path)
    } else {
        Vec::new()
    };

    // Our PNG→JPG conversion stores an oversized workflow split across COM
    // segments (`hkis-workflow-<i>-of-<n>:`). Reassemble it and present it as
    // a synthetic EXIF `Make` so `detect_ai_prompt` picks it up exactly like
    // the native ComfyUI `workflow:` payload (and it is hidden from the EXIF
    // table by the same ComfyUI filter below). The raw parts are dropped from
    // the comments list so the UI does not show marker garbage.
    let mut exif = exif;
    if let Some(wf) = reassemble_workflow_parts(&comments) {
        exif.push(MetadataEntry {
            key: "Make".to_string(),
            value: format!("workflow:{wf}"),
        });
        comments.retain(|c| !is_workflow_part(c));
    }

    let ai = detect_ai_prompt(&exif, &text_chunks, &comments);

    // ComfyUI stores its `prompt:`/`workflow:` JSON blobs in EXIF fields
    // (commonly Make/Model, sometimes ImageDescription/UserComment) — some
    // generators write a bare graph without a prefix. Those are surfaced in
    // the AI section, so keep the multi-KB strings out of the EXIF table.
    let exif_display: Vec<MetadataEntry> = exif
        .iter()
        .filter(|e| {
            !(matches!(
                e.key.as_str(),
                "Make" | "Model" | "ImageDescription" | "UserComment"
            ) && (is_comfyui_exif_value(&e.value) || looks_like_comfyui_graph(&e.value)))
        })
        .cloned()
        .collect();

    Ok(ImageMetadata {
        name,
        path: path.to_string(),
        format,
        size: meta.len(),
        modified: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
        exif: exif_display,
        text_chunks: text_chunks
            .iter()
            .map(|c| MetadataEntry {
                key: c.keyword.clone(),
                value: c.text.clone(),
            })
            .collect(),
        comments,
        ai,
    })
}

/// Renders the metadata as a readable text dump for the `.metadata.txt`
/// export. Mirrors the modal layout: file summary, AI prompt, chunks,
/// comments, EXIF.
pub fn to_text(meta: &ImageMetadata) -> String {
    let mut out = String::new();
    out.push_str(&format!("File: {}\n", meta.name));
    out.push_str(&format!("Path: {}\n", meta.path));
    out.push_str(&format!("Format: {}\n", meta.format));
    out.push_str(&format!("Size: {} bytes\n\n", meta.size));

    if let Some(ai) = &meta.ai {
        out.push_str(&format!("AI generation ({})\n", ai.source));
        if !ai.prompt.is_empty() {
            out.push_str(&format!("\nPrompt:\n{}\n", ai.prompt));
        }
        if let Some(neg) = &ai.negative_prompt {
            if !neg.is_empty() {
                out.push_str(&format!("\nNegative prompt:\n{neg}\n"));
            }
        }
        if !ai.params.is_empty() {
            out.push_str(&format!("\nParameters:\n{}\n", ai.params));
        }
        out.push_str(&format!("\nRaw:\n{}\n\n", ai.raw));
    } else {
        out.push_str("No AI generation prompt detected.\n\n");
    }

    if !meta.text_chunks.is_empty() {
        out.push_str("PNG text chunks:\n");
        for c in &meta.text_chunks {
            out.push_str(&format!("  {}: {}\n", c.key, c.value));
        }
        out.push('\n');
    }
    if !meta.comments.is_empty() {
        out.push_str("JPEG comments:\n");
        for c in &meta.comments {
            out.push_str(&format!("  {c}\n"));
        }
        out.push('\n');
    }
    if !meta.exif.is_empty() {
        out.push_str("EXIF:\n");
        for c in &meta.exif {
            out.push_str(&format!("  {}: {}\n", c.key, c.value));
        }
    }
    out
}

/// Human-readable file extension for the given image format.
fn format_extension(f: &image::ImageFormat) -> String {
    let exts = f.extensions_str();
    exts.first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "unknown".into())
}

fn extension(path: &str) -> &str {
    std::path::Path::new(path)
        .extension()
        .map(|e| e.to_str().unwrap_or(""))
        .unwrap_or("")
}

/// Reads EXIF via kamadak-exif. Returns an empty vector when the file has no
/// EXIF at all (that's not an error for our purposes).
fn read_exif(path: &str) -> Result<Vec<MetadataEntry>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = BufReader::new(file);
    let exif = match exif::Reader::new().read_from_container(&mut buf) {
        Ok(e) => e,
        // Some writers overlap the "next IFD" pointer with value data, which
        // the strict kamadak-exif rejects — fall back to the tolerant reader.
        Err(_) => {
            let Ok(bytes) = std::fs::read(path) else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for seg in jpeg_meta::extract_segments(&bytes) {
                if seg.marker == 0xE1 {
                    out.extend(extract_exif_text_fields(&seg.data));
                }
            }
            out.sort_by(|a, b| a.key.cmp(&b.key));
            return Ok(out);
        }
    };

    let mut out = Vec::new();
    for f in exif.fields() {
        let value = field_value_to_string(f);
        if value.is_empty() {
            continue;
        }
        out.push(MetadataEntry {
            key: f.tag.to_string(),
            value,
        });
    }
    // Also expose the EXIF IFD name so the UI can group them.
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(out)
}

/// Renders an EXIF field value as plain text. kamadak-exif's `display_value()`
/// wraps ASCII/UNDEFINED strings in quotes and escapes, which would garble
/// e.g. ComfyUI's `workflow:{...}` JSON stored in EXIF Make/Model, so those
/// fields are decoded directly from the raw `Value`. Every other tag keeps
/// its tag-specific `display_value` formatting (ExifVersion → "2.31", etc.).
fn field_value_to_string(f: &exif::Field) -> String {
    match f.tag {
        exif::Tag::Make
        | exif::Tag::Model
        | exif::Tag::UserComment
        | exif::Tag::ImageDescription => match &f.value {
            exif::Value::Ascii(parts) => parts
                .iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect::<Vec<_>>()
                .join("\n"),
            exif::Value::Undefined(bytes, _) => decode_undefined_text(bytes),
            _ => f.display_value().to_string(),
        },
        _ => f.display_value().to_string(),
    }
}

/// Best-effort text decoding of an EXIF UNDEFINED payload. UserComment is type
/// UNDEFINED and starts with an 8-byte charset header ("UNICODE\0",
/// "ASCII\0\0\0", …) followed by the actual text; the header is stripped.
fn decode_undefined_text(bytes: &[u8]) -> String {
    let (charset, body) = if bytes.len() >= 8 {
        (Some(&bytes[..8]), &bytes[8..])
    } else {
        (None, bytes)
    };
    match charset {
        Some(b"UNICODE\0") => decode_utf16(body),
        Some(b"ASCII\0\0\0") | Some(b"JIS\0\0\0\0") => String::from_utf8_lossy(body).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Decodes a UTF-16 payload (EXIF "UNICODE\0" UserComment). The byte order is
/// not reliably declared, so it is detected: ASCII/Latin text has a NUL byte in
/// every pair — at even offsets when big-endian, at odd offsets when
/// little-endian. When that is ambiguous (e.g. Cyrillic-only text), the EXIF
/// spec's big-endian interpretation is preferred and the reading with fewer
/// control characters wins.
fn decode_utf16(body: &[u8]) -> String {
    let n = body.len() / 2;
    if n == 0 {
        return String::new();
    }
    let be_units: Vec<u16> = body[..n * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect();
    let be_str = String::from_utf16(&be_units).unwrap_or_default();
    let le_units: Vec<u16> = body[..n * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let le_str = String::from_utf16(&le_units).unwrap_or_default();
    // Some writers emit a BOM (U+FEFF) at the start.
    let strip_bom = |s: String| s.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(s);
    let be_str = strip_bom(be_str);
    let le_str = strip_bom(le_str);

    let even_nuls = body.iter().step_by(2).filter(|&&b| b == 0).count();
    let odd_nuls = body.iter().skip(1).step_by(2).filter(|&&b| b == 0).count();
    if even_nuls > odd_nuls * 2 {
        return be_str;
    }
    if odd_nuls > even_nuls * 2 {
        return le_str;
    }
    // Ambiguous (non-ASCII text): prefer a meaningful candidate over an empty
    // one, then big-endian per the EXIF spec unless it is riddled with
    // control characters.
    match (be_str.is_empty(), le_str.is_empty()) {
        (true, false) => return le_str,
        (false, true) => return be_str,
        _ => {}
    }
    let controls = |s: &str| {
        s.chars()
            .filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
            .count()
    };
    if controls(&be_str) <= controls(&le_str) {
        be_str
    } else {
        le_str
    }
}

/// Returns the payload of every COM (comment) segment of a JPEG. Non-JPEG
/// files (or JPEGs without comments) yield an empty vector.
fn read_jpeg_comments(path: &str) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    jpeg_meta::extract_segments(&bytes)
        .iter()
        .filter(|s| s.marker == 0xFE)
        .map(|s| decode_legacy_text(&s.data))
        .filter(|s| !s.is_empty())
        .collect()
}

/// True when a JPEG comment is one of our oversized-workflow parts
/// (`hkis-workflow-<i>-of-<n>:` marker written by `png_to_jpg`).
fn is_workflow_part(comment: &str) -> bool {
    comment.starts_with("hkis-workflow-")
}

/// Reassembles a workflow split across COM segments by `png_to_jpg`.
/// Returns the complete JSON when every part is present, valid and the
/// indices line up; `None` on any inconsistency (the comments are then left
/// untouched and surface as plain comments).
fn reassemble_workflow_parts(comments: &[String]) -> Option<String> {
    let mut parts: Vec<(usize, usize, &str)> = comments
        .iter()
        .filter_map(|c| {
            let rest = c.strip_prefix("hkis-workflow-")?;
            let (idx, rest) = rest.split_once("-of-")?;
            let (total, body) = rest.split_once(':')?;
            Some((
                idx.parse::<usize>().ok()?,
                total.parse::<usize>().ok()?,
                body,
            ))
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    let total = parts[0].1;
    if total == 0 || parts.len() != total || parts.iter().any(|p| p.1 != total) {
        return None;
    }
    parts.sort_by_key(|p| p.0);
    let mut json = String::new();
    for (i, _, body) in parts {
        if i == 0 || i > total {
            return None;
        }
        json.push_str(body);
    }
    // Only a complete, parseable JSON document is useful to the AI detector.
    serde_json::from_str::<serde_json::Value>(&json).ok()?;
    Some(json)
}

/// Best-effort decoding of legacy JPEG comment bytes: strict UTF-8 first;
/// when that fails, Windows-1251 when the payload is Cyrillic-heavy,
/// otherwise ISO-8859-1 (each byte maps 1:1).
fn decode_legacy_text(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let high = bytes.iter().filter(|&&b| b >= 0x80).count();
    let cyr = bytes
        .iter()
        .filter(|&&b| (0xC0..=0xFF).contains(&b))
        .count();
    if high > 0 && cyr * 3 >= high * 2 {
        decode_cp1251(bytes)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    }
}

/// Windows-1251 (Cyrillic) decoder — the common encoding of legacy Russian
/// JPEG comments.
fn decode_cp1251(bytes: &[u8]) -> String {
    // Code points for 0x80..=0xFF (hex literals keep the table corruption-
    // proof and reviewable). 0x9E is undefined in CP1251 — mapped to NBSP.
    const CP1251: [u16; 128] = [
        0x0402, 0x0403, 0x201A, 0x0453, 0x201E, 0x2026, 0x2020, 0x2021, 0x20AC, 0x2030, 0x0409,
        0x2039, 0x040A, 0x040C, 0x040B, 0x040F, // 0x80–0x8F
        0x0452, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x2122, 0x0459, 0x045A,
        0x045C, 0x045B, 0x045F, 0x00A0, 0x040E, // 0x90–0x9F
        0x00A0, 0x045E, 0x0408, 0x00A4, 0x0490, 0x00A6, 0x00A7, 0x0401, 0x00A9, 0x0404, 0x00AB,
        0x00AC, 0x00AD, 0x00AE, 0x0407, 0x00B0, // 0xA0–0xAF
        0x00B1, 0x0406, 0x0456, 0x0491, 0x00B5, 0x00B6, 0x00B7, 0x0451, 0x2116, 0x0454, 0x00BB,
        0x0458, 0x0405, 0x0455, 0x0457, 0x0410, // 0xB0–0xBF
        0x0410, 0x0411, 0x0412, 0x0413, 0x0414, 0x0415, 0x0416, 0x0417, 0x0418, 0x0419, 0x041A,
        0x041B, 0x041C, 0x041D, 0x041E, 0x041F, 0x0420, 0x0421, 0x0422, 0x0423, 0x0424, 0x0425,
        0x0426, 0x0427, 0x0428, 0x0429, 0x042A, 0x042B, 0x042C, 0x042D, 0x042E,
        0x042F, // А–Я
        0x0430, 0x0431, 0x0432, 0x0433, 0x0434, 0x0435, 0x0436, 0x0437, 0x0438, 0x0439, 0x043A,
        0x043B, 0x043C, 0x043D, 0x043E, 0x043F, 0x0440, 0x0441, 0x0442, 0x0443, 0x0444, 0x0445,
        0x0446, 0x0447, 0x0448, 0x0449, 0x044A, 0x044B, 0x044C, 0x044D, 0x044E,
        0x044F, // а–я
    ];
    bytes
        .iter()
        .map(|&b| {
            if b < 0x80 {
                b as char
            } else {
                char::from_u32(CP1251[(b - 0x80) as usize] as u32).unwrap_or('?')
            }
        })
        .collect()
}

/// Checks a batch of image paths for the *presence* of an AI-generation
/// prompt, returning only the paths that have one. Lightweight by design: it
/// reads only the metadata (PNG text chunks / JPEG segments), never decodes
/// pixels, and skips files whose extension cannot carry AI metadata. The
/// frontend calls this for the visible list window and caches the results.
pub fn check_prompts(paths: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for path in paths {
        if has_prompt_fast(path) {
            out.push(path.clone());
        }
    }
    out
}

/// Extension-only gate before touching the file: only PNG and JPEG can carry
/// the AI metadata formats we understand (tEXt/zTXt/iTXt chunks, EXIF
/// UserComment/ImageDescription, COM segments).
fn can_carry_prompt(path: &str) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "jif")
}

/// Cheap presence check: returns true when the file has AI metadata. Doesn't
/// fully decode anything, so it stays fast on huge batches.
fn has_prompt_fast(path: &str) -> bool {
    if !can_carry_prompt(path) {
        return false;
    }

    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "png" {
        // PNG: walk the chunk stream with seeks (text chunks usually sit
        // AFTER the IDAT pixel data, so a prefix read would miss them — but
        // the pixels never need to be read).
        find_png_ai_chunk_file(path)
    } else {
        // JPEG: every metadata segment precedes SOS near the start of the
        // file — a bounded prefix finds them without pulling a 50 MB file
        // into memory for a badge check.
        match read_prefix(path, 2 * 1024 * 1024) {
            Some(bytes) => find_jpeg_prompt_marker(&bytes),
            None => false,
        }
    }
}

/// Reads at most `max_bytes` from the start of the file.
fn read_prefix(path: &str, max_bytes: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    (&mut f).take(max_bytes).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// True when a tEXt/zTXt/iTXt chunk's payload carries AI metadata (known
/// keyword, or an XMP payload with generation markers). Shared by the
/// in-memory walker and the streaming file walker so the two can't drift.
fn png_chunk_is_ai(data: &[u8]) -> bool {
    let Some(nul) = data.iter().position(|&b| b == 0) else {
        return false;
    };
    let keyword = &data[..nul];
    if is_ai_keyword(keyword) {
        return true;
    }
    // XMP chunks (Draw Things / Krea) carry the prompt inside
    // <dc:description> or <exif:UserComment> — only those are generation
    // metadata, plain tool XMP is not.
    keyword.eq_ignore_ascii_case(b"XML:com.adobe.xmp") && xmp_payload_looks_ai(data)
}

/// Streaming variant of the PNG chunk scan used by `has_prompt_fast`: walks
/// the file's chunk headers and seeks past the IDAT pixel data instead of
/// reading it — badge scans on multi-MB PNGs no longer pull whole files
/// into memory. Text chunks are read in full (their payload is the metadata
/// we check); absurdly large chunk lengths from corrupt files are treated
/// as data to skip, never as an allocation size.
fn find_png_ai_chunk_file(path: &str) -> bool {
    use std::io::{BufReader, Read, Seek, SeekFrom};
    const TEXT_CHUNK_MAX: u64 = 32 * 1024 * 1024;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut r = BufReader::new(file);
    let mut header = [0u8; 8];
    if r.read_exact(&mut header).is_err() {
        return false;
    }
    loop {
        let mut chdr = [0u8; 8];
        if r.read_exact(&mut chdr).is_err() {
            return false;
        }
        let len = u32::from_be_bytes([chdr[0], chdr[1], chdr[2], chdr[3]]) as u64;
        let is_text = matches!(&chdr[4..8], b"tEXt" | b"zTXt" | b"iTXt");
        if is_text && len <= TEXT_CHUNK_MAX {
            let mut data = vec![0u8; len as usize];
            if r.read_exact(&mut data).is_err() {
                return false;
            }
            if png_chunk_is_ai(&data) {
                return true;
            }
            // skip the CRC after the consumed payload
            if r.seek(SeekFrom::Current(4)).is_err() {
                return false;
            }
        } else {
            if &chdr[4..8] == b"IEND" {
                return false;
            }
            // pixel data (or an implausibly large "text" chunk): skip data + CRC
            if r.seek(SeekFrom::Current(len as i64 + 4)).is_err() {
                return false;
            }
        }
    }
}

fn is_ai_keyword(keyword: &[u8]) -> bool {
    matches!(
        keyword,
        b"parameters"
            | b"prompt"
            | b"workflow"
            | b"sd-metadata"
            | b"sd_metadata"
            | b"metadata"
            | b"positive_prompt"
            | b"negative_prompt"
            | b"generation_data"
            | b"invokeai_metadata"
            | b"invokeai_graph"
    )
}

/// True when an XMP payload contains generation metadata markers:
/// `<dc:description>` (Draw Things writes the A1111-style prompt there) or
/// `<exif:UserComment>` (structured JSON). Plain tool XMP (e.g. Photoshop
/// history) has neither.
fn xmp_payload_looks_ai(payload: &[u8]) -> bool {
    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }
    contains(payload, b"dc:description") || contains(payload, b"exif:UserComment")
}

/// True when an XMP text value (decoded) contains `<dc:description>` or
/// `<exif:UserComment>`.
fn xmp_text_looks_ai(value: &str) -> bool {
    value.contains("dc:description") || value.contains("exif:UserComment")
}

/// Scans JPEG segments for AI prompt markers. Looks inside COM segments for
/// A1111 parameter text, and inside APP1 EXIF for UserComment/ImageDescription
/// containing such markers.
fn find_jpeg_prompt_marker(bytes: &[u8]) -> bool {
    let segments = jpeg_meta::extract_segments(bytes);
    let mut com_hit = false;
    for seg in &segments {
        if seg.marker == 0xFE {
            // COM segment
            let text = String::from_utf8_lossy(&seg.data);
            if looks_like_a1111(&text) {
                com_hit = true;
                break;
            }
        }
    }
    if com_hit {
        return true;
    }
    // EXIF: some tools store the prompt in UserComment or ImageDescription
    // rather than a COM segment; ComfyUI puts `workflow:`/`prompt:` JSON in
    // Make/Model/ImageDescription/UserComment. Parse only the APP1 EXIF blob
    // via the tolerant reader.
    for seg in &segments {
        if seg.marker == 0xE1 && seg.data.starts_with(b"Exif\0\0") {
            if let Some(exif) = parse_exif_bytes(&seg.data) {
                for field in exif {
                    let k = field.key.to_ascii_lowercase();
                    if matches!(
                        k.as_str(),
                        "make" | "model" | "imagedescription" | "usercomment"
                    ) && (is_comfyui_exif_value(&field.value)
                        || looks_like_comfyui_graph(&field.value)
                        || looks_like_a1111(&field.value))
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Parses a raw APP1 EXIF payload (including the "Exif\0\0" header) into
/// key/value pairs, or returns None when it cannot be parsed. The 6-byte
/// header is stripped so the remainder is a bare TIFF structure that
/// `read_from_container` can recognise.
fn parse_exif_bytes(data: &[u8]) -> Option<Vec<MetadataEntry>> {
    let out = extract_exif_text_fields(data);
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Tolerant reader for the EXIF text fields we care about (Make, Model,
/// ImageDescription, UserComment, Software). Some real-world writers place the
/// first value directly after the IFD entries — overlapping the "next IFD"
/// pointer — which makes strict parsers like kamadak-exif fail with
/// "Unexpected next IFD". This walker never errors: every read is
/// bounds-checked and only plausible pointers are followed.
fn extract_exif_text_fields(payload: &[u8]) -> Vec<MetadataEntry> {
    let mut out = Vec::new();
    let Some(tiff) = payload.strip_prefix(b"Exif\0\0") else {
        return out;
    };
    if tiff.len() < 8 {
        return out;
    }
    let le = match &tiff[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return out,
    };
    let ifd0 = read_exif_u32(tiff, 4, le).unwrap_or(0) as usize;
    walk_exif_ifd(tiff, ifd0, true, 0, le, &mut out);
    out
}

fn read_exif_u16(data: &[u8], off: usize, le: bool) -> Option<u16> {
    let b = data.get(off..off + 2)?;
    Some(if le {
        u16::from_le_bytes([b[0], b[1]])
    } else {
        u16::from_be_bytes([b[0], b[1]])
    })
}

fn read_exif_u32(data: &[u8], off: usize, le: bool) -> Option<u32> {
    let b = data.get(off..off + 4)?;
    Some(if le {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    })
}

fn walk_exif_ifd(
    tiff: &[u8],
    offset: usize,
    follow_next: bool,
    depth: usize,
    le: bool,
    out: &mut Vec<MetadataEntry>,
) {
    if depth >= 4 {
        return;
    }
    let mut off = offset;
    let mut guard = 0;
    while off > 0 && off + 2 <= tiff.len() && guard < 8 {
        guard += 1;
        let Some(count) = read_exif_u16(tiff, off, le) else {
            return;
        };
        let count = count as usize;
        if off + 2 + count * 12 + 4 > tiff.len() {
            return;
        }
        for k in 0..count {
            let e = off + 2 + k * 12;
            let Some(tag) = read_exif_u16(tiff, e, le) else {
                return;
            };
            let Some(typ) = read_exif_u16(tiff, e + 2, le) else {
                return;
            };
            let Some(cnt) = read_exif_u32(tiff, e + 4, le) else {
                return;
            };
            let Some(val) = read_exif_u32(tiff, e + 8, le) else {
                return;
            };
            // Follow the ExifOffset pointer (IFD0 -> Exif sub-IFD).
            if tag == 0x8769 && typ == 4 && cnt == 1 {
                walk_exif_ifd(tiff, val as usize, false, depth + 1, le, out);
                continue;
            }
            let key = match tag {
                0x010E => "ImageDescription",
                0x010F => "Make",
                0x0110 => "Model",
                0x0131 => "Software",
                0x9286 => "UserComment",
                _ => continue,
            };
            let size: usize = match typ {
                1 | 2 | 7 => 1,
                3 => 2,
                4 | 9 => 4,
                5 | 10 => 8,
                _ => continue,
            };
            let byte_len = size.saturating_mul(cnt as usize);
            let raw: &[u8] = if byte_len <= 4 {
                &tiff[e + 8..(e + 8 + byte_len).min(tiff.len())]
            } else {
                let start = val as usize;
                let end = start.saturating_add(byte_len).min(tiff.len());
                if start >= tiff.len() {
                    continue;
                }
                &tiff[start..end]
            };
            let value = match typ {
                2 => {
                    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
                    String::from_utf8_lossy(&raw[..end]).into_owned()
                }
                7 => decode_undefined_text(raw),
                _ => continue,
            };
            if !value.is_empty() {
                out.push(MetadataEntry {
                    key: key.to_string(),
                    value,
                });
            }
        }
        if !follow_next {
            return;
        }
        let Some(next) = read_exif_u32(tiff, off + 2 + count * 12, le) else {
            return;
        };
        let next = next as usize;
        // A non-zero "next IFD" is often the start of value data overlapping
        // this field — stop unless it points at a plausible IFD.
        if next == 0 || next + 2 > tiff.len() {
            return;
        }
        off = next;
    }
}

/// Parses an XMP metadata blob (PNG `XML:com.adobe.xmp` iTXt chunk) for AI
/// generation info. Draw Things / Krea write two relevant fields:
/// - `<dc:description>` — an A1111-style text (prompt + `Steps: …` line),
///   which `parse_a1111_parameters` can handle directly;
/// - `<exif:UserComment>` — JSON with `c` (prompt), `uc` (negative prompt),
///   `steps`, `sampler`, `seed`, `model`, … (Draw Things' own format).
///
/// The XMP namespace/creator (`xmp:CreatorTool`) is used as the source.
fn parse_xmp_prompt(xmp: &str) -> Option<AiPromptInfo> {
    let source = extract_xmp_tag(xmp, "xmp:CreatorTool").unwrap_or_else(|| "XMP".to_string());

    // 1) dc:description — plain A1111-style payload
    if let Some(desc) = extract_xmp_field(xmp, "dc:description") {
        let desc = desc.trim();
        if !desc.is_empty() {
            if looks_like_a1111(desc) {
                if let Some(mut info) = parse_a1111_parameters(desc) {
                    info.source = source;
                    return Some(info);
                }
            }
            return Some(AiPromptInfo {
                source: source.clone(),
                prompt: desc.to_string(),
                negative_prompt: None,
                params: String::new(),
                raw: desc.to_string(),
            });
        }
    }

    // 2) exif:UserComment — structured JSON
    let uc = extract_xmp_field(xmp, "exif:UserComment")?;
    let v: serde_json::Value = serde_json::from_str(&uc).ok()?;
    let obj = v.as_object()?;
    let prompt = obj.get("c").and_then(|c| c.as_str()).unwrap_or("");
    if prompt.trim().is_empty() {
        return None;
    }
    let negative = obj.get("uc").and_then(|u| u.as_str()).map(str::to_string);
    // Remaining keys (steps, sampler, seed, model, …) become the parameters.
    let params: Vec<String> = obj
        .iter()
        .filter(|(k, _)| *k != "c" && *k != "uc")
        .map(|(k, v)| format!("{k}: {}", json_value_to_string(v)))
        .collect();
    Some(AiPromptInfo {
        source,
        prompt: prompt.to_string(),
        negative_prompt: negative,
        params: params.join("\n"),
        raw: uc,
    })
}

/// Renders a JSON value compactly for the parameters block.
fn json_value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Extracts the plain text of the first `<rdf:li>` inside the named XMP
/// field (e.g. `<dc:description>` or `<exif:UserComment>`). XML entities
/// (`&#xA;`, `&amp;`, …) are unescaped.
fn extract_xmp_field(xmp: &str, field: &str) -> Option<String> {
    let open = format!("<{field}>");
    let start = xmp.find(&open)? + open.len();
    let end = xmp[start..].find(&format!("</{field}>"))? + start;
    let block = &xmp[start..end];
    let li = block.find("<rdf:li")?;
    let gt = block[li..].find('>')? + li + 1;
    let close = block[gt..].find("</rdf:li>")? + gt;
    Some(xml_unescape(&block[gt..close]))
}

/// Extracts the content of a simple XML tag like
/// `<xmp:CreatorTool>Draw Things</xmp:CreatorTool>`.
fn extract_xmp_tag(xmp: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = xmp.find(&open)? + open.len();
    let end = xmp[start..].find(&format!("</{tag}>"))? + start;
    Some(xml_unescape(&xmp[start..end]))
}

/// Decodes the XML entities Draw Things uses (`&#xA;` newlines, `&amp;`,
/// `&quot;`, `&lt;`, `&gt;`, `&apos;`).
fn xml_unescape(s: &str) -> String {
    s.replace("&#xA;", "\n")
        .replace("&#xD;", "\r")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// Krea's `generation_data` PNG chunk: JSON with `models` (lora info) and the
/// full `prompt` text (+ size/steps/seed/…). Some writers append a trailing
/// NUL byte.
fn parse_generation_data(raw: &str) -> Option<AiPromptInfo> {
    let json = raw.trim_end_matches('\0').trim();
    let v: serde_json::Value =
        serde_json::from_str(&exif_writer::sanitize_non_finite(json)).ok()?;
    let obj = v.as_object()?;
    let prompt = obj
        .get("prompt")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if prompt.is_empty() {
        return None;
    }
    let negative = obj
        .get("negative_prompt")
        .and_then(|p| p.as_str())
        .map(str::to_string);
    let params: Vec<String> = obj
        .iter()
        .filter(|(k, _)| *k != "prompt" && *k != "negative_prompt")
        .map(|(k, val)| format!("{k}: {}", json_value_to_string(val)))
        .collect();
    Some(AiPromptInfo {
        source: "Krea".to_string(),
        prompt,
        negative_prompt: negative,
        params: params.join("\n"),
        raw: raw.to_string(),
    })
}

/// InvokeAI z-image metadata: `invokeai_metadata` is a JSON object with
/// `positive_prompt`/`negative_prompt` (+ model, loras, …); `invokeai_graph`
/// is the workflow graph, whose texts are found by walking the JSON tree.
fn parse_invokeai_metadata(keyword: &str, raw: &str) -> Option<AiPromptInfo> {
    let v: serde_json::Value = serde_json::from_str(&exif_writer::sanitize_non_finite(raw)).ok()?;
    if keyword == "invokeai_metadata" {
        let obj = v.as_object()?;
        let prompt = obj
            .get("positive_prompt")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if prompt.is_empty() {
            return None;
        }
        let negative = obj
            .get("negative_prompt")
            .and_then(|p| p.as_str())
            .map(str::to_string);
        let params: Vec<String> = obj
            .iter()
            .filter(|(k, _)| *k != "positive_prompt" && *k != "negative_prompt")
            .map(|(k, val)| format!("{k}: {}", json_value_to_string(val)))
            .collect();
        return Some(AiPromptInfo {
            source: "InvokeAI".to_string(),
            prompt,
            negative_prompt: negative,
            params: params.join("\n"),
            raw: raw.to_string(),
        });
    }
    // invokeai_graph: walk the tree for the prompt texts.
    let mut positive: Option<String> = None;
    let mut negative: Option<String> = None;
    collect_json_texts(&v, &mut positive, &mut negative, 0);
    let prompt = positive?.trim().to_string();
    if prompt.is_empty() {
        return None;
    }
    Some(AiPromptInfo {
        source: "InvokeAI".to_string(),
        prompt,
        negative_prompt: negative,
        params: String::new(),
        raw: raw.to_string(),
    })
}

/// Depth-limited walk over a JSON value collecting strings stored under
/// prompt-like keys (InvokeAI graphs nest them arbitrarily deep).
fn collect_json_texts(
    v: &serde_json::Value,
    positive: &mut Option<String>,
    negative: &mut Option<String>,
    depth: usize,
) {
    if depth > 16 || (positive.is_some() && negative.is_some()) {
        return;
    }
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map {
                let kl = k.to_ascii_lowercase();
                if let Some(s) = val.as_str() {
                    let s = s.trim();
                    if !s.is_empty() {
                        if kl.contains("negative") && negative.is_none() {
                            *negative = Some(s.to_string());
                        } else if (kl.contains("positive") || kl == "prompt") && positive.is_none()
                        {
                            *positive = Some(s.to_string());
                        }
                    }
                }
                collect_json_texts(val, positive, negative, depth + 1);
            }
        }
        serde_json::Value::Array(items) => {
            for it in items {
                collect_json_texts(it, positive, negative, depth + 1);
            }
        }
        _ => {}
    }
}

/// Detect and parse AI-generation prompts stored in the image.
fn detect_ai_prompt(
    exif: &[MetadataEntry],
    text_chunks: &[png_meta::PngTextChunk],
    comments: &[String],
) -> Option<AiPromptInfo> {
    // PNG: A1111/Forge store the full generation info in the `parameters`
    // chunk; ComfyUI stores `prompt` (the executed graph) and `workflow`
    // (the full UI graph) as JSON; InvokeAI uses `sd-metadata`; Draw Things /
    // Krea write an `XML:com.adobe.xmp` chunk; some generators split the
    // prompt into `positive_prompt` / `negative_prompt`.
    // A1111 `parameters` first: it is the most complete source (prompt +
    // negative + settings in one string). Some ComfyUI graphs also carry a
    // `parameters` chunk — prefer it over the graph.
    for c in text_chunks {
        if c.keyword == "parameters" {
            if let Some(info) = parse_a1111_parameters(&c.text) {
                return Some(info);
            }
        }
    }
    let mut positive_split: Option<String> = None;
    let mut negative_split: Option<String> = None;
    for c in text_chunks {
        match c.keyword.as_str() {
            "parameters" => {} // already handled above
            // Krea stores the real prompt (plus model/lora lists) in its
            // `generation_data` chunk; InvokeAI z-image in `invokeai_metadata`
            // (`invokeai_graph` is the workflow graph — its texts are found
            // by walking the JSON). Both are direct sources, preferred over
            // the executed graph, which may only hold lora tags + LLM
            // templates.
            "generation_data" => {
                if let Some(info) = parse_generation_data(&c.text) {
                    return Some(info);
                }
            }
            "invokeai_metadata" | "invokeai_graph" => {
                if let Some(info) = parse_invokeai_metadata(&c.keyword, &c.text) {
                    return Some(info);
                }
            }
            "prompt" | "workflow" => {
                if let Some(info) = parse_comfyui_json(&c.keyword, &c.text) {
                    // A graph with no extractable text (e.g. a bare UI-workflow
                    // shell) is not useful — keep looking.
                    if !info.prompt.is_empty() {
                        return Some(info);
                    }
                }
            }
            "sd-metadata" | "sd_metadata" => {
                return Some(AiPromptInfo {
                    source: "InvokeAI".to_string(),
                    prompt: String::new(),
                    negative_prompt: None,
                    params: c.text.clone(),
                    raw: c.text.clone(),
                });
            }
            "positive_prompt" => positive_split = Some(c.text.clone()),
            "negative_prompt" => negative_split = Some(c.text.clone()),
            _ => {
                // XMP: the chunk keyword is the XMP namespace
                // (`XML:com.adobe.xmp`); only chunks that carry generation
                // markers are worth parsing.
                if xmp_text_looks_ai(&c.text) {
                    if let Some(info) = parse_xmp_prompt(&c.text) {
                        return Some(info);
                    }
                }
            }
        }
    }
    if let Some(prompt) = positive_split {
        return Some(AiPromptInfo {
            source: "Stable Diffusion".to_string(),
            prompt: prompt.clone(),
            negative_prompt: negative_split,
            params: String::new(),
            raw: prompt,
        });
    }

    let exif_lookup = |key: &str| {
        exif.iter()
            .find(|e| e.key.eq_ignore_ascii_case(key))
            .map(|e| e.value.clone())
    };
    // ComfyUI JPEG convention: `prompt:{json}` / `workflow:{json}` payloads
    // may live in any of Make/Model/ImageDescription/UserComment (some
    // writers capitalise the prefix, e.g. `Prompt:`). Prefer the `prompt`
    // payload — it carries the executed prompt texts — and keep the UI
    // workflow graph visible too (it is hidden from the EXIF table, so it
    // must surface in the AI block).
    //
    // This scan runs BEFORE the A1111-comment check below: a converted
    // PNG→JPG carries the ComfyUI prompt in EXIF Model and may also carry an
    // A1111 `parameters` COM segment — the ComfyUI payload is the primary
    // source for ComfyUI-generated files.
    let mut workflow_json: Option<String> = None;
    let mut prompt_info: Option<AiPromptInfo> = None;
    for key in ["Make", "Model", "ImageDescription", "UserComment"] {
        let Some(v) = exif_lookup(key) else { continue };
        let Some((kind, json)) = split_comfyui_payload(&v) else {
            continue;
        };
        if !json.trim_start().starts_with('{') {
            continue;
        }
        match kind {
            "workflow" if workflow_json.is_none() => workflow_json = Some(json.to_string()),
            "prompt" if prompt_info.is_none() => {
                prompt_info = parse_comfyui_json("prompt", json);
            }
            _ => {}
        }
    }
    if let Some(mut info) = prompt_info {
        // A degenerate `prompt` graph with no extracted texts is not useful —
        // fall through to the workflow payload.
        if !info.prompt.is_empty() {
            if let Some(wf) = &workflow_json {
                info.params = wf.clone();
            }
            return Some(info);
        }
    }
    if let Some(wf) = &workflow_json {
        if let Some(info) = parse_comfyui_json("workflow", wf) {
            return Some(info);
        }
    }

    // JPEG: A1111-era web UIs often put the prompt in a COM segment; some
    // tools use EXIF UserComment or ImageDescription. Only reached when no
    // ComfyUI `prompt:`/`workflow:` payload was found above.
    for comment in comments {
        if looks_like_a1111(comment) {
            if let Some(info) = parse_a1111_parameters(comment) {
                return Some(info);
            }
        }
    }

    // A1111-style prompts or bare ComfyUI graphs (civitai AIR) in EXIF
    // UserComment / ImageDescription — neither has a `prompt:`/`workflow:`
    // prefix, so both are detected here.
    for key in ["UserComment", "ImageDescription"] {
        let Some(v) = exif_lookup(key) else { continue };
        if looks_like_a1111(&v) {
            if let Some(info) = parse_a1111_parameters(&v) {
                return Some(info);
            }
        }
        if looks_like_comfyui_graph(&v) {
            if let Some(info) = parse_comfyui_json("prompt", &v) {
                if !info.prompt.is_empty() {
                    return Some(info);
                }
            }
        }
    }

    None
}

/// Case-insensitively splits a ComfyUI EXIF value (`prompt:{json}` or
/// `workflow:{json}`) into its payload kind and the JSON body. Some writers
/// capitalise the prefix (`Prompt:`/`Workflow:`), so the match ignores case.
fn split_comfyui_payload(value: &str) -> Option<(&'static str, &str)> {
    let v = value.trim_start();
    for (prefix, kind) in [("prompt:", "prompt"), ("workflow:", "workflow")] {
        if v.len() >= prefix.len() && v[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return Some((kind, &v[prefix.len()..]));
        }
    }
    None
}

/// True when the value is a ComfyUI `prompt:`/`workflow:` JSON payload
/// (case-insensitive prefix).
fn is_comfyui_exif_value(value: &str) -> bool {
    split_comfyui_payload(value).is_some()
}

/// Heuristic: does this value look like a *bare* ComfyUI graph (JSON object
/// with `class_type` nodes) written without a `prompt:`/`workflow:` prefix?
/// Some generators (e.g. civitai AIR) store the graph directly in EXIF
/// UserComment.
fn looks_like_comfyui_graph(value: &str) -> bool {
    let v = value.trim_start();
    v.starts_with('{') && v.contains("\"class_type\"") && v.contains("\"inputs\"")
}

/// Heuristic: does this text look like an A1111 `parameters` payload?
fn looks_like_a1111(text: &str) -> bool {
    text.contains("Steps:")
        || text.contains("Sampler:")
        || text.contains("Seed:")
        || text.contains("Negative prompt:")
        || text.contains("CFG scale:")
}

/// Parses an AUTOMATIC1111 `parameters` string:
/// ```text
/// (masterpiece, best quality), dog
/// Negative prompt: blurry, low quality
/// Steps: 28, Sampler: Euler a, CFG scale: 7, Seed: 123, Size: 512x512, ...
/// ```
fn parse_a1111_parameters(raw: &str) -> Option<AiPromptInfo> {
    if !looks_like_a1111(raw) && !raw.contains('\n') {
        return None;
    }
    let lines: Vec<&str> = raw.lines().map(|l| l.trim_end()).collect();

    // Positive prompt: everything up to "Negative prompt:" (or the first
    // line that starts a parameter list).
    let mut prompt_lines = Vec::new();
    let mut negative = None;
    let mut params = String::new();
    let mut in_negative = false;
    let mut in_params = false;

    for line in lines {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Negative prompt:") {
            in_negative = true;
            in_params = false;
            let rest = rest.trim();
            if !rest.is_empty() {
                negative = Some(rest.to_string());
            }
            continue;
        }
        if looks_like_param_line(trimmed) {
            in_negative = false;
            in_params = true;
            params.push_str(trimmed);
            params.push('\n');
            continue;
        }
        if in_negative {
            // continuation of the negative prompt until the params block
            if let Some(n) = negative.as_mut() {
                n.push_str(trimmed);
            }
        } else if in_params {
            params.push_str(trimmed);
            params.push('\n');
        } else {
            prompt_lines.push(trimmed);
        }
    }

    let prompt = prompt_lines
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    Some(AiPromptInfo {
        source: "AUTOMATIC1111".to_string(),
        prompt,
        negative_prompt: negative,
        params: params.trim_end().to_string(),
        raw: raw.to_string(),
    })
}

fn looks_like_param_line(trimmed: &str) -> bool {
    trimmed.starts_with("Steps:")
        || trimmed.starts_with("Sampler:")
        || trimmed.starts_with("CFG scale:")
        || trimmed.starts_with("Seed:")
        || trimmed.starts_with("Size:")
        || trimmed.starts_with("Model:")
        || trimmed.starts_with("Model hash:")
        || trimmed.starts_with("Hires upscale:")
        || trimmed.starts_with("Denoising strength:")
        || trimmed.starts_with("Version:")
}

/// Reads the prompt text reachable from a ComfyUI text-encoding node. The
/// input key depends on the node family:
/// - `CLIPTextEncode` / `CLIPTextEncodeFlux` / `smZ CLIPTextEncode` → `inputs.text`
/// - Qwen-Image nodes (`TextEncodeQwenImage*`) → `inputs.prompt`
/// - Prompt-UI nodes (`ZML_PromptUINode`) → `inputs.positive_prompt`/`negative_prompt`
///
/// Many graphs *chain* text nodes (e.g. Krea: `TextEncodeKrea2.prompt` →
/// `PixaromaPauseText.text` → `CR Text Replace.text` → `TextGenerate.prompt`;
/// or `CLIPTextEncode.text → ["13", 0]` → `ZML_PromptUINode`), so a value that
/// is itself a link (`["node_id", output]`) is followed transitively. A
/// visited set guards against cycles.
fn comfyui_node_text(
    node: &serde_json::Value,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    comfyui_node_text_inner(node, obj, &mut Vec::new(), false)
}

/// Adds a trimmed non-empty fragment, skipping JSON blobs and duplicates.
fn push_fragment(fragments: &mut Vec<String>, s: &str) {
    let s = s.trim();
    if s.is_empty() || s.starts_with('{') {
        return;
    }
    if !fragments.iter().any(|f| f == s) {
        fragments.push(s.to_string());
    }
}

/// True when the text is only `<lora:…>` tags (possibly comma-separated) —
/// such a fragment is metadata (a model attachment), not a prompt.
fn is_lora_tag_only(s: &str) -> bool {
    let mut rest = s.trim();
    while let Some(i) = rest.find("<lora:") {
        if !rest[..i].trim().is_empty() {
            return false;
        }
        let Some(end) = rest[i..].find('>') else {
            return false;
        };
        rest = rest[i + end + 1..].trim_start_matches(',').trim_start();
    }
    rest.trim().is_empty()
}

/// Detects LLM system-prompt boilerplate that some generators (Krea) embed
/// in the executed graph — instructions to a prompt-rewriting model, not the
/// generation prompt itself.
fn is_system_template(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with("You are an expert prompt engineer")
        || t.starts_with("You are a visual detail engine")
        || (t.starts_with("You are an") && t.contains("text-to-image"))
}

/// A string that can serve as a generation prompt: not an LLM system
/// template and not a bare LoRA tag. Used everywhere a candidate text is
/// accepted or rejected, so the filter can never drift between paths.
fn is_meaningful_prompt(s: &str) -> bool {
    !is_system_template(s) && !is_lora_tag_only(s)
}

/// Krea-style LLM nodes store their output as JSON text inside a string
/// node: either a plain object with a `prompt` / `high_level_description`
/// key, or the `{"error":…, "raw":"```json\n{…}```"}` wrapper.
fn extract_prompt_from_json(s: &str) -> Option<String> {
    let mut text = s.trim().to_string();
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 4 {
            return None;
        }
        // Parse first: the wrapper may itself contain "```json" inside its
        // `raw` string, so un-fencing must happen only once the outer JSON
        // has been unwrapped.
        if let Ok(v) =
            serde_json::from_str::<serde_json::Value>(&exif_writer::sanitize_non_finite(&text))
        {
            // Krea wraps a failed LLM parse in {"error":…, "raw":"<json>"}.
            if let Some(raw) = v.get("raw").and_then(|r| r.as_str()) {
                let raw = raw.trim();
                if raw == text.as_str() {
                    return None;
                }
                text = raw.to_string();
                continue;
            }
            for key in ["prompt", "positive_prompt", "high_level_description", "c"] {
                if let Some(p) = v.get(key).and_then(|p| p.as_str()) {
                    let p = p.trim();
                    if !p.is_empty() && !p.starts_with('{') {
                        return Some(p.to_string());
                    }
                }
            }
        }
        // Not parseable: unwrap a ```json … ``` fenced block if present.
        if let Some(start) = text.find("```json") {
            if let Some(rel_end) = text[start + 7..].find("```") {
                text = text[start + 7..start + 7 + rel_end].trim().to_string();
                continue;
            }
        }
        return None;
    }
}

/// Keys of join-family nodes that are separators/controls rather than text
/// operands (delimiters, `间隔符_separator`, `*_mode` switches, …).
fn is_join_control_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k == "delimiter"
        || k == "sep"
        || k.contains("separator")
        || k.contains("delimiter")
        || k.ends_with("_sep")
        || k.contains("_mode")
        || k.contains("single_line")
}

/// Ordered concatenation of a join-family node's operands (ConditioningConcat
/// branches, StringConcatenate string_a/string_b, TextJoiner 文本N_textN,
/// PromptDPBuilder part keys, …). Operands may be plain strings or links;
/// serde_json maps iterate in key order, which matches the operand order of
/// these nodes. Bare lora tags and LLM system templates are dropped.
#[allow(clippy::ptr_arg)] // `Vec` is required: branches clone and extend it
fn comfyui_join_parts(
    inputs: &serde_json::Map<String, serde_json::Value>,
    obj: &serde_json::Map<String, serde_json::Value>,
    visited: &mut Vec<String>,
) -> Option<String> {
    let delimiter = inputs
        .get("delimiter")
        .and_then(|d| d.as_str())
        .filter(|d| !d.is_empty())
        .unwrap_or(", ");
    let mut parts: Vec<String> = Vec::new();
    for (key, v) in inputs {
        if is_join_control_key(key) {
            continue;
        }
        let candidate: Option<String> = match v {
            serde_json::Value::String(s) => {
                let s = s.trim();
                if s.is_empty() || s.starts_with('{') || !is_meaningful_prompt(s) {
                    None
                } else {
                    Some(s.to_string())
                }
            }
            serde_json::Value::Array(arr) => {
                let Some(id) = arr.first().and_then(|x| x.as_str()) else {
                    continue;
                };
                if visited.iter().any(|v| v == id) {
                    None
                } else {
                    let sub_prefer = arr.get(1).and_then(|x| x.as_i64()).unwrap_or(0) >= 1;
                    let mut sub = visited.clone();
                    sub.push(id.to_string());
                    obj.get(id)
                        .and_then(|n| comfyui_node_text_inner(n, obj, &mut sub, sub_prefer))
                }
            }
            _ => None,
        };
        if let Some(c) = candidate {
            if !parts.iter().any(|p| p == &c) {
                parts.push(c);
            }
        }
    }
    let joined = parts.join(delimiter);
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn comfyui_node_text_inner(
    node: &serde_json::Value,
    obj: &serde_json::Map<String, serde_json::Value>,
    visited: &mut Vec<String>,
    prefer_negative: bool,
) -> Option<String> {
    let inputs = node.get("inputs").and_then(|i| i.as_object())?;
    let class_type = node
        .get("class_type")
        .and_then(|c| c.as_str())
        .unwrap_or("");

    // Join-family nodes (Concat / TextJoiner / JoinStrings /
    // StringConcatenate / PromptDPBuilder): the prompt is the ordered
    // concatenation of ALL operands — a first-found walk would stop at the
    // alphabetically-first branch and truncate the prompt to its prefix.
    let is_join = class_type.contains("Concat")
        || class_type.contains("TextJoiner")
        || class_type.contains("JoinStrings")
        || class_type.contains("StringConcatenate")
        || class_type.contains("PromptDPBuilder");
    if is_join {
        return comfyui_join_parts(inputs, obj, visited);
    }

    // 1) Own text: the first standard key (priority order; negative-biased
    //    when the node is reached via a negative output, e.g. `["n", 1]`)
    //    that holds a meaningful string. This is authoritative — linked and
    //    arbitrary-string candidates never override the node's own text.
    let keys: &[&str] = if prefer_negative {
        &[
            "negative",
            "negative_prompt",
            "text_negative",
            "text",
            "prompt",
            "positive",
            "positive_prompt",
            "text_positive",
            "value",
            "string",
        ]
    } else {
        &[
            "text",
            "prompt",
            "positive",
            "positive_prompt",
            "text_positive",
            "negative",
            "negative_prompt",
            "text_negative",
            "value",
            "string",
        ]
    };
    for key in keys {
        if let Some(serde_json::Value::String(s)) = inputs.get(*key) {
            let s = s.trim();
            // Own text is authoritative, but never when it is only a LoRA
            // tag or an LLM system template (Krea's TextGenerate embeds its
            // system prompt in `prompt`) — those must fall through to the
            // link/candidate walk, which applies the filters per candidate.
            if !s.is_empty() && !s.starts_with('{') && is_meaningful_prompt(s) {
                return Some(s.to_string());
            }
        }
    }
    // 1.5) String-literal nodes may hold a structured JSON blob (Krea writes
    //      its LLM output there) — extract the prompt from it.
    if class_type.contains("String") {
        for key in ["value", "string"] {
            if let Some(serde_json::Value::String(s)) = inputs.get(key) {
                if s.trim_start().starts_with('{') {
                    if let Some(p) = extract_prompt_from_json(s) {
                        return Some(p);
                    }
                }
            }
        }
    }

    // 2) Follow every link input (conditioning ports, switch branches, …).
    //    Each branch gets its own visited clone (cycles stay guarded within a
    //    branch), and the output index of EACH link decides whether the
    //    target is read as a positive or negative output.
    let mut candidates: Vec<String> = Vec::new();
    for v in inputs.values() {
        let serde_json::Value::Array(arr) = v else {
            continue;
        };
        let Some(id) = arr.first().and_then(|x| x.as_str()) else {
            continue;
        };
        if visited.iter().any(|v| v == id) {
            continue;
        }
        let sub_prefer = arr.get(1).and_then(|x| x.as_i64()).unwrap_or(0) >= 1;
        let mut sub = visited.clone();
        sub.push(id.to_string());
        if let Some(next) = obj.get(id) {
            if let Some(t) = comfyui_node_text_inner(next, obj, &mut sub, sub_prefer) {
                push_fragment(&mut candidates, &t);
            }
        }
    }
    // 3) Text-family nodes can carry the prompt under arbitrary (often
    //    localized) key names (CLIPTextEncode with custom widgets, …). LLM/API
    //    nodes are excluded — their system prompts and endpoint URLs are not
    //    generation prompts. Short values are controls ("none", "comfy", …).
    if !class_type.contains("LLM")
        && (class_type.contains("Text")
            || class_type.contains("Prompt")
            || class_type.contains("Encode")
            || class_type.contains("String"))
    {
        for (_key, v) in inputs {
            if let serde_json::Value::String(s) = v {
                if s.trim().len() > 10 {
                    push_fragment(&mut candidates, s);
                }
            }
        }
    }

    // Switches and multi-branch chains expose several candidate texts; the
    // most informative (longest) meaningful one is the prompt.
    candidates
        .iter()
        .filter(|f| is_meaningful_prompt(f))
        .max_by_key(|f| f.len())
        .map(|f| f.to_string())
}

/// Resolves a ComfyUI link value (`["node_id", output]`) to the referenced
/// node's prompt text (following any further text links). The output index
/// matters for nodes with separate positive/negative outputs: index ≥ 1
/// selects the negative text.
fn comfyui_link_text(
    link: &serde_json::Value,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let arr = link.as_array()?;
    let id = arr.first()?.as_str()?;
    let out = arr.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
    comfyui_node_text_inner(obj.get(id)?, obj, &mut Vec::new(), out >= 1)
}

/// Resolves a sampler node's `positive`/`negative` conditioning. Classic
/// KSamplers carry them as direct link inputs; `SamplerCustomAdvanced`-family
/// nodes route the conditioning through a `guider` link (DualModelGuider,
/// CFGGuider), so the guider node's inputs are consulted as well.
fn resolve_sampler_input(
    inputs: &serde_json::Value,
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    if let Some(link) = inputs.get(key) {
        if let Some(t) = comfyui_link_text(link, obj) {
            return Some(t);
        }
    }
    let guider = inputs.get("guider")?;
    let guider_id = guider.as_array()?.first()?.as_str()?;
    let guider_node = obj.get(guider_id)?;
    let ginputs = guider_node.get("inputs")?.as_object()?;
    let link = ginputs.get(key)?;
    comfyui_link_text(link, obj)
}

/// Parses a ComfyUI `prompt`/`workflow` JSON and extracts the prompt texts.
///
/// When the graph is an *executed prompt* (the format generated by ComfyUI
/// itself and civitai AIR), the KSampler node's `positive`/`negative` inputs
/// reference the text-encoding nodes — those are split into the prompt and
/// negative prompt respectively. Without a sampler link (e.g. a workflow UI
/// graph), every text-encoding node is collected into the prompt block.
/// Bare `NaN` / `Infinity` / `-Infinity` JSON tokens (which some generators,
/// e.g. Krea, emit for `is_changed`) are rewritten to `null` by the shared
/// `exif_writer::sanitize_non_finite` (boundary-checked, mirroring
/// ComfyUI's own regex) so the strict serde_json parser accepts the graph.
/// Strings are left untouched.
fn parse_comfyui_json(keyword: &str, json: &str) -> Option<AiPromptInfo> {
    if !json.trim_start().starts_with('{') {
        return None;
    }
    let v: serde_json::Value =
        serde_json::from_str(&exif_writer::sanitize_non_finite(json)).ok()?;
    let obj = v.as_object()?;

    // Executed-prompt graphs: follow the sampler's positive/negative links
    // (through the `guider` for SamplerCustomAdvanced-family samplers).
    let mut positive: Option<String> = None;
    let mut negative: Option<String> = None;
    for (_, node) in obj {
        let class_type = node
            .get("class_type")
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if !class_type.contains("KSampler") && !class_type.contains("Sampler") {
            continue;
        }
        let Some(inputs) = node.get("inputs") else {
            continue;
        };
        if positive.is_none() {
            positive = resolve_sampler_input(inputs, obj, "positive");
        }
        if negative.is_none() {
            negative = resolve_sampler_input(inputs, obj, "negative");
        }
    }
    // Only take the sampler path when the *positive* text resolved — a
    // resolved negative with an unresolvable positive would produce an empty
    // prompt; fall back to collecting all text nodes instead.
    if let Some(pos_text) = positive {
        // Krea graphs zero out the negative with a `ConditioningZeroOut`
        // wired to the *same* CLIPTextEncode chain as the positive, so both
        // resolve to the identical string. Semantically that is an empty
        // negative — drop it rather than show the prompt twice.
        if negative.as_deref() == Some(pos_text.as_str()) {
            negative = None;
        }
        return Some(AiPromptInfo {
            source: "ComfyUI".to_string(),
            prompt: pos_text,
            negative_prompt: negative,
            params: if keyword == "workflow" {
                json.to_string()
            } else {
                String::new()
            },
            raw: json.to_string(),
        });
    }

    // No sampler link: collect every text-encoding node.
    let mut prompts: Vec<String> = obj
        .values()
        .filter_map(|n| comfyui_node_text(n, obj))
        .collect();
    // UI *workflow* graphs are shaped `{"id":…, "nodes":[…]}`: the executed
    // `inputs` aren't present, but each node in the array carries its widget
    // values — the prompt lives in `widgets_values[0]` of text/prompt-
    // encoding nodes.
    if prompts.is_empty() {
        if let Some(nodes) = obj.get("nodes").and_then(|n| n.as_array()) {
            for node in nodes {
                let ty = node.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if !ty.contains("Text") && !ty.contains("Prompt") && !ty.contains("Encode") {
                    continue;
                }
                if let Some(wv) = node.get("widgets_values").and_then(|w| w.as_array()) {
                    for v in wv {
                        if let Some(s) = v.as_str() {
                            let s = s.trim();
                            if !s.is_empty() {
                                prompts.push(s.to_string());
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
    Some(AiPromptInfo {
        source: "ComfyUI".to_string(),
        prompt: prompts.join("\n\n"),
        negative_prompt: None,
        params: if keyword == "workflow" {
            json.to_string()
        } else {
            String::new()
        },
        raw: json.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a1111_full_payload() {
        let raw = "masterpiece, best quality, dog\nNegative prompt: blurry, low quality\nSteps: 28, Sampler: Euler a, CFG scale: 7, Seed: 123, Size: 512x512";
        let info = parse_a1111_parameters(raw).unwrap();
        assert_eq!(info.prompt, "masterpiece, best quality, dog");
        assert_eq!(info.negative_prompt.as_deref(), Some("blurry, low quality"));
        assert!(info.params.contains("Steps: 28"));
        assert!(info.params.contains("Seed: 123"));
    }

    #[test]
    fn parses_comfyui_workflow() {
        let json = r#"{"1":{"class_type":"CLIPTextEncode","inputs":{"text":"a red car"}},"2":{"class_type":"CLIPTextEncode","inputs":{"text":"blurry"}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert_eq!(info.prompt, "a red car\n\nblurry");
        assert_eq!(info.source, "ComfyUI");
        assert!(info.raw.contains("CLIPTextEncode"));
    }

    #[test]
    fn non_prompt_png_has_no_ai() {
        let exif = vec![];
        let chunks = vec![png_meta::PngTextChunk {
            kind: png_meta::PngTextKind::Text,
            keyword: "Software".to_string(),
            text: "GIMP".to_string(),
        }];
        assert!(detect_ai_prompt(&exif, &chunks, &[]).is_none());
    }

    #[test]
    fn jpeg_exif_prompt_detected() {
        // A JPEG whose prompt lives in EXIF ImageDescription (no COM)
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exif.jpg");
        {
            let img = image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        // Inject an EXIF APP1 carrying an ImageDescription with A1111 markers.
        // Build a minimal EXIF blob: TIFF header + IFD0 with one SHORT tag.
        let text: &[u8] = b"Steps: 20, Sampler: Euler";
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II*\0"); // little-endian TIFF, magic 42
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
        tiff.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
                                                     // tag 0x010E (ImageDescription), type 2 (ASCII), count = text len
        tiff.extend_from_slice(&0x010Eu16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&(text.len() as u32).to_le_bytes());
        // value offset must point into the data area: IFD0 at 8 (8-byte TIFF
        // header) + 2-byte count + 12-byte entry + 4-byte next-IFD = 26.
        tiff.extend_from_slice(&26u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
        tiff.extend_from_slice(text); // data area begins at byte 26

        let mut exif_payload = b"Exif\0\0".to_vec();
        exif_payload.extend_from_slice(&tiff);
        let seg = jpeg_meta::JpegSegment {
            marker: 0xE1,
            data: exif_payload,
        };
        let with_exif = jpeg_meta::merge_missing_segments(&bytes, std::slice::from_ref(&seg));
        std::fs::write(&path, &with_exif).unwrap();

        let after = std::fs::read(&path).unwrap();
        assert!(
            find_jpeg_prompt_marker(&after),
            "prompt stored in EXIF ImageDescription must be detected"
        );
    }

    /// Builds a little-endian TIFF with an IFD0 that points (via ExifOffset)
    /// at a sub-IFD holding one UserComment (type UNDEFINED) with a
    /// `UNICODE\0` charset header followed by UTF-16 text — the layout some
    /// tools (e.g. A1111-era writers) use.
    fn build_tiff_with_utf16_usercomment(text: &str) -> Vec<u8> {
        let mut body = b"UNICODE\0".to_vec();
        for u in text.encode_utf16() {
            body.extend_from_slice(&u.to_be_bytes());
        }
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
        tiff.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
        let sub_ifd_off = 26u32; // 8 + 2 + 12 + 4
                                 // ExifOffset (0x8769), type 4, count 1 -> sub-IFD
        tiff.extend_from_slice(&0x8769u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&sub_ifd_off.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD0 = 0
                                                     // Sub-IFD @ 26: UserComment (0x9286), type 7, count = body len
        let uc_off = sub_ifd_off + 2 + 12 + 4;
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x9286u16.to_le_bytes());
        tiff.extend_from_slice(&7u16.to_le_bytes());
        tiff.extend_from_slice(&(body.len() as u32).to_le_bytes());
        tiff.extend_from_slice(&uc_off.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next sub-IFD = 0
        tiff.extend_from_slice(&body);
        tiff
    }

    #[test]
    fn decode_undefined_utf16_be() {
        // A1111 stores prompts in EXIF UserComment as UTF-16BE under UNICODE\0
        let mut bytes = b"UNICODE\0".to_vec();
        for u in "masterpiece, best quality\nNegative prompt: blurry\nSteps: 28".encode_utf16() {
            bytes.extend_from_slice(&u.to_be_bytes());
        }
        let out = decode_undefined_text(&bytes);
        assert!(out.starts_with("masterpiece"));
        assert!(out.contains("Negative prompt:"));
        assert!(out.contains("Steps: 28"));
    }

    #[test]
    fn decode_undefined_utf16_le() {
        let mut bytes = b"UNICODE\0".to_vec();
        for u in "Steps: 20, Sampler: Euler".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let out = decode_undefined_text(&bytes);
        assert!(out.contains("Steps: 20"));
    }

    #[test]
    fn jpeg_utf16_usercomment_prompt_detected() {
        // A1111 "parameters" text stored as UTF-16BE in EXIF UserComment (in
        // the Exif sub-IFD), like the `103876708.jpeg` sample.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("uc.jpg");
        {
            let img = image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let text = "tkmi2, masterpiece, best quality\nNegative prompt: blurry, low quality\nSteps: 34, Sampler: Euler a, CFG scale: 4.0, Seed: 1, Version: 1.10.1";
        let tiff = build_tiff_with_utf16_usercomment(text);
        let mut exif_payload = b"Exif\0\0".to_vec();
        exif_payload.extend_from_slice(&tiff);
        let seg = jpeg_meta::JpegSegment {
            marker: 0xE1,
            data: exif_payload,
        };
        let with_exif = jpeg_meta::merge_missing_segments(&bytes, std::slice::from_ref(&seg));
        std::fs::write(&path, &with_exif).unwrap();

        assert!(find_jpeg_prompt_marker(&std::fs::read(&path).unwrap()));
        let meta = get_metadata(path.to_str().unwrap()).unwrap();
        let ai = meta
            .ai
            .expect("UTF-16BE UserComment prompt must be detected");
        assert_eq!(ai.source, "AUTOMATIC1111");
        assert!(ai.prompt.contains("tkmi2, masterpiece"));
        assert!(ai.params.contains("Steps: 34"));
        assert!(ai.negative_prompt.as_deref().unwrap().contains("blurry"));
    }

    #[test]
    fn tolerant_reader_handles_overlapping_next_ifd() {
        // Mimics the real `103876708.jpeg`: the UserComment value starts right
        // after the entry, overlapping the "next IFD" pointer. kamadak-exif
        // rejects this with "Unexpected next IFD"; the tolerant reader must
        // still extract the prompt.
        let mut body = b"UNICODE\0".to_vec();
        for u in
            "tkmi2, masterpiece, best quality\nNegative prompt: blurry\nSteps: 34".encode_utf16()
        {
            body.extend_from_slice(&u.to_be_bytes());
        }
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
        tiff.extend_from_slice(&1u16.to_le_bytes());
        let sub_ifd_off = 26u32;
        tiff.extend_from_slice(&0x8769u16.to_le_bytes()); // ExifOffset
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&sub_ifd_off.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes()); // IFD0 next = 0
                                                     // Sub-IFD @ 26
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x9286u16.to_le_bytes()); // UserComment
        tiff.extend_from_slice(&7u16.to_le_bytes());
        tiff.extend_from_slice(&(body.len() as u32).to_le_bytes());
        // Value offset 40 overlaps the "next IFD" field of the sub-IFD
        tiff.extend_from_slice(&40u32.to_le_bytes());
        tiff.extend_from_slice(&body);

        let mut payload = b"Exif\0\0".to_vec();
        payload.extend_from_slice(&tiff);
        let fields = extract_exif_text_fields(&payload);
        let uc = fields
            .iter()
            .find(|f| f.key == "UserComment")
            .expect("UserComment must be read by the tolerant reader");
        assert!(uc.value.contains("tkmi2, masterpiece"));

        // End-to-end via a JPEG: badge + metadata
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("overlap.jpg");
        {
            let img = image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let seg = jpeg_meta::JpegSegment {
            marker: 0xE1,
            data: payload,
        };
        let with_exif = jpeg_meta::merge_missing_segments(&bytes, std::slice::from_ref(&seg));
        std::fs::write(&path, &with_exif).unwrap();

        assert!(find_jpeg_prompt_marker(&std::fs::read(&path).unwrap()));
        let meta = get_metadata(path.to_str().unwrap()).unwrap();
        let ai = meta
            .ai
            .expect("overlapping next-IFD UserComment prompt must be detected");
        assert_eq!(ai.source, "AUTOMATIC1111");
        assert!(ai.prompt.contains("tkmi2, masterpiece"));
    }

    /// Builds a minimal little-endian TIFF with the given ASCII fields, values
    /// appended after the IFD (mirrors how ComfyUI writes `workflow:`/`prompt:`
    /// into EXIF Make/Model).
    fn build_tiff_ascii(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
        tiff.extend_from_slice(&(fields.len() as u16).to_le_bytes());
        let data_off = 8 + 2 + fields.len() * 12 + 4;
        let mut data = Vec::new();
        for (name, value) in fields {
            let tag = match *name {
                "Make" => 0x010Fu16,
                "Model" => 0x0110,
                "ImageDescription" => 0x010E,
                _ => 0x010F,
            };
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&2u16.to_le_bytes()); // ASCII
            tiff.extend_from_slice(&((value.len() + 1) as u32).to_le_bytes()); // incl. NUL
            tiff.extend_from_slice(&((data_off + data.len()) as u32).to_le_bytes());
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD = 0
        tiff.extend_from_slice(&data);
        tiff
    }

    #[test]
    fn jpeg_comfyui_make_model_prompt_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comfy.jpg");
        {
            let img = image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let prompt_json = r#"{"250":{"inputs":{"text":"a red car driving","clip":["471",0]},"class_type":"CLIPTextEncode","_meta":{"title":"CLIP Text Encode (Prompt)"}}}"#;
        let workflow_json = r#"{"id":"abc","nodes":[{"type":"CLIPTextEncode","inputs":[]}]}"#;
        let tiff = build_tiff_ascii(&[
            ("Make", &format!("workflow:{workflow_json}")),
            ("Model", &format!("prompt:{prompt_json}")),
        ]);
        let mut exif_payload = b"Exif\0\0".to_vec();
        exif_payload.extend_from_slice(&tiff);
        let seg = jpeg_meta::JpegSegment {
            marker: 0xE1,
            data: exif_payload,
        };
        let with_exif = jpeg_meta::merge_missing_segments(&bytes, std::slice::from_ref(&seg));
        std::fs::write(&path, &with_exif).unwrap();

        // Badge-level detection
        assert!(find_jpeg_prompt_marker(&std::fs::read(&path).unwrap()));

        // Full metadata: prompt parsed from the Model `prompt:` payload
        let meta = get_metadata(path.to_str().unwrap()).unwrap();
        let ai = meta
            .ai
            .expect("ComfyUI prompt in EXIF Make/Model must be detected");
        assert_eq!(ai.source, "ComfyUI");
        assert!(ai.prompt.contains("a red car driving"));
        // The workflow graph from Make must not be lost
        assert!(ai.params.contains("\"nodes\""));
        // The multi-KB Make/Model blobs are hidden from the EXIF table
        assert!(!meta.exif.iter().any(|e| e.key == "Make"));
        assert!(!meta.exif.iter().any(|e| e.key == "Model"));
    }
    #[test]
    fn split_comfyui_payload_prefixes() {
        let prompt_json =
            r#"{"1":{"inputs":{"text":"hello world"},"class_type":"CLIPTextEncode"}}"#;
        let val = format!("prompt:{prompt_json}");
        let (kind, json) = split_comfyui_payload(&val).unwrap();
        assert_eq!(kind, "prompt");
        assert!(json.contains("hello world"));
        let info = parse_comfyui_json(kind, json).unwrap();
        assert_eq!(info.source, "ComfyUI");
        assert!(info.prompt.contains("hello world"));

        let (kind, wf) = split_comfyui_payload(r#"workflow:{"id":"x"}"#).unwrap();
        assert_eq!(kind, "workflow");
        let info = parse_comfyui_json(kind, wf).unwrap();
        assert!(info.raw.contains("\"id\""));

        assert!(split_comfyui_payload("Steps: 20, Sampler: Euler").is_none());
    }

    #[test]
    fn split_comfyui_payload_capitalized_prefixes() {
        // Some writers (e.g. WebUI-compatible generators) capitalise the
        // ComfyUI prefix: `Prompt:` / `Workflow:` instead of lowercase.
        let prompt_json = r#"{"1":{"inputs":{"text":"a red car"},"class_type":"CLIPTextEncode"}}"#;
        let val = format!("Prompt:{prompt_json}");
        let (kind, json) = split_comfyui_payload(&val).unwrap();
        assert_eq!(kind, "prompt");
        let info = parse_comfyui_json(kind, json).unwrap();
        assert_eq!(info.source, "ComfyUI");
        assert!(info.prompt.contains("a red car"));

        let (kind, wf) = split_comfyui_payload(r#"Workflow:{"id":"x"}"#).unwrap();
        assert_eq!(kind, "workflow");
        let info = parse_comfyui_json(kind, wf).unwrap();
        assert!(info.raw.contains("\"id\""));

        assert!(is_comfyui_exif_value("Prompt:{}"));
        assert!(is_comfyui_exif_value("Workflow:{}"));
        assert!(is_comfyui_exif_value("prompt:{}"));
        assert!(is_comfyui_exif_value("workflow:{}"));
        assert!(!is_comfyui_exif_value("Canon EOS"));
        assert!(!is_comfyui_exif_value(""));
    }

    #[test]
    fn comfyui_prompt_in_make_preferred_over_workflow_in_imagedescription() {
        // Mirrors `ComfyUI-euler-1.0-4-2025-09-24 19-12-57-0082.jpg`: the
        // workflow graph lives in EXIF ImageDescription as `Workflow:{...}` and
        // the executed prompt in Make as `Prompt:{...}` (capitalised). The
        // detector must surface the *prompt* (which carries the text), not the
        // workflow graph first.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.jpg");
        {
            let img = image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let prompt_json = r#"{"104":{"inputs":{"text":"a fluffy cat on a beach"},"class_type":"CLIPTextEncode"}}"#;
        let workflow_json = r#"{"id":"wf","nodes":[{"type":"CLIPTextEncode"}]}"#;
        // Make = Prompt:…, ImageDescription = Workflow:… (capitalised)
        let tiff = build_tiff_ascii(&[
            ("Make", &format!("Prompt:{prompt_json}")),
            ("ImageDescription", &format!("Workflow:{workflow_json}")),
        ]);
        let mut exif_payload = b"Exif\0\0".to_vec();
        exif_payload.extend_from_slice(&tiff);
        let seg = jpeg_meta::JpegSegment {
            marker: 0xE1,
            data: exif_payload,
        };
        let with_exif = jpeg_meta::merge_missing_segments(&bytes, std::slice::from_ref(&seg));
        std::fs::write(&path, &with_exif).unwrap();

        assert!(find_jpeg_prompt_marker(&std::fs::read(&path).unwrap()));
        let meta = get_metadata(path.to_str().unwrap()).unwrap();
        let ai = meta
            .ai
            .expect("capitalised ComfyUI prompt must be detected");
        assert_eq!(ai.source, "ComfyUI");
        assert!(
            ai.prompt.contains("a fluffy cat on a beach"),
            "the prompt payload must win over the workflow graph"
        );
        // The workflow graph must not be lost — it rides along in params.
        assert!(ai.params.contains("\"nodes\""));
        // The multi-KB Make/ImageDescription blobs are hidden from the EXIF
        // table (they surface in the AI block instead).
        assert!(!meta.exif.iter().any(|e| e.key == "Make"));
        assert!(!meta.exif.iter().any(|e| e.key == "ImageDescription"));
    }

    #[test]
    fn jpeg_comment_detected() {
        let exif = vec![];
        let chunks = vec![];
        let comments = vec!["Steps: 20, Sampler: DPM++ 2M, Seed: 42".to_string()];
        let info = detect_ai_prompt(&exif, &chunks, &comments).unwrap();
        assert_eq!(info.source, "AUTOMATIC1111");
        assert!(info.params.contains("Steps: 20"));
    }

    #[test]
    fn png_chunk_scanner_finds_ai_keyword() {
        // A1111-style trailing parameters chunk, scanned through the real
        // streaming walker (file on disk, IDAT skipped via seek).
        let mut bytes = Vec::new();
        {
            let w = &mut std::io::Cursor::new(&mut bytes);
            let mut enc = png::Encoder::new(w, 2, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&[0u8; 16]).unwrap();
            writer
                .write_text_chunk(&png::text_metadata::TEXtChunk::new(
                    "parameters".to_string(),
                    "Steps: 20".to_string(),
                ))
                .unwrap();
            writer.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, &bytes).unwrap();
        assert!(has_prompt_fast(path.to_str().unwrap()));
    }

    #[test]
    fn png_chunk_scanner_ignores_plain_chunks() {
        let mut bytes = Vec::new();
        {
            let w = &mut std::io::Cursor::new(&mut bytes);
            let mut enc = png::Encoder::new(w, 2, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&[0u8; 16]).unwrap();
            writer
                .write_text_chunk(&png::text_metadata::TEXtChunk::new(
                    "Software".to_string(),
                    "GIMP 2.10".to_string(),
                ))
                .unwrap();
            writer.finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.png");
        std::fs::write(&path, &bytes).unwrap();
        assert!(!has_prompt_fast(path.to_str().unwrap()));
    }

    #[test]
    fn check_prompts_filters_batch() {
        let dir = tempfile::tempdir().unwrap();
        let with_prompt = dir.path().join("a.png");
        std::fs::write(&with_prompt, {
            let mut bytes = Vec::new();
            {
                let w = &mut std::io::Cursor::new(&mut bytes);
                let mut enc = png::Encoder::new(w, 2, 2);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                let mut writer = enc.write_header().unwrap();
                writer.write_image_data(&[0u8; 16]).unwrap();
                writer
                    .write_text_chunk(&png::text_metadata::TEXtChunk::new(
                        "workflow".to_string(),
                        "{\"nodes\":[]}".to_string(),
                    ))
                    .unwrap();
                writer.finish().unwrap();
            }
            bytes
        })
        .unwrap();

        let no_prompt = dir.path().join("b.png");
        {
            let file = std::fs::File::create(&no_prompt).unwrap();
            let mut enc = png::Encoder::new(file, 2, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&[0u8; 16]).unwrap();
            writer.finish().unwrap();
        }
        let missing = dir.path().join("nope.png");
        let webp = dir.path().join("c.webp"); // unsupported extension
        std::fs::write(&webp, b"not an image").unwrap();

        let got = check_prompts(&[
            with_prompt.to_string_lossy().into_owned(),
            no_prompt.to_string_lossy().into_owned(),
            missing.to_string_lossy().into_owned(),
            webp.to_string_lossy().into_owned(),
        ]);
        assert_eq!(got, vec![with_prompt.to_string_lossy().into_owned()]);
    }

    /// `read_exif` must not error on files without EXIF and must find a known
    /// field on a synthetic JPEG that carries one.
    #[test]
    fn parse_comfyui_qwen_image_prompt_field() {
        // Qwen-Image workflows (TextEncodeQwenImage*) keep the prompt in
        // `inputs.prompt` instead of `inputs.text`.
        let json = r#"{"104":{"inputs":{"prompt":"a cat on a beach","clip":["38",0]},"class_type":"TextEncodeQwenImageEditPlus"}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info.prompt.contains("a cat on a beach"));
        assert_eq!(info.source, "ComfyUI");
    }

    #[test]
    fn bare_comfyui_graph_in_usercomment_detected() {
        // civitai AIR writes the ComfyUI graph directly into EXIF UserComment
        // (UTF-16BE, `UNICODE\0` header) without a `prompt:`/`workflow:`
        // prefix. It must still be detected and its prompt texts extracted.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("air.jpg");
        {
            let img = image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        // civitai AIR graph: resource-stack nodes + `smZ CLIPTextEncode` nodes
        // (positive = 6, negative = 7) referenced by the KSampler's
        // positive/negative links.
        let graph = r#"{"resource-stack":{"class_type":"CheckpointLoaderSimple","inputs":{}},"6":{"class_type":"smZ CLIPTextEncode","inputs":{"text":"a cat on a beach, masterpiece"}},"7":{"class_type":"smZ CLIPTextEncode","inputs":{"text":"worst quality, blurry"}},"3":{"class_type":"KSampler","inputs":{"positive":["6",0],"negative":["7",0]}}}"#;
        let tiff = build_tiff_with_utf16_usercomment(graph);
        let mut exif_payload = b"Exif\0\0".to_vec();
        exif_payload.extend_from_slice(&tiff);
        let seg = jpeg_meta::JpegSegment {
            marker: 0xE1,
            data: exif_payload,
        };
        let with_exif = jpeg_meta::merge_missing_segments(&bytes, std::slice::from_ref(&seg));
        std::fs::write(&path, &with_exif).unwrap();

        assert!(looks_like_comfyui_graph(graph));
        assert!(!looks_like_comfyui_graph("Canon EOS 5D"));
        assert!(!looks_like_comfyui_graph(""));

        // Badge-level detection
        assert!(find_jpeg_prompt_marker(&std::fs::read(&path).unwrap()));

        // Full metadata: prompt/negative split via the sampler's links
        let meta = get_metadata(path.to_str().unwrap()).unwrap();
        let ai = meta
            .ai
            .expect("bare ComfyUI graph in UserComment must be detected");
        assert_eq!(ai.source, "ComfyUI");
        assert!(ai.prompt.contains("a cat on a beach"));
        assert!(
            !ai.prompt.contains("worst quality"),
            "negative must not leak into prompt"
        );
        assert_eq!(ai.negative_prompt.as_deref(), Some("worst quality, blurry"));
        // The graph JSON is hidden from the EXIF table (surfaced in AI block)
        assert!(!meta.exif.iter().any(|e| e.key == "UserComment"));
    }

    #[test]
    fn comfyui_sampler_links_split_negative() {
        // A ComfyUI prompt graph whose KSampler references the text nodes
        // must separate positive from negative instead of merging them.
        let json = r#"{"1":{"class_type":"KSampler","inputs":{"positive":["2",0],"negative":["3",0]}},"2":{"class_type":"CLIPTextEncode","inputs":{"text":"a red car"}},"3":{"class_type":"CLIPTextEncode","inputs":{"text":"blurry, low quality"}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert_eq!(info.prompt, "a red car");
        assert_eq!(info.negative_prompt.as_deref(), Some("blurry, low quality"));
    }

    #[test]
    fn comfyui_workflow_without_sampler_merges_texts() {
        // No sampler link → every text node is collected into the prompt
        // (workflow-UI graphs, or graphs where links aren't resolvable).
        let json = r#"{"1":{"class_type":"CLIPTextEncode","inputs":{"text":"a red car"}},"2":{"class_type":"CLIPTextEncode","inputs":{"text":"blurry"}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert_eq!(info.prompt, "a red car\n\nblurry");
        assert_eq!(info.negative_prompt, None);
    }

    #[test]
    fn parses_draw_things_xmp_description() {
        // Draw Things writes an A1111-style prompt into <dc:description> and
        // a structured JSON into <exif:UserComment>.
        let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:exif="http://ns.adobe.com/exif/1.0/"><dc:description><rdf:Alt><rdf:li xml:lang="x-default">a red car&#xA;&#xA;on a highway&#xA;Steps: 12, Sampler: Euler A Trailing, Seed: 2573478191, Size: 1024x1280, Model: krea_2_raw_q8p.ckpt</rdf:li></rdf:Alt></dc:description><xmp:CreatorTool>Draw Things</xmp:CreatorTool><exif:UserComment><rdf:Alt><rdf:li xml:lang="x-default">{"c":"a red car","steps":12,"sampler":"Euler A Trailing","seed":2573478191,"model":"krea_2_raw_q8p.ckpt"}</rdf:li></rdf:Alt></exif:UserComment></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        assert!(xmp_text_looks_ai(xmp));

        // dc:description path → parsed as A1111 (Steps:/Sampler:/Seed:)
        let info = parse_xmp_prompt(xmp).unwrap();
        assert_eq!(info.source, "Draw Things");
        assert!(info.prompt.contains("a red car"));
        assert!(info.prompt.contains("on a highway"));
        assert!(info.params.contains("Steps: 12"));
        assert!(info.params.contains("Seed: 2573478191"));
    }

    #[test]
    fn parses_draw_things_xmp_usercomment_json() {
        // No dc:description → falls back to exif:UserComment JSON
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description xmlns:exif="http://ns.adobe.com/exif/1.0/" xmlns:xmp="http://ns.adobe.com/xap/1.0/"><xmp:CreatorTool>Draw Things</xmp:CreatorTool><exif:UserComment><rdf:Alt><rdf:li xml:lang="x-default">{"c":"a cat","uc":"blurry","steps":8,"sampler":"Euler","seed":42,"model":"waifast_f16.ckpt"}</rdf:li></rdf:Alt></exif:UserComment></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let info = parse_xmp_prompt(xmp).unwrap();
        assert_eq!(info.source, "Draw Things");
        assert_eq!(info.prompt, "a cat");
        assert_eq!(info.negative_prompt.as_deref(), Some("blurry"));
        assert!(info.params.contains("steps: 8"));
        assert!(info.params.contains("seed: 42"));
    }

    #[test]
    fn plain_tool_xmp_is_not_ai() {
        // Photoshop history XMP has neither dc:description nor exif:UserComment
        let xmp = r#"<x:xmpmeta><rdf:RDF><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"><xmp:CreatorTool>Adobe Photoshop</xmp:CreatorTool><xmpMM:History><rdf:Seq><rdf:li stEvt:action="created"/></rdf:Seq></xmpMM:History></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        assert!(!xmp_text_looks_ai(xmp));
        assert!(parse_xmp_prompt(xmp).is_none());
    }

    #[test]
    fn detects_positive_negative_prompt_chunks() {
        let exif = vec![];
        let chunks = vec![
            png_meta::PngTextChunk {
                kind: png_meta::PngTextKind::Text,
                keyword: "positive_prompt".to_string(),
                text: "(cameltoe:1.1), milf, large breasts".to_string(),
            },
            png_meta::PngTextChunk {
                kind: png_meta::PngTextKind::Text,
                keyword: "negative_prompt".to_string(),
                text: "blurry, low quality".to_string(),
            },
        ];
        let info = detect_ai_prompt(&exif, &chunks, &[]).unwrap();
        assert!(info.prompt.contains("cameltoe"));
        assert_eq!(info.negative_prompt.as_deref(), Some("blurry, low quality"));
    }

    #[test]
    fn png_badge_detects_xmp_and_positive_prompt() {
        // XMP chunk in a real PNG
        let xmp_text = r#"<x:xmpmeta><rdf:RDF><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:description><rdf:Alt><rdf:li xml:lang="x-default">a cat&#xA;Steps: 8</rdf:li></rdf:Alt></dc:description></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("xmp.png");
        std::fs::write(&path, {
            let mut bytes = Vec::new();
            {
                let w = &mut std::io::Cursor::new(&mut bytes);
                let mut enc = png::Encoder::new(w, 2, 2);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                let mut writer = enc.write_header().unwrap();
                writer.write_image_data(&[0u8; 16]).unwrap();
                writer
                    .write_text_chunk(&png::text_metadata::ITXtChunk::new(
                        "XML:com.adobe.xmp".to_string(),
                        xmp_text.to_string(),
                    ))
                    .unwrap();
                writer.finish().unwrap();
            }
            bytes
        })
        .unwrap();
        assert!(has_prompt_fast(path.to_str().unwrap()));

        // positive_prompt keyword alone triggers the badge
        let path2 = dir.path().join("pos.png");
        std::fs::write(&path2, {
            let mut bytes = Vec::new();
            {
                let w = &mut std::io::Cursor::new(&mut bytes);
                let mut enc = png::Encoder::new(w, 2, 2);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                let mut writer = enc.write_header().unwrap();
                writer.write_image_data(&[0u8; 16]).unwrap();
                writer
                    .write_text_chunk(&png::text_metadata::TEXtChunk::new(
                        "positive_prompt".to_string(),
                        "a cat".to_string(),
                    ))
                    .unwrap();
                writer.finish().unwrap();
            }
            bytes
        })
        .unwrap();
        assert!(has_prompt_fast(path2.to_str().unwrap()));
    }

    #[test]
    fn comfyui_primitive_string_links_resolved() {
        // Krea-style graphs: CLIPTextEncode.text → Any Switch →
        // PrimitiveStringMultiline (which stores the text in `value`, not
        // `text`). The sampler links must still resolve through the chain.
        let json = r#"{"78:75":{"class_type":"KSampler","inputs":{"positive":["16",0],"negative":["78:73",0]}},"16":{"class_type":"ConditioningKrea2Rebalance","inputs":{"conditioning":["63",0]}},"63":{"class_type":"RBG_Smart_Seed_Variance","inputs":{"conditioning":["78:15",0]}},"78:15":{"class_type":"CLIPTextEncode","inputs":{"text":["78:72",0]}},"78:72":{"class_type":"Any Switch (rgthree)","inputs":{"any_02":["143",0]}},"143":{"class_type":"PrimitiveStringMultiline","inputs":{"value":"a high-resolution photograph of a cat"}},"78:73":{"class_type":"ConditioningZeroOut","inputs":{"conditioning":["78:15",0]}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert_eq!(info.source, "ComfyUI");
        assert!(info
            .prompt
            .contains("a high-resolution photograph of a cat"));
    }

    #[test]
    fn comfyui_switch_branch_resolved() {
        // Krea2T: KSampler → CLIPTextEncode → ComfySwitchNode whose branches
        // are links — only the on_true branch leads to a text node. The
        // resolver must walk every branch until it finds the text.
        let json = r#"{"30:53":{"class_type":"ClownsharKSampler_Beta","inputs":{"positive":["30:6",0],"negative":["30:13",0]}},"30:6":{"class_type":"CLIPTextEncode","inputs":{"text":["30:28",0]}},"30:28":{"class_type":"ComfySwitchNode","inputs":{"switch":["30:23",0],"on_false":["30:24",0],"on_true":["30:27",0]}},"30:23":{"class_type":"PrimitiveBoolean","inputs":{"value":true}},"30:24":{"class_type":"PrimitiveBoolean","inputs":{"value":false}},"30:27":{"class_type":"PrimitiveStringMultiline","inputs":{"value":"a beautiful young woman on a bed"}},"30:13":{"class_type":"ConditioningZeroOut","inputs":{"conditioning":["30:6",0]}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        // Only the on_true branch of the switch leads to a text node
        assert!(info.prompt.contains("a beautiful young woman on a bed"));
    }

    #[test]
    fn comfyui_zeroed_negative_not_duplicated() {
        // Krea: KSampler.negative → ConditioningZeroOut → same CLIPTextEncode
        // chain as the positive. The resolved negative equals the prompt, so
        // it must be dropped (semantically an empty negative).
        let json = r#"{"78:75":{"class_type":"KSampler","inputs":{"positive":["78:15",0],"negative":["78:73",0]}},"78:73":{"class_type":"ConditioningZeroOut","inputs":{"conditioning":["78:15",0]}},"78:15":{"class_type":"CLIPTextEncode","inputs":{"text":"a red car on a highway"}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert_eq!(info.prompt, "a red car on a highway");
        assert_eq!(info.negative_prompt, None);
    }

    #[test]
    fn sanitize_removes_bare_nan_and_infinity() {
        // Krea graphs emit `is_changed: NaN` / `Infinity` — legal in Python,
        // rejected by serde_json. The sanitizer rewrites them to null while
        // leaving string contents (e.g. "NaN" inside a prompt) untouched.
        let json = r#"{"1":{"class_type":"KSampler","inputs":{"cfg":7.0,"seed":NaN,"is_changed":Infinity,"other":-Infinity,"text":"keep NaN here"}},"2":{"class_type":"CLIPTextEncode","inputs":{"text":"a cat"}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info.prompt.contains("a cat"));
        assert!(info.raw.contains("keep NaN here"));
    }

    #[test]
    fn comfyui_ui_workflow_widgets_values_collected() {
        // A UI workflow graph is shaped `{"id":…,"nodes":[…]}` — the prompt
        // text lives in `widgets_values[0]` of the text-encoding nodes.
        let json = r#"{"id":"abc","revision":0,"last_node_id":13,"nodes":[{"id":13,"type":"ZML_PromptUINode","widgets_values":["(masterpiece, best quality)"]},{"id":12,"type":"KSampler","widgets_values":[42,"euler"]},{"id":11,"type":"CLIPTextEncode","widgets_values":["a red car","anything"]}]}"#;
        let info = parse_comfyui_json("workflow", json).unwrap();
        assert!(info.prompt.contains("(masterpiece, best quality)"));
        assert!(info.prompt.contains("a red car"));
        assert!(info.params.contains("\"nodes\""));
    }

    #[test]
    fn comfyui_string_literal_chain_resolved() {
        // Real `1.png`: KSampler → CLIPTextEncode.text=["97",0] →
        // ImpactSwitch.input1=["96",0] → String Literal (`string` key, not
        // `text`/`value`). The sampler link must resolve through the whole
        // chain to the literal.
        let json = r#"{"9":{"class_type":"KSampler","inputs":{"positive":["4",0],"negative":["10",0]}},"4":{"class_type":"CLIPTextEncode","inputs":{"text":["97",0]}},"97":{"class_type":"ImpactSwitch","inputs":{"input1":["96",0]}},"96":{"class_type":"String Literal","inputs":{"string":"ttai_lalk@v2, lezdom, domination"}},"10":{"class_type":"ConditioningZeroOut","inputs":{"conditioning":["4",0]}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert_eq!(info.source, "ComfyUI");
        assert!(info.prompt.contains("ttai_lalk@v2, lezdom, domination"));
        // Zeroed negative shares the positive chain → dropped
        assert_eq!(info.negative_prompt, None);
    }

    #[test]
    fn comfyui_crystools_json_literal_not_taken_as_prompt() {
        // Crystools "Load image with metadata" writes a String Literal whose
        // value is a JSON `fileinfo` block (starts with `{`). It must not be
        // mistaken for the prompt — and a graph with only that literal yields
        // no prompt at all.
        let json = r#"{"13":{"class_type":"String Literal","inputs":{"string":"{\n  \"fileinfo\": {\"filename\": \"D:/x.png\"}\n}"}},"4":{"class_type":"Metadata extractor [Crystools]","inputs":{"metadata_raw":["1",3]}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info.prompt.is_empty(), "fileinfo JSON is not a prompt");
    }

    #[test]
    fn comfyui_concat_joins_all_branches() {
        // Real `1a2ff84a`: KSampler.positive → ConditioningConcat whose
        // `conditioning_from` (node 48) and `conditioning_to` (node 55, a
        // WeiLinPromptUI storing the text in `positive`) both contribute to
        // the final prompt. All branches must be joined, not just the first
        // reachable one (which truncated the prompt to its prefix).
        let json = r#"{"19":{"class_type":"KSampler","inputs":{"positive":["49",0],"negative":["12",0]}},"49":{"class_type":"ConditioningConcat","inputs":{"conditioning_from":["48",0],"conditioning_to":["55",1]}},"48":{"class_type":"CLIPTextEncode","inputs":{"text":"masterpiece, best quality, score_7."}},"55":{"class_type":"WeiLinPromptUI","inputs":{"positive":"tangxuan, 1girl, solo, long hair, maid"}},"12":{"class_type":"CLIPTextEncode","inputs":{"text":"worst quality, blurry"}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info.prompt.contains("masterpiece, best quality, score_7."));
        assert!(info
            .prompt
            .contains("tangxuan, 1girl, solo, long hair, maid"));
        assert_eq!(
            info.negative_prompt.as_deref(),
            Some("worst quality, blurry")
        );
    }

    #[test]
    fn comfyui_guider_sampler_resolved() {
        // SamplerCustomAdvanced has no positive/negative inputs — the
        // conditioning flows through the `guider` link (DualModelGuider).
        let json = r#"{"360:344":{"class_type":"SamplerCustomAdvanced","inputs":{"guider":["360:353",0],"sampler":["360:351",0],"noise":["360:355",0],"sigmas":["360:354",0],"latent_image":["360:348",0]}},"360:353":{"class_type":"DualModelGuider","inputs":{"positive":["360:346",0],"negative":["360:347",0]}},"360:346":{"class_type":"CLIPTextEncode","inputs":{"text":["362",0]}},"362":{"class_type":"PrimitiveStringMultiline","inputs":{"value":"a red-haired woman in a shower"}},"360:347":{"class_type":"ConditioningZeroOut","inputs":{"conditioning":["360:346",0]}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info.prompt.contains("a red-haired woman in a shower"));
        assert_eq!(info.negative_prompt, None);
    }

    #[test]
    fn comfyui_string_concatenate_operands_joined() {
        // Real `03e66bd5`: KSampler → CLIPTextEncode → StringConcatenate
        // whose operands are plain strings (`string_a`/`string_b`).
        let json = r#"{"163":{"class_type":"KSampler","inputs":{"positive":["6",0],"negative":["190",0]}},"6":{"class_type":"CLIPTextEncode","inputs":{"text":["214",0]}},"214":{"class_type":"StringConcatenate","inputs":{"string_a":"","string_b":"digital art, anime style, silhouetted female character","delimiter":", "}},"190":{"class_type":"ConditioningZeroOut","inputs":{"conditioning":["6",0]}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info
            .prompt
            .contains("digital art, anime style, silhouetted female character"));
    }

    #[test]
    fn comfyui_lora_tag_not_taken_as_prompt() {
        // Real `4d66d213`: the positive chain first meets a Lora Loader whose
        // `text` is a bare `<lora:…>` tag; the real prompt sits deeper in a
        // switch branch. The resolver must prefer the meaningful text.
        let json = r#"{"19":{"class_type":"KSampler","inputs":{"positive":["11",0],"negative":["84",0]}},"11":{"class_type":"CLIPTextEncode","inputs":{"text":["73",0]}},"73":{"class_type":"JoinStrings","inputs":{"delimiter":" ","string1":["72",0],"string2":["81",0]}},"72":{"class_type":"TriggerWord Toggle (LoraManager)","inputs":{"trigger_words":["71",2]}},"71":{"class_type":"Lora Loader (LoraManager)","inputs":{"text":"<lora:Brom_Krea2_v2:1.00>"}},"81":{"class_type":"ComfySwitchNode","inputs":{"switch":false,"on_false":["88",0]}},"88":{"class_type":"DPRandomGenerator","inputs":{"text":"A feral vampire noblewoman reclining inside a decaying palace"}},"84":{"class_type":"ConditioningZeroOut","inputs":{"conditioning":["11",0]}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info.prompt.contains("feral vampire noblewoman"));
        assert!(!info.prompt.contains("<lora:"));
        assert_eq!(info.negative_prompt, None);
    }

    #[test]
    fn comfyui_system_template_filtered() {
        // Krea's TextGenerate embeds its LLM system template in the executed
        // graph; with the user text generated at runtime it must not become
        // the prompt.
        let json = r#"{"1":{"class_type":"KSampler","inputs":{"positive":["2",0]}},"2":{"class_type":"CLIPTextEncode","inputs":{"text":["3",0]}},"3":{"class_type":"TextGenerate","inputs":{"prompt":"You are an expert prompt engineer for text-to-image models. Your task is to expand the user's prompt into a highly effective image-generation prompt."}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info.prompt.is_empty(), "system template is not a prompt");
    }

    #[test]
    fn comfyui_json_literal_prompt_extracted() {
        // Real `1e71cafd`: PrimitiveStringMultiline holds Krea's LLM output
        // as {"error":…,"raw":"```json\n{…}```"} — the prompt is inside.
        let json = r#"{"1":{"class_type":"KSampler","inputs":{"positive":["2",0]}},"2":{"class_type":"CLIPTextEncode","inputs":{"text":["3",0]}},"3":{"class_type":"PrimitiveStringMultiline","inputs":{"value":"{\"error\":\"no_json_found\",\"raw\":\"```json\\n{\\\"aspect_ratio\\\": \\\"4:5\\\",\\\"high_level_description\\\": \\\"A wet, red-haired woman looks up\\\"}```\"}"}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info.prompt.contains("A wet, red-haired woman looks up"));
    }

    #[test]
    fn generation_data_chunk_extracts_prompt() {
        // Real `5dec8a08`: Krea writes the real prompt in `generation_data`
        // (with a trailing NUL); the executed graph holds only lora tags.
        let raw = "{\"models\":[{\"type\":\"LORA\",\"label\":\"v1. 0\"}],\"prompt\":\"Realistic masterpiece. A young Latina woman with tan skin is bound to a wooden beam\",\"width\":768,\"steps\":10}\u{0}";
        let info = parse_generation_data(raw).unwrap();
        assert_eq!(info.source, "Krea");
        assert!(info.prompt.contains("Realistic masterpiece"));
        assert!(info.params.contains("width: 768"));
    }

    #[test]
    fn invokeai_metadata_chunk_extracts_prompt() {
        // Real `3deafb68`: InvokeAI z-image metadata carries the prompt.
        let raw = "{\"generation_mode\":\"z_image_txt2img\",\"positive_prompt\":\"LuisRoyo painting of an intense woman\",\"negative_prompt\":\"blurry\",\"steps\":28,\"model\":{\"key\":\"x\"}}";
        let info = parse_invokeai_metadata("invokeai_metadata", raw).unwrap();
        assert_eq!(info.source, "InvokeAI");
        assert!(info.prompt.contains("LuisRoyo painting"));
        assert_eq!(info.negative_prompt.as_deref(), Some("blurry"));
        assert!(info.params.contains("steps: 28"));
    }

    #[test]
    fn comfyui_text_joiner_joins_branches_with_styler_negative() {
        // Real `5e419914`: TextJoiner joins the quality tags, the style and
        // the extracted description; the negative chain reaches the styler's
        // negative output via `["n", 1]`.
        let json = r#"{"30":{"class_type":"KSampler (Efficient)","inputs":{"positive":["28",0],"negative":["29",0]}},"28":{"class_type":"CLIPTextEncode","inputs":{"text":["4",0]}},"4":{"class_type":"TextJoiner","inputs":{"text1":["2",0],"text2":["3",0]}},"2":{"class_type":"SDXLPromptStyler","inputs":{"text_positive":"masterpiece, best quality, highres","text_negative":"worst quality, low quality, blurry"}},"3":{"class_type":"AnimaStyleExplorer","inputs":{"text":"anime coloring, "}},"29":{"class_type":"CLIPTextEncode","inputs":{"text":["2",1]}}}"#;
        let info = parse_comfyui_json("prompt", json).unwrap();
        assert!(info.prompt.contains("masterpiece, best quality, highres"));
        assert!(info.prompt.contains("anime coloring"));
        assert_eq!(
            info.negative_prompt.as_deref(),
            Some("worst quality, low quality, blurry")
        );
    }

    #[test]
    fn cp1251_comment_decoded() {
        // "Привет, мир" in Windows-1251 (legacy JPEG comment encoding) must
        // decode to text instead of U+FFFD replacement characters.
        let bytes = b"\xCF\xF0\xE8\xE2\xE5\xF2, \xEC\xE8\xF0";
        assert_eq!(decode_legacy_text(bytes), "Привет, мир");
    }

    #[test]
    fn png_files_have_no_jpeg_comments() {
        // Regression: read_jpeg_comments was run on every file; on PNG bytes
        // the JPEG segment walker found coincidental 0xFF 0xFE sequences and
        // surfaced garbage as "JPEG comments".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.png");
        {
            let img = image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 4]));
            img.save(&path).unwrap();
        }
        let meta = get_metadata(path.to_str().unwrap()).unwrap();
        assert_eq!(meta.format, "png");
        assert!(
            meta.comments.is_empty(),
            "PNG must not produce JPEG comments"
        );
    }

    #[test]
    fn exif_reads_only_what_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.jpg");
        {
            let img = image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3]));
            img.save_with_format(&path, image::ImageFormat::Jpeg)
                .unwrap();
        }
        // No EXIF present → empty, not an error
        assert!(read_exif(path.to_str().unwrap()).unwrap().is_empty());
        assert!(get_metadata(path.to_str().unwrap()).unwrap().format == "jpg");
    }
}
