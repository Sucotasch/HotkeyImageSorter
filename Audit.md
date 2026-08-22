# Audit.md — Full Engineering Review of HotkeyImageSorter

Date: 2026-08-22 · Reviewer: ZCode (automated senior-level review) · Commit reviewed: `eef9ef4` (on top of `85273a9`)

Scope: the entire repository — frontend (`src/`), Rust backend (`src-tauri/src/`), config (`tauri.conf.json`, capabilities), tests, CI, packaging and docs. **No code was changed by this review.** Every finding below comes with a ready-to-use fix; a junior developer should be able to apply them one by one without re-deriving the analysis.

---

## 1. Validation baseline (all green)

| Check | Command | Result |
|---|---|---|
| JS unit tests | `npm test` (Node v24.19.0) | **28 passed, 0 failed** |
| Rust tests | `cd src-tauri && cargo test` | **116 passed, 0 failed** (0.64s) |
| CI | `.github/workflows/ci.yml` | fmt + clippy `-D warnings` + check + test (Rust), npm test (Node 22) — well configured, ubuntu deps documented |

Overall assessment: this is a well-kept codebase. Error handling in the Rust file operations, metadata-preservation logic and its test coverage (including real-world regression fixtures) are notably good. The findings below are mostly about **robustness edges, internal inconsistencies and documentation drift**, not fundamental design flaws.

Severity counts: **6 High**, **12 Medium**, **12 Low**.

---

## 2. High-priority findings (correctness / data safety)

### H1. Config writes are non-atomic and can be concurrent → possible config corruption and silent loss of hotkeys

**Where:** `src-tauri/src/config.rs:57-65` (`save_config_to`), `src/app.js:679-680` (`selectImage`) and ~15 other `invoke('save_config', ...)` call sites.

**Problem:**
1. `save_config_to` uses a plain `fs::write` — a crash or power loss mid-write leaves a truncated `config.json`. `load_config_from` (`config.rs:47-54`) then silently falls back to `default_config()`, and the **next save overwrites the corrupt file — the user's 26 hotkey assignments are gone without a trace**.
2. `selectImage` fires a full config save on **every arrow keypress**. Each `invoke` becomes an independent async command; on Windows, Rust's `fs::write` opens with `FILE_SHARE_READ|WRITE`, so two concurrent writers can interleave truncate+write on the same path. Rapid navigation makes this a real race, not a theoretical one.

**Fix (three small parts, apply all):**

(a) Atomic write in Rust — temp file + rename (rename-over-existing is atomic on Windows via `MOVEFILE_REPLACE_EXISTING` in std):

```rust
/// Save a config to an explicit path, creating parent directories as needed.
/// Writes to a sibling temp file and renames it into place, so a crash
/// mid-write can never leave a truncated config.json behind.
fn save_config_to(path: &Path, config: &AppConfig) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let content = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, content).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}
```

(b) Serialize writers with a managed lock so concurrent commands queue instead of racing:

```rust
// lib.rs
/// Serializes config writes; config.json is saved wholesale and concurrent
/// fs::write calls could interleave on Windows.
#[derive(Default)]
struct ConfigLock(std::sync::Mutex<()>);

// in run():  .manage(ConfigLock::default())
// and change the command:
#[tauri::command]
async fn save_config(config: AppConfig, state: State<'_, ConfigLock>) -> Result<(), String> {
    let _guard = state.0.lock().unwrap();
    config::save_config(config)
}
```

(c) Debounce in the frontend so navigation stops hammering the disk (also fixes H2's noise):

```js
// app.js — replace every bare `invoke('save_config', { config: state.config });`
// with scheduleConfigSave() (immediate=true for settings/hotkey changes).
let saveConfigTimer = null;
function scheduleConfigSave(immediate = false) {
  if (immediate) {
    clearTimeout(saveConfigTimer);
    saveConfigTimer = null;
    invoke('save_config', { config: state.config })
      .catch(err => notify('Failed to save settings: ' + err));
    return;
  }
  if (saveConfigTimer) return;
  saveConfigTimer = setTimeout(() => {
    saveConfigTimer = null;
    invoke('save_config', { config: state.config })
      .catch(err => notify('Failed to save settings: ' + err));
  }, 500);
}
```

**Rationale:** the config is the only persistent user state (hotkeys = significant setup effort); protecting it is cheap. `last_file` navigation saves only need the debounced path; Options-modal changes should pass `immediate = true`. Losing the last debounced flush on a hard close costs only "which file was last viewed" — acceptable.

**Test to add:** `save_config_to` interrupted mid-write leaves either old or new complete file (can be simulated by asserting the temp file is used: save, then assert no `*.json.tmp` remains and content round-trips).

---

### H2. All `save_config` errors are swallowed — settings may silently never persist

**Where:** every `invoke('save_config', ...)` in `src/app.js` (lines 166, 171, 175, 184, 209, 263, 269, 301, 323, 680, 1346) — fire-and-forget, no `.catch`.

**Problem:** `config_path()` (`config.rs:9-29`) picks the portable path next to the exe whenever it is "not under Program Files" — it does **not** verify writability. Running the exe from a read-only location (network share, optical media, a locked-down folder) makes every save fail; the user changes settings, sees the toggle move, and everything is gone on restart. No feedback anywhere.

**Fix:** the `scheduleConfigSave()` wrapper in H1 already routes every save through one place with a `.catch` + toast. Additionally, surface the first failure prominently:

```js
let configSaveFailed = false;
// inside the .catch:
if (!configSaveFailed) {
  configSaveFailed = true;
  notify('Settings cannot be saved (config location is read-only?) — ' + err);
}
```

**Rationale:** minimal change, converts a silent data-loss mode into a visible one. (Optionally `config_path()` could probe writability and fall back to AppData — that is a bigger behavioural change; see §7.)

---

### H3. `executeCrop` has no error handling — failed crops vanish silently

**Where:** `src/app.js:1299-1322`.

**Problem:** `await invoke('crop_image', ...)` is called without `try/catch` from a `dblclick` handler. If the backend fails (file locked by another process — exactly what the retry helpers exist for), the rejection is unhandled: no toast, no console-visible feedback for the user, and the code after the `await` (toggle/refresh/history stats) never runs. The user double-clicks and *nothing happens*.

**Fix:**

```js
async function executeCrop() {
  const imgRect = el.mainImage.getBoundingClientRect();
  const regionRect = el.cropRegion.getBoundingClientRect();
  // ... existing coordinate math unchanged ...
  try {
    await invoke('crop_image', { path: state.currentPath, x: finalX, y: finalY, width: finalW, height: finalH });
  } catch (err) {
    notify("Crop failed: " + err);
    return; // keep the crop region active so the user can retry
  }
  toggleCrop();
  refreshCurrentImage();
  updateHistoryStats();
}
```

This mirrors the error-handling pattern of `doRotate`/`doConvert` — the omission looks like an oversight, not a choice.

---

### H4. `loadFiles` error path leaves a stale list signature → unrecoverable scroll on the next identical load

**Where:** `src/app.js:349-358` (catch block) vs `sortAndRender` (`app.js:416-423`).

**Problem:** the success path resets `lastListSignature = ''` before rebuilding; the catch path does not. Sequence: load folder A (signature `S_A` stored) → folder becomes unreadable → error render empties the list **and sets the spacer height to 0** (`renderList` with 0 images) while `lastListSignature` still holds `S_A` → folder becomes readable again → reload produces the identical path list → `sortAndRender` sees `signature === lastListSignature`, **skips `renderList`** → rows are re-rendered by `renderWindow`, but the spacer keeps height 0 → the list shows one viewport worth of rows and **cannot be scrolled**.

**Fix** (one line, plus the filter rebuild for symmetry):

```js
} catch (err) {
    state.allImages = [];
    state.images = [];
    promptPaths.clear();
    promptChecked.clear();
    lastListSignature = '';
    rebuildFilterOptions();
    renderList();
    el.fileCount.textContent = '0 images';
    updateMetaCorner();
    notify("Cannot load folder: " + err);
}
```

---

### H5. `convert_pngs` busy flag leaks on a worker panic — conversions blocked until app restart

**Where:** `src-tauri/src/lib.rs:263-274`.

**Problem:** `busy.store(false, ...)` sits *after* `run_conversion(...)` inside the `spawn_blocking` closure. If `run_conversion` panics (e.g. `report.lock().unwrap()` on a poisoned mutex after a worker panic), the store never executes, the `JoinError` propagates as an Err to the frontend — and `busy` stays `true` **forever**: every future click on PNG→JPG returns "A conversion is already running" until the app restarts.

**Fix** — RAII guard, panic-safe by construction:

```rust
/// Resets the conversion busy flag on drop — including after a panic inside
/// `run_conversion`, which would otherwise leave conversions disabled until
/// the app restarts.
struct BusyGuard(Arc<AtomicBool>);
impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
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
        let _busy = BusyGuard(busy); // cleared on any exit path
        run_conversion(paths, replace, cancel, {
            let app = app_handle.clone();
            move |p| {
                let _ = app.emit("convert-progress", p);
            }
        })
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(result)
}
```

---

### H6. Crop mode survives image changes — double-click can crop the wrong file with a stale region

**Where:** `src/app.js:673-719` (`selectImage`), `doConvert` (`app.js:1054-1108`).

**Problem:** crop state (`isCropActive`, `cropFrac`, overlay) is never cancelled when `currentPath` changes. Reproduction: activate crop on `img.png` → press **Ctrl+J** with "Replace source" on and nothing selected → the PNG is converted and trashed → `loadFiles` selects a *different* image → the crop overlay is still active, repositioned over the new image → double-click **crops the new file** using a region chosen for the old one. The same stale-state path exists after Delete/A–Z move of the cropped file.

**Fix** — deactivate crop whenever the displayed file changes under it:

```js
async function selectImage(index) {
  if (index < 0 || index >= state.images.length) return;
  const img = state.images[index];
  // The crop region was chosen for a different file — never let it crop
  // whatever file happens to be shown next (e.g. after PNG→JPG replace).
  if (state.isCropActive && img.path !== state.currentPath) {
    toggleCrop();
  }
  state.currentIndex = index;
  // ... rest unchanged
```

(Intentional behaviour preserved: crop *staying* active across rotate/undo of the **same** file still works — `cropFrac` is fraction-based and `positionCropRegion` re-glues on load.)

---

## 3. Medium-priority findings (robustness, consistency, maintainability)

### M1. Every toast renders in error red — including success messages

**Where:** `src/style.css:743-762`.

`#toast`'s default background is `#7f1d1d` (dark red) and `#toast.error` is *the same colour*. `notify('Selection cleared', false)`, "Copied to clipboard", "Converted N PNG → JPG" etc. all look like failures.

**Fix:**

```css
#toast {
  /* ...existing rules unchanged... */
  background: #1f2937;                 /* neutral panel */
  border: 1px solid var(--border-color);
}
#toast.error { background: #7f1d1d; border-color: #b91c1c; }
```

### M2. Two different collision-naming conventions in the backend

**Where:** `file_ops.rs:238-242` (`move_image` → `name_1.jpg`) vs `file_ops.rs:177-192` / lib.rs conversion+export (`available_path` → `name (1).jpg`).

README documents the Explorer-style " (1)" suffix; the move hotkeys silently produce underscore names. `move_image` also re-implements the exists-loop inline instead of reusing `available_path`.

**Fix** — one shared helper, both call sites use it:

```rust
/// Returns a path in `dir` with the given full file name that does not exist
/// yet, appending Explorer-style " (1)", " (2)"… suffixes on collision. If
/// `dir` does not exist it is created first (like move_image does).
pub fn available_path_for(dir: &str, file_name: &str) -> Result<std::path::PathBuf, String> {
    let dir_path = Path::new(dir);
    if !dir_path.exists() {
        std::fs::create_dir_all(dir_path).map_err(|e| e.to_string())?;
    }
    if !dir_path.is_dir() {
        return Err(format!("Target is not a directory: {dir}"));
    }
    let p = Path::new(file_name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut target = dir_path.join(file_name);
    let mut count = 1;
    while target.exists() {
        target = dir_path.join(format!("{stem} ({count}){ext}"));
        count += 1;
    }
    Ok(target)
}

pub fn available_path(dir: &str, stem: &str, ext: &str) -> Result<std::path::PathBuf, String> {
    available_path_for(dir, &format!("{stem}.{ext}"))
}
```

In `move_image`, replace the inline loop (lines 224-242) with:

```rust
let file_name = source_path.file_name().ok_or("Invalid filename")?;
let target_path = available_path_for(&target_dir.to_string_lossy(), &file_name.to_string_lossy())?;
```

**Tests to update:** `move_image_renames_on_collision` expects `pic_1.jpg` → change to `pic (1).jpg`; add `available_path_for_preserves_double_extensions` if desired. Behaviour change is deliberate (standardising to the documented Explorer style) — mention it in the changelog/README RU+EN if collision naming is documented there.

### M3. Google Fonts loaded from the network on every launch

**Where:** `src/index.html:8`, `tauri.conf.json` CSP (`style-src … https://fonts.googleapis.com; font-src https://fonts.gstatic.com`).

A desktop app that phones home to Google on startup: adds startup latency offline (WebView2 waits/blocks on the stylesheet link), leaks usage metadata, and forces a permanently widened CSP for a font that has a system-stack fallback anyway.

**Fix:** vendor `Inter-*.woff2` (3 weights already used: 300/400/600) into `src/fonts/`, replace the `<link>` with:

```css
@font-face {
  font-family: 'Inter';
  src: url('fonts/inter-400.woff2') format('woff2');
  font-weight: 400; font-style: normal; font-display: swap;
}
/* repeat for 300 and 600 */
```

and tighten the CSP to `style-src 'self' 'unsafe-inline'; font-src 'self';` (`'unsafe-inline'` stays — inline `style.transform` writes are used by zoom/crop positioning).

### M4. `check_prompts` / `has_prompt_fast` read entire files into memory per badge scan

**Where:** `src-tauri/src/metadata.rs:433-454`.

The lazy badge scan reads the **whole file** for every unchecked row in the visible window (±20 overscan ⇒ up to ~60 files per scroll frame). A folder of 30 MB PNGs ⇒ ~1.8 GB of reads per scroll position (mitigated only by the OS cache). PNG chunk headers require walking the chunk stream, but JPEG metadata segments always precede SOS near the start of the file.

**Fix (staged, safest first):**
1. For the JPEG branch, read a bounded prefix (e.g. 2 MiB) via `File` + `take()` instead of `std::fs::read`. `extract_segments` already stops at SOS, so a truncated tail is harmless — a prefix without SOS simply yields the segments found, which is exactly what the badge needs.
2. For the PNG branch, `find_png_ai_chunk` can be converted to a streaming walk with `BufReader` + `seek` past `IDAT` lengths (it never looks at IDAT contents). Keep the full-read version as a fallback if seeking fails.

Suggested shape:

```rust
fn read_prefix(path: &str, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    (&mut f).take(max_bytes).read_to_end(&mut buf)?;
    Ok(buf)
}
```

**Trade-off:** a JPEG whose COM/APP1 metadata starts beyond 2 MiB would lose its badge (never observed in practice — encoders write markers first). If that risk is unacceptable, keep full reads and accept the I/O.

### M5. Blocking file I/O inside async commands

**Where:** `get_metadata`, `check_prompts`, `list_files`, `move_images`, `trash_images`, `rename_file`, `rotate_image`, `crop_image` (`src-tauri/src/lib.rs`).

All are `async fn` doing synchronous disk work on the tokio runtime threads. `convert_pngs` already does the right thing with `spawn_blocking`. The metadata commands are the worst offenders (full-file reads + parsing; `check_prompts` scans batches). Under load this can starve runtime workers and delay unrelated commands (e.g. the config saves from H1).

**Fix** — wrap the heavy ones:

```rust
#[tauri::command]
async fn get_metadata(path: String) -> Result<crate::models::ImageMetadata, String> {
    tauri::async_runtime::spawn_blocking(move || metadata::get_metadata(&path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn check_prompts(paths: Vec<String>) -> Vec<String> {
    match tauri::async_runtime::spawn_blocking(move || metadata::check_prompts(&paths)).await {
        Ok(v) => v,
        Err(_) => Vec::new(),
    }
}
```

Leave the trivially-fast commands (`list_files` on jwalk is already async internally? — no, it is sync; it is fast enough on an SSD but can also be wrapped the same way if desired).

### M6. Two divergent NaN/Infinity sanitizers

**Where:** `exif_writer.rs:24-94` (`sanitize_non_finite` — boundary-checked, matches ComfyUI's regex) vs `metadata.rs:1596-1640` (`sanitize_json_float_tokens` — no boundary checks, rewrites *any* `NaN`/`Infinity` bytes outside strings, e.g. inside a bare token like `NaNkey`).

The metadata one is used for parsing, the exif_writer one for writing/exporting. Same concept, two behaviours, double maintenance — and the weaker one is on the parse path where correctness matters just as much.

**Fix:** delete `sanitize_json_float_tokens` and route its three call sites (`metadata.rs:833`, `:866`, `:1312`, `:1646`) through `exif_writer::sanitize_non_finite`. The boundary checks make it strictly more correct; both functions have overlapping test coverage that will keep guarding the behaviour (`sanitize_removes_bare_nan_and_infinity`, `sanitizes_non_finite_tokens_like_comfyui`).

### M7. Context-menu extension list diverges from supported formats; `reg.exe` can flash a console window

**Where:** `src-tauri/src/context_menu.rs:15` (`IMAGE_EXTS` has 7 entries — `bmp`, `tif`, `tiff`, `ico` missing while `file_ops.rs` supports 11), and `context_menu.rs:33-43` (`Command::new("reg.exe")` without `CREATE_NO_WINDOW`).

Two consequences: BMP/TIFF/ICO files — fully supported by the app — get no Explorer integration (silent inconsistency users notice); and on a `windows_subsystem = "windows"` build, spawning a console subprocess without the no-window flag can flash a black console on every toggle of the Options checkbox.

**Fix:**

```rust
/// Image extensions that get the two context-menu items. Kept in sync with
/// file_ops::SUPPORTED_EXTENSIONS (asserted by a test below).
const IMAGE_EXTS: [&str; 11] = [
    "jpg", "jpeg", "jif", "png", "gif", "webp", "bmp", "tif", "tiff", "ico", "avif",
];

fn run_reg(args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("reg.exe");
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW); // no console flash from the GUI app
    }
    let out = cmd.output().map_err(|e| format!("failed to run reg.exe: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned().trim().to_string())
    }
}
```

Add a parity test so the lists can never drift again:

```rust
#[test]
fn image_exts_match_supported_extensions() {
    let mut a: Vec<_> = IMAGE_EXTS.to_vec();
    let mut b: Vec<_> = crate::file_ops::SUPPORTED_EXTENSIONS.to_vec();
    a.sort(); b.sort();
    assert_eq!(a, b, "context menu must cover every supported image type");
}
```

(This requires making `SUPPORTED_EXTENSIONS` `pub(crate)` — one-word change.)

### M8. `merge_missing_segments` dedup can drop distinct same-signature segments (ICC chains, sibling COM comments)

**Where:** `src-tauri/src/jpeg_meta.rs:88-109`.

Dedup key is `(marker, first 4 payload bytes)`. Multi-segment ICC profiles (large profiles are split across several APP2 segments, all starting `ICC_PROFILE\0`) and multiple COM comments sharing a 4-byte prefix collide on that key. Today's call sites make actual loss unlikely (dedup runs against the freshly-encoded target which has either all or none of the segments), but the function's contract silently invites data loss for any future caller that merges into a partially-populated JPEG.

**Fix** — dedup only where the standard allows one instance; compare full payloads for COM:

```rust
pub fn merge_missing_segments(jpeg: &[u8], originals: &[JpegSegment]) -> Vec<u8> {
    let existing = extract_segments(jpeg);
    let mut out = Vec::with_capacity(jpeg.len() + 1024);

    if jpeg.starts_with(&[0xFF, 0xD8]) {
        out.extend_from_slice(&[0xFF, 0xD8]);
        for seg in originals {
            // APP1 EXIF is single-instance by spec -> signature dedup is safe.
            // APP2 ICC chains are split across N segments -> never dedup.
            // Everything else (incl. COM) -> exact-content dedup.
            let dup = match seg.marker {
                0xE1 => existing.iter().any(|e| e.marker == seg.marker && e.sig() == seg.sig()),
                0xE2 => false,
                _ => existing.iter().any(|e| e.marker == seg.marker && e.data == seg.data),
            };
            if !dup {
                push_segment(&mut out, seg);
            }
        }
        out.extend_from_slice(&jpeg[2..]);
    } else {
        return jpeg.to_vec();
    }
    out
}
```

**Test to add:** two APP2 segments with `ICC_PROFILE\0` payloads both survive a merge into a bare JPEG.

### M9. Corrupt config is silently replaced — no recovery path for the user's hotkeys

**Where:** `src-tauri/src/config.rs:47-54`.

On a parse error the loader returns defaults and the *next* save overwrites the file (see H1 for how corruption can happen even after the atomic-write fix — e.g. disk-level damage or a user edit).

**Fix:** keep the corrupt file for manual recovery before falling back:

```rust
fn load_config_from(path: &Path) -> AppConfig {
    if let Ok(content) = fs::read_to_string(path) {
        match serde_json::from_str::<AppConfig>(&content) {
            Ok(config) => return config,
            Err(_) => {
                // Preserve the unreadable file so hotkeys can be recovered
                // manually instead of being overwritten by the next save.
                let _ = fs::rename(path, path.with_extension("json.corrupt"));
            }
        }
    }
    default_config()
}
```

**Test to add:** write garbage, `load_config_from` returns defaults **and** `config.json.corrupt` exists containing the garbage. (Adjust the existing `load_corrupt_file_returns_defaults` test accordingly.)

### M10. `swap_edit` can truncate the live image if the second copy fails mid-way

**Where:** `src-tauri/src/lib.rs:696-714`.

`copy(backup → live)` truncates `live` before writing; a mid-copy failure (disk full, source lock) leaves the live file half-written. The previous content survives in the `_tmp` file, but nothing wires that into recovery, and undo reports failure with the on-disk state corrupted.

**Fix** — stage next to the target, then rename (same-directory rename is atomic):

```rust
fn swap_edit(action: &UndoAction) -> Result<(), String> {
    let tmp = format!("{}_tmp", action.new_path);
    let staged = format!("{}.hkis-swap", action.old_path);
    let result = (|| {
        file_ops::copy_with_retry(std::path::Path::new(&action.old_path), std::path::Path::new(&tmp))?;
        // Stage the incoming bytes next to the live file, then rename: a
        // failed copy can never truncate the live file.
        file_ops::copy_with_retry(std::path::Path::new(&action.new_path), std::path::Path::new(&staged))?;
        if let Err(e) = file_ops::rename_with_retry(std::path::Path::new(&staged), std::path::Path::new(&action.old_path)) {
            let _ = std::fs::remove_file(&staged);
            return Err(e);
        }
        file_ops::copy_with_retry(std::path::Path::new(&tmp), std::path::Path::new(&action.new_path))
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}
```

The existing `swap_edit_*` tests keep passing unchanged (they only assert end states).

### M11. The conversion-report modal does not gate `Delete` / A–Z moves — contradicts the documented modal convention

**Where:** `src/app.js:801` and `:863` (both check only `settingsModalOpen()`), vs `AGENTS.md`/`knowledge.md`: *"Any new modal must join the `anyModalOpen()` guard that gates destructive hotkeys."*

The metadata modal deliberately leaves file-ops active (triage-by-prompt workflow, documented at `app.js:71-75` — good). But that comment's letter says "only the settings modal blocks them", which silently drags the **report** modal into the same behaviour: the report is a passive, purely informational dialog shown right after a batch conversion — having Delete/A–Z fire behind it is surprising and undocumented.

**Fix** — make the policy explicit instead of implied:

```js
// File-operation hotkeys (Delete, A–Z move) stay active while the metadata
// modal is open (triage by prompt presence without closing it). They ARE
// blocked by the settings modal (editable fields) and the conversion report
// (passive dialog over a possibly-changed file list).
function fileOpsAllowed() {
  return el.modalSettings.classList.contains('hidden') &&
         el.modalReport.classList.contains('hidden');
}
```

Replace `!settingsModalOpen()` at `app.js:801` and `app.js:863` with `fileOpsAllowed()` (keep `settingsModalOpen` if nothing else uses it — otherwise remove it), and update AGENTS.md/knowledge.md to name the actual policy.

### M12. Documentation drift (several stale claims)

| Where | Says | Reality |
|---|---|---|
| `knowledge.md:10` | "53 Rust tests · 20 JS tests" | 116 Rust · 28 JS |
| `knowledge.md:37` | `anyModalOpen()` = "settings OR metadata" | three modals since the report modal landed |
| `knowledge.md:19,40` + `AGENTS.md:20` | `src/main.js`, `src/styles.css` exist as dead template leftovers | the files no longer exist on disk (`src/` has 4 files) |
| `PLAN.md` | "Status: **PLANNING** (no code changed)" for H7/virtualization/lossless-rotate/metadata viewer | all shipped long ago (commits through 0.1.3) — stale planning doc |
| `src/index.html:5` | `<title>Minimal Sorter</title>` | product is HotkeyImageSorter everywhere else |
| Repo root | empty `Audit/` directory; empty `Errors.txt` (0 bytes, just committed) | leftover artifacts — delete |
| Repo root | `PNG/` — 140 MB of untracked images inside the repo tree | risk of an accidental `git add .` committing 140 MB; add `PNG/` to `.gitignore` or move it out of the repo |

**Fix:** update the four knowledge.md lines; retitle PLAN.md as historical or delete it; set the HTML title to `HotkeyImageSorter`; `rmdir Audit`, decide on Errors.txt, and gitignore `PNG/`.

---

## 4. Low-priority findings (polish)

**L1. `escapeAttr` duplicates `escapeHtml`.** `src/app.js:1504-1506`. `escapeHtml` (`utils.js:65-73`) already escapes `&<>"'`, which covers double-quoted attributes. Delete `escapeAttr`, use `escapeHtml` in `renderMetadata`. One escaping helper, tested once.

**L2. Multi-KB raw payloads embedded in HTML attributes.** `src/app.js:1428-1430` — `data-copy="${escapeAttr(ai.raw)}"` puts the full ComfyUI JSON (can be 60 KB+) into the DOM, once per button, and `renderMetadata` rebuilds that HTML on **every arrow-key navigation** while the modal is open. Store payloads in an array and reference by index:

```js
const metaPayloads = [];
// in renderMetadata(), for each button:
const idx = metaPayloads.push(ai.raw) - 1;
// template: <button class="btn-secondary" data-copy-idx="${idx}">
// listener:
const text = metaPayloads[Number(btn.dataset.copyIdx)];
```

(Reset `metaPayloads.length = 0` at the top of `renderMetadata`.) Noticeably snappier on ComfyUI-heavy folders.

**L3. Two byte-formatting helpers with different rounding.** `formatSizeMB` (`app.js:991`) vs `formatBytes` (`app.js:1497`) — and the sidebar uses a third inline variant (`app.js:696-697`). Move one `formatBytes` into `utils.js`, use everywhere, add a unit test (currently untested).

**L4. `package.json` has no `engines` field** although `npm test` needs Node ≥ 21 (`node --test` glob support). Add `"engines": { "node": ">=21" }`.

**L5. `rename_file` rejects invalid names only via OS errors.** `file_ops.rs:110-172` blocks separators and unsupported extensions (good) but Windows-invalid characters (`<>:"|?*`, trailing spaces, reserved device names like `CON`) surface as raw OS errors after the fact. Pre-validate for a friendly message:

```rust
const INVALID_CHARS: &[char] = &['<', '>', ':', '"', '|', '?', '*'];
if final_name.chars().any(|c| INVALID_CHARS.contains(&c)) || final_name.ends_with(' ') {
    return Err(format!("File name contains characters Windows does not allow"));
}
```

**L6. `get_history_stats` counts actions, UI labels imply batches.** `lib.rs:678-688` sums `b.len()` across batches; the buttons read "Undo (N)". A 30-file move shows "Undo (30)". Either document it or count batches (`history.len()`); counting batches matches "undo one operation" semantics better.

**L7. `ImageInfo.extension` stores original case; every consumer lowercases.** `file_ops.rs:38` + three `.toLowerCase()` in `app.js`. Normalize at the source (`ext.to_lowercase()`), delete the downstream lowercasing.

**L8. Window has no `minWidth`/`minHeight`** (`tauri.conf.json`) and defaults to 800×600 — tight for a 3-pane layout. Suggest `minWidth: 720, minHeight: 480, width: 1100, height: 720`.

**L9. No way to clear a hotkey** in Options (`app.js:1508-1536` — only "Set"). Add a small "✕" button next to assigned keys: `delete state.config.hotkeys[key]; renderSettingsList(); scheduleConfigSave(true);`.

**L10. `init()` has no top-level error handling** (`app.js:94-129`): if `get_config` or `get_launch_target` rejects, the app boots into a dead UI. Wrap in `try/catch` with a toast, and fall back to `state.config = { hotkeys: {} }`.

**L11. Hardcoded `ru-RU` date formatting** (`app.js:695`). Documented as intentional in knowledge.md — if that ever changes, use `navigator.language`. Listed only so the choice stays visible.

**L12. `updateSortButtonUI` uses inline `style` attributes** (`app.js:286-293`). Two CSS classes (`.sort-asc` / `.sort-desc`) would match the rest of the styling approach. Cosmetic only.

---

## 5. Test-quality assessment

**Rust (116 tests) — strong.** Real-world regression fixtures (civitai AIR graphs, Krea chains, overlapping-IFD EXIF, CP1251 comments) lock in the exact behaviours that previously broke. The conversion/undo/backup pipelines are tested through their real code paths with injected stubs — the right pattern.

**Gaps worth closing while applying the fixes above:**

1. **JS coverage stops at `utils.js`.** Pure-ish logic stranded in `app.js` is untested: `convertTargets()` (the PNG-filter rule), `filterImages`/`rebuildFilterOptions` interplay, `showReportModal`'s summary math (the `saved`/`pct` computation), and `escapeAttr`/`escapeHtml` attribute-safety. Recommendation: move `convertTargets` (take `(selectedPaths, currentPath)` as args) and a unified `formatBytes` into `utils.js` with tests — this also lands L3.
2. **`merge_missing_segments`** has no multi-segment-same-signature test (see M8).
3. **`config.rs`** has no corrupt-file-preservation test (see M9's new behaviour).
4. **`context_menu.rs`** has no ext-parity test (see M7).
5. **`file_ops.rs`** collision naming is asserted with the old underscore convention (see M2's test update).

**CI** is well configured (fmt, clippy `-D warnings`, tests both languages). The Rust job compiles the vendored dav1d — good. Nothing to fix there beyond keeping the ubuntu dependency list in sync if vendoring changes.

---

## 6. Security review

- **XSS hygiene: good.** Every interpolation into `innerHTML` in `renderMetadata`/`showReportModal`/`renderShortcuts` goes through `escapeHtml`/`escapeAttr`; list rows, paths, toasts and titles use `textContent`/property assignment. Keep the invariant "no unescaped string enters `innerHTML`" for any new modal (metadata values are attacker-controllable via crafted images — the existing code respects that).
- **CSP** is tight except for the Google-Fonts remote origins (M3). `assetProtocol.scope: ["**"]` exposes the whole filesystem to the webview — inherent to a "browse any folder" viewer and acceptable here since the frontend runs no remote content; worth remembering if remote resources are ever added.
- **File-safety patterns are deliberate and correct:** trash instead of delete for user files, collision-safe renames (never overwrite — `rename_file`, `available_path`, `move_image` all check), crop coordinates re-clamped server-side (`image_proc.rs:133-144`), cross-volume move fallback refuses to hide duplicates.
- **Registry writes are HKCU-only** (no elevation, uninstall cleans up including legacy keys). `reg.exe` output is surfaced on failure. No injection risk: arguments are passed via `args`, not a shell string.
- **No secrets, no network calls, no update endpoints.** The only external origin is fonts (M3).
- Nothing security-*critical* found; the security-relevant items are M3 (remote font) and the general note that `first_launch_path` trusts an existing path from argv — fine for a local viewer.

---

## 7. Remaining concerns not safely fixable automatically

1. **`config_path()` writability heuristic** (`config.rs:9-29`): "not under Program Files ⇒ portable" picks the exe dir even when it is read-only. H2 makes the failure visible, but choosing fallback-to-AppData-on-unwritable changes where existing users' configs live — needs a product decision plus a migration story before changing.
2. **Undo/redo history is session-only and in-memory** — documented, deliberate; a persisted journal would be a feature, not a fix.
3. **Rename is not undoable** — documented design decision (`UndoActionType = Move | Edit`).
4. **Full-file reads in `get_metadata`** (EXIF fallback + JPEG comments read the file twice): same class as M4 but on the single-file path where the cost is one file — left alone to keep the review's changes minimal; M4's `read_prefix` could be reused here later.
5. **`PNG/` folder (140 MB) sits inside the repo working tree** — I did not move or gitignore it (user data, not code); flagged in M12 for the owner to decide.
6. **turbojpeg marker preservation on lossless rotate** relies on `copy_none: false` defaults plus the `merge_missing_segments` safety net — verified by tests; no action, just noting the dependency chain is load-bearing for AI-metadata survival.

---

## 8. Assumptions made

1. Behaviour documented in `AGENTS.md`/`knowledge.md`/README is intended behaviour unless the code contradicts itself; where they conflicted (M11), I flagged the conflict instead of silently picking a side.
2. The Explorer-style " (1)" collision suffix is the canonical convention (README documents it; the underscore variant in `move_image` reads as the older code).
3. Windows is the primary platform (per AGENTS.md); fixes that need `#[cfg(windows)]` are marked, and the non-Windows build must keep compiling (CI runs ubuntu).
4. The review prompt's "don't change any code" was honoured: everything above is *proposed* code, not applied. The one pre-review commit (`eef9ef4`) only restored/updated `AGENTS.md` and removed the stale `Audit.md` as instructed by the `/init` + review-prompt workflow ("commit all changes first").
5. Test counts (116/28) reflect the current commit; docs quoting other numbers are listed as drift (M12).
6. Performance findings (M4, M5, L2) are based on code reading, not profiling — the shapes are clearly suboptimal, but measure before/after if you want numbers for the changelog.

---

## 9. Recommended fix order

| Order | Item | Why first |
|---|---|---|
| 1 | H1 + H2 (config atomicity + debounced central save) | protects the only persistent user data |
| 2 | H5 (busy guard), H3 (crop try/catch), H4 (signature reset), H6 (crop deactivation) | small, isolated, user-visible correctness |
| 3 | M9 (corrupt-config backup), M10 (swap staging) | completes the data-safety story |
| 4 | M1 (toast colour), M11 (fileOpsAllowed), M2 (collision naming + tests) | consistency, quick wins |
| 5 | M6 (sanitizer dedup), M7 (context-menu parity + no-window), M8 (merge dedup + test) | backend hygiene with test coverage |
| 6 | M3 (vendor font), M12 (docs drift), L4, L8 | packaging/docs |
| 7 | M4 + M5 (bounded reads, spawn_blocking) | measure first; behaviour-neutral if done as specified |
| 8 | L1–L3, L5–L7, L9–L12 | polish, ideally alongside their owning files' next change |

---

## 10. Implementation status (post-fix pass, 2026-08-22)

All fixes below were implemented, validated (`cargo fmt` + `cargo clippy --all-targets -D warnings` + `cargo test` **122 passed**; `npm test` **31 passed**) and release-built in one pass.

| Finding | Status | Notes |
|---|---|---|
| H1 config atomicity | ✅ | temp+rename in `save_config_to`; `ConfigLock` mutex serializes writers; JS `scheduleConfigSave()` debounces navigation saves (500 ms), explicit user actions flush immediately |
| H2 silent save failures | ✅ | single save path with `.catch`; first failure surfaces as a toast |
| H3 crop error handling | ✅ | try/catch, toast, crop region stays active for retry |
| H4 stale list signature | ✅ | catch path resets `lastListSignature` + rebuilds filter options |
| H5 busy-flag leak | ✅ | `BusyGuard` RAII reset, panic-safe |
| H6 crop survives file change | ✅ | `selectImage` deactivates crop when the path changes |
| M1 toast colours | ✅ | neutral default, red only with `.error` |
| M2 collision naming | ✅ | shared `available_path_for`; moves now produce `pic (1).jpg` |
| M3 remote font | ✅ (deviation) | dropped Google Fonts for a **system font stack** (`'Inter', system-ui, 'Segoe UI', …`) instead of vendoring woff2 subsets — zero network, zero binary assets, full Cyrillic via Segoe UI; CSP tightened to `'self'` |
| M4 badge-scan reads | ✅ (extended) | both stages: JPEG segments read from a 2 MiB prefix; PNG chunk walk is now streaming (seeks past IDAT, never reads pixels) with a 32 MiB text-chunk cap against corrupt lengths |
| M5 blocking I/O | ✅ | `get_metadata` + `check_prompts` via `spawn_blocking` |
| M6 sanitizer dedup | ✅ | `sanitize_json_float_tokens` deleted; all call sites use `exif_writer::sanitize_non_finite` |
| M7 context menu | ✅ | 11 extensions (parity test added), `CREATE_NO_WINDOW` on `reg.exe` |
| M8 merge dedup | ✅ (refined) | EXIF APP1 → signature dedup; everything else → exact-data dedup (preserves ICC chains AND catches byte-identical turbojpeg duplicates); tests added |
| M9 corrupt config backup | ✅ | renamed to `config.json.corrupt` before defaults kick in; test added |
| M10 swap staging | ✅ | staged + atomic same-dir rename; a failed copy can't truncate the live file |
| M11 report-modal gating | ✅ | `fileOpsAllowed()`; docs updated to the two-level policy |
| M12 docs drift | ✅ | counts (122/31), modal policy, dead-file references, `PLAN.md` marked historical, title fixed, empty `Audit/` + `Errors.txt` removed, `PNG/` gitignored |
| L1–L12 | ✅ except L11 | L1 escapeAttr→escapeHtml; L2 payload refs; L3 shared `formatBytes` + tests; L4 engines; L5 invalid-char/control-char validation + tests; L6 batch counts; L7 lowercase at source; L8 window min sizes + larger default; L9 hotkey ✕ clear button; L10 init error handling; L12 sort-button classes. L11 (ru-RU dates) left as the documented intentional choice |

**Deviations from the original proposals, with reasons:**

1. **M2** — the audit's single-helper sketch broke the ComfyUI export: `available_path(dir, stem, "workflow.json")` must insert the suffix before the *whole* compound extension (`w (1).workflow.json`), which a generic last-extension helper does not do. Final shape: `available_path_for` (full file name, Explorer-style last-extension placement, used by `move_image`) + `available_path` (stem + possibly-compound ext, used by convert/export) sharing the `ensure_dir` preamble.
2. **M3** — system font stack instead of vendored Inter (see table).
3. **L5** — the proposed trailing-space check was unreachable (`new_name.trim()` already strips it); replaced with a control-character check, which is the case that actually reaches the OS.

**Known residual (accepted):** a hard quit within the 500 ms debounce window can lose the last `last_file` update (navigation-only — every explicit setting change flushes immediately); `beforeunload` cannot await IPC, so this is the practical floor.

**Remaining concerns from §7 stay open** (config-path writability fallback, persisted undo journal, non-undoable rename, `get_metadata` full-file reads on the single-file path).
