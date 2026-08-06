//! Edit backups live in a per-session directory inside the system temp dir,
//! so a single `remove_dir_all` cleans everything (undo, redo, crash leftovers)
//! instead of orphaning individual `img_bak_*` files.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The session directory, created lazily on first backup.
/// Keyed by pid + startup timestamp so concurrent app instances don't collide.
static SESSION_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Returns the session backup directory, creating it on first use.
/// `get_or_init` is thread-safe: concurrent `backup_file` calls share one dir.
fn session_dir() -> &'static PathBuf {
    SESSION_DIR.get_or_init(|| {
        let pid = std::process::id();
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let dir = std::env::temp_dir().join(format!("imagesorter_{}_{}", pid, ts));
        std::fs::create_dir_all(&dir).expect("create session backup dir");
        dir
    })
}

/// Copies `path` into the session backup directory (retrying transient
/// locks, matching `file_ops::copy_with_retry` semantics). Returns the
/// backup path. The backup is the only pre-edit copy, so it is preserved
/// for as long as the operation stays in the undo history.
pub fn backup_file(path: &str) -> Result<String, String> {
    let old_path = Path::new(path);
    let dir = session_dir();
    let ext = old_path.extension().unwrap_or_default();
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros();
    let backup_path = dir.join(format!("img_bak_{}.{}", id, ext.to_string_lossy()));
    crate::file_ops::copy_with_retry(old_path, &backup_path)?;
    Ok(backup_path.to_string_lossy().to_string())
}

/// Removes the whole session backup directory (called when the window is
/// destroyed / the app quits). A no-op if no backup was ever created.
pub fn cleanup_session() {
    if let Some(dir) = SESSION_DIR.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Parses the startup-timestamp suffix from an `imagesorter_<pid>_<ts>` dir
/// name. Returns `None` when the name has no parseable timestamp.
fn dir_timestamp(name: &str) -> Option<u64> {
    let (_, tail) = name.split_once('_')?; // "imagesorter"
    let (_, ts) = tail.rsplit_once('_')?; // pid
    ts.parse().ok()
}

/// Best-effort cleanup of stale `imagesorter_*` dirs under `temp_root`,
/// called at startup to remove crash leftovers from previous runs.
/// Staleness is decided by the timestamp in the dir name (preferred) and
/// falls back to the directory mtime for unrecognised names. Dirs that
/// can't be proven stale are kept (never delete data we're unsure about).
pub fn cleanup_stale_sessions_in(temp_root: &Path, max_age: Duration) {
    let now = SystemTime::now();
    if let Ok(entries) = std::fs::read_dir(temp_root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("imagesorter_") {
                continue;
            }
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let age = dir_timestamp(&name)
                .and_then(|ts| {
                    UNIX_EPOCH
                        .checked_add(Duration::from_secs(ts))
                        .and_then(|t| now.duration_since(t).ok())
                })
                .or_else(|| {
                    // Fallback for dirs whose name carries no timestamp.
                    entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| now.duration_since(t).ok())
                })
                .unwrap_or_default();
            if age > max_age {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
    }
}

/// Convenience wrapper over the real system temp dir.
pub fn cleanup_stale_sessions(max_age: Duration) {
    cleanup_stale_sessions_in(&std::env::temp_dir(), max_age);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_file_lands_in_a_session_dir_and_survives_copy() {
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("photo.jpg");
        std::fs::write(&src, b"original").unwrap();

        let backup = backup_file(src.to_str().unwrap()).unwrap();
        let backup_path = Path::new(&backup);
        assert!(backup_path.exists(), "backup file must exist");
        assert_eq!(std::fs::read_to_string(backup_path).unwrap(), "original");

        let name = backup_path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("imagesorter_"),
            "backup must live in a session dir, got {name}"
        );
        // The session dir must sit inside the system temp dir
        assert!(backup_path.starts_with(std::env::temp_dir()));

        cleanup_session();
        assert!(
            !backup_path.parent().unwrap().exists(),
            "cleanup_session must remove the whole session dir"
        );
    }

    #[test]
    fn cleanup_removes_stale_sessions_and_keeps_fresh() {
        let root = tempfile::tempdir().unwrap();
        let old_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 100 * 24 * 3600; // 100 days ago
        let fresh_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let stale = root.path().join(format!("imagesorter_1_{old_ts}"));
        let fresh = root.path().join(format!("imagesorter_2_{fresh_ts}"));
        let unrelated = root.path().join("other_dir");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();
        // Add a marker file to the stale dir to prove whole-dir removal
        std::fs::write(stale.join("img_bak_1.jpg"), b"x").unwrap();

        cleanup_stale_sessions_in(root.path(), Duration::from_secs(24 * 3600));

        assert!(!stale.exists(), "stale session dir must be removed");
        assert!(fresh.exists(), "fresh session dir must be kept");
        assert!(unrelated.exists(), "non-session dirs must be untouched");
    }

    #[test]
    fn cleanup_keeps_dirs_without_parseable_timestamp() {
        let root = tempfile::tempdir().unwrap();
        // Unknown-named dir: staleness cannot be proven from the name and the
        // mtime fallback is now-ish (just created) → must be kept.
        let unknown = root.path().join("imagesorter_weird");
        std::fs::create_dir_all(&unknown).unwrap();
        cleanup_stale_sessions_in(root.path(), Duration::from_secs(1));
        assert!(unknown.exists(), "unprovable-stale dir must be kept");
    }
}
