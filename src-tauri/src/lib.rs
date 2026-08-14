mod backup;
mod config;
mod context_menu;
mod exif_writer;
mod file_ops;
mod image_proc;
mod jpeg_meta;
mod metadata;
mod models;
mod png_meta;

use crate::models::{AppConfig, ImageInfo, UndoAction, UndoActionType};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::Emitter;
use tauri::Manager;
use tauri::State;

/// Maximum number of undo (and redo) batches kept in memory.
const MAX_HISTORY: usize = 10;

struct AppState {
    history: Mutex<Vec<Vec<UndoAction>>>,
    redo_history: Mutex<Vec<Vec<UndoAction>>>,
}

/// Shared state for the PNG→JPG conversion: a cancellation flag (set by the
/// frontend's Cancel button) and a busy flag (guards against two concurrent
/// conversions).
struct ConvertState {
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

/// One converted file's entry in the final report.
#[derive(Clone, Default, serde::Serialize)]
struct ConvertItem {
    /// Original PNG path.
    source: String,
    /// Created JPG path.
    output: String,
    in_size: u64,
    out_size: u64,
    /// `true` when the source PNG was moved to the trash (replace mode).
    replaced: bool,
}

/// Progress event streamed to the frontend after every file.
#[derive(Clone, serde::Serialize)]
struct ConvertProgress {
    done: usize,
    total: usize,
    /// File name currently being converted (for the live readout).
    current: String,
}

/// Final conversion report returned to the frontend.
#[derive(Clone, Default, serde::Serialize)]
struct ConvertReport {
    converted: Vec<ConvertItem>,
    /// Files skipped because prompt+workflow exceed the JPEG EXIF limit.
    skipped_large: Vec<String>,
    errors: Vec<String>,
}

/// The result of converting a single file (no I/O to trash yet).
#[derive(Debug)]
enum FileOutcome {
    Converted {
        source: String,
        output: String,
        in_size: u64,
        out_size: u64,
    },
    /// Metadata does not fit the JPEG EXIF limit — nothing was written.
    TooLarge {
        source: String,
    },
    /// Not a .png file (caller usually filters already; safety net).
    Skipped,
    Error {
        source: String,
        error: String,
    },
}

/// Converts one PNG file to JPG next to it (auto-renamed on collision) and,
/// in replace mode, hands the original to `dispose` (the real worker uses the
/// OS trash; tests inject a stub). Returns the outcome without touching the
/// shared report so this stays a pure, unit-testable function.
fn convert_one(
    src: &str,
    replace: bool,
    dispose: impl Fn(&str) -> Result<(), String>,
) -> FileOutcome {
    let is_png = std::path::Path::new(src)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("png"))
        .unwrap_or(false);
    if !is_png {
        return FileOutcome::Skipped;
    }

    let path = std::path::Path::new(src);
    let Some(dir) = path.parent() else {
        return FileOutcome::Error {
            source: src.to_string(),
            error: "no parent directory".to_string(),
        };
    };
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    let Some(stem) = stem else {
        return FileOutcome::Error {
            source: src.to_string(),
            error: "invalid file name".to_string(),
        };
    };

    let dst = match file_ops::available_path(&dir.to_string_lossy(), &stem, "jpg") {
        Ok(p) => p,
        Err(e) => {
            return FileOutcome::Error {
                source: src.to_string(),
                error: e,
            }
        }
    };
    let dst_str = dst.to_string_lossy().into_owned();

    match image_proc::png_to_jpg(src, &dst_str, 90) {
        Ok(image_proc::PngToJpgOutcome::Converted {
            in_size, out_size, ..
        }) => {
            if replace {
                if let Err(e) = dispose(src) {
                    return FileOutcome::Error {
                        source: src.to_string(),
                        error: format!("converted to {dst_str} but failed to trash original: {e}"),
                    };
                }
            }
            FileOutcome::Converted {
                source: src.to_string(),
                output: dst_str,
                in_size,
                out_size,
            }
        }
        Ok(image_proc::PngToJpgOutcome::TooLarge) => FileOutcome::TooLarge {
            source: src.to_string(),
        },
        Err(e) => FileOutcome::Error {
            source: src.to_string(),
            error: e,
        },
    }
}

/// Runs the parallel conversion over `paths`. Workers pull file indices from a
/// shared counter, check the cancellation flag between files, and emit a
/// `convert-progress` event per file. Returns the accumulated report.
fn run_conversion(
    paths: Vec<String>,
    replace: bool,
    cancel: Arc<AtomicBool>,
    emit: impl Fn(ConvertProgress) + Send + Sync + 'static,
) -> ConvertReport {
    let total = paths.len();
    if total == 0 {
        return ConvertReport::default();
    }

    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let report = Arc::new(Mutex::new(ConvertReport::default()));

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 8);
    let emit = Arc::new(emit);
    let mut handles = Vec::new();
    for _ in 0..workers {
        let next = next.clone();
        let done = done.clone();
        let report = report.clone();
        let cancel = cancel.clone();
        let emit = emit.clone();
        let paths = paths.clone();
        handles.push(std::thread::spawn(move || loop {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let i = next.fetch_add(1, Ordering::SeqCst);
            if i >= total {
                break;
            }
            let path = &paths[i];
            let outcome = convert_one(path, replace, |p| {
                trash::delete(p).map_err(|e| e.to_string())
            });
            {
                let mut rep = report.lock().unwrap();
                match outcome {
                    FileOutcome::Converted {
                        source,
                        output,
                        in_size,
                        out_size,
                    } => rep.converted.push(ConvertItem {
                        source,
                        output,
                        in_size,
                        out_size,
                        replaced: replace,
                    }),
                    FileOutcome::TooLarge { source } => rep.skipped_large.push(source),
                    FileOutcome::Skipped => {}
                    FileOutcome::Error { source, error } => {
                        rep.errors.push(format!("{source}: {error}"))
                    }
                }
            }
            let d = done.fetch_add(1, Ordering::SeqCst) + 1;
            let current = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            emit(ConvertProgress {
                done: d,
                total,
                current,
            });
        }));
    }
    for h in handles {
        let _ = h.join();
    }

    let rep = report.lock().unwrap().clone();
    // Cancellation is not an error: the partial report is what the frontend
    // needs to show "canceled after N files".
    rep
}

#[tauri::command]
async fn convert_pngs(
    app: tauri::AppHandle,
    paths: Vec<String>,
    replace: bool,
    state: State<'_, ConvertState>,
) -> Result<ConvertReport, String> {
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err("A conversion is already running".to_string());
    }
    state.cancel.store(false, Ordering::SeqCst);
    let cancel = state.cancel.clone();
    let busy = state.busy.clone();
    let app_handle = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let report = run_conversion(paths, replace, cancel, {
            let app = app_handle.clone();
            move |p| {
                let _ = app.emit("convert-progress", p);
            }
        });
        busy.store(false, Ordering::SeqCst);
        report
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(result)
}

/// Asks the running conversion to stop after the current file. Already-created
/// JPGs stay on disk; the report shows what finished before the cancel.
#[tauri::command]
async fn cancel_convert(state: State<'_, ConvertState>) -> Result<(), String> {
    state.cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// Installs or removes the Explorer context-menu items. Called from the
/// Options toggle; the config flag is saved separately by save_config.
#[tauri::command]
async fn set_context_menu(enabled: bool) -> Result<(), String> {
    if enabled {
        context_menu::install()
    } else {
        context_menu::uninstall()
    }
}

/// Exports the full metadata of an image to a sidecar file next to it:
/// `<name>.metadata.json` or `<name>.metadata.txt`. Used by the Ctrl+Q modal
/// "Export" buttons.
#[tauri::command]
async fn export_metadata(path: String, format: String) -> Result<String, String> {
    let meta = metadata::get_metadata(&path)?;
    let src = std::path::Path::new(&path);
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string());
    let dir = src
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());

    let (out_path, content) = if format.eq_ignore_ascii_case("txt") {
        (format!("{dir}/{stem}.metadata.txt"), metadata::to_text(&meta))
    } else {
        let json = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
        (format!("{dir}/{stem}.metadata.json"), json)
    };
    std::fs::write(&out_path, content).map_err(|e| e.to_string())?;
    Ok(out_path)
}

/// The path the app was launched with (e.g. Windows "Open with…"), handed to
/// the frontend at startup and replaced on subsequent single-instance
/// launches. `metadata` is true when launched via the "Show Metadata Info"
/// context-menu item, so the frontend opens the Ctrl+Q modal right away.
struct LaunchArgs {
    path: Mutex<Option<String>>,
    metadata: bool,
}

/// A launch target for the frontend: a folder to list, or a file to show (its
/// parent folder is listed and the file is selected).
#[derive(Clone, serde::Serialize)]
struct LaunchTarget {
    path: String,
    is_dir: bool,
    dir: String,
    /// "open" shows the file; "metadata" additionally opens the Ctrl+Q modal
    /// (used by the "Show Metadata Info" context-menu item).
    action: String,
}

fn make_launch_target_with_action(path: &str, action: &str) -> Option<LaunchTarget> {
    // Normalise relative paths (e.g. `pic.jpg`) to absolute so a file's
    // parent is never the empty path.
    let p = std::path::absolute(path).ok()?;
    if p.is_dir() {
        Some(LaunchTarget {
            path: path.to_string(),
            is_dir: true,
            dir: path.to_string(),
            action: action.to_string(),
        })
    } else if p.is_file() {
        let dir = p.parent()?.to_string_lossy().into_owned();
        Some(LaunchTarget {
            path: path.to_string(),
            is_dir: false,
            dir,
            action: action.to_string(),
        })
    } else {
        None
    }
}

/// Picks the first existing filesystem path from the process arguments,
/// skipping the binary itself — i.e. the file or folder the app was asked to
/// open ("Open with…" on Windows passes the path as argv[1]).
fn first_launch_path(args: &[String]) -> Option<String> {
    let exe = std::env::current_exe()
        .ok()
        .map(|e| e.to_string_lossy().into_owned());
    args.iter()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty() && exe.as_deref() != Some(a.as_str()))
        .find(|a| std::path::Path::new(a).exists())
}

/// One-shot: returns the path the app was launched with (a folder, or a file
/// whose folder should be opened with the file selected), then clears it so a
/// plain restart never reopens a stale path.
#[tauri::command]
fn get_launch_target(state: State<'_, LaunchArgs>) -> Option<LaunchTarget> {
    let path = state.path.lock().ok()?.take()?;
    let action = if state.metadata { "metadata" } else { "open" };
    make_launch_target_with_action(&path, action)
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

/// Returns everything the Ctrl+Q metadata modal needs for one image.
#[tauri::command]
async fn get_metadata(path: String) -> Result<crate::models::ImageMetadata, String> {
    metadata::get_metadata(&path)
}

/// Copies the given text to the system clipboard (for metadata copy buttons).
#[tauri::command]
async fn copy_text(text: String) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set_text(text).map_err(|e| e.to_string())
}

/// Returns the subset of `paths` whose files contain AI-generation metadata
/// (used for the lazy "has prompt" badges in the list).
#[tauri::command]
async fn check_prompts(paths: Vec<String>) -> Vec<String> {
    metadata::check_prompts(&paths)
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

            // "Open with…" passes the path as a command-line argument: keep it
            // for the frontend, which consumes it at startup. Managed here so
            // it is registered by the time the webview invokes commands.
            // args_os (not args) so a non-UTF-8 path from Explorer can't panic.
            let args: Vec<String> = std::env::args_os()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            let metadata = args
                .iter()
                .any(|a| a.eq_ignore_ascii_case("--metadata"));
            app.manage(LaunchArgs {
                path: Mutex::new(first_launch_path(&args)),
                metadata,
            });

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
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // A second instance was started with a path ("Open with…" or a
            // context-menu item while the app is already running): forward it
            // to the frontend, remembering whether to open the metadata modal.
            let metadata = args
                .iter()
                .any(|a| a.eq_ignore_ascii_case("--metadata"));
            if let Some(path) = first_launch_path(&args) {
                let action = if metadata { "metadata" } else { "open" };
                if let Some(target) = make_launch_target_with_action(&path, action) {
                    let _ = app.emit("open-file", target);
                }
            }
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
        .manage(ConvertState {
            cancel: Arc::new(AtomicBool::new(false)),
            busy: Arc::new(AtomicBool::new(false)),
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
            hide_window,
            get_metadata,
            copy_text,
            check_prompts,
            get_launch_target,
            convert_pngs,
            cancel_convert,
            set_context_menu,
            export_metadata
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

    /// Builds a tiny PNG with the given text chunks (ComfyUI prompt/workflow).
    fn synth_png(dir: &std::path::Path, name: &str, chunks: &[(&str, &str)]) -> std::path::PathBuf {
        use png::{BitDepth, ColorType};
        let path = dir.join(name);
        let mut bytes = Vec::new();
        {
            let w = &mut std::io::Cursor::new(&mut bytes);
            let mut enc = png::Encoder::new(w, 4, 4);
            enc.set_color(ColorType::Rgba);
            enc.set_depth(BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer.write_image_data(&[0u8; 4 * 4 * 4]).unwrap();
            for (kw, text) in chunks {
                writer
                    .write_text_chunk(&png::text_metadata::TEXtChunk::new(
                        kw.to_string(),
                        text.to_string(),
                    ))
                    .unwrap();
            }
            writer.finish().unwrap();
        }
        std::fs::write(&path, &bytes).unwrap();
        path
    }

    #[test]
    fn convert_one_creates_jpg_next_to_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = synth_png(
            dir.path(),
            "comfy.png",
            &[("prompt", "{\"a\":1}"), ("workflow", "{\"nodes\":[]}")],
        );
        let outcome = convert_one(src.to_str().unwrap(), false, |_| {
            Err("must not trash".to_string())
        });
        match outcome {
            FileOutcome::Converted {
                output, in_size, ..
            } => {
                assert!(std::path::Path::new(&output).exists());
                assert!(in_size > 0);
                // JPG next to the PNG, same stem
                assert_eq!(
                    std::path::Path::new(&output)
                        .file_name()
                        .unwrap()
                        .to_string_lossy(),
                    "comfy.jpg"
                );
                // source untouched in next-to mode
                assert!(src.exists());
            }
            other => panic!("expected Converted, got {other:?}"),
        }
    }

    #[test]
    fn convert_one_replace_trashes_source_after_success() {
        let dir = tempfile::tempdir().unwrap();
        let src = synth_png(dir.path(), "comfy.png", &[("prompt", "{\"a\":1}")]);
        let trashed = std::sync::Mutex::new(Vec::new());
        let outcome = convert_one(src.to_str().unwrap(), true, |p| {
            trashed.lock().unwrap().push(p.to_string());
            Ok(())
        });
        assert!(matches!(outcome, FileOutcome::Converted { .. }));
        let trashed = trashed.lock().unwrap();
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0], src.to_string_lossy().to_string());
    }

    #[test]
    fn convert_one_skips_non_png() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pic.jpg");
        std::fs::write(&src, b"jpeg").unwrap();
        let outcome = convert_one(src.to_str().unwrap(), false, |_| Err(String::new()));
        assert!(matches!(outcome, FileOutcome::Skipped));
    }

    #[test]
    fn convert_one_reports_too_large_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let src = synth_png(
            dir.path(),
            "big.png",
            &[
                ("prompt", &"p".repeat(50_000)),
                ("workflow", &"w".repeat(50_000)),
            ],
        );
        let outcome = convert_one(src.to_str().unwrap(), false, |_| Err(String::new()));
        assert!(matches!(outcome, FileOutcome::TooLarge { .. }));
        // no JPG left behind
        assert!(!dir.path().join("big.jpg").exists());
    }

    #[test]
    fn run_conversion_aggregates_report_and_auto_renames() {
        let dir = tempfile::tempdir().unwrap();
        synth_png(dir.path(), "a.png", &[("prompt", "{\"a\":1}")]);
        synth_png(dir.path(), "b.png", &[("workflow", "{}")]);
        // Collision: a.jpg already exists -> a (1).jpg
        std::fs::write(dir.path().join("a.jpg"), b"existing").unwrap();
        // Non-png is skipped silently
        std::fs::write(dir.path().join("note.txt"), b"x").unwrap();

        let paths: Vec<String> = vec!["a.png", "b.png", "note.txt"]
            .into_iter()
            .map(|n| dir.path().join(n).to_string_lossy().into_owned())
            .collect();
        let report = run_conversion(paths, false, Arc::new(AtomicBool::new(false)), |_| {});

        assert_eq!(report.converted.len(), 2);
        assert!(report.errors.is_empty());
        assert!(report.skipped_large.is_empty());
        let names: Vec<String> = report
            .converted
            .iter()
            .map(|c| {
                std::path::Path::new(&c.output)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(names.contains(&"a (1).jpg".to_string()), "got {names:?}");
        assert!(names.contains(&"b.jpg".to_string()), "got {names:?}");
        // Originals untouched in next-to mode
        assert!(dir.path().join("a.png").exists());
        assert!(dir.path().join("b.png").exists());
    }

    #[test]
    fn run_conversion_respects_cancel() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            synth_png(dir.path(), &format!("f{i:02}.png"), &[("prompt", "{}")]);
        }
        let paths: Vec<String> = (0..20)
            .map(|i| {
                dir.path()
                    .join(format!("f{i:02}.png"))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let cancel = Arc::new(AtomicBool::new(true)); // pre-canceled
        let report = run_conversion(paths, false, cancel, |_| {});
        assert!(
            report.converted.is_empty(),
            "canceled run must convert nothing"
        );
    }

    #[test]
    fn first_launch_path_picks_existing_path_after_exe() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pic.jpg");
        std::fs::write(&file, b"x").unwrap();
        let file_str = file.to_string_lossy().into_owned();
        // argv[0] is the binary; the real path is argv[1] ("Open with…").
        let args = vec!["ignored-exe".to_string(), file_str.clone()];
        assert_eq!(first_launch_path(&args).as_deref(), Some(file_str.as_str()));
        // Missing paths are skipped entirely.
        let args = vec!["C:\\definitely\\missing.jpg".to_string()];
        assert_eq!(first_launch_path(&args), None);
    }

    #[test]
    fn make_launch_target_distinguishes_file_and_dir() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pic.jpg");
        std::fs::write(&file, b"x").unwrap();
        let dir_str = dir.path().to_string_lossy().into_owned();
        let file_str = file.to_string_lossy().into_owned();

        let t = make_launch_target_with_action(&dir_str, "open").unwrap();
        assert!(t.is_dir, "folder target must be a dir");
        assert_eq!(t.dir, dir_str);
        assert_eq!(t.path, dir_str);
        assert_eq!(t.action, "open");

        let t = make_launch_target_with_action(&file_str, "open").unwrap();
        assert!(!t.is_dir, "file target must not be a dir");
        assert_eq!(t.path, file_str);
        assert_eq!(t.dir, dir_str, "file target lists its parent folder");
        assert_eq!(t.action, "open");

        let t = make_launch_target_with_action(&file_str, "metadata").unwrap();
        assert_eq!(t.action, "metadata");

        assert!(make_launch_target_with_action("C:\\definitely\\missing.jpg", "open").is_none());
    }
}
