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
    history: Mutex<Vec<UndoAction>>,
    redo_history: Mutex<Vec<UndoAction>>,
}

#[tauri::command]
async fn list_files(dir: String) -> Result<Vec<ImageInfo>, String> {
    Ok(file_ops::list_images(&dir))
}

#[tauri::command]
async fn move_image(source: String, target_dir: String, state: State<'_, AppState>) -> Result<String, String> {
    let new_path = file_ops::move_image(&source, &target_dir)?;
    
    // Track undo action
    let mut history = state.history.lock().unwrap();
    history.push(UndoAction {
        old_path: source,
        new_path: new_path.clone(),
        action_type: "move".to_string(),
    });
    if history.len() > 5 {
        let removed = history.remove(0);
        if removed.action_type == "edit" {
            let _ = std::fs::remove_file(&removed.new_path);
        }
    }
    
    let mut redo = state.redo_history.lock().unwrap();
    for action in redo.drain(..) {
        if action.action_type == "edit" {
            let _ = std::fs::remove_file(&action.new_path);
        }
    }
    
    Ok(new_path)
}

#[tauri::command]
async fn undo_last_move(state: State<'_, AppState>) -> Result<String, String> {
    let mut history = state.history.lock().unwrap();
    if let Some(last_action) = history.pop() {
        let mut redo = state.redo_history.lock().unwrap();
        if last_action.action_type == "move" {
            std::fs::rename(&last_action.new_path, &last_action.old_path).map_err(|e| e.to_string())?;
            redo.push(last_action.clone());
            return Ok(last_action.old_path);
        } else if last_action.action_type == "edit" {
            // Swap files
            let tmp = format!("{}_tmp", last_action.new_path);
            std::fs::rename(&last_action.old_path, &tmp).map_err(|e| e.to_string())?;
            std::fs::rename(&last_action.new_path, &last_action.old_path).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &last_action.new_path).map_err(|e| e.to_string())?;
            redo.push(last_action.clone());
            return Ok(last_action.old_path);
        }
    }
    Err("No more actions to undo".to_string())
}

#[tauri::command]
async fn redo_last_move(state: State<'_, AppState>) -> Result<String, String> {
    let mut redo = state.redo_history.lock().unwrap();
    if let Some(action) = redo.pop() {
        let mut history = state.history.lock().unwrap();
        if action.action_type == "move" {
            std::fs::rename(&action.old_path, &action.new_path).map_err(|e| e.to_string())?;
            history.push(action.clone());
            return Ok(action.new_path);
        } else if action.action_type == "edit" {
            // Swap files
            let tmp = format!("{}_tmp", action.new_path);
            std::fs::rename(&action.old_path, &tmp).map_err(|e| e.to_string())?;
            std::fs::rename(&action.new_path, &action.old_path).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &action.new_path).map_err(|e| e.to_string())?;
            history.push(action.clone());
            return Ok(action.old_path);
        }
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
    history.push(UndoAction { old_path: path.clone(), new_path: backup, action_type: "edit".to_string() });
    if history.len() > 5 {
        let removed = history.remove(0);
        if removed.action_type == "edit" {
            let _ = std::fs::remove_file(&removed.new_path);
        }
    }
    let mut redo = state.redo_history.lock().unwrap();
    for action in redo.drain(..) {
        if action.action_type == "edit" {
            let _ = std::fs::remove_file(&action.new_path);
        }
    }
    Ok(())
}

#[tauri::command]
async fn crop_image(path: String, x: u32, y: u32, width: u32, height: u32, state: State<'_, AppState>) -> Result<(), String> {
    let backup = backup_file(&path)?;
    image_proc::crop_image(&path, x, y, width, height)?;
    let mut history = state.history.lock().unwrap();
    history.push(UndoAction { old_path: path.clone(), new_path: backup, action_type: "edit".to_string() });
    if history.len() > 5 {
        let removed = history.remove(0);
        if removed.action_type == "edit" {
            let _ = std::fs::remove_file(&removed.new_path);
        }
    }
    let mut redo = state.redo_history.lock().unwrap();
    for action in redo.drain(..) {
        if action.action_type == "edit" {
            let _ = std::fs::remove_file(&action.new_path);
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
async fn trash_image(path: String) -> Result<(), String> {
    trash::delete(&path).map_err(|e| e.to_string())
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
            move_image,
            undo_last_move,
            redo_last_move,
            get_history_stats,
            rotate_image,
            crop_image,
            get_config,
            save_config,
            trash_image,
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
                    for action in history.iter() {
                        if action.action_type == "edit" {
                            let _ = std::fs::remove_file(&action.new_path);
                        }
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
