use crate::models::AppConfig;
use directories::ProjectDirs;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

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

/// The default configuration used when no (valid) config file exists.
fn default_config() -> AppConfig {
    AppConfig {
        hotkeys: HashMap::new(),
        last_folder: None,
        last_file: None,
        sort_by: Some("name".to_string()),
        sort_dir: Some(1),
        minimize_to_tray: Some(true),
        convert_replace: Some(false),
        convert_show_report: Some(true),
        context_menu: Some(false),
    }
}

/// Load a config from an explicit path; falls back to defaults on missing/corrupt file.
fn load_config_from(path: &Path) -> AppConfig {
    if let Ok(content) = fs::read_to_string(path) {
        if let Ok(config) = serde_json::from_str::<AppConfig>(&content) {
            return config;
        }
    }
    default_config()
}

/// Save a config to an explicit path, creating parent directories as needed.
fn save_config_to(path: &Path, config: &AppConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let content = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    fs::write(path, content).map_err(|e| e.to_string())
}

pub fn get_config() -> AppConfig {
    load_config_from(&config_path())
}

pub fn save_config(config: AppConfig) -> Result<(), String> {
    save_config_to(&config_path(), &config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = default_config();
        cfg.hotkeys.insert("a".to_string(), "C:/x".to_string());
        cfg.hotkeys.insert("z".to_string(), "D:/y".to_string());
        cfg.sort_by = Some("size".to_string());
        cfg.sort_dir = Some(-1);
        cfg.minimize_to_tray = Some(false);
        cfg.convert_replace = Some(true);
        cfg.convert_show_report = Some(false);
        cfg.context_menu = Some(true);
        cfg.last_folder = Some("C:/images".to_string());

        save_config_to(&path, &cfg).unwrap();
        let loaded = load_config_from(&path);

        assert_eq!(loaded.hotkeys, cfg.hotkeys);
        assert_eq!(loaded.sort_by, cfg.sort_by);
        assert_eq!(loaded.sort_dir, cfg.sort_dir);
        assert_eq!(loaded.minimize_to_tray, cfg.minimize_to_tray);
        assert_eq!(loaded.convert_replace, cfg.convert_replace);
        assert_eq!(loaded.convert_show_report, cfg.convert_show_report);
        assert_eq!(loaded.context_menu, cfg.context_menu);
        assert_eq!(loaded.last_folder, cfg.last_folder);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let loaded = load_config_from(&path);
        assert_eq!(loaded.hotkeys, default_config().hotkeys);
        assert_eq!(loaded.sort_by, default_config().sort_by);
        assert_eq!(loaded.minimize_to_tray, default_config().minimize_to_tray);
    }

    #[test]
    fn load_corrupt_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        let loaded = load_config_from(&path);
        assert_eq!(loaded.hotkeys, default_config().hotkeys);
        assert_eq!(loaded.sort_by, default_config().sort_by);
        assert_eq!(loaded.minimize_to_tray, default_config().minimize_to_tray);
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/nested/dir/config.json");
        let cfg = default_config();
        save_config_to(&path, &cfg).unwrap();
        assert!(path.exists());
    }
}
