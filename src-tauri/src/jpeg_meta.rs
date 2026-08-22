//! Raw JPEG segment manipulation: extract APPn/COM segments (EXIF, XMP, ICC,
//! Photoshop, comments) before re-encoding and splice the ones the re-encoder
//! dropped back in, so metadata (including AI-generation prompts stored in
//! EXIF `UserComment`/`ImageDescription` or a COM segment) survives edits.

/// A raw JPEG segment: `marker` is the second byte (e.g. 0xE1 for APP1,
/// 0xE2 for APP2, 0xFE for COM); `data` is the payload WITHOUT the two length
/// bytes (the bytes the length field counts beyond itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JpegSegment {
    pub marker: u8,
    pub data: Vec<u8>,
}

impl JpegSegment {
    /// The 4-byte signature (if any) used for deduplication, e.g. "Exif\0",
    /// "http://ns.adobe.com/xap/1.0/", "ICC_PROFILE\0".
    fn sig(&self) -> [u8; 4] {
        let mut s = [0u8; 4];
        let n = self.data.len().min(4);
        s[..n].copy_from_slice(&self.data[..n]);
        s
    }
}

/// Walks the JPEG marker stream and returns every APPn / COM segment before
/// the SOS (start of scan) marker. Stops at SOS/EOI — everything after is
/// entropy-coded data, not a segment table.
pub fn extract_segments(jpeg: &[u8]) -> Vec<JpegSegment> {
    let mut out = Vec::new();
    let mut i = 0usize;

    // Skip a leading SOI if present
    if jpeg.len() >= 2 && jpeg[0] == 0xFF && jpeg[1] == 0xD8 {
        i = 2;
    }

    while i + 1 < jpeg.len() {
        // Find a marker (skip 0xFF fill bytes)
        while i < jpeg.len() && jpeg[i] == 0xFF {
            i += 1;
        }
        if i >= jpeg.len() {
            break;
        }
        let marker = jpeg[i];
        i += 1;

        match marker {
            0xD8 => {}               // SOI
            0x01 | 0xD0..=0xD7 => {} // standalone markers (TEM, RSTn)
            0xD9 => break,           // EOI
            0xDA => break,           // SOS — entropy data follows
            0xE0..=0xEF | 0xFE => {
                // APPn / COM: 2-byte big-endian length (includes itself)
                if i + 1 >= jpeg.len() {
                    break;
                }
                let len = ((jpeg[i] as usize) << 8) | jpeg[i + 1] as usize;
                if len < 2 || i + len > jpeg.len() {
                    break;
                }
                let data = jpeg[i + 2..i + len].to_vec();
                out.push(JpegSegment { marker, data });
                i += len;
            }
            _ => {
                // Other segments also carry a length (DQT 0xDB, DHT 0xC4, SOF…)
                if i + 1 >= jpeg.len() {
                    break;
                }
                let len = ((jpeg[i] as usize) << 8) | jpeg[i + 1] as usize;
                if len < 2 || i + len > jpeg.len() {
                    break;
                }
                i += len;
            }
        }
    }
    out
}

/// Copies the given segments into `jpeg` right after its SOI, skipping any
/// segment already present in the file. This is the safety net for the
/// re-encode fallback: `image`-re-encoded JPEGs lose all APPn/COM, so the
/// originals are re-inserted; if turbojpeg already preserved some of them,
/// dedup prevents doubles.
///
/// Dedup rules: EXIF APP1 is single-instance by spec, so signature dedup
/// catches even a byte-different re-serialized copy. Everything else may
/// legitimately repeat (multi-segment ICC APP2 chains, several COM comments,
/// standard + extended XMP APP1) and is only dropped when byte-identical.
pub fn merge_missing_segments(jpeg: &[u8], originals: &[JpegSegment]) -> Vec<u8> {
    let existing = extract_segments(jpeg);

    if !jpeg.starts_with(&[0xFF, 0xD8]) {
        // Not a valid JPEG header — leave untouched
        return jpeg.to_vec();
    }
    let mut out = Vec::with_capacity(jpeg.len() + 1024);
    out.extend_from_slice(&[0xFF, 0xD8]);
    // Insert missing segments between SOI and the rest
    for seg in originals {
        let dup = if seg.marker == 0xE1 && seg.data.starts_with(b"Exif") {
            existing
                .iter()
                .any(|e| e.marker == seg.marker && e.sig() == seg.sig())
        } else {
            existing
                .iter()
                .any(|e| e.marker == seg.marker && e.data == seg.data)
        };
        if !dup {
            push_segment(&mut out, seg);
        }
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn push_segment(out: &mut Vec<u8>, seg: &JpegSegment) {
    let len = seg.data.len() + 2;
    out.push(0xFF);
    out.push(seg.marker);
    out.push((len >> 8) as u8);
    out.push((len & 0xFF) as u8);
    out.extend_from_slice(&seg.data);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic JPEG byte stream with the given segments before SOS.
    fn synth_jpeg(segments: &[JpegSegment]) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8]; // SOI
        for s in segments {
            push_segment(&mut v, s);
        }
        v.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 1, 2, 3, 4, 5, 6]); // SOS + "data"
        v.extend_from_slice(&[0xFF, 0xD9]); // EOI
        v
    }

    #[test]
    fn extract_finds_app1_app2_com() {
        let exif = JpegSegment {
            marker: 0xE1,
            data: b"Exif\0rest".to_vec(),
        };
        let icc = JpegSegment {
            marker: 0xE2,
            data: b"ICC_PROFILE\0x".to_vec(),
        };
        let com = JpegSegment {
            marker: 0xFE,
            data: b"hello".to_vec(),
        };
        let jpeg = synth_jpeg(&[exif.clone(), icc.clone(), com.clone()]);

        let got = extract_segments(&jpeg);
        assert_eq!(got.len(), 3);
        assert!(got.contains(&exif));
        assert!(got.contains(&icc));
        assert!(got.contains(&com));
    }

    #[test]
    fn extract_stops_at_sos() {
        let exif = JpegSegment {
            marker: 0xE1,
            data: b"Exif\0".to_vec(),
        };
        let jpeg = synth_jpeg(std::slice::from_ref(&exif));
        // Even with extra bytes after SOS that look like markers, nothing is
        // parsed beyond SOS.
        let got = extract_segments(&jpeg);
        assert_eq!(got, vec![exif]);
    }

    #[test]
    fn merge_does_not_duplicate_existing_segments() {
        let exif = JpegSegment {
            marker: 0xE1,
            data: b"Exif\0a".to_vec(),
        };
        let icc = JpegSegment {
            marker: 0xE2,
            data: b"ICC_PROFILE\0".to_vec(),
        };

        // Output already has EXIF (kept by turbojpeg), missing ICC
        let out_with_exif = synth_jpeg(std::slice::from_ref(&exif));
        let merged = merge_missing_segments(&out_with_exif, &[exif.clone(), icc.clone()]);

        let got = extract_segments(&merged);
        let exif_count = got.iter().filter(|s| s.marker == 0xE1).count();
        let icc_count = got.iter().filter(|s| s.marker == 0xE2).count();
        assert_eq!(exif_count, 1, "existing EXIF must not be duplicated");
        assert_eq!(icc_count, 1, "missing ICC must be added");
    }

    #[test]
    fn merge_into_bare_jpeg_adds_everything() {
        // image-re-encoded JPEG: no APPn/COM at all
        let bare = vec![0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x08, 9, 9, 9, 9];
        let exif = JpegSegment {
            marker: 0xE1,
            data: b"Exif\0z".to_vec(),
        };
        let merged = merge_missing_segments(&bare, std::slice::from_ref(&exif));
        let got = extract_segments(&merged);
        assert_eq!(got, vec![exif]);
        // SOS payload must be intact after the inserted segment
        assert!(
            merged.ends_with(&[9, 9, 9, 9]) || merged[merged.len() - 4..].iter().all(|&b| b == 9)
        );
    }

    #[test]
    fn merge_keeps_all_parts_of_a_multi_segment_icc_profile() {
        // Large ICC profiles are split across several APP2 segments that all
        // share the "ICC_PROFILE\0" signature — signature dedup would keep
        // only the first part and corrupt the profile.
        let part1 = JpegSegment {
            marker: 0xE2,
            data: b"ICC_PROFILE\0\x01\x02aaaa".to_vec(),
        };
        let part2 = JpegSegment {
            marker: 0xE2,
            data: b"ICC_PROFILE\0\x02\x01bbbb".to_vec(),
        };
        let bare = vec![0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x08, 9, 9, 9, 9];
        let merged = merge_missing_segments(&bare, &[part1.clone(), part2.clone()]);
        let got = extract_segments(&merged);
        assert!(got.contains(&part1), "first ICC part must survive");
        assert!(got.contains(&part2), "second ICC part must survive");
    }

    #[test]
    fn merge_drops_only_byte_identical_com_duplicates() {
        // Two distinct COM comments with the same 4-byte prefix must both
        // survive; an exact duplicate of one is dropped.
        let com1 = JpegSegment {
            marker: 0xFE,
            data: b"prompt A".to_vec(),
        };
        let com2 = JpegSegment {
            marker: 0xFE,
            data: b"prompt B".to_vec(),
        };
        let with_com1 = synth_jpeg(std::slice::from_ref(&com1));
        let merged = merge_missing_segments(&with_com1, &[com1.clone(), com2.clone()]);
        let got = extract_segments(&merged);
        assert_eq!(
            got.iter().filter(|s| s.marker == 0xFE).count(),
            2,
            "distinct comments kept, exact duplicate dropped"
        );
        assert!(got.contains(&com2));
    }
}
