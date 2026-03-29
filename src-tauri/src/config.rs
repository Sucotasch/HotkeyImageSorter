use crate::models::AppConfig;
use std::fs;
use std::collections::HashMap;
use std::path::PathBuf;
use directories::ProjectDirs;

/// Returns the config file path.
/// Priority: <exe_dir>/config.json (portable) → AppData/config.json
fn config_path() -> PathBuf {
    // Try portable: config.json next to the exe
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let portable = exe_dir.join("config.json");
            // Use portable mode if the file already exists there,
            // OR if the directory is writable and not a system path
            // (simple heuristic: not under Program Files)
            let dir_str = exe_dir.to_string_lossy().to_lowercase();
            let in_program_files = dir_str.contains("program files");
            if portable.exists() || (!in_program_files) {
                return portable;
            }
        }
    }
    // Fallback to AppData
    if let Some(proj_dirs) = ProjectDirs::from("com", "imagesorter", "app") {
        return proj_dirs.config_dir().join("config.json");
    }
    PathBuf::from("config.json")
}

pub fn get_config() -> AppConfig {
    let path = config_path();
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(config) = serde_json::from_str::<AppConfig>(&content) {
            return config;
        }
    }
    AppConfig {
        hotkeys: HashMap::new(),
        last_folder: None,
        last_file: None,
        sort_by: Some("name".to_string()),
        sort_dir: Some(1),
        minimize_to_tray: Some(true),
    }
}

pub fn save_config(config: AppConfig) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let content = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    fs::write(&path, content).map_err(|e| e.to_string())
}
