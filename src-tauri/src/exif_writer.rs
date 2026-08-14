//! Minimal EXIF writer: builds a JPEG APP1 segment carrying ComfyUI
//! generation metadata, so a PNG→JPG conversion can embed the workflow the
//! way ComfyUI's own SaveImage does:
//!   - `prompt`   -> EXIF Make             (0x010F) = "Prompt: <JSON>"
//!   - `workflow` -> EXIF ImageDescription (0x010E) = "Workflow: <JSON>"
//!
//! The app's own metadata reader (and ComfyUI's drag-and-drop) reads exactly
//! these tags with these prefixes, so the round trip is guaranteed.

/// Maximum payload of one APP1 segment (2 length bytes are counted in the
/// segment's own length field; 0xFFFF total minus 2).
const APP1_MAX: usize = 65533;

/// Builds a complete APP1 segment payload (including the "Exif\0\0" header)
/// with Make and ImageDescription ASCII values. Returns `Ok(None)` when both
/// values are absent; `Err` when the payload would exceed the JPEG APP1 limit.
pub fn build_exif_app1(
    prompt: Option<&str>,
    workflow: Option<&str>,
) -> Result<Option<Vec<u8>>, String> {
    let mut entries: Vec<(u16, Vec<u8>)> = Vec::new();
    if let Some(p) = prompt {
        entries.push((0x010F, format!("Prompt: {p}\0").into_bytes()));
    }
    if let Some(w) = workflow {
        entries.push((0x010E, format!("Workflow: {w}\0").into_bytes()));
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

    fn exif_make_desc(jpeg: &[u8]) -> (Option<String>, Option<String>) {
        let mut buf = BufReader::new(std::io::Cursor::new(jpeg));
        let exif = exif::Reader::new().read_from_container(&mut buf).unwrap();
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
    fn roundtrips_via_kamadak() {
        let app1 = build_exif_app1(Some("{\"a\":1}"), Some("{\"b\":2}"))
            .unwrap()
            .unwrap();
        let (make, desc) = exif_make_desc(&wrap_as_jpeg(&app1));
        assert_eq!(make.as_deref(), Some("Prompt: {\"a\":1}"));
        assert_eq!(desc.as_deref(), Some("Workflow: {\"b\":2}"));
    }

    #[test]
    fn rejects_oversized_payload() {
        let big = "x".repeat(60_000);
        let err = build_exif_app1(Some(&big), Some(&big)).unwrap_err();
        assert!(err.contains("65533"), "got: {err}");
        // prompt alone (30 KB) fits
        assert!(build_exif_app1(Some(&big), None).unwrap().is_some());
    }

    #[test]
    fn handles_short_inline_values() {
        // "Prompt: a\0" is 9 bytes -> off-table; the short-value inline path
        // is exercised by an even tinier value.
        let app1 = build_exif_app1(Some("a"), None).unwrap().unwrap();
        assert!(app1.starts_with(b"Exif\0\0"));
        assert_eq!(app1.len(), 42); // 6 hdr + 8 tiff + 2 count + 12 entry + 4 next + 9 val + 1 pad
    }

    #[test]
    fn none_when_no_values() {
        assert!(build_exif_app1(None, None).unwrap().is_none());
    }
}
