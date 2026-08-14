//! Minimal EXIF writer: builds a JPEG APP1 segment carrying ComfyUI
//! generation metadata, so a PNG→JPG conversion can embed the workflow the
//! way ComfyUI's own SaveImage does. Real ComfyUI JPEGs store the two JSON
//! payloads **minified** (whitespace stripped) to fit the 65533-byte APP1
//! limit, in these tags:
//!   - `workflow` -> EXIF Make  (0x010F) = "workflow:<JSON>"
//!   - `prompt`   -> EXIF Model (0x0110) = "prompt:<JSON>"
//!
//! Minification mirrors ComfyUI byte-for-byte, so a PNG whose pretty-printed
//! graph exceeds the APP1 limit still converts (ComfyUI's own JPGs are
//! exactly this: the same graph, compacted). The app's metadata reader scans
//! Make/Model/ImageDescription/UserComment case-insensitively, so the round
//! trip is guaranteed.

/// Maximum payload of one APP1 segment (2 length bytes are counted in the
/// segment's own length field; 0xFFFF total minus 2).
const APP1_MAX: usize = 65533;

/// Replaces bare `NaN` / `Infinity` / `-Infinity` tokens with `null`,
/// exactly as ComfyUI's own `parseJsonWithNonFinite` does when it loads its
/// files (Python's `json.dumps` emits those literals, which are not valid
/// JSON — a plain `JSON.parse` like the one in the Image-Saver drag-drop
/// plugin rejects them). Tokens inside quoted strings are left untouched.
pub fn sanitize_non_finite(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut in_string = false;
    let mut esc = false;
    // ComfyUI's regex: `(?<![\w.-])(-?Infinity|NaN)(?![\w.])`
    let token_start = |i: usize| {
        i == 0
            || !(bytes[i - 1].is_ascii_alphanumeric()
                || bytes[i - 1] == b'_'
                || bytes[i - 1] == b'.'
                || bytes[i - 1] == b'-')
    };
    let token_end = |i: usize| {
        i >= bytes.len()
            || !(bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
    };
    while i < bytes.len() {
        if in_string {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += ch.len_utf8();
            continue;
        }
        match bytes[i] {
            b'"' => {
                in_string = true;
                out.push('"');
                i += 1;
            }
            b'N'
                if bytes[i..].starts_with(b"NaN")
                    && token_start(i)
                    && token_end(i + 3) =>
            {
                out.push_str("null");
                i += 3;
            }
            b'I'
                if bytes[i..].starts_with(b"Infinity")
                    && token_start(i)
                    && token_end(i + 8) =>
            {
                out.push_str("null");
                i += 8;
            }
            b'-'
                if bytes.get(i + 1..).is_some_and(|r| r.starts_with(b"Infinity"))
                    && token_start(i)
                    && token_end(i + 9) =>
            {
                out.push_str("null");
                i += 9;
            }
            _ => {
                let ch = s[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// Minifies a JSON document (parse + compact re-serialize), or `None` when
/// the input is not valid JSON (some writers put plain text in these
/// chunks). Bare `NaN`/`Infinity` tokens are sanitised to `null` first, so
/// ComfyUI graphs (which Python emits with those literals) become standard
/// JSON that any reader — including Image-Saver's plain `JSON.parse` —
/// accepts.
pub fn try_minify_json(s: &str) -> Option<String> {
    let sanitized = sanitize_non_finite(s);
    let v: serde_json::Value = serde_json::from_str(&sanitized).ok()?;
    serde_json::to_string(&v).ok()
}

/// Minifies a JSON document; non-JSON content is returned unchanged.
pub fn minify_json(s: &str) -> String {
    try_minify_json(s).unwrap_or_else(|| s.to_string())
}

/// Builds a complete APP1 segment payload (including the "Exif\0\0" header)
/// with Make and Model ASCII values in ComfyUI's own layout. Returns
/// `Ok(None)` when both values are absent; `Err` when the payload would
/// exceed the JPEG APP1 limit.
pub fn build_exif_app1(
    prompt: Option<&str>,
    workflow: Option<&str>,
) -> Result<Option<Vec<u8>>, String> {
    let mut entries: Vec<(u16, Vec<u8>)> = Vec::new();
    if let Some(w) = workflow {
        entries.push((0x010F, format!("workflow:{}\0", minify_json(w)).into_bytes()));
    }
    if let Some(p) = prompt {
        entries.push((0x0110, format!("prompt:{}\0", minify_json(p)).into_bytes()));
    }
    if entries.is_empty() {
        return Ok(None);
    }

    let n = entries.len();
    // TIFF header (8) + entry count (2) + entries (12 each) + next-IFD ptr (4)
    let ifd_end = 8 + 2 + n * 12 + 4;
    let mut total = 6 + ifd_end; // "Exif\0\0" + TIFF
    for (_, v) in &entries {
        total += v.len() + (v.len() & 1); // pad each off-table value to even
    }
    if total > APP1_MAX {
        return Err(format!(
            "EXIF APP1 payload would be {total} bytes (JPEG limit {APP1_MAX})"
        ));
    }

    let mut buf: Vec<u8> = Vec::with_capacity(total);
    buf.extend_from_slice(b"Exif\0\0");
    buf.extend_from_slice(b"II"); // little-endian
    buf.extend_from_slice(&42u16.to_le_bytes());
    buf.extend_from_slice(&8u32.to_le_bytes()); // IFD0 offset
    buf.extend_from_slice(&(n as u16).to_le_bytes());

    let mut value_off = ifd_end;
    let mut value_bytes: Vec<&[u8]> = Vec::new();
    for (tag, v) in &entries {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes()); // ASCII
        buf.extend_from_slice(&(v.len() as u32).to_le_bytes());
        if v.len() <= 4 {
            let mut inline = [0u8; 4];
            inline[..v.len()].copy_from_slice(v);
            buf.extend_from_slice(&inline);
        } else {
            buf.extend_from_slice(&(value_off as u32).to_le_bytes());
            value_off += v.len() + (v.len() & 1);
            value_bytes.push(v);
        }
    }
    buf.extend_from_slice(&0u32.to_le_bytes()); // next IFD = none
    for v in value_bytes {
        buf.extend_from_slice(v);
        if v.len() & 1 == 1 {
            buf.push(0);
        }
    }
    Ok(Some(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    /// Wraps an APP1 payload in a minimal bare JPEG so kamadak-exif's
    /// container reader can find it (mirrors the real splicing in
    /// jpeg_meta::merge_missing_segments).
    fn wrap_as_jpeg(app1: &[u8]) -> Vec<u8> {
        let mut jpeg = vec![0xFF, 0xD8];
        let len = app1.len() + 2;
        jpeg.extend_from_slice(&[0xFF, 0xE1, (len >> 8) as u8, (len & 0xFF) as u8]);
        jpeg.extend_from_slice(app1);
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 1, 2, 3, 4, 5, 6, 0xFF, 0xD9]);
        jpeg
    }

    /// Reads Make/Model from a JPEG the way the app's metadata reader does
    /// (ComfyUI's own JPG layout: Make holds the workflow, Model the prompt).
    fn exif_make_model(jpeg: &[u8]) -> (Option<String>, Option<String>) {
        let mut buf = BufReader::new(std::io::Cursor::new(jpeg));
        let exif = exif::Reader::new().read_from_container(&mut buf).unwrap();
        let mut make = None;
        let mut model = None;
        for f in exif.fields() {
            if f.tag == exif::Tag::Make {
                if let exif::Value::Ascii(parts) = &f.value {
                    make = Some(String::from_utf8_lossy(&parts[0]).into_owned());
                }
            }
            if f.tag == exif::Tag::Model {
                if let exif::Value::Ascii(parts) = &f.value {
                    model = Some(String::from_utf8_lossy(&parts[0]).into_owned());
                }
            }
        }
        (make, model)
    }

    #[test]
    fn roundtrips_via_kamadak() {
        let app1 = build_exif_app1(Some("{\"a\":1}"), Some("{\"b\":2}"))
            .unwrap()
            .unwrap();
        let (make, model) = exif_make_model(&wrap_as_jpeg(&app1));
        assert_eq!(make.as_deref(), Some("workflow:{\"b\":2}"));
        assert_eq!(model.as_deref(), Some("prompt:{\"a\":1}"));
    }

    #[test]
    fn minifies_pretty_json_before_embedding() {
        // ComfyUI PNG chunks are pretty-printed; the JPG stores the compact
        // form (as ComfyUI's own SaveImage does).
        let app1 = build_exif_app1(
            Some("{\n  \"text\": \"masterpiece, dog\"\n}"),
            Some("{\n  \"id\": \"abc\",\n  \"nodes\": []\n}"),
        )
        .unwrap()
        .unwrap();
        let (make, model) = exif_make_model(&wrap_as_jpeg(&app1));
        assert_eq!(
            make.as_deref(),
            Some("workflow:{\"id\":\"abc\",\"nodes\":[]}")
        );
        assert_eq!(model.as_deref(), Some("prompt:{\"text\":\"masterpiece, dog\"}"));
    }

    #[test]
    fn sanitizes_non_finite_tokens_like_comfyui() {
        // ComfyUI graphs carry bare NaN/Infinity (Python json.dumps). They
        // must become `null` — the exact lossy coercion ComfyUI itself does
        // in parseJsonWithNonFinite — so plain JSON.parse readers (e.g. the
        // Image-Saver drag-drop plugin) accept the value.
        let app1 = build_exif_app1(
            Some("{\"seed\":[NaN],\"pos\":[-Infinity],\"txt\":\"NaN is a string\",\"n\":1.5}"),
            None,
        )
        .unwrap()
        .unwrap();
        let (_, model) = exif_make_model(&wrap_as_jpeg(&app1));
        let model = model.unwrap();
        assert!(model.starts_with("prompt:{"), "got: {model:.60}");
        // It must parse with a strict JSON.parse (what the plugin uses), and
        // the tokens must have become null — exactly ComfyUI's coercion.
        let parsed: serde_json::Value = serde_json::from_str(&model["prompt:".len()..]).unwrap();
        assert!(parsed["seed"][0].is_null());
        assert!(parsed["pos"][0].is_null());
        assert_eq!(parsed["txt"], "NaN is a string");
        assert_eq!(parsed["n"], 1.5);
    }

    #[test]
    fn keeps_non_json_values_raw() {
        // Not valid JSON -> embedded unchanged, not rejected.
        let app1 = build_exif_app1(None, Some("not json at all")).unwrap().unwrap();
        let (make, model) = exif_make_model(&wrap_as_jpeg(&app1));
        assert_eq!(make.as_deref(), Some("workflow:not json at all"));
        assert_eq!(model, None);
    }

    #[test]
    fn rejects_oversized_payload() {
        // "x".repeat is not valid JSON, so minification cannot help.
        let big = "x".repeat(60_000);
        let err = build_exif_app1(Some(&big), Some(&big)).unwrap_err();
        assert!(err.contains("65533"), "got: {err}");
        // workflow alone (30 KB) fits
        assert!(build_exif_app1(None, Some(&big)).unwrap().is_some());
    }

    #[test]
    fn minification_makes_oversized_pretty_json_fit() {
        // A whitespace-heavy document whose raw size exceeds the APP1 limit
        // must fit once compacted (the real-world ComfyUI case: pretty-
        // printed PNG chunks, minified graphs in the JPG).
        let mut pretty = String::from("{\n  \"nodes\": [");
        for i in 0..2000 {
            pretty.push_str(&format!(
                "\n    {{\n      \"id\": {i},\n      \"x\": {i},\n      \"y\": 0\n    }},"
            ));
        }
        // strip the trailing comma, then close the array
        pretty.truncate(pretty.len() - 1);
        pretty.push_str("\n  ]\n}");
        assert!(pretty.len() > APP1_MAX, "fixture must exceed APP1 limit raw");
        assert!(
            serde_json::to_string(&serde_json::from_str::<serde_json::Value>(&pretty).unwrap())
                .unwrap()
                .len()
                < 62_000,
            "fixture must fit once minified"
        );
        let app1 = build_exif_app1(None, Some(&pretty)).unwrap().unwrap();
        assert!(app1.len() < APP1_MAX + 12);
        let (make, _) = exif_make_model(&wrap_as_jpeg(&app1));
        let body = make.unwrap();
        assert!(body.starts_with("workflow:{"));
        // The embedded JSON must be valid and equal to the original graph.
        let parsed: serde_json::Value = serde_json::from_str(&body["workflow:".len()..]).unwrap();
        let expected: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn handles_short_inline_values() {
        // "prompt:a\0" is 9 bytes (7+1+1) -> off-table, padded to 10; the
        // short-value inline path is exercised by an even tinier value.
        let app1 = build_exif_app1(Some("a"), None).unwrap().unwrap();
        assert!(app1.starts_with(b"Exif\0\0"));
        assert_eq!(app1.len(), 42); // 6 hdr + 8 tiff + 2 count + 12 entry + 4 next + 9 val + 1 pad
    }

    #[test]
    fn none_when_no_values() {
        assert!(build_exif_app1(None, None).unwrap().is_none());
    }
}
