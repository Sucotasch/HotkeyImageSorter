# Project knowledge

This file gives Freebuff context about your project: goals, commands, conventions, and gotchas.

## Quickstart
- **What it is**: HotkeyImageSorter — a Tauri 2 desktop app (vanilla JS frontend + Rust backend) for keyboard-driven image sorting: preview images, assign target folders to keys A–Z, move/trash with one key, undo/redo, rotate/crop/zoom, metadata viewer (Ctrl+I) with AI-prompt detection. Bilingual README (EN + RU).
- **Setup**: `npm install` (only `@tauri-apps/cli` as devDep; runtime plugins: dialog, single-instance, window-state).
- **Dev**: `npm run dev` (runs `tauri dev`)
- **Build**: `npm run build` (runs `tauri build`)
- **Test**: `cd src-tauri && cargo test` (53 Rust tests) · `npm test` (20 JS tests, `node --test test/*.test.js`, needs Node ≥ 21)
- **Native build note**: `cargo build` compiles `dav1d-sys` (vendored, AVIF decode) and `turbojpeg-sys` (vendored libjpeg-turbo, lossless JPEG rotation). Both need MSVC + nasm + cmake + pkg-config + meson/ninja (see below). First build takes a few minutes.

## Architecture
- **Frontend (`src/`)**: No bundler. `tauri.conf.json` → `frontendDist: "../src"` serves the folder directly. `index.html` loads only `app.js` as an ES module.
  - `index.html` — layout: sidebar image list (virtualized), main viewer, right shortcuts panel, settings modal, metadata modal (Ctrl+I)
  - `app.js` — ALL app logic: state, virtualized list rendering, keyboard handling, hotkey moves, crop UI, settings, metadata modal, prompt badges
  - `utils.js` — pure helpers (no Tauri): `basename`, `escapeHtml`, `computeResizeRect` (crop), `computeVisibleRange` (virtual list) — unit-tested in `test/`
  - `style.css` — the real styles (dark theme, CSS vars in `:root`)
  - `main.js` / `styles.css` — **leftover Tauri template boilerplate, unused/dead; don't edit or reference them**
- **Backend (`src-tauri/src/`)**: commands registered in `lib.rs` via `invoke_handler`; frontend calls them through `window.__TAURI__.core.invoke(...)` (global Tauri API, `withGlobalTauri: true` — NOT npm imports).
  - `lib.rs` — command registration, undo/redo history (in-memory `AppState` Mutexes), tray icon + minimize-to-tray on close, single-instance, backup/session lifecycle, `get_metadata`/`copy_text`/`check_prompts`
  - `file_ops.rs` — `list_images` (jwalk, max_depth 1) and `move_image` (auto-renames collisions, cross-volume copy fallback)
  - `image_proc.rs` — `rotate_image` / `crop_image` via `image` crate + metadata preservation (JPEG segments, PNG text chunks); JPEG rotate tries **lossless turbojpeg** first
  - `jpeg_meta.rs` — raw JPEG segment extraction / splicing (EXIF, ICC, XMP, COM) so metadata survives re-encode
  - `png_meta.rs` — PNG tEXt/zTXt/iTXt extraction + re-injection (AI prompts survive edits)
  - `metadata.rs` — `get_metadata` (EXIF, PNG chunks, comments, AI-prompt parsing for A1111/ComfyUI/InvokeAI) and `check_prompts` (lazy badge scan)
  - `backup.rs` — session-based edit backups (`%TEMP%/imagesorter_<pid>_<ts>`), cap 10, stale-dir cleanup on startup + quit
  - `config.rs` — config path resolution (portable `config.json` next to exe → else AppData via `ProjectDirs`)
  - `models.rs` — `ImageInfo`, `AppConfig`, `UndoAction`/`UndoActionType` enum, `ImageMetadata`/`AiPromptInfo`/`MetadataEntry`
- **Data flow**: User action → JS handler → `invoke('command', params)` → Rust command → filesystem → response updates state

## Conventions
- Rust: standard `#[tauri::command]` async fns returning `Result<T, String>`; `use tauri::State` for shared state; commands snake_case (e.g. `move_images`, `trash_images`, `get_history_stats`, `get_metadata`, `copy_text`, `check_prompts`).
- JS: vanilla ES modules, `const { invoke } = window.__TAURI__.core;`, global `state` object, `const el = { ... }` UI-element cache, camelCase helpers.
- Config is saved wholesale via `save_config` with the full `AppConfig` object (has `#[serde(default)]` on all fields — keep fields backward compatible).
- History: undo/redo stored as batches; capped at **10 batches** in `lib.rs` (old `edit` backups deleted when evicted). `UndoActionType` is an enum: `Move` | `Edit` (rename is NOT undoable — known limitation).
- Modal handling: `anyModalOpen()` (settings OR metadata) gates destructive hotkeys; new modals must join this guard.

## Gotchas / constraints
- **Dead files**: `src/main.js` and `src/styles.css` are unused Tauri template leftovers — ignore them.
- **Supported extensions** in `list_images` (`SUPPORTED_EXTENSIONS` in `file_ops.rs`): `jpg, jpeg, jif, png, gif, webp, bmp, tif, tiff, ico, avif` (case-insensitive). Also enforced on rename so files can't be renamed out of the list. `.jif` = JPEG Interchange Format: the `image` crate doesn't know the extension, so `image_proc.rs` `detect_format()` and `metadata.rs` `get_metadata` map `jif` → `Jpeg` explicitly.
- **AVIF/WebP**: full support (preview via WebView2, edit via `image` crate). AVIF decode needs `dav1d`; image crate features `avif` (ravif encoder) + `avif-native` (dav1d decoder).
- **Metadata preservation**: rotate/crop re-encode through `image` crate — JPEG APPn/COM segments and PNG text chunks are extracted and re-injected (JPEG is otherwise lossy on re-encode). JPEG rotate uses **lossless turbojpeg** (`perfect=true`) when dimensions are MCU-aligned; falls back to metadata-preserving re-encode otherwise.
- **CSP** (`tauri.conf.json`): `img-src 'self' asset: http://asset.localhost blob: data:` — images rendered via `convertFileSrc(path)`; cache-busted with `?t=` after edits (critical for undo/redo to show fresh frames).
- **Tray behavior**: closing the window hides to tray (config `minimize_to_tray`, default `true`). Real quit is via tray menu → Quit. `Escape` with no modal open calls `hide_window`.
- **Config location**: portable `config.json` next to the exe unless running from Program Files; otherwise `%APPDATA%\com\imagesorter\app\config.json`.
- **Undo/redo is session-only** — history resets on restart. Edit backups live in the session dir under `%TEMP%` (cleaned on window Destroyed; stale dirs cleaned at startup). Backups are on **C:** while images are usually on D:/E/F/G — `swap_edit` copies cross-volume.
- **Sorting**: by `name`/`modified`/`size` with dir saved in config; dates formatted with `ru-RU` locale in `selectImage`.
- **Windows dev**: needs WebView2 (usually preinstalled) + VS Build Tools (C++ workload). Linux needs webkit2gtk-4.1 dev packages (see README).
- **Native build gotchas (Windows)**: `src-tauri/vendor/dav1d-sys` is a patched copy of dav1d-sys 0.8.3 (`[patch.crates-io]`). Its `build.rs` finds `vcvars64.bat`, builds dav1d with **MSVC cl** (MinGW-built libdav1d.a won't link), disables tools/tests (windres breaks on paths with spaces). `turbojpeg-sys` needs **cmake + nasm**. Tool binaries: nasm (`D:\c\Tools\nasm\nasm-3.02`), pkg-config-lite (`D:\c\Tools\pkgconfig\pkg-config-lite-0.28-1\bin`), meson+ninja pip (`D:\Works\Python\Scripts`). The repo's bash maps `/c/` to `d:\c\`. CI installs `meson ninja-build nasm pkg-config cmake` on ubuntu.
- **Key handling**: global `keydown` skips when focus is in an editable element (INPUT/SELECT/TEXTAREA/contenteditable); A–Z keys move current (or multi-selected via Shift/Insert/Ctrl-click) images to configured hotkey folders; Ctrl+I opens metadata; Ctrl+R/C/Z/X are guarded by `anyModalOpen()`.
- **AI metadata formats understood**: A1111/Forge → PNG tEXt `parameters`; ComfyUI → tEXt `prompt`/`workflow` (JSON) **or** JPEG EXIF `Make`/`Model`/`ImageDescription`/`UserComment` with `Prompt:`/`Workflow:` JSON prefixes (**case-insensitive** — some writers capitalise, e.g. `Prompt:{json}` in Make + `Workflow:{json}` in ImageDescription); **bare ComfyUI graphs** (civitai AIR) stored in EXIF UserComment as UTF-16BE JSON **without** any `prompt:`/`workflow:` prefix (`looks_like_comfyui_graph` heuristic: starts with `{`, contains `class_type` + `inputs`); Qwen-Image workflows store the prompt in `inputs.prompt` (not `inputs.text`) — both keys are collected; InvokeAI → `sd-metadata`; JPEG prompts also in COM or EXIF UserComment/ImageDescription. `check_prompts` scans raw chunk/segment headers (fast, no pixel decode).
- **EXIF values**: don't use `field.display_value()` for text extraction — it wraps ASCII/UNDEFINED in quotes+escapes. `metadata.rs::field_value_to_string` decodes raw `Value::Ascii`/`Value::Undefined` for Make/Model/UserComment/ImageDescription (strips the 8-byte charset header on UserComment), other tags keep tag-specific display. ComfyUI Make/Model JSON blobs are hidden from the EXIF table and shown in the AI section instead.
- **kamadak-exif is strict**: it errors `InvalidFormat("Unexpected next IFD")` on real-world files that overlap the "next IFD" pointer with value data (e.g. `Test-Now\JPG\103876708.jpeg`). `metadata.rs` falls back to `extract_exif_text_fields` (a tolerant TIFF walker for Make/Model/ImageDescription/UserComment/Software) in `read_exif` and `parse_exif_bytes`. Don't assume kamadak-exif parses every JPEG.
- **EXIF UserComment charset**: `UNICODE\0` header means UTF-16 with **unspecified byte order** — A1111-era writers store prompts as UTF-16BE. `decode_utf16` detects BE/LE via NUL-position heuristic (even offsets = BE, odd = LE) and prefers BE per EXIF spec when ambiguous.
