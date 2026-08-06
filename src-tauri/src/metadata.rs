//! Image metadata extraction for the Ctrl+Q viewer: EXIF, PNG text chunks
//! (AI prompts/workflows), JPEG comments, and heuristic detection/parsing of
//! AI-generation metadata (AUTOMATIC1111, ComfyUI, InvokeAI).

use crate::models::{AiPromptInfo, ImageMetadata, MetadataEntry};
use crate::{jpeg_meta, png_meta};
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
    let comments = read_jpeg_comments(path);

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
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    let be_str = String::from_utf16(&be_units).unwrap_or_default();
    let le_units: Vec<u16> = body[..n * 2]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
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
        .map(|s| String::from_utf8_lossy(&s.data).into_owned())
        .filter(|s| !s.is_empty())
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
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };

    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "png" {
        // PNG: scan raw chunk types only (tEXt/zTXt/iTXt with known keywords).
        // Walk the chunk stream cheaply instead of the full png decode.
        find_png_ai_chunk(&bytes)
    } else {
        // JPEG: look for known prompt markers in COM segments and EXIF fields.
        find_jpeg_prompt_marker(&bytes)
    }
}

/// Scans PNG chunk headers for text chunks whose keyword looks like AI
/// metadata (parameters / prompt / workflow / sd-metadata / metadata). Chunk
/// types and keywords are visible without inflating IDAT.
fn find_png_ai_chunk(bytes: &[u8]) -> bool {
    let mut i = 8usize; // skip 8-byte signature
    while i + 8 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        let ctype = &bytes[i + 4..i + 8];
        match ctype {
            b"tEXt" | b"zTXt" | b"iTXt" => {
                // keyword runs up to the NUL separator
                let data_start = i + 8;
                let data_end = data_start.saturating_add(len).min(bytes.len());
                if let Some(nul) = bytes[data_start..data_end].iter().position(|&b| b == 0) {
                    let keyword = &bytes[data_start..data_start + nul];
                    if is_ai_keyword(keyword) {
                        return true;
                    }
                }
            }
            _ => {}
        }
        i = i + 12 + len; // 8 header + 4 CRC
        if i > bytes.len() {
            break;
        }
    }
    false
}

fn is_ai_keyword(keyword: &[u8]) -> bool {
    matches!(
        keyword,
        b"parameters" | b"prompt" | b"workflow" | b"sd-metadata" | b"sd_metadata" | b"metadata"
    )
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

/// Detect and parse AI-generation prompts stored in the image.
fn detect_ai_prompt(
    exif: &[MetadataEntry],
    text_chunks: &[png_meta::PngTextChunk],
    comments: &[String],
) -> Option<AiPromptInfo> {
    // PNG: A1111/Forge store the full generation info in the `parameters`
    // chunk; ComfyUI stores `prompt` (the executed graph) and `workflow`
    // (the full UI graph) as JSON; InvokeAI uses `sd-metadata`.
    for c in text_chunks {
        match c.keyword.as_str() {
            "parameters" => {
                if let Some(info) = parse_a1111_parameters(&c.text) {
                    return Some(info);
                }
            }
            "prompt" | "workflow" => {
                if let Some(info) = parse_comfyui_json(&c.keyword, &c.text) {
                    return Some(info);
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
            _ => {}
        }
    }

    // JPEG: A1111-era web UIs often put the prompt in a COM segment; some
    // tools use EXIF UserComment or ImageDescription.
    for comment in comments {
        if looks_like_a1111(comment) {
            if let Some(info) = parse_a1111_parameters(comment) {
                return Some(info);
            }
        }
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

/// Reads the prompt text from a ComfyUI text-encoding node. The input key
/// depends on the model family:
/// - `CLIPTextEncode` / `CLIPTextEncodeFlux` / `smZ CLIPTextEncode` → `inputs.text`
/// - Qwen-Image nodes (`TextEncodeQwenImage*`) → `inputs.prompt`
fn comfyui_node_text(node: &serde_json::Value) -> Option<String> {
    let inputs = node.get("inputs").and_then(|i| i.as_object())?;
    for key in ["text", "prompt"] {
        if let Some(t) = inputs.get(key).and_then(|t| t.as_str()) {
            let t = t.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Resolves a ComfyUI link value (`["node_id", output]`) to the referenced
/// node's prompt text.
fn comfyui_link_text(
    link: &serde_json::Value,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let id = link.as_array()?.first()?.as_str()?;
    comfyui_node_text(obj.get(id)?)
}

/// Parses a ComfyUI `prompt`/`workflow` JSON and extracts the prompt texts.
///
/// When the graph is an *executed prompt* (the format generated by ComfyUI
/// itself and civitai AIR), the KSampler node's `positive`/`negative` inputs
/// reference the text-encoding nodes — those are split into the prompt and
/// negative prompt respectively. Without a sampler link (e.g. a workflow UI
/// graph), every text-encoding node is collected into the prompt block.
fn parse_comfyui_json(keyword: &str, json: &str) -> Option<AiPromptInfo> {
    if !json.trim_start().starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;

    // Executed-prompt graphs: follow the sampler's positive/negative links.
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
            positive = inputs
                .get("positive")
                .and_then(|l| comfyui_link_text(l, obj));
        }
        if negative.is_none() {
            negative = inputs
                .get("negative")
                .and_then(|l| comfyui_link_text(l, obj));
        }
    }
    if positive.is_some() || negative.is_some() {
        return Some(AiPromptInfo {
            source: "ComfyUI".to_string(),
            prompt: positive.unwrap_or_default(),
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
    let prompts: Vec<String> = obj.values().filter_map(comfyui_node_text).collect();
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
        // A1111-style trailing parameters chunk
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
        assert!(find_png_ai_chunk(&bytes));
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
        assert!(!find_png_ai_chunk(&bytes));
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
