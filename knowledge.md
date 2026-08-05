# Project knowledge

This file gives Freebuff context about your project: goals, commands, conventions, and gotchas.

## Quickstart
- **What it is**: HotkeyImageSorter — a Tauri 2 desktop app (vanilla JS frontend + Rust backend) for keyboard-driven image sorting: preview images, assign target folders to keys A–Z, move/trash with one key, undo/redo, rotate/crop/zoom. Bilingual README (EN + RU).
- **Setup**: `npm install` (only `@tauri-apps/cli` as devDep; runtime plugins: dialog, fs, opener, window-state).
- **Dev**: `npm run dev` (runs `tauri dev`)
- **Build**: `npm run build` (runs `tauri build`)
- **Test**: `cd src-tauri && cargo test` (no JS test infra exists — `package.json` has no test script)

## Architecture
- **Frontend (`src/`)**: No bundler. `tauri.conf.json` → `frontendDist: "../src"` serves the folder directly. `index.html` loads only `app.js` as an ES module.
  - `index.html` — layout: sidebar image list, main viewer, right shortcuts panel, settings modal
  - `app.js` — ALL app logic: state, list rendering, keyboard handling, hotkey moves, crop UI, settings
  - `style.css` — the real styles (dark theme, CSS vars in `:root`)
  - `main.js` / `styles.css` — **leftover Tauri template boilerplate, unused/dead; don't edit or reference them**
- **Backend (`src-tauri/src/`)**: commands registered in `lib.rs` via `invoke_handler`; frontend calls them through `window.__TAURI__.core.invoke(...)` (global Tauri API, `withGlobalTauri: true` — NOT npm imports).
  - `lib.rs` — command registration, undo/redo history (in-memory `AppState` Mutexes), tray icon + minimize-to-tray on close, single-instance, temp-backup lifecycle
  - `file_ops.rs` — `list_images` (jwalk, max_depth 1) and `move_image` (auto-renames collisions: `name_1.ext`)
  - `image_proc.rs` — `rotate_image` / `crop_image` via the pure-Rust `image` crate
  - `config.rs` — config path resolution (portable `config.json` next to exe → else AppData via `ProjectDirs`)
  - `models.rs` — `ImageInfo`, `AppConfig`, `UndoAction` (serde)
- **Data flow**: User action → JS handler → `invoke('command', params)` → Rust command → filesystem → response updates state

## Conventions
- Rust: standard `#[tauri::command]` async fns returning `Result<T, String>`; `use tauri::State` for shared state; commands snake_case (e.g. `move_images`, `trash_images`, `get_history_stats`).
- JS: vanilla ES modules, `const { invoke } = window.__TAURI__.core;`, global `state` object, `const el = { ... }` UI-element cache, camelCase helpers.
- Config is saved wholesale via `save_config` with the full `AppConfig` object (has `#[serde(default)]` on all fields — keep fields backward compatible).
- History: undo/redo stored as batches; capped at **5 batches** in `lib.rs` (history beyond cap is dropped; old `edit` backups deleted). `UndoAction.action_type` is `"move"` or `"edit"`.

## Gotchas / constraints
- **Dead files**: `src/main.js` and `src/styles.css` are unused Tauri template leftovers — ignore them.
- **Supported extensions** in `list_images`: `jpg, jpeg, png, webp, jif` (case-insensitive). NOT gif/bmp/tiff, despite README claims.
- **Lossy edits**: rotate/crop re-encode through the `image` crate (lossy for JPEG) — intentional, to avoid cmake/nasm C dependencies at build time.
- **CSP** (`tauri.conf.json`): `img-src 'self' asset: http://asset.localhost blob: data:` — images rendered via `convertFileSrc(path)`; cache-busted with `?t=` after edits.
- **Tray behavior**: closing the window hides to tray (config `minimize_to_tray`, default `true`). Real quit is via tray menu → Quit. `Escape` with no modal open calls `hide_window`.
- **Config location**: portable `config.json` next to the exe unless running from Program Files; otherwise `%APPDATA%\com\imagesorter\app\config.json`.
- **Undo/redo is session-only** — history resets on restart. Edit-backup temp files (`img_bak_*` in system temp) are cleaned on window Destroyed.
- **Sorting**: by `name`/`modified`/`size` with dir saved in config; dates formatted with `ru-RU` locale in `selectImage`.
- **Windows dev**: needs WebView2 (usually preinstalled) + VS Build Tools (C++ workload). Linux needs webkit2gtk-4.0 dev packages (see README).
- **Key handling**: global `keydown` skips when focus is in an `INPUT`; A–Z keys move current (or multi-selected via Shift/Insert/Ctrl-click) images to configured hotkey folders.
