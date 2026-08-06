//! Image metadata extraction for the Ctrl+I viewer: EXIF, PNG text chunks
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
        .unwrap_or_else(|_| extension(path).to_string());

    let exif = read_exif(path)?;
    let text_chunks = png_meta::extract_text_chunks(path).unwrap_or_default();
    let comments = read_jpeg_comments(path);

    let ai = detect_ai_prompt(&exif, &text_chunks, &comments);

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
        exif,
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
        Err(_) => return Ok(Vec::new()),
    };

    let mut out = Vec::new();
    for f in exif.fields() {
        let value = f.display_value().to_string();
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
    let user_comment = exif_lookup("UserComment").filter(|v| looks_like_a1111(v));
    if let Some(uc) = user_comment {
        if let Some(info) = parse_a1111_parameters(&uc) {
            return Some(info);
        }
    }
    let description = exif_lookup("ImageDescription");
    if let Some(desc) = description {
        if looks_like_a1111(&desc) {
            if let Some(info) = parse_a1111_parameters(&desc) {
                return Some(info);
            }
        }
    }

    None
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

/// Parses a ComfyUI `prompt`/`workflow` JSON: collects the `text` fields of
/// every CLIP text-encoding node (the actual prompts) and keeps the JSON as
/// the raw payload.
fn parse_comfyui_json(keyword: &str, json: &str) -> Option<AiPromptInfo> {
    if !json.trim_start().starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;

    // Collect all nodes whose inputs contain a "text" string (CLIPTextEncode,
    // CLIPTextEncodeFlux, etc.).
    let mut prompts: Vec<String> = Vec::new();
    for (_, node) in obj {
        if let Some(inputs) = node.get("inputs").and_then(|i| i.as_object()) {
            if let Some(text) = inputs.get("text").and_then(|t| t.as_str()) {
                if !text.trim().is_empty() {
                    prompts.push(text.trim().to_string());
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
    fn jpeg_comment_detected() {
        let exif = vec![];
        let chunks = vec![];
        let comments = vec!["Steps: 20, Sampler: DPM++ 2M, Seed: 42".to_string()];
        let info = detect_ai_prompt(&exif, &chunks, &comments).unwrap();
        assert_eq!(info.source, "AUTOMATIC1111");
        assert!(info.params.contains("Steps: 20"));
    }

    /// `read_exif` must not error on files without EXIF and must find a known
    /// field on a synthetic JPEG that carries one.
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
