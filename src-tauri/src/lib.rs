mod models;
mod file_ops;
mod image_proc;
mod config;

use crate::models::{ImageInfo, AppConfig, UndoAction};
use std::sync::Mutex;
use tauri::State;
use tauri::Manager;
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::menu::{Menu, MenuItem};

struct AppState {
    history: Mutex<Vec<Vec<UndoAction>>>,
    redo_history: Mutex<Vec<Vec<UndoAction>>>,
}

#[tauri::command]
async fn list_files(dir: String) -> Result<Vec<ImageInfo>, String> {
    Ok(file_ops::list_images(&dir))
}

#[tauri::command]
async fn move_images(sources: Vec<String>, target_dir: String, state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let mut batch = Vec::new();
    let mut new_paths = Vec::new();
    
    for source in sources {
        if let Ok(new_path) = file_ops::move_image(&source, &target_dir) {
            batch.push(UndoAction {
                old_path: source,
                new_path: new_path.clone(),
                action_type: "move".to_string(),
            });
            new_paths.push(new_path);
        }
    }
    
    if !batch.is_empty() {
        let mut history = state.history.lock().unwrap();
        history.push(batch);
        if history.len() > 5 {
            let removed_batch = history.remove(0);
            for removed in removed_batch {
                if removed.action_type == "edit" {
                    let _ = std::fs::remove_file(&removed.new_path);
                }
            }
        }
        
        let mut redo = state.redo_history.lock().unwrap();
        for batch in redo.drain(..) {
            for action in batch {
                if action.action_type == "edit" {
                    let _ = std::fs::remove_file(&action.new_path);
                }
            }
        }
    }
    
    Ok(new_paths)
}

#[tauri::command]
async fn undo_last_move(state: State<'_, AppState>) -> Result<bool, String> {
    let mut history = state.history.lock().unwrap();
    if let Some(mut last_batch) = history.pop() {
        let mut redo = state.redo_history.lock().unwrap();
        let mut redo_batch = Vec::new();
        
        // Reverse iterate to undo in opposite order
        while let Some(last_action) = last_batch.pop() {
            if last_action.action_type == "move" {
                let _ = std::fs::rename(&last_action.new_path, &last_action.old_path);
                redo_batch.push(last_action);
            } else if last_action.action_type == "edit" {
                let tmp = format!("{}_tmp", last_action.new_path);
                let _ = std::fs::rename(&last_action.old_path, &tmp);
                let _ = std::fs::rename(&last_action.new_path, &last_action.old_path);
                let _ = std::fs::rename(&tmp, &last_action.new_path);
                redo_batch.push(last_action);
            }
        }
        redo_batch.reverse(); // restore forward direction for redo
        redo.push(redo_batch);
        return Ok(true);
    }
    Err("No more actions to undo".to_string())
}

#[tauri::command]
async fn redo_last_move(state: State<'_, AppState>) -> Result<bool, String> {
    let mut redo = state.redo_history.lock().unwrap();
    if let Some(action_batch) = redo.pop() {
        let mut history = state.history.lock().unwrap();
        let mut history_batch = Vec::new();
        
        for action in action_batch {
            if action.action_type == "move" {
                let _ = std::fs::rename(&action.old_path, &action.new_path);
                history_batch.push(action);
            } else if action.action_type == "edit" {
                let tmp = format!("{}_tmp", action.new_path);
                let _ = std::fs::rename(&action.old_path, &tmp);
                let _ = std::fs::rename(&action.new_path, &action.old_path);
                let _ = std::fs::rename(&tmp, &action.new_path);
                history_batch.push(action);
            }
        }
        history.push(history_batch);
        return Ok(true);
    }
    Err("No more actions to redo".to_string())
}

#[tauri::command]
async fn get_history_stats(state: State<'_, AppState>) -> Result<(usize, usize), String> {
    let undo_count = state.history.lock().unwrap().len();
    let redo_count = state.redo_history.lock().unwrap().len();
    Ok((undo_count, redo_count))
}

fn backup_file(path: &str) -> Result<String, String> {
    let old_path = std::path::Path::new(path);
    let tmp = std::env::temp_dir();
    let ext = old_path.extension().unwrap_or_default();
    let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros();
    let backup_path = tmp.join(format!("img_bak_{}.{}", id, ext.to_string_lossy()));
    std::fs::copy(old_path, &backup_path).map_err(|e| e.to_string())?;
    Ok(backup_path.to_string_lossy().to_string())
}

#[tauri::command]
async fn rotate_image(path: String, degrees: i32, state: State<'_, AppState>) -> Result<(), String> {
    let backup = backup_file(&path)?;
    image_proc::rotate_image(&path, degrees)?;
    let mut history = state.history.lock().unwrap();
    history.push(vec![UndoAction { old_path: path.clone(), new_path: backup, action_type: "edit".to_string() }]);
    if history.len() > 5 {
        let removed_batch = history.remove(0);
        for removed in removed_batch {
            if removed.action_type == "edit" {
                let _ = std::fs::remove_file(&removed.new_path);
            }
        }
    }
    let mut redo = state.redo_history.lock().unwrap();
    for batch in redo.drain(..) {
        for action in batch {
            if action.action_type == "edit" {
                let _ = std::fs::remove_file(&action.new_path);
            }
        }
    }
    Ok(())
}

#[tauri::command]
async fn crop_image(path: String, x: u32, y: u32, width: u32, height: u32, state: State<'_, AppState>) -> Result<(), String> {
    let backup = backup_file(&path)?;
    image_proc::crop_image(&path, x, y, width, height)?;
    let mut history = state.history.lock().unwrap();
    history.push(vec![UndoAction { old_path: path.clone(), new_path: backup, action_type: "edit".to_string() }]);
    if history.len() > 5 {
        let removed_batch = history.remove(0);
        for removed in removed_batch {
            if removed.action_type == "edit" {
                let _ = std::fs::remove_file(&removed.new_path);
            }
        }
    }
    let mut redo = state.redo_history.lock().unwrap();
    for batch in redo.drain(..) {
        for action in batch {
            if action.action_type == "edit" {
                let _ = std::fs::remove_file(&action.new_path);
            }
        }
    }
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

#[tauri::command]
async fn trash_images(paths: Vec<String>) -> Result<(), String> {
    for path in paths {
        let _ = trash::delete(&path);
    }
    Ok(())
}

#[tauri::command]
async fn hide_window(window: tauri::Window) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_i, &quit_i])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .menu_on_left_click(false)
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
                .on_tray_icon_event(|tray, event| match event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } => {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .manage(AppState {
            history: Mutex::new(Vec::new()),
            redo_history: Mutex::new(Vec::new()),
        })
        .invoke_handler(tauri::generate_handler![
            list_files,
            move_images,
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
                    window.hide().unwrap();
                }
            }
            if let tauri::WindowEvent::Destroyed = event {
                // Clean up any temp backup files left in the undo history
                if let Some(state) = window.try_state::<AppState>() {
                    let history = state.history.lock().unwrap();
                    for batch in history.iter() {
                        for action in batch {
                            if action.action_type == "edit" {
                                let _ = std::fs::remove_file(&action.new_path);
                            }
                        }
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
