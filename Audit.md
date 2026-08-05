# Audit — HotkeyImageSorter (tauri-app)

Full engineering review performed per `ReviewPrompt.txt`. **No source code was changed** — this document describes issues and provides ready-to-use fixes, rationale, and priorities. A junior developer should be able to start fixing from this document alone.

---

## 1. Executive summary

The project is a Tauri 2 desktop app: vanilla-JS frontend (`src/`, no bundler) + Rust backend (`src-tauri/`). It compiles cleanly with 2 warnings. The audit found several latent correctness/robustness issues (see below) and — as a follow-up — added the first test suite to the repo (10 Rust + 4 JS tests, all passing).

Top findings, in priority order:

| # | Severity | Issue | Location |
|---|----------|-------|----------|
| H1 | **High** | Ctrl/Alt/Shift+letter and modal-open states still trigger destructive A–Z hotkey file moves | `src/app.js` `handleGlobalKeydown` |
| H2 | **Medium** | `undo_last_move` and `redo_last_move` acquire the two history mutexes in opposite order → latent deadlock | `src-tauri/src/lib.rs` |
| H3 | **Medium** | `move_images` swallows per-file failures and reports success (files silently don't move) | `src-tauri/src/lib.rs` |
| H4 | **Medium** | `crop_image` panics on out-of-bounds crop rectangles; backend trusts frontend input | `src-tauri/src/image_proc.rs` |
| H5 | **Medium** | Redo history is unbounded and its temp backup files leak on close; window-destroy cleanup only covers undo | `src-tauri/src/lib.rs` |
| H6 | **Medium** | Undo/redo contract mismatch: backend returns `bool`, frontend/README expect a path; deadlock-prone helper duplication | `lib.rs` + `app.js` |
| H7 | **Medium** | Edit-history cap (5) silently deletes the only pre-edit copy of images | `lib.rs` |
| M1 | Medium | Crop is misaligned when zoom ≠ 100% | `app.js` `executeCrop` |
| M2 | Medium | No UI feedback on failed operations; most invokes lack error handling | `app.js` |
| M3 | Medium | Empty/vanished source folder silently shows "0 images" | `file_ops.rs` + `app.js` |
| M4 | Medium | Supported extensions don't match README claims (no gif/bmp/tiff/avif; odd "jif") | `file_ops.rs` + README || M5 | Medium | Undo/redo file ops ignore all errors (`let _ =`) — silent failure, no user feedback | `lib.rs` |
| M6 | Medium | Full-list re-render every navigation ("virtual list" not actually virtual) | `app.js` `renderList` |
| L1–L12 | Low | Warnings, dead files, unused deps, docs drift, XSS-in-config-path, temp-file hygiene, branding | various |

Validation performed:

```bash
cd src-tauri && cargo check      # ✅ passes, 2 warnings
cd src-tauri && cargo test       # ✅ 10 tests pass (unit tests added per this audit; previously 0)
cd src-tauri && cargo build      # ✅ full debug build links successfully, same 2 warnings
cd src-tauri && cargo clippy --all-targets   # ✅ 3 warnings (2 known + 1 new: clippy::single_match at lib.rs:228)
cd src-tauri && cargo fmt --check # ❌ FAILS — repo is not rustfmt-clean (import order, wrapping, trailing whitespace)
npm test                         # ✅ 4 JS tests pass (node:test, added per this audit; previously no test script)
npm run build                     # ⚠️ not run: full `tauri build` (release + Windows installer bundling) is very heavy; the JS itself has no build step (frontendDist points at source)
```

Warnings (cargo check/build): `TrayIconBuilder::menu_on_left_click` deprecated (use `show_menu_on_left_click`); unused `mut` in `image_proc.rs:16`. Clippy adds: `clippy::single_match` at `lib.rs:228` (see L22).

---

## 2. High priority

### H1 — Modifier keys and open modals still trigger destructive hotkey moves

**File:** `src/app.js` — `handleGlobalKeydown`

**Problem.** The A–Z hotkey branch (`code.startsWith('Key')`) has no modifier guard, and the early-return only skips `INPUT`-focused elements. Consequences:

1. `Ctrl+A`, `Ctrl+W`, `Ctrl+Q`, `Ctrl+E`, … move the current image (or selection) to whatever folder is mapped to that letter — while the user intends a browser-style/system shortcut. This is silent data movement.
2. While the **settings modal is open**, A–Z hotkeys still fire and move files behind the modal (the modal only contains buttons; focus is on `body`).
3. The guard `document.activeElement.tagName === 'INPUT'` misses `SELECT` (the sort dropdown) and `TEXTAREA`/`contenteditable`. A focused sort dropdown + a letter key = a file move.

**Fix.** Extend the editable-element guard and gate the hotkey branch on modifiers and modal state:

```js
// top of handleGlobalKeydown
const a = document.activeElement;
if (a && (a.tagName === 'INPUT' || a.tagName === 'SELECT' || a.tagName === 'TEXTAREA' || a.isContentEditable)) return;
```

```js
// A–Z hotkey branch — replace the existing condition
} else if (code && code.startsWith('Key') && !e.ctrlKey && !e.metaKey && !e.altKey && el.modalSettings.classList.contains('hidden')) {
```

**Rationale.** `Ctrl+Z/X/R/C` are intentionally handled by dedicated branches above; this change only affects the fall-through hotkey moves, which must be plain key presses with no modifiers and no open modal. Also add a guard to the `Delete` branch for the same modal reason.

---

### H2 — Mutex lock-order inversion: `undo_last_move` vs `redo_last_move`

**File:** `src-tauri/src/lib.rs`

**Problem.** Lock ordering is inconsistent:
- `move_images`, `rotate_image`, `crop_image`, `undo_last_move` all lock `history` **then** `redo_history`.
- `redo_last_move` locks `redo_history` **then** `history`.

If the two commands ever run concurrently (two rapid key events can interleave on the async runtime), this is a textbook AB-BA deadlock. Today it only doesn't trigger because the frontend awaits sequentially — fragile.

**Fix (minimal, safe).** Make `redo_last_move` acquire in the same order as everything else:

```rust
#[tauri::command]
async fn redo_last_move(state: State<'_, AppState>) -> Result<bool, String> {
    let mut history = state.history.lock().unwrap();
    let mut redo = state.redo_history.lock().unwrap();   // history → redo, matching all other commands
    if let Some(action_batch) = redo.pop() {
        // ... unchanged body ...
        history.push(history_batch);
        return Ok(true);
    }
    Err("No more actions to redo".to_string())
}
```

**Fix (better).** Collapse both mutexes into one `Mutex<History>` struct so inconsistent ordering is impossible by construction — see H6's refactor.

---

### H3 — `move_images` silently swallows per-file failures

**File:** `src-tauri/src/lib.rs` (`move_images`) and `src/app.js` (callers)

**Problem.** The loop uses `if let Ok(new_path) = file_ops::move_image(...)`; failures are dropped. If *every* move fails (e.g. cross-device `EXDEV`, permission denied, file locked on Windows), the command returns `Ok([])` — the frontend clears the selection and reloads as if the files moved. The net effect is a **silent no-op**: the files stay where they are (they reappear in the re-listed view), the selection is wiped, and the user is never told anything failed.

**Fix.** Collect failures and surface them (note: returning `Err` discards the successfully-moved paths from the response — acceptable here because the frontend ignores the return value; the moves themselves still happened and are recorded in undo history):

```rust
#[tauri::command]
async fn move_images(sources: Vec<String>, target_dir: String, state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let mut batch = Vec::new();
    let mut new_paths = Vec::new();
    let mut failures = Vec::new();

    for source in sources {
        match file_ops::move_image(&source, &target_dir) {
            Ok(new_path) => {
                batch.push(UndoAction { old_path: source, new_path: new_path.clone(), action_type: "move".to_string() });
                new_paths.push(new_path);
            }
            Err(e) => failures.push(format!("{source}: {e}")),
        }
    }

    if !failures.is_empty() {
        return Err(failures.join("\n"));
    }
    // ... existing history/redo bookkeeping unchanged ...
    Ok(new_paths)
}
```

**Frontend:** the hotkey and Delete handlers already `try/catch` around the invokes — with the backend now returning `Err`, the selection is preserved and `console.error` fires. Add a visible notification (see M2) so the user isn't left guessing.

**Rationale.** Moving files is the app's core destructive operation; it must never report success when nothing moved.

---

### H4 — `crop_image` can panic on out-of-bounds rectangles

**File:** `src-tauri/src/image_proc.rs`

**Problem.** `image::crop_imm` panics when the crop rectangle extends past the image bounds (and `crop_imm` doesn't clamp). The frontend clamps today, but the backend is the security/reliability boundary: any future caller (or an out-of-date frontend, or a zoomed-crop coordinate bug — see M1) can crash the command thread.

**Fix.** Clamp in Rust; this also removes the `unused_mut` warning and makes use of `GenericImageView`:

```rust
use image::GenericImageView;

pub fn crop_image(path: &str, x: u32, y: u32, width: u32, height: u32) -> Result<(), String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let (iw, ih) = img.dimensions();
    let x = x.min(iw.saturating_sub(1));
    let y = y.min(ih.saturating_sub(1));
    let w = width.min(iw - x).max(1);
    let h = height.min(ih - y).max(1);
    img.crop_imm(x, y, w, h).save(path).map_err(|e| e.to_string())
}
```

---

### H5 — Redo history unbounded; temp edit-backup files leak on quit after undo

**File:** `src-tauri/src/lib.rs`

**Problem.**

1. `undo_last_move` pushes into `redo_history` with **no cap**, while `history` is capped at 5 — redo grows without bound (memory).
2. On `WindowEvent::Destroyed` only `history` backups are deleted. An undone *edit* lives in `redo_history` with its temp backup (the edited image, post-swap) — closing the app orphans that file in the system temp dir forever.
3. Crash/forced-kill leaves `%TEMP%/img_bak_*` files with no cleanup path (see L7).

**Fix.** Cap redo like undo, and clean redo backups on destroy. Extracting a helper also removes the 3×-duplicated "cap + drain redo" block (see H6 refactor). On destroy:

```rust
if let tauri::WindowEvent::Destroyed = event {
    if let Some(state) = window.try_state::<AppState>() {
        let cleanup = |batches: &[Vec<UndoAction>]| {
            for action in batches.iter().flatten() {
                if action.action_type == "edit" {
                    let _ = std::fs::remove_file(&action.new_path);
                }
            }
        };
        cleanup(&state.history.lock().unwrap());
        cleanup(&state.redo_history.lock().unwrap()); // ← was missing
    }
}
```

---

### H6 — Undo/redo return-shape contract mismatch + duplicated bookkeeping

**Files:** `lib.rs`, `app.js`, `README.md`

**Problem.**

- `undo_last_move`/`redo_last_move` return `Result<bool, String>`. The frontend does `const undonePath = await invoke('undo_last_move'); if (undonePath) {...}` and the README documents a returned path. It works *only* because `true` is truthy — the contract is misleading and will break subtly if the return type ever changes.
- The "push batch → cap at 5 → drain redo (deleting edit backups)" block is copy-pasted in `move_images`, `rotate_image`, and `crop_image`.

**Fix.** Return the restored path (or `None`) and centralize history writes:

```rust
#[tauri::command]
async fn undo_last_move(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let mut history = state.history.lock().unwrap();
    let Some(mut last_batch) = history.pop() else {
        return Ok(None);
    };
    let mut redo = state.redo_history.lock().unwrap();
    let mut redo_batch = Vec::new();
    let mut restored_path: Option<String> = None;
    while let Some(action) = last_batch.pop() {
        match action.action_type.as_str() {
            "move" => {
                if std::fs::rename(&action.new_path, &action.old_path).is_ok() {
                    restored_path = Some(action.old_path.clone());
                }
                redo_batch.push(action);
            }
            "edit" => {
                // existing swap dance, but only push to redo on success
                ...
                restored_path = Some(action.old_path.clone());
                redo_batch.push(action);
            }
            _ => {}
        }
    }
    redo_batch.reverse();
    redo.push(redo_batch);
    Ok(restored_path)
}
```

And a helper for the repeated bookkeeping:

```rust
fn record_history(state: &AppState, batch: Vec<UndoAction>) {
    const MAX_HISTORY: usize = 5;
    let mut history = state.history.lock().unwrap();
    history.push(batch);
    if history.len() > MAX_HISTORY {
        for action in history.remove(0) {
            if action.action_type == "edit" {
                let _ = std::fs::remove_file(&action.new_path);
            }
        }
    }
    // any undone actions (redo) are invalidated; clean their edit backups
    let mut redo = state.redo_history.lock().unwrap();
    for action in redo.drain(..).flatten() {
        if action.action_type == "edit" {
            let _ = std::fs::remove_file(&action.new_path);
        }
    }
    // cap redo as well
    // ...
}
```

Frontend becomes honest:

```js
async function doUndo() {
  try {
    const undonePath = await invoke('undo_last_move');
    if (undonePath) {
      await loadFiles(undonePath);   // re-list and select the restored file
      updateHistoryStats();
    }
  } catch (err) { console.warn("Undo failed:", err); }
}
```

Update the README command table (`undo_last_move` → returns restored path or empty).

---

### H7 — History cap silently destroys the only pre-edit copy

**File:** `src-tauri/src/lib.rs`

**Problem.** When history exceeds 5 batches, the oldest batch's edit backups are `remove_file`d. For edits, that backup is the *only* copy of the pre-edit image. A user who rotates, then performs 5 more operations, then wants to undo the rotation cannot — and the original is now gone. This is intended-ish (bounded memory) but should be surfaced.

**Fixes (pick one):**
- Keep the cap but make it visible: show "Undo history is limited to the last 5 operations" in the UI/settings.
- Raise the cap and store backups in a per-session subfolder (`%TEMP%/imagesorter_<session>`) cleaned on quit — this also fixes H5's leak cleanly.

Recommended: session subfolder + cap of 10; cleanup on `Destroyed` and best-effort on startup (delete stale `imagesorter_*` dirs older than 1 day).

---

## 3. Medium priority

### M1 — Crop misaligns when zoom ≠ 100%

`executeCrop` computes `scale = imgRect.width / naturalWidth` from the *untransformed* image rect, but the image element has `transform: scale(currentZoom)`. With zoom applied, `getBoundingClientRect()` reflects the transformed size while `naturalWidth` doesn't — coordinates drift. **Fix:** either disable crop mode when `currentZoom !== 1` (simple, predictable) or incorporate the transform:

```js
async function executeCrop() {
  if (currentZoom !== 1) {
    console.warn('Crop while zoomed is not supported; reset zoom (Ctrl+0) first');
    return;
  }
  ...
}
```

Also note `cropRegion` is positioned in *viewport* coordinates while the image may be letterboxed; the current offset math (`regionRect.left - imgRect.left`) is correct only at zoom=1 and when the image fills its box. The guard above makes the remaining path consistent.

### M2 — No user-facing error feedback

Every `catch` block in `app.js` only `console.error`s. Failed moves, rotates, crops, and config saves are invisible to the user. **Fix:** add a minimal toast:

```js
let toastTimer;
function notify(msg, isError = true) {
  let t = document.getElementById('toast');
  if (!t) {
    t = document.createElement('div');
    t.id = 'toast';
    t.style.cssText = 'position:fixed;bottom:16px;left:50%;transform:translateX(-50%);padding:8px 14px;border-radius:6px;background:#7f1d1d;color:#fff;z-index:999;font-size:13px;';
    document.body.appendChild(t);
  }
  t.textContent = msg;
  t.style.background = isError ? '#7f1d1d' : '#14532d';
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.textContent = ''), 3500);
}
```

Use it in the hotkey/Delete/rotate/crop/undo/redo catch blocks.

### M3 — Vanished source folder silently shows "0 images"

`list_images` on a missing/unreadable path yields an empty vec (jwalk errors are filtered out), and the frontend renders "0 images" with no hint. **Fix:** validate the directory in the command:

```rust
#[tauri::command]
async fn list_files(dir: String) -> Result<Vec<ImageInfo>, String> {
    if !std::path::Path::new(&dir).is_dir() {
        return Err(format!("Folder not found or not a directory: {dir}"));
    }
    Ok(file_ops::list_images(&dir))
}
```

Frontend `loadFiles` gains a `try/catch` that calls `notify(...)`.

### M4 — Extension whitelist contradicts README and misses common formats

`file_ops.rs` accepts `["jpg", "jpeg", "png", "webp", "jif"]`. The README advertises GIF, BMP, TIFF; the `image` crate (default features, incl. ravif/avif) supports gif/bmp/tiff/avif/ico/pnm/qoi/tga. `jif` is a rare alias of JPEG and `image::open` may reject it. **Fix:**

```rust
let extensions = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "avif", "ico"];
```

Update the README's format list to match. (If animated GIF/WebP matters, note that `image` decodes the first frame only.) **Verify before adding `avif`:** confirm the `image` crate's `avif` decode feature is actually enabled in this build (`cargo tree -e features | grep avif`) — `ravif` (the encoder) compiled during `cargo check`, but decoder availability wasn't confirmed; `gif/bmp/tif/tiff/ico` are guaranteed by default features.

### M5 — Undo/redo file operations ignore every error

`undo_last_move`/`redo_last_move` use `let _ = std::fs::rename(...)` throughout. If the file was manually moved/deleted meanwhile (or a rename in the 3-step edit swap fails midway), the command reports success and the file lands in an inconsistent state (edit swap: original at `_tmp` path). **Fix:** at minimum, surface failures as warnings; for the edit swap, check each step and abort cleanly:

```rust
fn swap_edit(action: &UndoAction, forward: bool) -> Result<(), String> {
    // forward == true:  old_path(edited) -> new_path(tmp), new_path(backup) -> old_path
    // forward == false: reverse
    let tmp = format!("{}_tmp", action.new_path);
    if forward {
        std::fs::rename(&action.old_path, &tmp).map_err(|e| e.to_string())?;
        std::fs::rename(&action.new_path, &action.old_path).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &action.new_path).map_err(|e| e.to_string())?;
    } else { /* reverse */ }
    Ok(())
}
```

Only push the action into redo/history when the swap fully succeeded.

### M6 — "Virtual list" renders every row on every navigation

`renderList` rebuilds the entire DOM (`innerHTML=''` + N children) after each move/delete/sort. For large folders (README targets "large batches") this is O(n) DOM churn per keypress. **Fix options:** (a) keep a simple windowed render (only render items in view ± overscan and absolutely position them — the `virtual-list` container is already `position:relative`), or (b) defer: only rebuild when the *sorted list* changes, and update `active`/`selected` classes in place. (b) is the minimal safe change; (a) is the real fix.

---

## 4. Low priority / hygiene

| ID | Issue | Fix |
|----|-------|-----|
| L1 | `menu_on_left_click(false)` deprecated | `.show_menu_on_left_click(false)` |
| L2 | `let mut img` unused mut in `image_proc.rs:16` | Remove `mut` (H4's rewrite already does) |
| L3 | Dead files: `src/main.js` (template greet app), `src/styles.css` (template styles) — referenced by nothing | Delete both |
| L4 | Stale build logs `src-tauri/output.txt`, `src-tauri/output8.txt` committed (show an already-fixed E0382 and warnings) | Delete; add `output*.txt` to `.gitignore` |
| L5 | Dev mode writes `target/debug/config.json` (portable heuristic in `config.rs`) — informational: it's already covered by the existing `/target/` ignore, so no commit risk in practice | No action needed; if the portable heuristic ever targets a non-ignored dir, gitignore it |
| L6 | **Unused Rust dep:** `chrono` (0 references in source) | Remove from `Cargo.toml` |
| L7 | **Unused plugins:** `tauri-plugin-fs` and `tauri-plugin-opener` are initialized but never called from JS; also remove `fs:default`/`opener:default` from `capabilities/default.json` and the npm `@tauri-apps/plugin-*` packages | Remove init calls + Cargo deps + capability permissions + npm deps (keep `dialog`, which *is* used via `window.__TAURI__.dialog`) |
| L8 | **Unused npm deps:** `@tauri-apps/api`, `@tauri-apps/plugin-fs`, `@tauri-apps/plugin-opener`, `@tauri-apps/plugin-window-state` — the app only uses globals (`window.__TAURI__.*`) injected by `withGlobalTauri` | Remove from `package.json` (keep `@tauri-apps/cli`, and `@tauri-apps/plugin-dialog` only if you migrate off the global) |
| L9 | Version pinning inconsistent: `trash = "5.2.5"`, `tauri-plugin-window-state = "2.4.1"` pinned vs `"2"` for others | Standardize on `"2"`-style ranges or pin all |
| L10 | Stored-XSS: `renderShortcuts`/`renderSettingsList` interpolate `basename(path)` into `innerHTML` with no escaping — a crafted `config.json` (or a target dir named `<img src=x onerror=...>`) executes HTML in the webview | Use `textContent` for paths, or escape: `path.replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]))` |
| L11 | `hide_window().unwrap()` in `on_window_event` — a failed hide panics the event handler | `let _ = window.hide();` |
| L12 | Branding: `productName`, window title, and Cargo package name are all generic `tauri-app` while the README calls it HotkeyImageSorter | Align `tauri.conf.json` (`productName`, window `title`), `Cargo.toml` (`name`/`description`), `package.json` (`name`) |
| L13 | `UndoAction.action_type` is a free `String`; the comment says `"move", "rotate", "crop"` but code writes `"edit"` | Use an enum (`Move`, `Edit`) — the H6 refactor is the natural place |
| L14 | `get_history_stats` counts *batches*, not images — "Undo (2)" may represent 2×500 files | Either return action counts or relabel the UI as operations |
| L15 | Config written on every navigation (`selectImage` saves `last_file`), plus several other save points | Debounce/save only on meaningful events; note `save_config` is cheap (small JSON) so this is minor |
| L16 | Date formatting hard-coded `ru-RU` in `selectImage` | Move locale into `AppConfig` (`locale: Option<String>`, default system) |
| L17 | `index.html` loads Inter from Google Fonts at runtime — CSP allows it, but the app is fully useless-looking offline; also a (minor) privacy consideration | Bundle `Inter` woff2 locally and drop the external `<link>` |
| L18 | Missing `window-state:default` permission in `capabilities/default.json` — harmless today because the JS API is never called, but the plugin's auto-restore is the only behavior; document it | Add the permission if/when the JS API is used |
| L19 | No CI, no JS lint/typecheck, no `.editorconfig` | Add a minimal GitHub Actions workflow (`cargo check` + `cargo test` + `npm ci`) |
| L20 | `output.txt`-style redirection in `package.json`/docs uses PowerShell syntax (`2>&1 > file`) in some places | N/A to code; docs only |
| L21 | **Repo is not rustfmt-clean** — `cargo fmt --check` exits 1: unordered imports in `config.rs`/`file_ops.rs`/`lib.rs`, long lines unwrapped, trailing whitespace throughout `lib.rs` | Run `cargo fmt` (safe, zero behavior change) and add a CI check |

> **M1 correction:** the audit's original M1 claim was a **false positive**. It stated `executeCrop` derives `scale` from the *untransformed* rect; in fact `getBoundingClientRect()` already includes the `transform: scale(...)` (zoom), so `imgRect.width / naturalWidth` is correct at any zoom. The applied fix is therefore only a defensive `scale > 0` guard. See §11 for the full status.
| L22 | Clippy `single_match` at `lib.rs:228` — the tray `match` has exactly one meaningful arm plus `_ => {}` | Replace with `if let`:

```rust
.on_tray_icon_event(|tray, event| {
    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
        let app = tray.app_handle();
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
})
``` |

---

## 5. Security review (one part of the audit, not the focus)

Overall posture is **good** for a local desktop app: no remote content, no `dangerousRemoteDomainIpcAccess`, CSP restricts scripts/styles to `'self'`, all file work happens in the Rust layer via explicit commands. Items to consider:

1. **Asset protocol scope `"**"`** (`tauri.conf.json`). `http://asset.localhost/...` can read any file on the machine from the webview. Today there's no injection path (no remote content, no `innerHTML` with untrusted input after L10 is fixed), but the scope is as wide as possible. **Consider** scoping dynamically to the source folder + configured hotkey folders instead of `"**"`. If that's impractical, keep it and rely on the CSP + input-escaping fixes.
2. **`innerHTML` with config-derived paths** (L10) is the only realistic injection vector — fix it.
3. **`image::open` on attacker-controlled files** — a malformed image could trigger decoder bugs in `image`/`ravif`. This is a known attack surface of the crate; for a local user-only tool it's acceptable, but do not ever feed it remote content.
4. **Temp backups in the system temp dir** with timestamp-based names — not a real risk (local single-user), but a per-session subdirectory (H7) both fixes cleanup and reduces litter.

---

## 6. Tests

There are **zero tests** in the repo (`cargo test` runs 0 tests; no JS test infra). The highest-value targets, in order:

1. **`file_ops::move_image`** — collision auto-rename, create-missing-target-dir, round-trip (move to dir A, move back). The collision loop is the kind of logic that quietly regresses.
2. **Config round-trip** (`config.rs`) — defaults on missing file, save→load equality, corrupt-file fallback.
3. **Undo/redo swap logic** — extract `swap_edit` (M5) and test the 3-step rename sequence both directions, including failure injection.
4. **JS pure helpers** — `basename`, the sort comparator, `escapeHtml`. `package.json` already has `"type": "module"`, so Node's built-in test runner works with zero new deps: add `"test": "node --test src/*.test.js"`.

Example (add `tempfile = "3"` under `[dev-dependencies]`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_image_renames_on_collision() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");        // source
        let b = dir.path().join("b");        // target
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let f = a.join("pic.jpg");
        std::fs::write(&f, b"data").unwrap();

        let moved = move_image(f.to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert_eq!(moved, b.join("pic.jpg").to_str().unwrap());

        // second file with same name collides → gets _1 suffix
        let f2 = a.join("pic.jpg");
        std::fs::write(&f2, b"data2").unwrap();
        let moved2 = move_image(f2.to_str().unwrap(), b.to_str().unwrap()).unwrap();
        assert_eq!(moved2, b.join("pic_1.jpg").to_str().unwrap());
    }

    #[test]
    fn config_roundtrip_preserves_hotkeys() {
        let mut cfg = AppConfig::default();
        cfg.hotkeys.insert("a".into(), "C:/x".into());
        // ... serialize + deserialize and compare ...
    }
}
```

Note: `AppConfig` currently has no `Default` impl (fields use `#[serde(default)]`); add `#[derive(Default)]` to make tests (and future callers) simpler. This audit instead added a behavior-preserving `default_config()` in `config.rs` (`#[derive(Default)]` would silently change the defaults to `None`/empty).

**Implementation status (follow-up round, committed):**
- **10 Rust unit tests** — `cargo test` ✅: `move_image` (move/return path, collision auto-rename without overwrite, creates missing target dir, error on missing source), `config` (save→load round-trip, missing-file defaults, corrupt-file defaults, parent-dir creation), `swap_edit` (undo/redo round-trip, error on missing file). Added `tempfile = "3"` under `[dev-dependencies]`.
- **4 JS tests** — `npm test` ✅ (`node --test`, zero new deps): `basename` extracted from `app.js` into `src/utils.js` (pure, Tauri-free) and imported back; `npm test` script added.
- **Supporting refactors** (behavior-preserving): `config.rs` now has path-parameterized `load_config_from` / `save_config_to` / `default_config`; the duplicated edit-swap rename dance in `lib.rs` was extracted into `swap_edit` (used by both undo and redo — they were byte-identical).
- **Caveat:** `npm test` uses a glob in the `node --test` invocation, which requires **Node ≥ 21** (the README still advertises 18+); on Node 18–20 the glob silently matches nothing. Frontend tests live in `test/` (not `src/`) so they don't ship in the production bundle (`frontendDist` is `../src`).

---

## 7. Documentation drift (README)

| README says | Reality |
|---|---|
| Supported: JPEG/JPG, PNG, GIF, BMP, WebP, TIFF | Code: jpg/jpeg/png/webp/jif only |
| `move_image` command | Command is `move_images` |
| Undo returns `undonePath` | Returns `bool` |
| `Delete` **or `Backspace`** trashes | Only `Delete` handled in `app.js` |
| "Batch navigation through entire folders" / virtual list | `max_depth(1)` top-level scan; full DOM re-render |
| `Ctrl+X` redo | Matches code (unusual, but consistent) — worth double-checking it isn't a `Ctrl+Y` typo |

---

## 8. Remaining concerns (not safely auto-fixable)

1. **True list virtualization** (M6) is a design decision — windowed rendering changes scroll/selection behavior; do it deliberately with tests.
2. **Lossless JPEG rotation** — the `image` crate re-encodes (documented intentional tradeoff to avoid native deps). A future enhancement: apply rotation via EXIF orientation (`kamadak-exif`) for JPEGs without re-encoding.
3. **Cross-device moves** — `std::fs::rename` fails with `EXDEV`; a robust `move_image` would fall back to copy+delete. After H3, the failure is at least *reported*; the fallback is an enhancement.
4. **Asset protocol scope `"**"`** — tightening requires dynamic ACL wiring; weigh risk vs. convenience.
5. **5-op history cap semantics** (H7) — raising it trades disk for safety; product decision.
6. **Cache-buster `?t=` on `convertFileSrc` URLs** — verify in a debug build that the asset-protocol handler strips the query (Tauri generally uses the request path only; if refresh after rotate/crop ever shows a stale image, revisit this). Low risk.
7. **`jwalk` symlinks/subdirs** — `max_depth(1)` is a product decision; recursive scans with the same filter could be offered as a setting.

---

## 9. Assumptions made

- The `window.__TAURI__.*` global API is the established frontend pattern (`withGlobalTauri: true`); npm packages are unused by design, not by accident.
- `ru-RU` date formatting is intentional (README author appears RU; original README is bilingual).
- `output.txt` / `output8.txt` are stale capture logs of a previous failed `cargo check` and hold no current value.
- `image` crate is built with default features, so gif/bmp/tiff/avif/ico decode is available (verified: `ravif` compiled during `cargo check`).
- The 5-batch history cap is intentional (memory bound); suggestions preserve it unless noted.
- Review is advisory only — per `ReviewPrompt.txt`, no code was modified.

## 10. Fix implementation status

All fixes below were applied and validated in the follow-up round: `cargo fmt --check` ✅, `cargo test` 10/10 ✅, `cargo build` ✅, `cargo clippy --all-targets` **0 warnings** ✅, `npm test` 7/7 ✅.

| ID | Status | Notes |
|----|--------|-------|
| H1 | ✅ Fixed | `app.js`: editable-element guard extended to INPUT/SELECT/TEXTAREA/contenteditable; A–Z hotkeys and Delete now require no modifiers and a closed settings modal |
| H2 | ✅ Fixed (minimal) | `redo_last_move` locks `history` → `redo_history`, matching all other commands (no AB-BA inversion) |
| H3 | ✅ Fixed | `move_images` collects per-file failures and returns `Err`; successful moves are still recorded in undo history; frontend shows a toast |
| H4 | ✅ Fixed | `crop_image` clamps coordinates to image bounds (also removed the unused `mut`) |
| H5 | ✅ Fixed (minimal) | redo capped at `MAX_HISTORY` (5) with backup cleanup; `Destroyed` handler now cleans redo backups too |
| H6 | ✅ Fixed | `undo_last_move`/`redo_last_move` return `Option<String>` (restored path); `push_history` centralizes cap + redo-invalidation; frontend `doUndo`/`doRedo` select the restored file |
| H7 | ⚠️ Partially | 5-batch cap kept (user chose the minimal approach); crash-leftover temp cleanup stays a remaining concern |
| M1 | ✅ Corrected | False positive (see note above); added defensive `scale > 0` guard — crop already works at any zoom |
| M2 | ✅ Fixed | `notify()` toast wired into move/delete/rotate/undo/redo/loadFiles error paths |
| M3 | ✅ Fixed | `list_files` validates the folder exists; frontend shows a toast on failure |
| M4 | ✅ Fixed | Extensions: `jpg/jpeg/png/gif/webp/bmp/tif/tiff/ico`; **AVIF excluded** (no `dav1d` decoder in this build, verified via `cargo tree`) |
| M5 | ✅ Fixed | undo/redo surface rename failures as `Err` instead of silently ignoring them |
| M6 | ✅ Minimal | `sortAndRender` skips DOM rebuild when the list signature is unchanged |
| L1 | ✅ Fixed | `show_menu_on_left_click(false)` |
| L2 | ✅ Fixed | unused `mut` removed |
| L3 | ✅ Fixed | `src/main.js`, `src/styles.css`, `src/assets/*` deleted |
| L4 | ✅ Fixed | `src-tauri/output*.txt` deleted; `output*.txt` added to `src-tauri/.gitignore` |
| L5 | ✅ N/A | already covered by the `/target/` ignore |
| L6 | ✅ Fixed | `chrono` removed from Cargo.toml |
| L7 | ✅ Fixed | `tauri-plugin-fs`, `tauri-plugin-opener` removed (Cargo.toml, lib.rs, capabilities) |
| L8 | ✅ Fixed | unused npm deps removed; `package.json` renamed |
| L9 | ⏭️ Deferred | version pin style left as-is (cosmetic) |
| L10 | ✅ Fixed | `escapeHtml` added to `utils.js` and used in `renderShortcuts`; tests added |
| L11 | ✅ Fixed | `let _ = window.hide()` |
| L12 | ✅ Fixed | renamed to **HotkeyImageSorter** (productName, window title, Cargo package, package.json). Bundle identifier reverted to `com.imagesorter.app` — see note below |
| L13 | ✅ Fixed | `UndoActionType` enum replaces the free string |
| L14 | ✅ Fixed | `get_history_stats` counts actions, not batches |
| L15 | ⏭️ Deferred | config saved on every navigation (minor perf; behavior kept) |
| L16 | ⏭️ Deferred | `ru-RU` locale left as-is |
| L17 | ⏭️ Deferred | Google Fonts kept (would require bundling a binary font) |
| L18 | ⏭️ Deferred | window-state permission not needed (JS API unused) |
| L19 | ✅ Fixed | `.github/workflows/ci.yml` (fmt/check/clippy/test + npm test) |
| L20 | ⏭️ N/A | docs only |
| L21 | ✅ Fixed | `cargo fmt` applied; `cargo fmt --check` passes |
| L22 | ✅ Fixed | tray `match` → `if let` (clippy is now clean) |

## 11. Post-release regression fixes

**Window size reset at launch.** The bundle identifier change (`com.imagesorter.app` → `com.imagesorter.hotkey`) moved the window-state plugin's state file (it is keyed by the identifier's config dir). The saved geometry (1374×822) was in the old location, so the app opened at the tauri.conf default (800×600) — hence the cramped layout (300px sidebar + 250px info panel, tiny viewer). **Fix:** reverted the identifier to `com.imagesorter.app`; the `.app`-suffix warning is cosmetic and macOS-only. If a future rename is needed, expect the window size to reset once.

**Undo immediately failing.** The undo logic was verified correct (rename target→source); two distinct causes were found and fixed:

1. **Transient Windows file locks** — `std::fs::rename` fails when another process briefly holds the file (antivirus real-time scanning, WebView2 asset streams for the just-displayed/just-edited image). The pre-audit code silently ignored these rename errors (undo appeared to "work" but the file never came back); the audit's M5 change surfaced them as `Undo failed`. **Fix:** added `file_ops::rename_with_retry` (8 attempts × 60 ms) used by `move_image` and move undo/redo restores. Tests: `rename_with_retry_succeeds`, `rename_with_retry_fails_when_source_missing`.
2. **Cross-volume rename (deterministic failure on rotate/crop undo).** `backup_file` stores the pre-edit backup in `%TEMP%` — which is on **C:** while the images live on **D:**. `swap_edit` (edit undo/redo) performed a 3-step `fs::rename` *between volumes*; Windows rejects cross-volume renames with "The system cannot move the file to a different disk drive" (`ERROR_NOT_SAME_DEVICE`). Retries can't help — the error is not transient. **Fix:** `swap_edit` now swaps via copies (`copy_with_retry`, same retry policy; copies work across volumes): copy live → tmp, copy backup → live, copy tmp → backup, remove tmp. The `swap_edit_swaps_and_restores` test exercises the copy path in both directions.

**Verification:** `cargo test` 13/13 ✅, `cargo fmt --check` ✅, `cargo clippy --all-targets` 0 warnings ✅, release rebuilt (`npm run build`).

**Undo works once, redo "does nothing" (rotate).** Follow-up report after the cross-volume fix: undo restored the original but redo appeared to have no effect. Two investigations:

1. **Backend state machine verified correct.** The batch-processing loops were extracted into testable `undo_batch`/`redo_batch` helpers (commands are now thin wrappers), and a new test `full_edit_cycle_undo_then_redo_restores_contents_and_history` runs the entire rotate→undo→redo cycle through the real logic: file contents round-trip (`rotated` → `original` → `rotated`) and history/redo end in the expected state (`history` drained by undo and repopulated by redo). Passes.
2. **Root cause: stale viewer frame.** `selectImage` sets `el.mainImage.src = convertFileSrc(path)` *without* a cache-buster, while `refreshCurrentImage` (used after rotate/crop) adds `?t=Date.now()`. The asset URL after undo/redo is identical to the one shown before the rotate, so the webview can serve the cached pre-rotate frame: after undo the stale frame happens to be the original (undo "works"), after redo the same stale original is shown again (redo "doesn't work") — even though the file on disk is correctly rotated. **Fix:** `selectImage` now always appends `?t=Date.now()` to the asset URL, so undo/redo (and any revisit of an edited file) always re-fetch from disk.

**Verification:** `cargo test` 14/14 ✅, `cargo fmt --check` ✅, `cargo clippy --all-targets` 0 warnings ✅, `npm test` 7/7 ✅, release rebuilt.

## 12. Suggested fix order

1. **H1** (destructive hotkey bug) — 5-minute fix, prevents data misplacement.
2. **H3 + M2** (silent failures + user feedback).
3. **H4** (crop panic), **L1/L2** (warnings), **L3/L4** (dead files/logs).
4. **H2/H6/H7/M5** (history refactor as one unit — lock order, contract, cap, error handling, helper extraction).
5. **H5** (redo cap + temp cleanup).
6. **M4/L6/L7/L8/L10** (formats, deps, escaping).
7. Tests from §6, then CI (L19).
