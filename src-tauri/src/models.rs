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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UndoAction {
    pub old_path: String,
    pub new_path: String,
    pub action_type: String, // "move", "rotate", "crop"
}
