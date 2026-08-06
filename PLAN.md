# Plan — next milestone

Scope: **H7** (session backups), **M6** (true list virtualization), **Lossless JPEG + metadata-preserving edits**, **Metadata viewer (Ctrl+I) with AI-prompt detection + lazy "has prompt" badges**.

Status: **PLANNING** (no code changed). Decisions confirmed with the user:
1. JPEG rotation → **hybrid**: `turbojpeg` (libjpeg-turbo) lossless 90/180/270, fallback to metadata-preserving re-encode.
2. AI prompts + EXIF/XMP/ICC must be **preserved** through rotate/crop.
3. "Has prompt" badge → **lazy**: detect only for selected + visible rows (async, cached).

---

## 0. Ground truth (verified)

### Spike: turbojpeg builds on this machine ✅ (2026-08-06)
`turbojpeg 1.5.1` + `turbojpeg-sys 1.2.0` compiled from source (cmake 4.3.2 + MSVC + nasm in PATH) in **49.31s** in `D:\tmp\tjspike` (fresh, no vendor patch needed).

Key facts from turbojpeg-sys 1.2.0 `build.rs` (read the actual source):
- **Bundles libjpeg-turbo inside the crate** (`libjpeg-turbo/` dir) — no external clone needed (unlike dav1d).
- Default features `cmake` + `pkg-config` + `require-simd`; with `cmake` enabled and no `TURBOJPEG_SOURCE` env, it builds from source via `cmake::Config` (`-DREQUIRE_SIMD=ON`). `check_nasm()` only warns when `require-simd` is off; with it ON, nasm must be found by cmake.
- Pre-generated bindings are used by default (no bindgen run).
- **`Transform.copy_none: bool` defaults to `false` → ALL markers (EXIF/ICC/COM/XMP) are preserved** by lossless transform. So the turbojpeg path preserves AI metadata for free (still verify with a test).
- API confirmed: `Transform::op(TransformOp::Rot90)`; `Transformer::new(&bytes)?` then `.transform(&transform)? -> Vec<u8>`; `TransformOp::{Rot90, Rot180, Rot270}`.

### Environment facts (Windows 10, multiple drives)
- **5 fixed drives: C, D, E, F, G.** `std::env::temp_dir()` == Windows `GetTempPath()` == `C:\Users\sucot\AppData\Local\Temp` (on **C:**) — verified live.
- Project + images live on **D:/E/F/G** → edit backups (temp dir on C:) are **cross-volume** vs. the images. Already handled by `swap_edit` (copy-based), but H7's session dir will also live on C: — keep copy-based swap.
- **Bash `/tmp` maps to `D:\tmp`** (NOT the Windows temp); `D:\tmp\tjspike` is the spike dir. Do NOT confuse with the Rust temp dir used for backups.
- Tools: cmake 4.3.2 (`D:\Works\Python\Scripts`), nasm 3.02 (`D:\c\Tools\nasm\nasm-3.02`), MSVC 2019 BuildTools, Node v25.2.1.
- CI (ubuntu): turbojpeg build needs `apt-get install cmake nasm` (bundled source, gcc present); dav1d/meson/ninja step stays.

| Fact | Source |
|---|---|
| `image::save()` drops JPEG EXIF/XMP/ICC **and** PNG tEXt chunks (`parameters`, `workflow`, `prompt`) | researcher-docs |
| `turbojpeg` crate (0.6/0.7) wraps libjpeg-turbo, has `Transformer` for lossless rotation; needs cmake+C compiler on Windows | researcher-web |
| `kamadak-exif` is **read-only** (fine — we read, never write EXIF) | researcher-web |
| `png` crate reads text via `Info::uncompressed_latin1_text` / `compressed_latin1_text` / `utf8_text`; writes via `add_text_chunk` / `add_ztxt_chunk` / `add_itxt_chunk` | researcher-web |
| A1111/Forge → PNG tEXt `parameters` (A1111-format string); JPG → usually EXIF `UserComment`/`ImageDescription` or COM segment | researcher-web |
| ComfyUI → PNG tEXt/zTXt `prompt` + `workflow` (JSON); InvokeAI → `sd-metadata` (JSON) | researcher-web |
| Flux via ComfyUI → same `prompt`/`workflow` chunks; detect by node classes `CLIPTextEncodeFlux`, `FluxGuidance`, model names | researcher-web |
| Current `rotate_image`/`crop_image` re-encode via `image` crate → **destroy all metadata** | `image_proc.rs` |
| Backups go to bare `%TEMP%/img_bak_<micros>.<ext>`; `MAX_HISTORY = 5`; `Destroyed` cleans history+redo | `lib.rs` |
| `virtual-list` container is `position:relative`; rows are rebuilt wholesale; minimal skip-rebuild already in place | `app.js` |

---

## 1. H7 — Session-based edit backups (backend)

**Goal:** no orphaned `img_bak_*` in `%TEMP%`, history cap raised, crash leftovers cleaned on next start.

1. **Session directory** — lazily create `%TEMP%/imagesorter_<pid>_<startup_ts>/` (created on first `backup_file` call; store path in a `OnceLock<PathBuf>`; safe from concurrent commands).
2. `backup_file` writes into the session dir instead of bare `%TEMP%` (name stays `img_bak_<micros>.<ext>`).
3. **On `WindowEvent::Destroyed`** — `remove_dir_all` the session dir (replaces per-file cleanup; keep the existing history/redo loop for robustness but it becomes a no-op).
4. **Startup cleanup** — best-effort scan of `%TEMP%` for `imagesorter_*` dirs older than 1 day and delete them (handles crash leftovers). Runs in `setup`.
5. **Raise cap** — `MAX_HISTORY: usize = 10` (audit recommendation). Update the audit note; UI already shows counts, no label change needed.
6. `swap_edit` unchanged (works on paths; backup now lives in session dir, still possibly cross-volume vs. the image — copy-based swap already handles that).

**Tests (Rust):**
- `session_dir_is_created_and_used_by_backup_file`
- `stale_session_dirs_removed_on_startup_fresh_kept` (call cleanup fn with a temp dir + mtime manipulation)
- cap=10: `push_history` drops oldest edit backup only when >10 batches

**Files:** `lib.rs`, new small `backup.rs` (or keep in lib.rs). ~40 LOC.

---

## 2. M6 — Real windowed list virtualization (frontend)

**Goal:** O(visible) DOM instead of O(n) per navigation on large folders.

1. **Fixed row height** — `.list-item` gets a fixed height (e.g. `height: 32px`, `padding` moved inside, text `ellipsis` + `title` for full name). Virtualization needs a constant row height.
2. **Structure:** `.virtual-list` keeps `overflow-y:auto; position:relative`. Inside: a spacer element (height `n * ROW_H`, sets scroll height) + a rendered window of rows absolutely positioned at `top = i * ROW_H`.
3. **Pure helper** `computeVisibleRange(scrollTop, viewportH, rowH, total, overscan)` → `{start, end}` — added to `utils.js`, fully unit-tested (top/middle/bottom, overscan clamp, empty list, total < viewport).
4. **Window render** — `renderWindow()` rebuilds only `[start,end)`, reusing per-row markup (click/dblclick/rename handlers identical to today). Selection/active classes applied in place.
5. **Scroll handler** — rAF-throttled; recompute window only when `start/end` changed.
6. **Navigation integration** — `selectImage` already calls `scrollIntoView`; after scroll, recompute the window (same rAF path). `Insert`/`Shift+↑↓` selection updates classes for visible rows only.
7. **Refresh edge cases** — on `loadFiles`/sort/delete/rename: reset scroll, re-render window, restore `currentIndex` item into view.
8. Rename input inside a window row: on commit `renderList()`-equivalent = `renderWindow()` + list reload; keep `renamingIndex` logic.

**Tests (JS, `npm test`):** `computeVisibleRange` cases (above) + a small `windowChanged(old,new)` helper if useful.

**Files:** `src/app.js`, `src/utils.js`, `src/style.css`, `test/utils.test.js`. ~120 LOC.

**Risks:** keyboard scroll + `scrollIntoView` interplay; mitigate by driving scroll via `scrollTop = index*ROW_H` directly in `selectImage` when a window row is off-screen.

---

## 3. Lossless JPEG + metadata preservation (backend, core)

### 3.1 Shared: JPEG segment extraction / splicing — new `src-tauri/src/jpeg_meta.rs`

- `extract_segments(bytes) -> Vec<JpegSegment>` — walk markers: APP0/APP1/APP2/APP13 (EXIF/XMP/ICC/Photoshop) and COM; capture raw segment bytes. (~80 LOC, no deps.)
- `merge_missing_segments(out_bytes, original_segments)` — insert only segments whose (marker + first-4 signature) is **not already present** in the output (dedupe; avoids doubles when turbojpeg already kept them). Placed after SOI.
- Pure functions → unit-testable without turbojpeg (round-trip: extract→merge on synthetic marker stream).
- **Stale EXIF caveat:** re-encoded JPEGs may carry stale `PixelXDimension`/thumbnail — acceptable; viewers tolerate it. Documented, not fixed (writing EXIF is out of scope).

### 3.2 Lossless JPEG rotation — `turbojpeg`

- Add `turbojpeg = "0.6"` (or current) to `Cargo.toml`.
- `rotate_jpeg_lossless(path, degrees)`:
  - read bytes → `Transformer::new().transform(&mut input, &[Transform { op: Rot90|Rot180|Rot270, .. }], flags)`.
  - **Verify marker preservation** (does the crate copy APPn/COM by default?). If not → feed the transformed output through `merge_missing_segments` with the originals extracted beforehand. Tests will pin the behaviour.
- **Fallback:** if turbojpeg fails to build/transform (corrupt file, unsupported), fall back to 3.3.
- **Windows build risk:** libjpeg-turbo via cmake in a path with spaces — same class of problem as dav1d/windres. **Mitigation:** if `turbojpeg-sys` source build fails, mirror the dav1d solution (vendor `turbojpeg-sys` + patch build.rs, or point at a prebuilt libjpeg-turbo via `TURBOJPEG_SYS_*` env, or bundle prebuilt DLLs). Reserve time for this in estimation.
- **Linux CI:** `apt-get install -y libturbojpeg0-dev cmake` (or set `turbojpeg-sys` feature to use system libjpeg-turbo — verify crate's feature name at implementation time; `turbojpeg-sys` exposes one).

### 3.3 Metadata-preserving re-encode (crop, and rotate fallback)

**JPEG (`jpeg_meta.rs` + `image_proc.rs`):**
1. Extract original segments (incl. EXIF UserComment/ImageDescription, COM — where AI prompts live in JPG).
2. `image::open` → crop/rotate → save to temp with `jpeg` encoder quality **95** (crop can't be lossless anyway).
3. `merge_missing_segments` → write result back over the original path.

**PNG (new `src-tauri/src/png_meta.rs`, using the `png` crate as a direct dep):**
1. Read original via `png::Decoder` → capture `Info` text chunks (`tEXt`/`zTXt`/`iTXt`) and ancillary chunks (`pHYs`, `tRNS`, `sRGB`/`gAMA`).
2. Apply the transform on pixels (decode to RGBA → `image` rotate/crop).
3. Re-encode via `png::Encoder`, re-adding text chunks (`add_text_chunk`/`add_ztxt_chunk`/`add_itxt_chunk`) + copying `pHYs`/`tRNS`/`sRGB`/`gAMA` when present (preserves AI chunks AND appearance).
   - Tradeoff: PNG re-encode drops `interlace` and may bump color depth to RGBA8; acceptable for a sorter (documented).

**Other formats (webp/avif/gif/bmp/tiff/ico):** keep current `image`-crate path (no metadata preservation). AVIF/WebP from AI tools are rare; noted as follow-up.

**Undo interplay:** none — backup file is copied before the edit, so the original metadata is already safe in the backup. Undo/redo swap restored contents+metadata as a unit.

**Tests (Rust):**
- JPEG fixture with an EXIF APP1 + COM containing an A1111 `parameters` string → `crop_image` → re-read: `parameters` still present; `rotate_image` via turbojpeg → still present, orientation/dimensions correct.
- PNG fixture with `parameters` + `workflow` chunks → rotate → chunks intact.
- `jpeg_meta` pure round-trip tests; `merge_missing_segments` dedupe test.

**Files:** `Cargo.toml` (+`turbojpeg`, +`png`, +`kamadak-exif` in 4.1), `src-tauri/src/jpeg_meta.rs`, `src-tauri/src/png_meta.rs`, `src-tauri/src/image_proc.rs`.

---

## 4. Metadata viewer + AI prompts (Ctrl+I) (backend + frontend)

### 4.1 Backend — new `src-tauri/src/metadata.rs`

- Command `get_metadata(path) -> MetadataReport` (serde struct `MetadataReport` in `models.rs`):
  ```
  { file: {name, size, dimensions, modified},
    exif: { key: value, ... },          // kamadak-exif: Make/Model/DateTimeOriginal/Exposure/FNumber/ISO/FocalLength/Software/Orientation/ImageDescription/UserComment/GPS...
    ai: Option<AiInfo>,                 // None = no prompt found
  }
  AiInfo { generator: "stable-diffusion-webui" | "comfyui" | "invokeai" | "unknown",
           prompt: String, negative_prompt: Option<String>,
           parameters: { seed, steps, sampler, cfg, model, ... } (A1111 parse),
           raw: String }                // full original string/JSON for "copy raw"
  ```
- **AI detection** (`ai_prompt.rs`):
  - PNG: scan text chunks for keywords `parameters`, `prompt`, `workflow`, `sd-metadata`, `Description`; decompress `zTXt` (the `png` crate already surfaces decoded zTXt? — if not, `flate2`; verify).
  - JPEG: read EXIF `UserComment`/`ImageDescription` via `kamadak-exif` + COM segment via `jpeg_meta`; detect A1111 format markers (`Steps:`, `Negative prompt:`).
  - ComfyUI `prompt`/`workflow` are JSON → `serde_json` (already a dep); extract human-readable prompt: walk node graph for `class_type` `CLIPTextEncode*` inputs `.text`; detect Flux by `CLIPTextEncodeFlux`/`FluxGuidance`/flux model names.
- Command `copy_text(text: String)` — robust clipboard **without** webview clipboard-permission quirks. Use `arboard` (pure-Rust, small) OR `tauri-plugin-clipboard-manager`. **Recommend `arboard`** (no plugin wiring, works on Windows). Verify at implementation; `navigator.clipboard.writeText` alone may fail under Tauri CSP.

### 4.2 Frontend — Ctrl+I overlay

- **Trigger:** `Ctrl+I` in `handleGlobalKeydown` (guard: not typing, no modal). Toggles `#modal-metadata`.
- **Placement:** centered over the viewer (the image area), reusing `.modal` backdrop + a wider `.modal-content` (max-width ~640px, max-height 80vh, scroll). Add to the modal-open guard so A–Z hotkeys don't fire while open (same pattern as settings modal).
- **Content:** sections — *File*, *EXIF* (key/value grid), *AI Generation* (prompt in a `<pre>` with **Copy prompt** / **Copy negative** / **Copy raw** buttons; parameters grid; generator chip e.g. "ComfyUI • Flux"). **Copy all** button top-right.
- **Fetch:** `invoke('get_metadata')` on open; loading state; error → toast inside modal. Cache by path in `state.metaCache` (invalidate on rotate/crop/undo/redo — file changed).
- **Copy:** buttons call `copy_text` command → success toast. Values rendered via `escapeHtml`/`textContent` (no innerHTML injection).
- **Close:** Ctrl+I / Esc / backdrop click.

**Tests:** Rust — `AiInfo` parsing unit tests (A1111 string, ComfyUI JSON, InvokeAI JSON, Flux detection, negative case). JS — a tiny pure formatter (`formatAiReport`) if any string building is extracted to `utils.js`; mostly manual/`node --check`.

---

## 5. Lazy "has prompt" badges (frontend + small backend)

1. **Backend** — command `check_prompts(paths: Vec<String>) -> HashMap<String, bool>`: reuses `ai_prompt::has_prompt(path)` (PNG: scan text-chunk keywords; JPEG: APP1/COM scan). Fast, whole-file read only for PNGs (acceptable: bounded to ~viewport rows + selected).
2. **Frontend** — `ImageInfo` stays as-is; badges stored in `state.promptFlags: Map<path, bool>`:
   - After `loadFiles`/`renderWindow`, collect the **visible window paths** + current path → `invoke('check_prompts', {paths})` (debounced ~150ms), merge into the map, patch badges in place (no full re-render).
   - On scroll/window change → request next batch. Cache prevents re-reads.
3. **Badge UI** — small chip in the row (e.g. `✦` or "AI") shown next to the name when `promptFlags.get(path)`. Also a hint in the info panel for the current file ("Has generation prompt — Ctrl+I"). Tooltip `title` with generator name when available (from cache of the selected file's `AiInfo`).
4. **In sync with 4:** selecting a file whose prompt was already fetched updates the info-panel hint for free.

**Tests:** Rust `has_prompt` detection tests (PNG with/without parameters, JPG with EXIF prompt); JS none (DOM-only).

---

## 6. Dependencies & CI

| Add | Why | Build note |
|---|---|---|
| `turbojpeg` (+`turbojpeg-sys`) | lossless JPEG rotate | cmake + MSVC on Windows (tools present); CI: `apt-get install -y libturbojpeg0-dev cmake`; risk of path-with-spaces → vendor fallback planned |
| `png` (direct dep) | read/write PNG text chunks | pure Rust |
| `kamadak-exif` | read EXIF (JPEG/PNG/WebP) | pure Rust |
| `arboard` | clipboard for `copy_text` | pure Rust (Windows OK) |

CI (`ci.yml`): add libjpeg-turbo + cmake to the ubuntu job. Keep dav1d/meson/ninja/nasm step as-is.

---

## 7. Order of work & estimates

1. **H7** (backup.rs, ~40 LOC + tests) — *small*; independent, do first.
2. **M6** (virtualization, ~120 LOC + tests) — *medium*; independent, do second.
3. **jpeg_meta + png_meta + metadata-preserving edit** (§3.3) — *medium-large*; foundation for both 4 and 5. **Do before 4/5** so edits stop destroying prompts ASAP.
4. **turbojpeg lossless rotate** (§3.2) — *medium + build risk*; needs the vendor fallback decision; pair with CI change.
5. **Metadata viewer + Ctrl+I + AI parsing** (§4) — *medium-large*; depends on 3.1/jpeg_meta + ai_prompt.
6. **Lazy badges** (§5) — *small-medium*; depends on 4.1 `has_prompt`.

Validation per step: `cargo test` (growing suite), `cargo fmt --check`, `cargo clippy --all-targets` 0 warnings, `node --check src/app.js`, `npm test` (growing), `npm run build` at the end. Commit per step.

---

## 8. Concrete design decisions (verified against real code & APIs)

### H7 — concrete design
- New module `backup.rs`; session dir = `OnceLock<PathBuf>` → `temp_dir().join(format!("imagesorter_{}_{}", pid, unix_ts))`. Created lazily in `backup_file` via `create_dir_all` (thread-safe: `OnceLock` + first-call init).
- Cleanup: on `WindowEvent::Destroyed` → `remove_dir_all(session_dir)` (replaces the per-file loop; keep the history/redo loop as a harmless fallback). Startup (`setup`): scan `temp_dir()` for `imagesorter_*` entries, `remove_dir_all` those with mtime older than 24h (crash leftovers).
- `MAX_HISTORY = 5 → 10` (lib.rs:14).
- `swap_edit` unchanged — backups live on C: (temp), images on D:/E/F/G → the existing **copy-based swap** is already correct and stays.
- Tests: session dir created & reused; Destroyed cleanup removes the dir; startup cleanup removes stale dirs but keeps fresh; cap=10 drops oldest edit backup.

### M6 — concrete design
- **Fixed row height `ROW_H = 32px`** in `.list-item` (style.css): replace `padding:10px 16px` + `word-break:break-all` with height + ellipsis (`white-space:nowrap; overflow:hidden; text-overflow:ellipsis`) + `title` attr for the full name.
- DOM: `.virtual-list` (already `position:relative; overflow-y:auto`) gets: (a) spacer `<div>` height `n*ROW_H` (sets scroll height), (b) absolutely-positioned rows `top: i*ROW_H`.
- Pure helper `computeVisibleRange(scrollTop, viewportH, rowH, total, overscan)` → `{start,end}` in `utils.js`; tested for top/middle/bottom/empty/overscan-clamp.
- `renderWindow()` builds only `[start,end)`; **event delegation**: one `click` + one `dblclick` listener on the container reading `data-index` (replaces per-row listeners; keeps `handleItemClick`/`startRename` logic, called with the parsed index).
- **`selectImage` must stop using `scrollIntoView`** (the row may not be rendered). Replace with `container.scrollTop = index*ROW_H` (+ re-render window on scroll). `sortAndRender`'s `el.imageList.children[newIndex]` and `querySelectorAll('.list-item').forEach((e,i)=>…)` index-based class toggles become `data-index` lookups over the *visible* window only.
- While `renamingIndex !== -1`, `renderWindow` skips re-rendering the row that holds the input (else the blur→commit path breaks); after commit/cancel, full window re-render.
- `Insert`/selection logic: classes applied by `data-index` within the window only.

### JPEG lossless rotate — concrete design
- Cargo: `turbojpeg = { version = "1.5", default-features = false, features = ["cmake"] }`? — **decide at implementation**: default features (`cmake`+`pkg-config`+`require-simd`) are fine on this machine (spike passed). On CI add `apt-get install cmake nasm`. Keep defaults unless issues.
- `rotate_jpeg_lossless(path, degrees)`: read file → `Transformer::new(&bytes)` → `transform(&Transform::op(op))` → write result. `op` = `Rot90`/`Rot180`/`Rot270`. `copy_none=false` (default) → EXIF/ICC/COM preserved. **Verify with a test** (round-trip on a JPEG with an EXIF APP1 + COM): markers must survive.
- Fallback (any `Error`, incl. imperfect-MCU): metadata-preserving re-encode (§3.3) so the operation never fails harder than today.
- `image_proc::rotate_image` dispatch: `.jpg/.jpeg` → turbojpeg (fallback re-encode); `.png` → png_meta pipeline; everything else → current `image` path.

### Metadata-preserving re-encode — concrete design
- **JPEG** (`jpeg_meta.rs`): `extract_segments(&[u8]) -> Vec<JpegSegment>` walks markers (APP0/1/2/13 + COM; skip entropy-coded SOS payload); `merge_missing_segments(out, orig)` inserts only segments whose (marker + first-4 sig, e.g. `Exif`/`http`) is absent, right after SOI. Re-encode via `image::codecs::jpeg::JpegEncoder::new_with_quality(w, 95)` (API confirmed in image 0.25.10 source).
- **PNG** (`png_meta.rs`): decode via `png::Decoder` → `read_info()` → keep `Info.pixel_dims`, `trns`, `srgb`, and text chunks (`uncompressed_latin1_text`/`compressed_latin1_text`/`utf8_text`); decode RGBA, apply rotate/crop, encode via `png::Encoder` with `set_color(Rgba)`/`set_depth(Eight)`; re-add text chunks **after image data** via `Writer::write_text_chunk` (confirmed: writes before IEND — matches A1111/ComfyUI layout); copy `pixel_dims` via `set_pixel_dims`. zTXt auto-decompressed by the reader (confirmed in docs) → re-compress with `ZTXtChunk::new`.
- Tradeoffs accepted: PNG interlace dropped, depth normalized to RGBA8; JPEG stale EXIF dims/thumbnail left as-is.

### Metadata viewer (Ctrl+I) — concrete design
- `metadata.rs` + `ai_prompt.rs`; `get_metadata(path) -> MetadataReport` (serde in `models.rs`). EXIF via `kamadak-exif` `Reader::new().read_from(&mut file)` + `get_field(Tag::…, In::PRIMARY)` (API confirmed; read-only, which is fine).
- `ai_prompt.rs`: PNG → scan text chunks for keywords `parameters`/`prompt`/`workflow`/`sd-metadata`/`Description`; parse A1111 (split on `Negative prompt:`; params after last newline: `Steps:`, `Sampler:`, `CFG scale:`, `Seed:`, `Model:`, `Model hash:`); ComfyUI/Flux → `serde_json::Value`, walk nodes for `class_type` containing `CLIPTextEncode` → collect `inputs.text`, detect Flux by `CLIPTextEncodeFlux`/`FluxGuidance`/model names containing `flux`; InvokeAI → `sd-metadata` JSON. JPEG → EXIF `UserComment`/`ImageDescription` + COM via `jpeg_meta`.
- Copy: Rust command `copy_text(String)` via **`arboard`** (recommended; no plugin wiring, works on Windows). Fallback option `tauri-plugin-clipboard-manager` if arboard misbehaves.
- Frontend: new `#modal-metadata` fixed overlay centered over the **viewer** area (not the whole window), guarded in `handleGlobalKeydown` like the settings modal (add to the `el.modalSettings.classList.contains('hidden')` checks — extract a `anyModalOpen()` helper). `Ctrl+I` branch before the A–Z fallthrough. Esc closes (extend the Escape chain). Copy buttons call `invoke('copy_text')`; values via `textContent`/`escapeHtml`.

### Lazy badges — concrete design
- Command `check_prompts(paths: Vec<String>) -> HashMap<String,bool>` reusing `ai_prompt::has_prompt` (PNG: chunk-keyword scan; JPEG: EXIF/COM scan via `jpeg_meta`).
- Frontend: `state.promptFlags: Map<path,bool>` + `state.metaCache: Map<path,MetadataReport>`. After each `loadFiles`/`renderWindow`, collect visible-window paths + current path, debounce ~150ms, invoke `check_prompts`, patch only the visible rows (small chip `✦` next to the name; `title` from generator when already known). Cache survives scroll.

## 9. Cross-cutting consequences (checked)
1. **Undo/redo with preserved metadata**: `backup_file` copies the original *before* the edit → the pre-edit file (with its prompt) is already safe in the backup; undo restores it whole. No interaction with the new edit pipelines.
2. **Turbojpeg marker behavior must be test-pinned**: `copy_none=false` should preserve markers, but the exact subset (XMP APP1?) needs a test; `merge_missing_segments` is the safety net for the re-encode fallback only.
3. **Ctrl+I vs existing hotkeys**: `Ctrl+I` is unused today (checked `handleGlobalKeydown`); placing the branch before the A–Z fallthrough is safe. The metadata modal must be added to every `modalSettings.classList.contains('hidden')` guard (Delete, Enter, A–Z) via the `anyModalOpen()` helper, or destructive hotkeys fire behind it.
4. **M6 + badges**: both touch `renderWindow`/row markup — implement M6 first, then badges patch rows by `data-index`. Ordering fixed in §7.
5. **Multi-drive**: all temp work stays on C: (Windows temp); images on D:/E/F/G. `swap_edit` copy-based = correct. Session dirs on C: only.
6. **CI**: ubuntu job needs `cmake nasm` for turbojpeg (bundled source), plus existing dav1d/meson/ninja. No new system libjpeg-turbo needed (crate builds vendored source).

## 10. Open questions / decisions to make during implementation

1. `turbojpeg-sys` build on Windows: source build via cmake vs. vendor+patch (dav1d-style) vs. prebuilt DLLs. Decide after a spike.
2. Clipboard: `arboard` vs `tauri-plugin-clipboard-manager` — decide at implementation (recommend `arboard`).
3. PNG re-encode fidelity: drop interlace + force RGBA8 (accepted) vs. preserve bit depth (more code). Default: accepted tradeoff.
4. Stale EXIF fields after re-encode (PixelXDimension, thumbnail) — accepted, documented.
5. Metadata preservation scope for non-JPEG/PNG (webp/avif/...) — follow-up, out of scope now.
