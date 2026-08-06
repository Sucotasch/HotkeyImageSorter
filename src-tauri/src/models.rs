use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImageInfo {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub extension: String,
    pub modified: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub hotkeys: std::collections::HashMap<String, String>, // Key (A-Z) -> Folder Path
    #[serde(default)]
    pub last_folder: Option<String>,
    #[serde(default)]
    pub last_file: Option<String>,
    #[serde(default)]
    pub sort_by: Option<String>,
    #[serde(default)]
    pub sort_dir: Option<i32>,
    #[serde(default)]
    pub minimize_to_tray: Option<bool>,
}

/// A single metadata key/value pair (EXIF field, PNG text chunk, ...).
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MetadataEntry {
    pub key: String,
    pub value: String,
}

/// A recognized AI-generation prompt found inside an image.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AiPromptInfo {
    /// Detected generator, e.g. "AUTOMATIC1111", "ComfyUI", "InvokeAI".
    pub source: String,
    /// The positive prompt (extracted where the format allows).
    pub prompt: String,
    /// The negative prompt, if the format separates it (A1111).
    pub negative_prompt: Option<String>,
    /// Generation parameters / raw payload (steps, sampler, workflow JSON...).
    pub params: String,
    /// The complete raw text as stored in the file.
    pub raw: String,
}

/// Everything the Ctrl+I metadata viewer needs for one image.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ImageMetadata {
    pub name: String,
    pub path: String,
    pub format: String,
    pub size: u64,
    pub modified: u64,
    pub exif: Vec<MetadataEntry>,
    pub text_chunks: Vec<MetadataEntry>,
    pub comments: Vec<String>,
    pub ai: Option<AiPromptInfo>,
}

/// The kind of operation an undo/redo entry records.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub enum UndoActionType {
    /// A file was moved to another directory.
    Move,
    /// An image was edited (rotate/crop); `new_path` holds the temp backup.
    Edit,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UndoAction {
    pub old_path: String,
    pub new_path: String,
    pub action_type: UndoActionType,
}
