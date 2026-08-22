# AGENTS.md — HotkeyImageSorter

Tauri 2 desktop app (Windows-first): vanilla JS frontend + Rust backend for keyboard-driven image sorting — preview, assign folders to keys A–Z, move/trash with one key, undo/redo, rotate/crop/zoom, Ctrl+Q metadata viewer with AI-prompt detection, PNG→JPG conversion that preserves AI prompts/workflows, optional Explorer context menu.

## Commands

- `npm install` — only devDep is `@tauri-apps/cli`
- `npm run dev` — `tauri dev`
- `npm run build` — `tauri build` (release)
- `npm test` — JS tests: `node --test test/*.test.js` (needs Node ≥ 21)
- `cd src-tauri && cargo test` — Rust tests
- Native build needs MSVC + nasm + cmake (+ meson/ninja for dav1d); first build is slow. `dav1d-sys` is vendored in `src-tauri/vendor/` with a patched build.rs because the build path contains spaces.

## Layout

- `src/` — frontend, **no bundler**; `tauri.conf.json` serves `../src` directly.
  - `app.js` — ALL app logic (state, virtualized list, keyboard, modals)
  - `utils.js` — pure helpers, unit-tested in `test/`
  - `index.html`, `style.css` — layout and dark-theme styles (CSS vars in `:root`)
  - `src/main.js` and `src/styles.css` are **dead Tauri template leftovers — ignore them**
- `src-tauri/src/` — Rust backend; commands registered in `lib.rs` via `invoke_handler`:
  - `file_ops.rs` — list/move (collision auto-rename, cross-volume fallback)
  - `image_proc.rs` — rotate/crop with metadata preservation; lossless JPEG rotate via turbojpeg first; PNG→JPG conversion preserving AI prompt/workflow metadata
  - `jpeg_meta.rs` / `png_meta.rs` — raw JPEG segment and PNG text-chunk extraction/re-injection so EXIF/ICC/XMP and AI prompt chunks survive edits
  - `exif_writer.rs` — EXIF writing support
  - `metadata.rs` — `get_metadata`, `check_prompts` (A1111/ComfyUI/InvokeAI parsing)
  - `context_menu.rs` — optional per-user Explorer cascading context menu (no admin rights)
  - `backup.rs` — session backups in `%TEMP%`, cap 10, cleaned on startup/quit
  - `config.rs` — portable `config.json` next to exe → else AppData (`ProjectDirs`)
  - `models.rs` — `ImageInfo`, `AppConfig`, `UndoAction`/`UndoActionType`, metadata types
- `knowledge.md` — deeper project notes; read before touching sensitive areas (metadata preservation, undo history, context menu).

## Conventions

- Frontend calls Rust via `window.__TAURI__.core.invoke(...)` (global API, `withGlobalTauri: true`) — NOT npm `@tauri-apps/api` imports.
- Rust commands: `#[tauri::command]` async fns returning `Result<T, String>`, snake_case names, `tauri::State` for shared state.
- JS: ES modules, global `state` object, `el = { ... }` UI-element cache, camelCase helpers.
- `AppConfig` fields all have `#[serde(default)]`; keep config backward compatible — it is saved wholesale via `save_config`.
- Undo/redo stored as batches, capped at 10 in `lib.rs`; `UndoActionType` = `Move` | `Edit` (rename is not undoable by design).
- Any new modal must join the `anyModalOpen()` guard that gates destructive hotkeys.
- README is bilingual (EN + RU) — update both halves when documenting features.

## Gotchas

- Single-instance + tray: closing minimizes to tray; `--metadata` launch flag routes through the single-instance plugin (context-menu integration).
- Undo history eviction deletes old `edit` backups — don't decouple without accounting for that.
- Workspace path contains spaces — never assume it doesn't (this broke dav1d originally).
