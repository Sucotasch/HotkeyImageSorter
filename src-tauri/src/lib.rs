mod backup;
mod config;
mod file_ops;
mod image_proc;
mod models;

use crate::models::{AppConfig, ImageInfo, UndoAction, UndoActionType};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Manager;
use tauri::State;

/// Maximum number of undo (and redo) batches kept in memory.
const MAX_HISTORY: usize = 10;

struct AppState {
    history: Mutex<Vec<Vec<UndoAction>>>,
    redo_history: Mutex<Vec<Vec<UndoAction>>>,
}

/// Records a completed operation batch: pushes to history, truncates the oldest
/// batch (deleting its edit backups) and invalidates any pending redo actions.
fn push_history(state: &AppState, batch: Vec<UndoAction>) {
    let mut history = state.history.lock().unwrap();
    history.push(batch);
    if history.len() > MAX_HISTORY {
        let removed_batch = history.remove(0);
        for removed in removed_batch {
            if removed.action_type == UndoActionType::Edit {
                let _ = std::fs::remove_file(&removed.new_path);
            }
        }
    }
    let mut redo = state.redo_history.lock().unwrap();
    for action in redo.drain(..).flatten() {
        if action.action_type == UndoActionType::Edit {
            let _ = std::fs::remove_file(&action.new_path);
        }
    }
}

/// Truncates `redo` to MAX_HISTORY, deleting edit backups of dropped batches.
fn cap_redo(redo: &mut Vec<Vec<UndoAction>>) {
    while redo.len() > MAX_HISTORY {
        let removed_batch = redo.remove(0);
        for removed in removed_batch {
            if removed.action_type == UndoActionType::Edit {
                let _ = std::fs::remove_file(&removed.new_path);
            }
        }
    }
}

#[tauri::command]
async fn list_files(dir: String) -> Result<Vec<ImageInfo>, String> {
    if !std::path::Path::new(&dir).is_dir() {
        return Err(format!("Folder not found or not a directory: {dir}"));
    }
    Ok(file_ops::list_images(&dir))
}

#[tauri::command]
async fn move_images(
    sources: Vec<String>,
    target_dir: String,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let mut batch = Vec::new();
    let mut new_paths = Vec::new();
    let mut failures = Vec::new();

    for source in sources {
        match file_ops::move_image(&source, &target_dir) {
            Ok(new_path) => {
                batch.push(UndoAction {
                    old_path: source,
                    new_path: new_path.clone(),
                    action_type: UndoActionType::Move,
                });
                new_paths.push(new_path);
            }
            Err(e) => failures.push(format!("{source}: {e}")),
        }
    }

    // Successful moves must remain undoable even when some files failed.
    if !batch.is_empty() {
        push_history(&state, batch);
    }

    if !failures.is_empty() {
        return Err(format!(
            "Failed to move some files:\n{}",
            failures.join("\n")
        ));
    }

    Ok(new_paths)
}

/// Undoes the most recent batch. Returns `(restored_path, first_error)`.
/// The batch is moved to `redo` even on partial failure so no action is lost.
fn undo_batch(
    history: &mut Vec<Vec<UndoAction>>,
    redo: &mut Vec<Vec<UndoAction>>,
) -> (Option<String>, Option<String>) {
    let Some(mut last_batch) = history.pop() else {
        return (None, None);
    };
    let mut redo_batch = Vec::new();
    let mut restored: Option<String> = None;
    let mut failure: Option<String> = None;

    // Reverse iterate to undo in opposite order
    while let Some(action) = last_batch.pop() {
        match action.action_type {
            UndoActionType::Move => {
                let ok = file_ops::rename_with_retry(
                    std::path::Path::new(&action.new_path),
                    std::path::Path::new(&action.old_path),
                )
                .is_ok();
                if ok {
                    restored = Some(action.old_path.clone());
                } else if failure.is_none() {
                    failure = Some(format!("Failed to restore {}", action.old_path));
                }
                redo_batch.push(action);
            }
            UndoActionType::Edit => {
                if swap_edit(&action).is_ok() {
                    restored = Some(action.old_path.clone());
                } else if failure.is_none() {
                    failure = Some(format!("Failed to undo edit on {}", action.old_path));
                }
                redo_batch.push(action);
            }
        }
    }
    redo_batch.reverse(); // restore forward direction for redo
    redo.push(redo_batch);
    cap_redo(redo);

    (restored, failure)
}

/// Redoes the most recent undone batch. Returns `(restored_path, first_error)`.
/// The batch is moved back to `history` even on partial failure.
fn redo_batch(
    history: &mut Vec<Vec<UndoAction>>,
    redo: &mut Vec<Vec<UndoAction>>,
) -> (Option<String>, Option<String>) {
    let Some(action_batch) = redo.pop() else {
        return (None, None);
    };
    let mut history_batch = Vec::new();
    let mut restored: Option<String> = None;
    let mut failure: Option<String> = None;

    for action in action_batch {
        match action.action_type {
            UndoActionType::Move => {
                let ok = file_ops::rename_with_retry(
                    std::path::Path::new(&action.old_path),
                    std::path::Path::new(&action.new_path),
                )
                .is_ok();
                if ok {
                    restored = Some(action.new_path.clone());
                } else if failure.is_none() {
                    failure = Some(format!("Failed to redo move of {}", action.old_path));
                }
                history_batch.push(action);
            }
            UndoActionType::Edit => {
                if swap_edit(&action).is_ok() {
                    restored = Some(action.old_path.clone());
                } else if failure.is_none() {
                    failure = Some(format!("Failed to redo edit on {}", action.old_path));
                }
                history_batch.push(action);
            }
        }
    }
    history.push(history_batch);

    (restored, failure)
}

#[tauri::command]
async fn rename_file(path: String, new_name: String) -> Result<String, String> {
    file_ops::rename_file(&path, &new_name)
}

#[tauri::command]
async fn undo_last_move(state: State<'_, AppState>) -> Result<Option<String>, String> {
    // Lock order: history before redo_history, matching every other command.
    let mut history = state.history.lock().unwrap();
    let mut redo = state.redo_history.lock().unwrap();
    let (restored, failure) = undo_batch(&mut history, &mut redo);
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(restored)
}

#[tauri::command]
async fn redo_last_move(state: State<'_, AppState>) -> Result<Option<String>, String> {
    // Lock order: history before redo_history, matching every other command
    // (prevents the AB-BA deadlock between undo and redo).
    let mut history = state.history.lock().unwrap();
    let mut redo = state.redo_history.lock().unwrap();
    let (restored, failure) = redo_batch(&mut history, &mut redo);
    if let Some(e) = failure {
        return Err(e);
    }
    Ok(restored)
}

#[tauri::command]
async fn get_history_stats(state: State<'_, AppState>) -> Result<(usize, usize), String> {
    let undo_count = state.history.lock().unwrap().iter().map(|b| b.len()).sum();
    let redo_count = state
        .redo_history
        .lock()
        .unwrap()
        .iter()
        .map(|b| b.len())
        .sum();
    Ok((undo_count, redo_count))
}

/// Swaps the contents of `old_path` (live file) and `new_path` (temp backup).
/// Used identically by both undo and redo for edit actions.
///
/// Uses copies instead of renames: the backup lives in the system temp dir,
/// which is often on a different volume than the image (e.g. C: vs D:), and
/// Windows rejects cross-volume renames. Copies work across volumes.
fn swap_edit(action: &UndoAction) -> Result<(), String> {
    let tmp = format!("{}_tmp", action.new_path);
    let result = (|| {
        file_ops::copy_with_retry(
            std::path::Path::new(&action.old_path),
            std::path::Path::new(&tmp),
        )?;
        file_ops::copy_with_retry(
            std::path::Path::new(&action.new_path),
            std::path::Path::new(&action.old_path),
        )?;
        file_ops::copy_with_retry(
            std::path::Path::new(&tmp),
            std::path::Path::new(&action.new_path),
        )
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

#[tauri::command]
async fn rotate_image(
    path: String,
    degrees: i32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let backup = backup::backup_file(&path)?;
    image_proc::rotate_image(&path, degrees)?;
    push_history(
        &state,
        vec![UndoAction {
            old_path: path.clone(),
            new_path: backup,
            action_type: UndoActionType::Edit,
        }],
    );
    Ok(())
}

#[tauri::command]
async fn crop_image(
    path: String,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let backup = backup::backup_file(&path)?;
    image_proc::crop_image(&path, x, y, width, height)?;
    push_history(
        &state,
        vec![UndoAction {
            old_path: path.clone(),
            new_path: backup,
            action_type: UndoActionType::Edit,
        }],
    );
    Ok(())
}

#[tauri::command]
async fn get_config() -> AppConfig {
    config::get_config()
}

#[tauri::command]
async fn save_config(config: AppConfig) -> Result<(), String> {
    config::save_config(config)
}

/// Moves the given paths to the system trash, reporting every failure instead
/// of silently dropping them (a trashed file that never reached the trash bin
/// is data at risk).
fn trash_files(paths: Vec<String>) -> Result<(), String> {
    let mut failures = Vec::new();
    for path in paths {
        if let Err(e) = trash::delete(&path) {
            failures.push(format!("{path}: {e}"));
        }
    }
    if !failures.is_empty() {
        return Err(format!(
            "Failed to move some files to trash:\n{}",
            failures.join("\n")
        ));
    }
    Ok(())
}

#[tauri::command]
async fn trash_images(paths: Vec<String>) -> Result<(), String> {
    trash_files(paths)
}

#[tauri::command]
async fn hide_window(window: tauri::Window) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            // Remove crash leftovers from previous runs (best effort).
            backup::cleanup_stale_sessions(std::time::Duration::from_secs(24 * 3600));

            let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(AppState {
            history: Mutex::new(Vec::new()),
            redo_history: Mutex::new(Vec::new()),
        })
        .invoke_handler(tauri::generate_handler![
            list_files,
            move_images,
            rename_file,
            undo_last_move,
            redo_last_move,
            get_history_stats,
            rotate_image,
            crop_image,
            get_config,
            save_config,
            trash_images,
            hide_window
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let minimize = config::get_config().minimize_to_tray.unwrap_or(true);
                if minimize {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            if let tauri::WindowEvent::Destroyed = event {
                // Clean up any temp backup files left in the undo/redo history
                if let Some(state) = window.try_state::<AppState>() {
                    let cleanup = |batches: &[Vec<UndoAction>]| {
                        for action in batches.iter().flatten() {
                            if action.action_type == UndoActionType::Edit {
                                let _ = std::fs::remove_file(&action.new_path);
                            }
                        }
                    };
                    cleanup(&state.history.lock().unwrap());
                    cleanup(&state.redo_history.lock().unwrap());
                }
                // Drop the whole session dir (undo/redo backups + any leftovers)
                backup::cleanup_session();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &std::path::Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn swap_edit_swaps_and_restores() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("img.jpg");
        let backup = dir.path().join("backup.jpg");
        write_file(&real, "edited");
        write_file(&backup, "original");
        let action = UndoAction {
            old_path: real.to_string_lossy().into_owned(),
            new_path: backup.to_string_lossy().into_owned(),
            action_type: UndoActionType::Edit,
        };

        // Undo: live file is replaced by the pre-edit backup, edited copy goes to temp
        swap_edit(&action).unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "edited");

        // Redo: same symmetric swap restores the edited version
        swap_edit(&action).unwrap();
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "edited");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "original");
    }

    #[test]
    fn trash_files_reports_missing_paths() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("pic.jpg");
        std::fs::write(&existing, b"data").unwrap();
        let missing = dir.path().join("nope.jpg");

        // The missing path must surface as an error even though one file trashed
        let result = trash_files(vec![
            existing.to_string_lossy().into_owned(),
            missing.to_string_lossy().into_owned(),
        ]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nope.jpg"));
    }

    #[test]
    fn swap_edit_reports_error_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("missing.jpg");
        let backup = dir.path().join("backup.jpg");
        let action = UndoAction {
            old_path: real.to_string_lossy().into_owned(),
            new_path: backup.to_string_lossy().into_owned(),
            action_type: UndoActionType::Edit,
        };
        assert!(swap_edit(&action).is_err());
    }

    /// Full rotate→undo→redo cycle through the real batch logic: contents must
    /// round-trip and history/redo must end up in the expected state.
    #[test]
    fn full_edit_cycle_undo_then_redo_restores_contents_and_history() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("photo.jpg");
        let backup = dir.path().join("backup.jpg");
        // State right after rotate_image: live = edited, backup = original
        write_file(&live, "rotated");
        write_file(&backup, "original");
        let action = UndoAction {
            old_path: live.to_string_lossy().into_owned(),
            new_path: backup.to_string_lossy().into_owned(),
            action_type: UndoActionType::Edit,
        };
        let mut history: Vec<Vec<UndoAction>> = vec![vec![action]];
        let mut redo: Vec<Vec<UndoAction>> = Vec::new();

        // Undo
        let (restored, failure) = undo_batch(&mut history, &mut redo);
        assert_eq!(failure, None);
        assert_eq!(restored.as_deref(), Some(live.to_str().unwrap()));
        assert_eq!(std::fs::read_to_string(&live).unwrap(), "original");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "rotated");
        assert!(history.is_empty(), "undo must drain history");
        assert_eq!(redo.len(), 1, "undo must populate redo");

        // Redo
        let (restored, failure) = redo_batch(&mut history, &mut redo);
        assert_eq!(failure, None);
        assert_eq!(restored.as_deref(), Some(live.to_str().unwrap()));
        assert_eq!(std::fs::read_to_string(&live).unwrap(), "rotated");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "original");
        assert!(redo.is_empty(), "redo must drain redo");
        assert_eq!(history.len(), 1, "redo must restore history");
    }
}
