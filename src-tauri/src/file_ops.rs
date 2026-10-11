use crate::models::ImageInfo;
use jwalk::WalkDir;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

/// Extensions accepted by the image list (and enforced on rename so a file
/// can't be renamed into a format the app no longer lists). Decodable by the
/// `image` crate with the enabled features: default formats + AVIF (decoder
/// via dav1d through the `avif-native` feature, encoder via ravif).
/// The Explorer context menu (context_menu.rs) must cover the same set — a
/// test asserts the two lists stay in sync.
pub(crate) const SUPPORTED_EXTENSIONS: [&str; 11] = [
    "jpg", "jpeg", "jif", "png", "gif", "webp", "bmp", "tif", "tiff", "ico", "avif",
];

/// Lists image files in `dir_path`. With `recursive`, the whole subtree is
/// walked (the ↳ subfolder-scan toggle) and every image's full path is
/// returned; otherwise only the first level, as before. Directories are
/// never listed — the app sorts files, and folder rows would complicate the
/// hotkey semantics.
///
/// `on_progress` receives the number of entries discovered so far, throttled
/// to ~8 updates per second — the UI shows live progress while a 25k-file
/// tree is being walked.
pub fn list_images(
    dir_path: &str,
    recursive: bool,
    on_progress: impl Fn(usize) + Send + Sync + 'static,
) -> Vec<ImageInfo> {
    let mut images = Vec::new();

    // Arc'd so the jwalk worker threads and the final report can both call it.
    let on_progress = Arc::new(on_progress);
    let discovered = Arc::new(AtomicUsize::new(0));
    let last_emit_ms = Arc::new(AtomicU64::new(0));
    let started = Instant::now();
    let discovered_for_cb = discovered.clone();
    let last_emit_for_cb = last_emit_ms.clone();
    let progress_for_cb = on_progress.clone();
    let mut walk = WalkDir::new(dir_path).process_read_dir(move |_, _, _, children| {
        discovered_for_cb.fetch_add(children.len(), Ordering::Relaxed);
        let now_ms = started.elapsed().as_millis() as u64;
        let last = last_emit_for_cb.load(Ordering::Relaxed);
        if now_ms >= last + 120
            && last_emit_for_cb
                .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            progress_for_cb(discovered_for_cb.load(Ordering::Relaxed));
        }
    });
    if !recursive {
        walk = walk.max_depth(1);
    }
    for entry in walk.into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if SUPPORTED_EXTENSIONS.contains(&ext.to_lowercase().as_str()) {
                    let metadata = entry.metadata().ok();
                    let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified = metadata
                        .and_then(|m| m.modified().ok())
                        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);

                    images.push(ImageInfo {
                        name: entry.file_name().to_string_lossy().to_string(),
                        path: path.to_string_lossy().to_string(),
                        size,
                        // Normalized to lowercase so consumers (type filter,
                        // format checks) never need to re-fold the case.
                        extension: ext.to_lowercase(),
                        modified,
                    });
                }
            }
        }
    }
    // Always report the final count — the throttle skips small/fast walks.
    on_progress(discovered.load(Ordering::Relaxed));
    images
}

/// Renames a file, retrying briefly to survive transient locks (antivirus
/// real-time scanning, webview asset streams) that are common on Windows.
/// `std::fs::rename` fails with "access denied" if another process holds the
/// file open, so a few short retries turn most transient failures into success.
pub fn rename_with_retry(from: &Path, to: &Path) -> Result<(), String> {
    let mut last_err = String::from("unknown error");
    for attempt in 0..8 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e.to_string();
                if attempt < 7 {
                    std::thread::sleep(std::time::Duration::from_millis(60));
                }
            }
        }
    }
    Err(last_err)
}

/// Copies a file, retrying briefly to survive transient locks (same rationale
/// as `rename_with_retry`). Unlike rename, copy works across volumes.
pub fn copy_with_retry(from: &Path, to: &Path) -> Result<(), String> {
    let mut last_err = String::from("unknown error");
    for attempt in 0..8 {
        match std::fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(e) => {
                last_err = e.to_string();
                if attempt < 7 {
                    std::thread::sleep(std::time::Duration::from_millis(60));
                }
            }
        }
    }
    Err(last_err)
}

/// Removes a file, retrying briefly to survive transient locks. A missing file
/// counts as success (it's already gone). Used by the cross-volume move
/// fallback after the copy succeeded.
pub fn remove_with_retry(path: &Path) -> Result<(), String> {
    let mut last_err = String::from("unknown error");
    for attempt in 0..8 {
        match std::fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last_err = e.to_string();
                if attempt < 7 {
                    std::thread::sleep(std::time::Duration::from_millis(60));
                }
            }
        }
    }
    Err(last_err)
}

/// Renames `source` in place. `new_name` must be a bare file name (no path
/// separators). If the typed name has no extension, the original extension is
/// kept so the file stays listable as an image. Fails on collision instead of
/// overwriting.
pub fn rename_file(source: &str, new_name: &str) -> Result<String, String> {
    let source_path = Path::new(source);
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("File name cannot be empty".to_string());
    }
    if new_name.contains('/') || new_name.contains('\\') {
        return Err("File name cannot contain path separators".to_string());
    }

    let parent = source_path.parent().ok_or("Invalid source path")?;
    // Windows strips trailing dots; normalize them away so "holiday." doesn't
    // become "holiday..jpg" after the extension is re-appended.
    let mut final_name = new_name.trim_end_matches('.').to_string();
    if final_name.is_empty() {
        return Err("File name cannot be empty".to_string());
    }
    // Windows-invalid characters (reserved punctuation, control chars) only
    // fail at rename time with a cryptic OS error — reject them up front
    // with a clear message. (Trailing dots/spaces are already normalized
    // away by trim + the dot-trim above.)
    if final_name
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_control())
    {
        return Err("File name contains characters Windows does not allow".to_string());
    }
    if Path::new(&final_name).extension().is_none() {
        if let Some(ext) = source_path.extension() {
            if !ext.is_empty() {
                final_name.push('.');
                final_name.push_str(&ext.to_string_lossy());
            }
        }
    }

    // Refuse extensions the app wouldn't list, so the renamed file can't
    // silently vanish from the image list.
    let final_ext = Path::new(&final_name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .ok_or_else(|| "File name must keep an image extension".to_string())?;
    if !SUPPORTED_EXTENSIONS.contains(&final_ext.as_str()) {
        return Err(format!(
            "Extension \".{}\" is not a supported image format",
            final_ext
        ));
    }

    let target = parent.join(&final_name);
    // Case-only renames (Pic.jpg → pic.jpg) are a no-op on Windows — compare
    // against the SOURCE file name, not the target (which always matches).
    // On case-sensitive platforms a case-only rename is a real rename.
    let same_name = if cfg!(windows) {
        target == source_path
            || source_path
                .file_name()
                .map(|n| n.to_string_lossy().eq_ignore_ascii_case(&final_name))
                .unwrap_or(false)
    } else {
        target == source_path
    };
    if same_name {
        // Nothing to do — report the actual on-disk name (source casing)
        return Ok(source_path.to_string_lossy().to_string());
    }
    if target.exists() {
        return Err(format!("A file named \"{}\" already exists", final_name));
    }

    rename_with_retry(source_path, &target)?;
    Ok(target.to_string_lossy().to_string())
}

/// Makes sure `dir` exists and is a directory (creating it when missing) and
/// returns it as an owned path for the collision helpers below.
fn ensure_dir(dir: &str) -> Result<std::path::PathBuf, String> {
    let dir_path = Path::new(dir);
    if !dir_path.exists() {
        std::fs::create_dir_all(dir_path).map_err(|e| e.to_string())?;
    }
    if !dir_path.is_dir() {
        return Err(format!("Target is not a directory: {dir}"));
    }
    Ok(dir_path.to_path_buf())
}

/// Returns a path in `dir` with the given full file name that does not exist
/// yet, appending Explorer-style " (1)", " (2)"… suffixes on collision
/// (inserted before the last extension, like Explorer: "photo.tar (1).gz").
pub fn available_path_for(dir: &str, file_name: &str) -> Result<std::path::PathBuf, String> {
    let dir_path = ensure_dir(dir)?;
    let p = Path::new(file_name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let mut target = dir_path.join(file_name);
    let mut count = 1;
    while target.exists() {
        target = dir_path.join(format!("{stem} ({count}){ext}"));
        count += 1;
    }
    Ok(target)
}

/// Returns a path in `dir` with the given stem/ext that does not exist yet,
/// appending " (1)", " (2)"… suffixes on collision. Unlike
/// `available_path_for`, `ext` may be a compound extension (e.g.
/// "workflow.json") and the suffix is inserted before the whole of it, so
/// "w" + "workflow.json" → "w (1).workflow.json" — the suffix never breaks
/// the ".workflow.json" naming ComfyUI drag-drop relies on.
pub fn available_path(dir: &str, stem: &str, ext: &str) -> Result<std::path::PathBuf, String> {
    let dir_path = ensure_dir(dir)?;
    let mut target = dir_path.join(format!("{stem}.{ext}"));
    let mut count = 1;
    while target.exists() {
        target = dir_path.join(format!("{stem} ({count}).{ext}"));
        count += 1;
    }
    Ok(target)
}

pub fn move_image(source: &str, target_dir: &str) -> Result<String, String> {
    let source_path = Path::new(source);
    let target_dir_path = Path::new(target_dir);

    if !target_dir_path.exists() {
        std::fs::create_dir_all(target_dir_path).map_err(|e| e.to_string())?;
    }
    if !target_dir_path.is_dir() {
        return Err(format!("Target is not a directory: {target_dir}"));
    }

    // Moving into the folder the file already lives in (e.g. a hotkey pointing
    // at the source folder) is a no-op — never auto-rename the file next to
    // itself. Compare canonicalized paths so drive-letter case differences on
    // Windows don't defeat the check.
    let same_dir = source_path
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .map(|sp| {
            target_dir_path
                .canonicalize()
                .ok()
                .map(|tp| sp == tp)
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if same_dir {
        return Ok(source_path.to_string_lossy().to_string());
    }

    let file_name = source_path.file_name().ok_or("Invalid filename")?;
    // Explorer-style auto-rename on collision (shared with convert/export).
    let target_path = available_path_for(target_dir, &file_name.to_string_lossy())?;

    // rename first (fast, same volume). Only when the failure is genuinely a
    // cross-device move (e.g. C: → D:) fall back to copy+delete — a plain
    // rename fails there with "not the same volume" on Windows / EXDEV.
    match std::fs::rename(source_path, &target_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_with_retry(source_path, &target_path)?;
            if let Err(e) = remove_with_retry(source_path) {
                // The file IS in the target, but the source copy couldn't be
                // removed — never hide a potential duplicate.
                return Err(format!(
                    "Moved to {} but could not remove the original: {e}",
                    target_path.display()
                ));
            }
        }
        Err(_) => {
            // Not cross-device: transient lock, permission, etc. Retry, then
            // surface the real error.
            rename_with_retry(source_path, &target_path)?;
        }
    }

    Ok(target_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn move_image_moves_and_returns_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let src = a.join("pic.jpg");
        fs::write(&src, b"data").unwrap();

        let moved = move_image(src.to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert_eq!(moved, b.join("pic.jpg").to_string_lossy().to_string());
        assert!(!src.exists());
        assert!(b.join("pic.jpg").exists());
    }

    #[test]
    fn move_image_renames_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("pic.jpg"), b"data1").unwrap();

        let moved = move_image(a.join("pic.jpg").to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert_eq!(Path::new(&moved), b.join("pic.jpg"));

        // Second file with the same name must be auto-renamed, not overwritten
        fs::write(a.join("pic.jpg"), b"data2").unwrap();
        let moved2 = move_image(a.join("pic.jpg").to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert_eq!(Path::new(&moved2), b.join("pic (1).jpg"));
        assert_eq!(fs::read_to_string(b.join("pic.jpg")).unwrap(), "data1");
        assert_eq!(fs::read_to_string(b.join("pic (1).jpg")).unwrap(), "data2");
    }

    #[test]
    fn move_image_creates_missing_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b/deep/nested");
        fs::create_dir_all(&a).unwrap();
        let src = a.join("pic.jpg");
        fs::write(&src, b"data").unwrap();

        let moved = move_image(src.to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert!(Path::new(&moved).exists());
        assert!(!src.exists());
    }

    #[test]
    fn copy_with_retry_copies_and_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.txt");
        let to = dir.path().join("b.txt");
        fs::write(&from, b"data").unwrap();
        fs::write(&to, b"old").unwrap();
        copy_with_retry(&from, &to).unwrap();
        assert_eq!(fs::read_to_string(&to).unwrap(), "data");
    }

    #[test]
    fn rename_with_retry_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("a.txt");
        let to = dir.path().join("b.txt");
        fs::write(&from, b"x").unwrap();
        rename_with_retry(&from, &to).unwrap();
        assert!(to.exists());
        assert!(!from.exists());
    }

    #[test]
    fn rename_with_retry_fails_when_source_missing() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("missing.txt");
        let to = dir.path().join("b.txt");
        assert!(rename_with_retry(&from, &to).is_err());
    }

    #[test]
    fn move_image_fails_when_source_missing() {
        let dir = tempfile::tempdir().unwrap();
        let b = dir.path().join("b");
        fs::create_dir_all(&b).unwrap();
        let missing = dir.path().join("nope.jpg");
        assert!(move_image(missing.to_str().unwrap(), b.to_str().unwrap()).is_err());
    }

    #[test]
    fn rename_file_renames_and_returns_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pic.jpg");
        fs::write(&src, b"data").unwrap();

        let renamed = rename_file(src.to_str().unwrap(), "holiday.jpg").unwrap();
        assert_eq!(
            renamed,
            dir.path().join("holiday.jpg").to_string_lossy().to_string()
        );
        assert!(!src.exists());
        assert!(dir.path().join("holiday.jpg").exists());
    }

    #[test]
    fn rename_file_keeps_extension_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pic.jpg");
        fs::write(&src, b"data").unwrap();

        let renamed = rename_file(src.to_str().unwrap(), "holiday").unwrap();
        assert!(Path::new(&renamed).ends_with("holiday.jpg"));
        assert!(Path::new(&renamed).exists());
        assert!(!src.exists());
    }

    #[test]
    fn rename_file_rejects_path_separators() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pic.jpg");
        fs::write(&src, b"data").unwrap();
        assert!(rename_file(src.to_str().unwrap(), "a/b.jpg").is_err());
        assert!(rename_file(src.to_str().unwrap(), "a\\b.jpg").is_err());
    }

    #[test]
    fn rename_file_rejects_empty_name() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pic.jpg");
        fs::write(&src, b"data").unwrap();
        assert!(rename_file(src.to_str().unwrap(), "   ").is_err());
    }

    #[test]
    fn rename_file_errors_on_collision_without_touching_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pic.jpg");
        fs::write(&src, b"data").unwrap();
        fs::write(dir.path().join("other.jpg"), b"x").unwrap();

        assert!(rename_file(src.to_str().unwrap(), "other.jpg").is_err());
        assert!(src.exists(), "source must be untouched on collision");
    }

    #[test]
    fn rename_file_errors_when_source_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.jpg");
        assert!(rename_file(missing.to_str().unwrap(), "new.jpg").is_err());
    }

    #[test]
    fn rename_file_rejects_unsupported_extension() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pic.jpg");
        fs::write(&src, b"data").unwrap();
        assert!(rename_file(src.to_str().unwrap(), "pic.txt").is_err());
        assert!(src.exists(), "source untouched on rejected extension");
    }

    #[test]
    fn rename_file_rejects_windows_invalid_characters() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pic.jpg");
        fs::write(&src, b"data").unwrap();
        for bad in [
            "a<b.jpg", "a>b.jpg", "a:b.jpg", "a\"b.jpg", "a|b.jpg", "a?b.jpg", "a*b.jpg",
            "a\tb.jpg",
        ] {
            assert!(
                rename_file(src.to_str().unwrap(), bad).is_err(),
                "name {bad:?} must be rejected up front"
            );
        }
        assert!(src.exists(), "source untouched on rejected names");
    }

    #[test]
    fn list_images_recursive_includes_subfolders() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.png"), b"1").unwrap();
        let sub = dir.path().join("flux");
        fs::create_dir_all(sub.join("deep")).unwrap();
        fs::write(sub.join("b.png"), b"2").unwrap();
        fs::write(sub.join("deep").join("c.jpg"), b"3").unwrap();
        fs::write(sub.join("notes.txt"), b"skip").unwrap();

        let flat = list_images(dir.path().to_str().unwrap(), false, |_| {});
        assert_eq!(flat.len(), 1, "non-recursive: only the top level");
        assert_eq!(flat[0].name, "a.png");

        let progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let progress_for_cb = progress.clone();
        let all = list_images(dir.path().to_str().unwrap(), true, move |n| {
            progress_for_cb.store(n, Ordering::Relaxed)
        });
        assert!(
            progress.load(Ordering::Relaxed) >= 6,
            "progress must report discovered entries (≥ 6 items incl. dirs), got {}",
            progress.load(Ordering::Relaxed)
        );
        let mut names: Vec<&str> = all.iter().map(|i| i.name.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            vec!["a.png", "b.png", "c.jpg"],
            "recursive walks the subtree, files only"
        );
    }

    #[test]
    fn available_path_for_keeps_double_extensions() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("photo.tar.gz"), b"1").unwrap();
        let p = available_path_for(dir.path().to_str().unwrap(), "photo.tar.gz").unwrap();
        assert_eq!(p.file_name().unwrap().to_string_lossy(), "photo.tar (1).gz");
    }

    #[test]
    fn move_image_into_its_own_folder_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("pic.jpg");
        fs::write(&src, b"data").unwrap();

        let moved = move_image(src.to_str().unwrap(), dir.path().to_str().unwrap()).unwrap();
        assert_eq!(Path::new(&moved), src);
        assert!(src.exists());
        assert!(
            !dir.path().join("pic (1).jpg").exists(),
            "must not auto-rename the file next to itself"
        );
    }

    #[test]
    fn available_path_renames_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("foo.jpg"), b"1").unwrap();
        fs::write(dir.path().join("foo (1).jpg"), b"2").unwrap();

        let p1 = available_path(dir.path().to_str().unwrap(), "foo", "jpg").unwrap();
        assert_eq!(p1.file_name().unwrap().to_string_lossy(), "foo (2).jpg");
        let p2 = available_path(dir.path().to_str().unwrap(), "bar", "jpg").unwrap();
        assert_eq!(p2.file_name().unwrap().to_string_lossy(), "bar.jpg");
    }

    #[test]
    fn available_path_creates_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("deep/nested");
        let p = available_path(sub.to_str().unwrap(), "foo", "png").unwrap();
        assert!(p.parent().unwrap().exists());
        assert!(!p.exists());
    }

    #[test]
    fn remove_with_retry_removes_and_tolerates_missing() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, b"x").unwrap();
        remove_with_retry(&f).unwrap();
        assert!(!f.exists());
        // already gone — counts as success
        remove_with_retry(&f).unwrap();
    }
}
