import { basename, escapeHtml, computeVisibleRange, compareImages, formatBytes } from './utils.js';

const { invoke } = window.__TAURI__.core;
const { open } = window.__TAURI__.dialog;

// State management
let state = {
  // Full listing from the backend (unfiltered); `images` is the filtered view.
  allImages: [],
  images: [],
  currentIndex: -1,
  currentPath: '',
  folder: '',
  config: { hotkeys: {} },
  isCropActive: false,
  sort: { by: 'name', dir: 1 },
  selectedPaths: new Set(),
  // '' = all types; otherwise a lowercase extension like 'png'.
  filterType: '',
  // true = show only images that carry AI generation metadata (✦).
  filterAI: false,
  // Lowercase substring for the name search (empty = no name filter).
  filterName: '',
  isConverting: false
};

let currentZoom = 1;

// Crop mode state: while active, a Cropper.js instance owns the image —
// the crop box is constrained to the image (viewMode 1), wheel-zooms to the
// cursor, and getData() returns exact natural-pixel coordinates.
let cropper = null;

// UI Elements
const el = {
  imageList: document.getElementById('image-list-container'),
  mainImage: document.getElementById('main-image'),
  viewport: document.getElementById('image-viewport'),
  currentFolder: document.getElementById('current-folder'),
  filePath: document.getElementById('current-file-path'),
  fileMeta: document.getElementById('file-meta'),
  infoDimensions: document.getElementById('info-dimensions'),
  infoDate: document.getElementById('info-date'),
  infoSize: document.getElementById('info-size'),
  fileCount: document.getElementById('file-count'),
  selectionChip: document.getElementById('selection-chip'),
  btnSelectFolder: document.getElementById('btn-select-folder'),
  btnRotate: document.getElementById('btn-rotate'),
  btnCrop: document.getElementById('btn-crop'),
  btnConvert: document.getElementById('btn-convert'),
  filterType: document.getElementById('filter-type'),
  convertMode: document.getElementById('convert-mode'),
  convertProgress: document.getElementById('convert-progress'),
  convertProgressFill: document.getElementById('convert-progress-fill'),
  convertProgressText: document.getElementById('convert-progress-text'),
  btnCancelConvert: document.getElementById('btn-cancel-convert'),
  shortcutLegend: document.getElementById('shortcut-legend'),
  cropActions: document.getElementById('crop-actions'),
  btnCropApply: document.getElementById('btn-crop-apply'),
  btnCropCancel: document.getElementById('btn-crop-cancel'),
  modalSettings: document.getElementById('modal-settings'),
  hotkeyEditList: document.getElementById('hotkey-edit-list'),
  modalMetadata: document.getElementById('modal-metadata'),
  metadataBody: document.getElementById('metadata-body'),
  metaCorner: document.getElementById('btn-meta-corner'),
  modalReport: document.getElementById('modal-report'),
  reportBody: document.getElementById('report-body'),
  btnToggleInfo: document.getElementById('btn-toggle-info'),
  filterAI: document.getElementById('filter-ai'),
  btnSearch: document.getElementById('btn-search'),
  searchRow: document.getElementById('search-row'),
  searchInput: document.getElementById('search-input'),
  btnSearchClear: document.getElementById('btn-search-clear'),
  infoZoom: document.getElementById('info-zoom')
};

// True when any modal is open (settings, metadata or conversion report). Used
// to suppress global hotkeys (Delete, Enter, A–Z, Esc) that must not fire
// behind a modal.
function anyModalOpen() {
  return !el.modalSettings.classList.contains('hidden') ||
         !el.modalMetadata.classList.contains('hidden') ||
         !el.modalReport.classList.contains('hidden');
}

// File-operation hotkeys (Delete, A–Z move) stay active while the metadata
// modal is open, so files can be triaged by prompt presence without closing
// it. The settings modal (editable fields) and the conversion report (a
// passive dialog over a possibly-changed file list) do block them.
function fileOpsAllowed() {
  return el.modalSettings.classList.contains('hidden') &&
         el.modalReport.classList.contains('hidden');
}

// --- Modal focus management (ARIA dialog contract) -------------------------

let lastFocusedEl = null;

function openModal(modal, focusTarget) {
  lastFocusedEl = document.activeElement;
  modal.classList.remove('hidden');
  if (focusTarget) focusTarget.focus();
}

function closeModal(modal) {
  modal.classList.add('hidden');
  if (lastFocusedEl && lastFocusedEl.isConnected) lastFocusedEl.focus();
  lastFocusedEl = null;
}

// Keep Tab cycling inside the open modal instead of wandering the page
// behind it.
function trapModalTab(e) {
  const modal = [el.modalSettings, el.modalMetadata, el.modalReport]
    .find(m => !m.classList.contains('hidden'));
  if (!modal) return;
  const focusables = [...modal.querySelectorAll(
    'button, input, select, textarea, a[href], [tabindex]:not([tabindex="-1"])'
  )].filter(n => !n.disabled && n.offsetParent !== null);
  if (!focusables.length) {
    e.preventDefault();
    return;
  }
  const first = focusables[0];
  const last = focusables[focusables.length - 1];
  const active = document.activeElement;
  if (!modal.contains(active) || (e.shiftKey && active === first)) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && active === last) {
    e.preventDefault();
    first.focus();
  }
}

// Header row: the open folder is the orientation anchor of the whole app —
// show it even though switching folders is a secondary action.
function updateFolderHeader() {
  const name = state.folder ? basename(state.folder) : '';
  el.currentFolder.textContent = name || 'No folder selected';
  el.currentFolder.title = state.folder || '';
}

// One date format everywhere (footer + metadata modal): pinned ru-RU.
function formatDateTime(ts) {
  const date = new Date(ts * 1000);
  return date.toLocaleDateString('ru-RU', { day: '2-digit', month: '2-digit', year: 'numeric' }) + ' ' +
         date.toLocaleTimeString('ru-RU', { hour: '2-digit', minute: '2-digit' });
}

// --- Config persistence ----------------------------------------------------

// Config saves go through one debounced path: navigation (selectImage) fires
// a save per keypress otherwise, and each one becomes a separate file write
// on the Rust side. Explicit user changes (Options, sort, toggles) flush
// immediately.
let saveConfigTimer = null;
let configSaveFailed = false;

async function flushConfigSave() {
  clearTimeout(saveConfigTimer);
  saveConfigTimer = null;
  try {
    await invoke('save_config', { config: state.config });
  } catch (err) {
    // One visible failure beats a toast per save: settings silently not
    // persisting (e.g. read-only config location) must be surfaced once.
    if (!configSaveFailed) {
      configSaveFailed = true;
      notify('Failed to save settings: ' + err);
    }
  }
}

function scheduleConfigSave(immediate = false) {
  if (immediate) {
    flushConfigSave();
    return;
  }
  if (saveConfigTimer) return;
  saveConfigTimer = setTimeout(flushConfigSave, 500);
}

// Transient error/info toast
let toastTimer;
function notify(msg, isError = true) {
  let t = document.getElementById('toast');
  if (!t) {
    t = document.createElement('div');
    t.id = 'toast';
    document.body.appendChild(t);
  }
  t.textContent = msg;
  t.classList.toggle('error', isError);
  clearTimeout(toastTimer);
  if (msg) toastTimer = setTimeout(() => (t.textContent = ''), 4000);
}

// Initialization
async function init() {
  // A config that fails to load must not leave a dead UI — fall back to
  // defaults and keep booting.
  try {
    state.config = await invoke('get_config');
  } catch (err) {
    state.config = { hotkeys: {} };
    notify('Failed to load settings, using defaults: ' + err);
  }
  if (state.config.sort_by) state.sort.by = state.config.sort_by;
  if (state.config.sort_dir) state.sort.dir = state.config.sort_dir;

  document.getElementById('sort-by').value = state.sort.by;
  updateSortButtonUI();

  renderShortcuts();
  setupEventListeners();
  // A path passed on the command line ("Open with…") takes priority over the
  // remembered folder.
  let launchTarget = null;
  try {
    launchTarget = await invoke('get_launch_target');
  } catch (err) {
    console.warn('Failed to read launch target:', err);
  }
  if (launchTarget) {
    await openLaunchTarget(launchTarget);
  } else if (state.config.last_folder) {
    state.folder = state.config.last_folder;
    await loadFiles(state.config.last_file);
  }
  
  if (state.config.minimize_to_tray !== false) {
    document.getElementById('toggle-tray').checked = true;
  }
  if (state.config.convert_replace === true) {
    document.getElementById('toggle-convert-replace').checked = true;
    el.convertMode.value = 'replace';
  }
  if (state.config.convert_show_report !== false) {
    document.getElementById('toggle-convert-report').checked = true;
  }
  if (state.config.context_menu === true) {
    document.getElementById('toggle-context-menu').checked = true;
  }
  applyInfoPanel(state.config.info_panel_hidden === true);

  updateHistoryStats();
}

// The right info panel (shortcuts + commands) is the user's to reclaim: one
// footer button flips the state, naming the ACTION it performs (▸ hides,
// ◂ shows). Persisted via config.info_panel_hidden.
function applyInfoPanel(hidden) {
  document.body.classList.toggle('info-hidden', hidden);
  el.btnToggleInfo.textContent = hidden ? '◂' : '▸';
  el.btnToggleInfo.title = hidden ? 'Show info panel (Ctrl+B)' : 'Hide info panel (Ctrl+B)';
}

function toggleInfoPanel() {
  const hidden = !document.body.classList.contains('info-hidden');
  applyInfoPanel(hidden);
  state.config.info_panel_hidden = hidden;
  scheduleConfigSave(true);
}

function setupEventListeners() {
  // A second instance was launched with a file/folder ("Open with…" while the
  // app is already running).
  window.__TAURI__.event.listen('open-file', (event) => {
    if (event.payload) openLaunchTarget(event.payload);
  });
  el.btnSelectFolder.addEventListener('click', selectFolder);
  document.getElementById('btn-config-hotkeys').addEventListener('click', toggleSettings);
  document.getElementById('btn-close-settings').addEventListener('click', toggleSettings);
  document.getElementById('btn-close-metadata').addEventListener('click', () => toggleMetadataModal());
  el.metaCorner.addEventListener('click', () => toggleMetadataModal());
  el.btnRotate.addEventListener('click', () => doRotate());
  el.btnCrop.addEventListener('click', () => toggleCrop());
  el.btnConvert.addEventListener('click', () => doConvert());
  document.getElementById('btn-undo').addEventListener('click', () => doUndo());
  document.getElementById('btn-redo').addEventListener('click', () => doRedo());
  document.getElementById('btn-refresh').addEventListener('click', () => loadFiles(state.currentPath));

  // Type filter (its options are rebuilt on every folder load)
  el.filterType.addEventListener('change', (e) => {
    state.filterType = e.target.value;
    applyFilter();
  });

  // Selection chip = one-click clear
  el.selectionChip.addEventListener('click', () => {
    state.selectedPaths.clear();
    applyRowClasses();
  });

  // ✦ AI-metadata filter: first enable triggers a folder-wide scan (chunked,
  // with progress in the counter) — badge data is lazy, the filter needs the
  // complete picture before the list can be trusted.
  el.filterAI.addEventListener('click', async () => {
    if (promptScanRunning) return;
    setFilterAI(!state.filterAI);
    if (state.filterAI) await ensurePromptScan();
    applyFilter();
  });

  // Info panel toggle (also Ctrl+B)
  el.btnToggleInfo.addEventListener('click', toggleInfoPanel);

  // Zoom readout doubles as click-to-reset
  el.infoZoom.addEventListener('click', resetZoom);

  // Name search: toggle row, live substring filter, Esc/clear semantics
  el.btnSearch.addEventListener('click', toggleNameSearch);
  el.searchInput.addEventListener('input', () => {
    state.filterName = el.searchInput.value.trim().toLowerCase();
    applyFilter();
  });
  el.searchInput.addEventListener('keydown', (e) => {
    // The global handler skips text fields, so Esc semantics live here:
    // first Esc wipes the query, second closes the row.
    e.stopPropagation();
    if (e.key === 'Escape') {
      if (el.searchInput.value) {
        clearNameSearch();
      } else {
        closeNameSearch();
      }
    } else if (e.key === 'Enter') {
      e.preventDefault();
      el.searchInput.blur();
    }
  });
  el.btnSearchClear.addEventListener('click', () => {
    clearNameSearch();
    el.searchInput.focus();
  });

  // Cancel the running conversion; created JPGs stay on disk.
  el.btnCancelConvert.addEventListener('click', () => {
    invoke('cancel_convert');
    el.btnCancelConvert.disabled = true;
  });

  // Conversion mode (Next to source / Replace source): the toolbar switch and
  // the settings checkbox stay in sync — both write convert_replace.
  el.convertMode.addEventListener('change', (e) => {
    state.config.convert_replace = e.target.value === 'replace';
    document.getElementById('toggle-convert-replace').checked = state.config.convert_replace;
    scheduleConfigSave(true);
  });
  document.getElementById('toggle-convert-replace').addEventListener('change', (e) => {
    state.config.convert_replace = e.target.checked;
    el.convertMode.value = state.config.convert_replace ? 'replace' : 'next';
    scheduleConfigSave(true);
  });
  document.getElementById('toggle-convert-report').addEventListener('change', (e) => {
    state.config.convert_show_report = e.target.checked;
    scheduleConfigSave(true);
  });
  // Explorer context-menu registration (HKCU — no admin needed). Reverting
  // the toggle on failure keeps the UI truthful about what's registered.
  document.getElementById('toggle-context-menu').addEventListener('change', async (e) => {
    const want = e.target.checked;
    try {
      await invoke('set_context_menu', { enabled: want });
      state.config.context_menu = want;
      await flushConfigSave();
      notify(want ? 'Context menu items added' : 'Context menu items removed', false);
    } catch (err) {
      e.target.checked = !want;
      notify('Context menu: ' + err);
    }
  });
  document.getElementById('btn-close-report').addEventListener('click', () => closeReportModal());

  // Live conversion progress (Rust emits one event per file). Rendered with
  // a 50 ms throttle: at thousands of files the events arrive far faster than
  // the screen can repaint, and every synchronous DOM write would otherwise
  // add up.
  let progressThrottle = null;
  window.__TAURI__.event.listen('convert-progress', (event) => {
    if (!event.payload) return;
    const payload = event.payload;
    if (progressThrottle) return; // last write still fresh
    showConvertProgress(payload.done, payload.total, payload.current);
    progressThrottle = setTimeout(() => { progressThrottle = null; }, 50);
  });
  
  document.getElementById('toggle-tray').addEventListener('change', (e) => {
    state.config.minimize_to_tray = e.target.checked;
    scheduleConfigSave(true);
  });

  el.mainImage.addEventListener('wheel', (e) => {
    e.preventDefault();
    applyZoom(e.deltaY);
  }, { passive: false });
  // While crop is active Cropper owns the image (the original <img> is
  // hidden), including its own wheel zoom — nothing to forward here.

  window.addEventListener('resize', () => {
    // Cropper handles its own re-layout while active (responsive: true).
    scheduleRenderWindow();
  });

  // Virtual list: re-render the window on scroll
  el.imageList.addEventListener('scroll', () => scheduleRenderWindow(), { passive: true });

  // Global hotkeys (arrows, Delete, Enter, Esc, Insert, Home/End, A–Z moves,
  // Ctrl+Q/R/C/Z/X). Note: this must stay registered — a refactor removed it
  // once, silently disabling every keyboard shortcut.
  window.addEventListener('keydown', handleGlobalKeydown);

  // Delegated row events (rows are recreated per window)
  el.imageList.addEventListener('click', (e) => {
    const row = e.target.closest('.list-item');
    if (!row) return;
    const index = Number(row.dataset.index);
    if (!Number.isInteger(index)) return;
    handleItemClick(e, index);
  });
  el.imageList.addEventListener('dblclick', (e) => {
    const row = e.target.closest('.list-item');
    if (!row) return;
    const index = Number(row.dataset.index);
    if (!Number.isInteger(index)) return;
    startRename(index, row);
  });
  
  
  // Sorting
  document.getElementById('sort-by').addEventListener('change', (e) => {
    state.sort.by = e.target.value;
    state.config.sort_by = state.sort.by;
    scheduleConfigSave(true);
    sortAndRender();
  });
  document.getElementById('btn-sort-dir').addEventListener('click', () => {
    state.sort.dir *= -1;
    state.config.sort_dir = state.sort.dir;
    scheduleConfigSave(true);
    updateSortButtonUI();
    sortAndRender();
  });

  // Fullscreen: double-click the image or the file name
  el.mainImage.addEventListener('dblclick', toggleFullscreen);
  el.filePath.addEventListener('dblclick', toggleFullscreen);

  // Crop (Cropper.js): confirm on double-click inside the selection (not on
  // its resize points), or via the buttons; Esc/Cancel destroy the cropper.
  el.viewport.addEventListener('dblclick', (e) => {
    if (!state.isCropActive) return;
    if (e.target.closest('.cropper-point')) return;
    if (!e.target.closest('.cropper-crop-box')) return;
    executeCrop();
  });
  el.btnCropApply.addEventListener('click', () => executeCrop());
  el.btnCropCancel.addEventListener('click', () => toggleCrop());
}

function updateSortButtonUI() {
  const btn = document.getElementById('btn-sort-dir');
  btn.innerHTML = state.sort.dir === 1
    ? '<span class="on">↑</span> <span class="off">↓</span>'
    : '<span class="off">↑</span> <span class="on">↓</span>';
}

// Opens a folder, or the folder containing the given file with that file
// selected ("Open with…" from Explorer passes the path on the command line).
async function openLaunchTarget(target) {
  state.folder = target.dir;
  state.config.last_folder = target.dir;
  state.config.last_file = target.is_dir ? null : target.path;
  scheduleConfigSave();
  if (target.is_dir) {
    el.imageList.scrollTop = 0;
    await loadFiles();
  } else {
    await loadFiles(target.path);
    // "Show Metadata Info" context-menu item: open the modal right away.
    if (target.action === 'metadata') {
      await toggleMetadataModal();
    }
  }
}

// Actions
async function selectFolder() {
  const selected = await open({
    directory: true,
    multiple: false,
  });
  if (selected) {
    state.folder = selected;
    state.config.last_folder = selected;
    scheduleConfigSave(true);
    // A brand-new folder starts at the top of the list
    el.imageList.scrollTop = 0;
    loadFiles();
  }
}

// Remembers which folder loadFiles last served, to tell a folder switch
// (AI filter resets) from a same-folder refresh (filter persists + rescan).
let lastLoadedFolder = null;

async function loadFiles(initialLoadPath = null) {
  updateFolderHeader();
  const folderChanged = state.folder !== lastLoadedFolder;
  lastLoadedFolder = state.folder;
  // A new folder starts unfiltered: enabling the AI filter means a
  // folder-wide scan, which must never run implicitly at startup.
  if (folderChanged) setFilterAI(false);
  try {
    state.allImages = await invoke('list_files', { dir: state.folder });
    // The folder listing changed (files moved/edited/deleted): prompt badges
    // from the previous listing are stale, so rebuild the cache from scratch.
    promptPaths.clear();
    promptChecked.clear();
    lastListSignature = null;
    // Filter stayed on across a same-folder refresh (move/delete/convert):
    // the caches were just cleared, so rescan before filtering.
    if (state.filterAI) await ensurePromptScan();
    rebuildFilterOptions();
    state.images = filterImages();
    sortAndRender(initialLoadPath);
    // The current image may have disappeared (deleted/moved last file) — hide
    // the corner button so it never points at a stale selection.
    updateMetaCorner();
    // With nothing left to view, the metadata modal must not keep showing the
    // removed file's data.
    if (state.images.length === 0 && !el.modalMetadata.classList.contains('hidden')) {
      el.metadataBody.innerHTML = '<div class="metadata-noai">No images left.</div>';
    }
  } catch (err) {
    state.allImages = [];
    state.images = [];
    promptPaths.clear();
    promptChecked.clear();
    // Reset like the success path does: a stale signature would make the next
    // successful load of the same folder skip the DOM rebuild (spacer height
    // stays 0 → the list can't scroll).
    lastListSignature = null;
    rebuildFilterOptions();
    renderList();
    el.fileCount.textContent = '0 images';
    updateMetaCorner();
    notify("Cannot load folder: " + err);
  }
}

// Rebuilds the type-filter dropdown from the extensions actually present in
// the current folder. The list is already in memory, so this is a cheap pass
// (no disk scanning) even for 20k-file folders.
function rebuildFilterOptions() {
  // ImageInfo.extension is normalized to lowercase by the backend.
  const types = new Set(state.allImages.map(i => i.extension));
  const current = state.filterType;
  el.filterType.innerHTML = '';
  const all = document.createElement('option');
  all.value = '';
  all.textContent = 'All types';
  el.filterType.appendChild(all);
  [...types].sort().forEach(t => {
    const opt = document.createElement('option');
    opt.value = t;
    opt.textContent = t.toUpperCase();
    el.filterType.appendChild(opt);
  });
  // Drop a filter whose type no longer exists in this folder
  if (current && !types.has(current)) state.filterType = '';
  el.filterType.value = state.filterType;
}

// Applies state.filterType + state.filterAI to state.allImages, producing
// the visible list. The AI filter requires promptPaths to be complete — the
// toggle handler runs the full-folder scan before applying.
function filterImages() {
  let list = state.filterType
    ? state.allImages.filter(i => i.extension === state.filterType)
    : state.allImages.slice();
  if (state.filterAI) {
    list = list.filter(i => promptPaths.has(i.path));
  }
  if (state.filterName) {
    const needle = state.filterName;
    list = list.filter(i => i.name.toLowerCase().includes(needle));
  }
  return list;
}

// True when any view filter narrows the list — drives the "N of M" counter
// and the "no matches" hint.
function anyFilterActive() {
  return !!(state.filterType || state.filterAI || state.filterName);
}

// AI-metadata filter: visual state of the ✦ toggle.
function setFilterAI(on) {
  state.filterAI = on;
  el.filterAI.classList.toggle('active', on);
  el.filterAI.setAttribute('aria-pressed', String(on));
}

// The lazy badge scan only covers the visible window — a folder-wide filter
// needs the status of every file. Scans in chunks so the UI stays responsive
// and the counter can show progress; results feed the same promptPaths cache
// the badges use.
let promptScanRunning = false;
async function ensurePromptScan() {
  const unscanned = state.allImages.map(i => i.path).filter(p => !promptChecked.has(p));
  if (!unscanned.length || promptScanRunning) return;
  promptScanRunning = true;
  const scanFolder = state.folder;
  const CHUNK = 100;
  try {
    for (let i = 0; i < unscanned.length; i += CHUNK) {
      // Folder changed or the user toggled the filter off mid-scan — stop.
      if (!state.filterAI || state.folder !== scanFolder) break;
      const chunk = unscanned.slice(i, i + CHUNK);
      const found = await invoke('check_prompts', { paths: chunk });
      found.forEach(p => promptPaths.add(p));
      chunk.forEach(p => promptChecked.add(p));
      el.fileCount.textContent = `Scanning ${Math.min(i + CHUNK, unscanned.length)} / ${unscanned.length}…`;
    }
  } catch (err) {
    notify('Metadata scan failed: ' + err);
  }
  promptScanRunning = false;
}

function applyFilter() {
  state.images = filterImages();
  sortAndRender(state.currentPath);
}

// --- Name search -----------------------------------------------------------

// Live substring filter over file names (any part of the name, case-
// insensitive). The input lives in its own header row that appears on
// demand (⌕ button / Ctrl+F) so the one-line header is not crowded.
function openNameSearch() {
  el.searchRow.classList.remove('hidden');
  el.btnSearch.classList.add('active');
  el.searchInput.focus();
}

function closeNameSearch() {
  clearNameSearch();
  el.searchRow.classList.add('hidden');
  el.btnSearch.classList.remove('active');
  el.searchInput.blur();
}

function clearNameSearch() {
  if (!state.filterName && !el.searchInput.value) return;
  state.filterName = '';
  el.searchInput.value = '';
  applyFilter();
}

function toggleNameSearch() {
  if (el.searchRow.classList.contains('hidden')) {
    openNameSearch();
  } else {
    closeNameSearch();
  }
}

// Selects all VISIBLE rows (the type filter narrows what "all" means: filter
// to PNG, then Ctrl+A selects exactly the PNGs). Pressing it again when
// everything is already selected clears the selection.
function selectAllOrClear() {
  const allVisible = state.images.map(i => i.path);
  const allSelected = allVisible.length > 0
    && allVisible.every(p => state.selectedPaths.has(p));
  if (allSelected) {
    state.selectedPaths.clear();
    applyRowClasses();
    notify('Selection cleared', false);
  } else {
    state.selectedPaths.clear();
    allVisible.forEach(p => state.selectedPaths.add(p));
    applyRowClasses();
    notify(state.selectedPaths.size + ' selected', false);
  }
}

// null (not '') so the very first sortAndRender always builds the DOM —
// otherwise the empty-folder hint never appears on a fresh start.
let lastListSignature = null;

function sortAndRender(initialLoadPath = null) {
  state.images.sort((a, b) => compareImages(a, b, state.sort.by, state.sort.dir));
  // Avoid rebuilding the DOM when the list content/order is unchanged
  const signature = state.images.map(i => i.path).join('\n');
  if (signature !== lastListSignature) {
    renderList();
    lastListSignature = signature;
  }
  el.fileCount.textContent = anyFilterActive()
    ? `${state.images.length} of ${state.allImages.length} images`
    : `${state.images.length} images`;
  
  if (initialLoadPath) {
      let idx = state.images.findIndex(i => i.path === initialLoadPath);
      // Windows paths are case-insensitive: fall back to a case-folded match
      // (e.g. when the shell passed a differently-cased path).
      if (idx === -1) {
          const needle = initialLoadPath.toLowerCase();
          idx = state.images.findIndex(i => i.path.toLowerCase() === needle);
      }
      if (idx !== -1) {
          selectImage(idx);
          return;
      }
  }

  // Follow current path
  if (state.currentPath) {
     const newIndex = state.images.findIndex(i => i.path === state.currentPath);
     if (newIndex !== -1) {
         state.currentIndex = newIndex;
         applyRowClasses();
         scrollIndexIntoView(newIndex);
     } else {
         selectImage(0);
     }
  } else if (state.images.length > 0) {
      selectImage(0);
  }
}

// Virtual List Rendering: only the visible window ± overscan rows are in the
// DOM, absolutely positioned; the spacer sets the scroll height. This keeps
// rendering O(visible) instead of O(total) on large folders.
const ROW_H = 32;
const OVERSCAN = 20;
let listStart = 0;
let listEnd = 0;

// Lazy "has AI prompt" badges. `promptPaths` is a Set of file paths known to
// carry generation metadata; `promptChecked` remembers every path already
// queried (positive or negative) so scrolling never re-reads the same files.
// Both are populated only for the rendered window (±overscan) via
// check_prompts, so scanning stays O(visible) instead of O(folder).
const promptPaths = new Set();
const promptChecked = new Set();
let promptCheckPending = null;

// Request badge info for the not-yet-checked paths in the rendered window.
function schedulePromptCheck() {
  if (promptCheckPending) return;
  promptCheckPending = requestAnimationFrame(() => {
    promptCheckPending = null;
    // Don't scan while an inline-rename input is active
    if (renamingIndex !== -1) return;
    const { start, end } = computeListRange();
    const slice = state.images.slice(Math.max(0, start), Math.min(state.images.length, end));
    const unqueried = slice.map(i => i.path).filter(p => !promptChecked.has(p));
    if (!unqueried.length) return;
    invoke('check_prompts', { paths: unqueried })
      .then(found => {
        const before = promptPaths.size;
        found.forEach(p => promptPaths.add(p));
        unqueried.forEach(p => promptChecked.add(p));
        // The corner metadata button depends on the same data — refresh it in
        // case the current image was just checked.
        updateMetaCorner();
        // Only re-render when something changed (and never while a rename
        // input is live — the rAF path checks this, the async .then must too).
        // force: the visible range is unchanged, but the badges inside the
        // already-rendered rows are not — a plain renderWindow() would early-
        // return and the ✦ marks would only appear after the next scroll.
        if (promptPaths.size !== before && renamingIndex === -1) {
          renderWindow(true);
        }
      })
      .catch(() => {});
  });
}

// Render the badge glyph for a row, if its path is known to have a prompt.
function hasPromptBadge(path) {
  return promptPaths.has(path);
}

// Show/hide the corner metadata button: visible only when the current image
// carries AI-generation metadata (same lazy data as the list badges).
function updateMetaCorner() {
  const show = !!state.currentPath && promptPaths.has(state.currentPath);
  el.metaCorner.classList.toggle('hidden', !show);
}

function renderList() {
  const container = el.imageList;
  // Preserve scroll across rebuilds (e.g. rename cancel) where reasonable;
  // the browser clamps it if the new list is shorter.
  const prevScroll = container.scrollTop;
  container.innerHTML = '';
  const spacer = document.createElement('div');
  spacer.className = 'list-spacer';
  spacer.style.height = (state.images.length * ROW_H) + 'px';
  container.appendChild(spacer);
  if (state.images.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'list-empty';
    // Distinguish an empty folder from filters that match nothing.
    empty.textContent = state.allImages.length > 0
      ? 'No images match the current filters'
      : 'No images in this folder';
    container.appendChild(empty);
  }
  container.scrollTop = prevScroll;
  listStart = 0;
  listEnd = 0;
  scheduleRenderWindow();
}

function computeListRange() {
  return computeVisibleRange(
    el.imageList.scrollTop,
    el.imageList.clientHeight,
    ROW_H,
    state.images.length,
    OVERSCAN
  );
}

function renderWindow(force = false) {
  const container = el.imageList;
  const { start, end } = computeListRange();
  if (!force && start === listStart && end === listEnd) return;
  listStart = start;
  listEnd = end;

  container.querySelectorAll('.list-item').forEach((n) => n.remove());
  for (let i = start; i < end; i++) {
    const img = state.images[i];
    const div = document.createElement('div');
    div.className = 'list-item';
    div.dataset.index = String(i);
    div.style.top = (i * ROW_H) + 'px';
    div.title = img.name;
    // Name text
    const name = document.createElement('span');
    name.className = 'list-item-name';
    name.textContent = img.name;
    div.appendChild(name);
    // AI prompt badge (lazy — filled in by schedulePromptCheck)
    if (hasPromptBadge(img.path)) {
      const badge = document.createElement('span');
      badge.className = 'prompt-badge';
      badge.textContent = '✦';
      badge.title = 'Contains AI generation metadata — press Ctrl+Q';
      div.appendChild(badge);
    }
    container.appendChild(div);
  }
  applyRowClasses();
  schedulePromptCheck();
}

// Refresh active/selected classes for the currently rendered window.
function applyRowClasses() {
  el.imageList.querySelectorAll('.list-item').forEach((row) => {
    const i = Number(row.dataset.index);
    const img = state.images[i];
    row.classList.toggle('active', i === state.currentIndex);
    row.classList.toggle('selected', state.selectedPaths.has(img.path));
  });
  updateSelectionChip();
}

// Selection state must be visible without scanning rows; the chip doubles as
// the one-click "clear" affordance for mouse users.
function updateSelectionChip() {
  const n = state.selectedPaths.size;
  el.selectionChip.classList.toggle('hidden', n === 0);
  if (n > 0) el.selectionChip.textContent = `${n} selected ✕`;
}

let windowRaf = null;
function scheduleRenderWindow() {
  if (windowRaf) return;
  windowRaf = requestAnimationFrame(() => {
    windowRaf = null;
    // Don't rebuild the row that holds an active inline-rename input
    if (renamingIndex !== -1) return;
    renderWindow();
  });
}

// Ensure row `index` is visible, scrolling the container if needed.
function scrollIndexIntoView(index) {
  const container = el.imageList;
  const rowTop = index * ROW_H;
  const viewportH = container.clientHeight;
  if (rowTop < container.scrollTop || rowTop + ROW_H > container.scrollTop + viewportH) {
    container.scrollTop = Math.max(0, rowTop - (viewportH - ROW_H) / 2);
  }
  scheduleRenderWindow();
}

// Inline rename of a list item (double-click the name)
let renamingIndex = -1;

function startRename(index, itemEl) {
  if (renamingIndex !== -1 || index < 0 || index >= state.images.length) return;
  const img = state.images[index];
  renamingIndex = index;

  itemEl.textContent = '';
  const input = document.createElement('input');
  input.type = 'text';
  input.className = 'rename-input';
  input.value = img.name;
  itemEl.appendChild(input);
  input.focus();
  // Pre-select the file stem so typing replaces the name but keeps the extension
  const dot = img.name.lastIndexOf('.');
  const selEnd = dot > 0 ? dot : img.name.length;
  input.setSelectionRange(0, selEnd);

  let finished = false;
  const finish = async (commit) => {
    if (finished) return;
    finished = true;
    renamingIndex = -1;
    const newName = input.value.trim();
    if (commit && newName && newName !== img.name) {
      try {
        const newPath = await invoke('rename_file', { path: img.path, newName });
        notify(`Renamed to ${basename(newPath)}`, false);
        await loadFiles(newPath);
      } catch (err) {
        notify("Rename failed: " + err);
        renderList();
      }
      return;
    }
    renderList();
  };

  input.addEventListener('keydown', (e) => {
    e.stopPropagation();
    if (e.key === 'Enter') finish(true);
    else if (e.key === 'Escape') finish(false);
  });
  input.addEventListener('blur', () => finish(true));
}

function handleItemClick(e, index) {
  const path = state.images[index].path;
  if (e.ctrlKey) {
    if (state.selectedPaths.has(path)) state.selectedPaths.delete(path);
    else state.selectedPaths.add(path);
    selectImage(index);
  } else if (e.shiftKey) {
    const start = Math.min(state.currentIndex === -1 ? 0 : state.currentIndex, index);
    const end = Math.max(state.currentIndex === -1 ? 0 : state.currentIndex, index);
    state.selectedPaths.clear();
    for (let i = start; i <= end; i++) {
        state.selectedPaths.add(state.images[i].path);
    }
    selectImage(index);
  } else {
    state.selectedPaths.clear();
    selectImage(index);
  }
}

async function selectImage(index) {
  if (index < 0 || index >= state.images.length) return;
  const img = state.images[index];
  // The crop region was chosen for a different file — never let it crop
  // whatever file happens to be shown next (e.g. after PNG→JPG replace moved
  // the source to the trash and the selection jumped to a neighbour).
  if (state.isCropActive && img.path !== state.currentPath) {
    toggleCrop();
  }
  state.currentIndex = index;
  state.currentPath = img.path;

  state.config.last_file = img.path;
  scheduleConfigSave();
  
  // Use Tauri's convertFileSrc for local images. The cache-buster forces the
  // webview to re-fetch from disk: without it, the same URL (this file was
  // already displayed before the last rotate/crop/undo/redo) can be served
  // from the webview cache and undo/redo appear to do nothing.
  const src = window.__TAURI__.core.convertFileSrc(img.path) + '?t=' + Date.now();
  el.mainImage.src = src;
  el.filePath.textContent = img.path;
  
  currentZoom = 1;
  el.mainImage.style.transform = 'scale(1)';
  updateZoomIndicator();

  // Update meta info
  el.infoDate.textContent = formatDateTime(img.modified);
  el.infoSize.textContent = formatBytes(img.size);
  el.infoDimensions.textContent = '…px';
  el.fileMeta.classList.remove('hidden');
  el.filePath.title = img.path;
  // get natural dimensions after load
  el.mainImage.onload = () => {
    el.infoDimensions.textContent = `${el.mainImage.naturalWidth} × ${el.mainImage.naturalHeight} px`;
  };
  
  // Update active and selected statuses in the rendered window
  applyRowClasses();

  // Corner metadata button follows the current image
  updateMetaCorner();

  // Live metadata preview: when the metadata modal is open, refresh it for the
  // newly focused image instead of requiring open/close on every file.
  if (!el.modalMetadata.classList.contains('hidden')) {
    loadMetadata(false);
  }

  // Scroll into view if needed
  scrollIndexIntoView(index);
}

// Global Keys
async function handleGlobalKeydown(e) {
  // Tab stays inside the open modal (ARIA dialog contract).
  if (e.key === 'Tab' && anyModalOpen()) {
    trapModalTab(e);
    return;
  }
  const active = document.activeElement;
  const key = e.key.toLowerCase();
  const code = e.code;
  // In text fields Ctrl+A means "select all text" (rename input), so let the
  // browser handle it there. Everywhere else it selects all visible rows —
  // including when focus sits on a <select> (the type filter), which would
  // otherwise make the browser select the whole page instead.
  const inTextField = active && (active.tagName === 'INPUT' || active.tagName === 'TEXTAREA' || active.isContentEditable);
  if (e.ctrlKey && (e.code === 'KeyA' || key === 'a') && !inTextField && !anyModalOpen()) {
    e.preventDefault();
    selectAllOrClear();
    // Drop focus from the type-filter <select> so the next Ctrl+A (and the
    // arrow keys) behave like the list owns the keyboard again.
    if (active && active.tagName === 'SELECT') active.blur();
    return;
  }
  if (active && (active.tagName === 'INPUT' || active.tagName === 'SELECT' || active.tagName === 'TEXTAREA' || active.isContentEditable)) {
    // Plain keys and text-editing combos (A/C/X/V/Z — select, copy, cut,
    // paste, undo) belong to the field. The app's other Ctrl shortcuts
    // (0/F/Q/J/R/B) keep working while a field has focus — e.g. the
    // persistent search input must not silence Ctrl+0/F/Q/B.
    if (!e.ctrlKey || ['KeyA', 'KeyC', 'KeyX', 'KeyV', 'KeyZ', 'KeyY'].includes(code)) return;
  }

  if (code === 'ArrowDown' || key === 'arrowdown') {
    e.preventDefault();
    if (e.shiftKey && state.currentIndex >= 0 && state.currentIndex < state.images.length) {
      state.selectedPaths.add(state.currentPath);
      const nextIndex = state.currentIndex + 1;
      if (nextIndex < state.images.length) {
         state.selectedPaths.add(state.images[nextIndex].path);
      }
    }
    selectImage(state.currentIndex + 1);
  } else if (code === 'ArrowUp' || key === 'arrowup') {
    e.preventDefault();
    if (e.shiftKey && state.currentIndex >= 0 && state.currentIndex < state.images.length) {
      state.selectedPaths.add(state.currentPath);
      const nextIndex = state.currentIndex - 1;
      if (nextIndex >= 0) {
         state.selectedPaths.add(state.images[nextIndex].path);
      }
    }
    selectImage(state.currentIndex - 1);
  } else if (code === 'Home' || key === 'home') {
    e.preventDefault();
    if (state.images.length > 0) selectImage(0);
  } else if (code === 'End' || key === 'end') {
    e.preventDefault();
    if (state.images.length > 0) selectImage(state.images.length - 1);
  } else if (code === 'Escape' || key === 'escape') {
    e.preventDefault();
    if (state.isCropActive) {
      toggleCrop();
    } else if (!el.modalMetadata.classList.contains('hidden')) {
      toggleMetadataModal();
    } else if (!el.modalReport.classList.contains('hidden')) {
      closeReportModal();
    } else if (!el.modalSettings.classList.contains('hidden')) {
      toggleSettings();
    } else if (await isFullscreenActive()) {
      // Exit fullscreen first; surface failures instead of hiding the window
      try {
        const win = window.__TAURI__.window.getCurrentWindow();
        await win.setFullscreen(false);
      } catch (err) {
        notify("Failed to exit fullscreen: " + err);
      }
    } else {
      invoke('hide_window');
    }
  } else if (code === 'Insert' || key === 'insert') {
    e.preventDefault();
    if (state.currentPath) {
        if (state.selectedPaths.has(state.currentPath)) state.selectedPaths.delete(state.currentPath);
        else state.selectedPaths.add(state.currentPath);
        
        if (state.currentIndex < state.images.length - 1) {
            selectImage(state.currentIndex + 1);
        } else {
            renderList();
        }
    }
  } else if ((code === 'Delete' || key === 'delete') && !e.ctrlKey && !e.metaKey && !e.altKey && fileOpsAllowed()) {
    if (state.currentPath) {
      const pathsToRemove = state.selectedPaths.size > 0 ? Array.from(state.selectedPaths) : [state.currentPath];
      
      const removalSet = new Set(pathsToRemove);
      let successorPath = null;
      
      // If the file under cursor is NOT being deleted — keep focus on it
      if (!removalSet.has(state.currentPath)) {
        successorPath = state.currentPath;
      } else {
        // Otherwise find the nearest non-deleted neighbour (first look down, then up)
        for (let i = state.currentIndex + 1; i < state.images.length; i++) {
          if (!removalSet.has(state.images[i].path)) {
            successorPath = state.images[i].path;
            break;
          }
        }
        if (!successorPath) {
          for (let i = state.currentIndex - 1; i >= 0; i--) {
            if (!removalSet.has(state.images[i].path)) {
              successorPath = state.images[i].path;
              break;
            }
          }
        }
      }

      try {
        await invoke('trash_images', { paths: pathsToRemove });
        state.selectedPaths.clear();
        await loadFiles(successorPath);
        if (state.images.length === 0) {
            el.mainImage.src = '';
            state.currentPath = '';
        }
      } catch (err) {
        notify("Delete failed: " + err);
        // Some files may have been trashed — reflect the real state on disk,
        // keeping focus on the nearest remaining neighbour if possible
        await loadFiles(successorPath);
      }
    }
  } else if (e.ctrlKey && (code === 'KeyQ' || key === 'q')) {
    e.preventDefault();
    await toggleMetadataModal();
  } else if (e.ctrlKey && (code === 'KeyJ' || key === 'j') && !anyModalOpen()) {
    e.preventDefault();
    await doConvert();
  } else if (e.ctrlKey && (code === 'KeyR' || key === 'r') && !anyModalOpen()) {
    await doRotate();
  } else if (e.ctrlKey && (code === 'KeyC' || key === 'c') && !anyModalOpen()) {
    toggleCrop();
  } else if (e.ctrlKey && (code === 'KeyZ' || key === 'z') && !anyModalOpen()) {
    e.preventDefault();
    await doUndo();
  } else if (e.ctrlKey && (code === 'KeyX' || key === 'x') && !anyModalOpen()) {
    e.preventDefault();
    await doRedo();
  } else if (e.ctrlKey && (code === 'KeyB' || key === 'b') && !anyModalOpen()) {
    e.preventDefault();
    toggleInfoPanel();
  } else if (e.ctrlKey && (code === 'Digit0' || key === '0') && !anyModalOpen()) {
    // Reset zoom to fit (documented in README/Commands).
    e.preventDefault();
    resetZoom();
  } else if (e.ctrlKey && (code === 'KeyF' || key === 'f') && !anyModalOpen()) {
    e.preventDefault();
    openNameSearch();
  } else if ((key === 'enter' || code === 'Enter') && !(active && active.tagName === 'BUTTON') && !anyModalOpen()) {
    e.preventDefault();
    if (state.isCropActive) {
      // Enter confirms the crop; Esc (handled above) cancels it.
      await executeCrop();
    } else {
      await toggleFullscreen();
    }
  } else if (code && code.startsWith('Key') && !e.ctrlKey && !e.metaKey && !e.altKey && fileOpsAllowed()) {
    const char = code.replace('Key', '').toLowerCase();
    const targetDir = state.config.hotkeys[char];
    if (targetDir && state.currentPath) {
        const pathsToMove = state.selectedPaths.size > 0 ? Array.from(state.selectedPaths) : [state.currentPath];
        
        const removalSet = new Set(pathsToMove);
        let successorPath = null;

        // If the file under cursor is NOT being moved — keep focus on it
        if (!removalSet.has(state.currentPath)) {
          successorPath = state.currentPath;
        } else {
          // Otherwise find the nearest non-moved neighbour (first look down, then up)
          for (let i = state.currentIndex + 1; i < state.images.length; i++) {
            if (!removalSet.has(state.images[i].path)) {
              successorPath = state.images[i].path;
              break;
            }
          }
          if (!successorPath) {
            for (let i = state.currentIndex - 1; i >= 0; i--) {
              if (!removalSet.has(state.images[i].path)) {
                successorPath = state.images[i].path;
                break;
              }
            }
          }
        }

        try {
            await invoke('move_images', { sources: pathsToMove, targetDir });
            state.selectedPaths.clear();
            await loadFiles(successorPath);
            if (state.images.length === 0) {
              el.mainImage.src = '';
              state.currentPath = '';
            }
            updateHistoryStats();
        } catch (err) {
            notify("Move failed: " + err);
            // Some files may have moved — reflect the real state on disk,
            // keeping focus on the nearest remaining neighbour if possible
            await loadFiles(successorPath);
            updateHistoryStats();
        }
    }
  }
}

async function doRotate() {
  if (!state.currentPath) return;
  try {
    await invoke('rotate_image', { path: state.currentPath, degrees: 90 });
    refreshCurrentImage();
    updateHistoryStats();
  } catch (err) { notify("Rotate failed: " + err); }
}

async function doUndo() {
  try {
    const restoredPath = await invoke('undo_last_move');
    if (restoredPath) {
      await loadFiles(restoredPath);
      updateHistoryStats();
    }
  } catch (err) {
    notify("Undo failed: " + err);
    // Some actions may still have been undone — reflect the real state on disk
    await loadFiles();
    updateHistoryStats();
  }
}

async function doRedo() {
  try {
    const restoredPath = await invoke('redo_last_move');
    if (restoredPath) {
      await loadFiles(restoredPath);
      updateHistoryStats();
    }
  } catch (err) {
    notify("Redo failed: " + err);
    // Some actions may still have been redone — reflect the real state on disk
    await loadFiles();
    updateHistoryStats();
  }
}

async function updateHistoryStats() {
  try {
    const [undoCount, redoCount] = await invoke('get_history_stats');
    document.getElementById('count-undo').textContent = undoCount;
    document.getElementById('count-redo').textContent = redoCount;
  } catch(err) { console.warn("Failed to get history stats:", err); }
}

// --- PNG → JPG conversion ----------------------------------------------

// The files to convert: every selected PNG, or the current file if it's a PNG.
function convertTargets() {
  let paths;
  if (state.selectedPaths.size > 0) {
    paths = Array.from(state.selectedPaths);
  } else if (state.currentPath) {
    paths = [state.currentPath];
  } else {
    return [];
  }
  return paths.filter(p => p.toLowerCase().endsWith('.png'));
}

function showConvertProgress(done, total, current) {
  el.convertProgress.classList.remove('hidden');
  const pct = total > 0 ? Math.round((done / total) * 100) : 0;
  el.convertProgressFill.style.width = pct + '%';
  el.convertProgressText.textContent = current
    ? `${done} / ${total} · ${current}`
    : `${done} / ${total}`;
}

function hideConvertProgress() {
  el.convertProgress.classList.add('hidden');
  el.convertProgressFill.style.width = '0%';
  el.convertProgressText.textContent = '';
  el.btnCancelConvert.disabled = false;
}

function formatSizeMB(bytes) {
  return (bytes / (1024 * 1024)).toFixed(1) + ' MB';
}

function closeReportModal() {
  closeModal(el.modalReport);
  el.reportBody.innerHTML = '';
}

// Renders the detailed conversion report into the modal. The modal stays open
// until the user closes it (no auto-dismiss) so large batches can be reviewed.
function showReportModal(report, total) {
  const converted = report.converted || [];
  const skippedLarge = report.skipped_large || [];
  const errors = report.errors || [];
  const processed = converted.length + skippedLarge.length + errors.length;
  const canceled = processed < total;

  let totalIn = 0, totalOut = 0;
  converted.forEach(c => { totalIn += c.in_size; totalOut += c.out_size; });
  const saved = totalIn - totalOut;
  const pct = totalIn > 0 ? Math.round((1 - totalOut / totalIn) * 100) : 0;

  const lines = [];
  const MAX_LINES = 200;
  const push = (arr, map, cls) => {
    arr.slice(0, MAX_LINES).forEach((x, i) => {
      lines.push(`<div class="report-line ${cls || ''}">${map(x, i)}</div>`);
    });
    if (arr.length > MAX_LINES) {
      lines.push(`<div class="report-line">… and ${arr.length - MAX_LINES} more</div>`);
    }
  };

  let summary = `Converted ${converted.length} of ${total} PNG → JPG`;
  if (saved > 0) summary += ` · saved ${formatSizeMB(saved)} (−${pct}%)`;
  if (skippedLarge.length) summary += ` · ${skippedLarge.length} skipped (metadata too large)`;
  if (errors.length) summary += ` · ${errors.length} errors`;
  if (canceled) summary += ' · stopped early';

  let html = `<div class="report-summary">${escapeHtml(summary)}</div>`;
  if (converted.length) {
    html += `<div class="report-section"><h4>Created (${converted.length})</h4>`;
    push(converted, c => `${escapeHtml(basename(c.output))} — ${formatSizeMB(c.in_size)} → ${formatSizeMB(c.out_size)}`);
    html += lines.join('') + '</div>';
    lines.length = 0;
  }
  if (skippedLarge.length) {
    html += `<div class="report-section"><h4>Skipped — metadata too large for JPEG (${skippedLarge.length})</h4>`;
    push(skippedLarge, p => escapeHtml(basename(p)));
    html += lines.join('') + '</div>';
    lines.length = 0;
  }
  if (errors.length) {
    html += `<div class="report-section"><h4>Errors (${errors.length})</h4>`;
    push(errors, e => escapeHtml(e), 'report-error');
    html += lines.join('') + '</div>';
  }

  el.reportBody.innerHTML = html;
  openModal(el.modalReport, document.getElementById('btn-close-report'));
}

async function doConvert() {
  if (state.isConverting) return;
  const targets = convertTargets();
  if (!targets.length) {
    notify('No PNG files selected');
    return;
  }

  const replace = state.config.convert_replace === true;
  if (replace) {
    // Native confirm before anything touches the originals.
    const confirmed = await window.__TAURI__.dialog.confirm(
      `Convert ${targets.length} PNG file(s) and move the originals to the Recycle Bin?`,
      { title: 'Convert & replace', kind: 'warning' }
    );
    if (!confirmed) return;
  }

  state.isConverting = true;
  showConvertProgress(0, targets.length, '');
  try {
    const report = await invoke('convert_pngs', { paths: targets, replace });
    state.isConverting = false;
    hideConvertProgress();

    // Files neither converted nor skipped nor errored mean the run was
    // cancelled before all files were processed.
    const processed = report.converted.length
      + (report.skipped_large ? report.skipped_large.length : 0)
      + (report.errors ? report.errors.length : 0);
    if (state.config.convert_show_report !== false) {
      // Detailed, user-dismissed report (default on).
      showReportModal(report, targets.length);
    } else {
      // Short toast only.
      const parts = [`Converted ${report.converted.length} PNG → JPG`];
      if (report.skipped_large && report.skipped_large.length) {
        parts.push(`${report.skipped_large.length} skipped (metadata too large)`);
      }
      if (report.errors && report.errors.length) {
        parts.push(`${report.errors.length} errors`);
      }
      const savedBytes = report.converted.reduce((s, c) => s + (c.in_size - c.out_size), 0);
      if (savedBytes > 0) parts.push(`saved ${formatSizeMB(savedBytes)}`);
      if (processed < targets.length) parts.push('canceled');
      notify(parts.join(' · '), !!(report.errors && report.errors.length));
    }
    await loadFiles(state.currentPath);
  } catch (err) {
    state.isConverting = false;
    hideConvertProgress();
    notify('Convert failed: ' + err);
    await loadFiles(state.currentPath);
  }
}

function refreshCurrentImage() {
  // Cache buster: the same URL can be served from the webview cache after a
  // rotate/crop/undo, making the edit invisible.
  const src = window.__TAURI__.core.convertFileSrc(state.currentPath) + '?t=' + Date.now();
  el.mainImage.src = src;
  // While crop is active Cropper renders its own copy of the image — feed it
  // the fresh frame too (rotate/undo during crop stay live).
  if (cropper) cropper.replace(src);
}

// Toggle native window fullscreen (dbl-click image/file name, or Enter)
async function toggleFullscreen() {
  try {
    const win = window.__TAURI__.window.getCurrentWindow();
    await win.setFullscreen(!(await win.isFullscreen()));
  } catch (err) {
    notify("Fullscreen failed: " + err);
  }
}

async function isFullscreenActive() {
  try {
    return await window.__TAURI__.window.getCurrentWindow().isFullscreen();
  } catch (err) {
    console.warn("Failed to read fullscreen state:", err);
    return false;
  }
}

// Crop Logic (Cropper.js)

// Wheel zoom; the footer readout shows the percentage while zoomed in
// (scale 1 = fit, indicator hidden) and doubles as a click-to-reset.
function applyZoom(deltaY) {
  if (deltaY < 0) currentZoom = Math.min(10, currentZoom + 0.1);
  else currentZoom = Math.max(0.1, currentZoom - 0.1);
  el.mainImage.style.transform = `scale(${currentZoom})`;
  updateZoomIndicator();
}

function resetZoom() {
  currentZoom = 1;
  el.mainImage.style.transform = 'scale(1)';
  updateZoomIndicator();
}

function updateZoomIndicator() {
  const zoomed = Math.abs(currentZoom - 1) > 0.001;
  el.infoZoom.classList.toggle('hidden', !zoomed);
  if (zoomed) el.infoZoom.textContent = `${Math.round(currentZoom * 100)}%`;
}

// Enters crop mode: Cropper takes over the displayed image (the original
// <img> is hidden and re-rendered inside a constrained canvas). The crop box
// can never leave the image (viewMode 1), wheel-zoom goes to the cursor, and
// there is no native text selection to leak into the side panels.
function toggleCrop() {
  if (state.isCropActive) {
    destroyCropper();
    return;
  }
  if (!state.currentPath) {
    notify('No image selected');
    return;
  }
  if (!el.mainImage.complete || !el.mainImage.naturalWidth) {
    notify('Image is still loading — try again in a moment');
    return;
  }
  state.isCropActive = true;
  el.btnCrop.classList.add('active');
  el.cropActions.classList.remove('hidden');
  // The metadata corner button would sit over Cropper's UI and open a modal
  // on top of an active crop — hide it for the duration.
  el.metaCorner.classList.add('hidden');
  cropper = new Cropper(el.mainImage, {
    viewMode: 1,        // crop box confined to the canvas (the image)
    background: false,  // keep the app's dark viewport, no checkerboard
    autoCropArea: 0.5,  // like the old centered box, relative to the image
    movable: true,      // drag the image to fine-position the frame
    zoomOnWheel: true,
    responsive: true,   // re-layout on window resize
  });
}

function destroyCropper() {
  if (cropper) {
    cropper.destroy();
    cropper = null;
  }
  state.isCropActive = false;
  el.btnCrop.classList.remove('active');
  el.cropActions.classList.add('hidden');
  updateMetaCorner();
}

async function executeCrop() {
  if (!cropper) return;
  // getData(true) returns rounded coordinates in natural image pixels —
  // no scale math, exact regardless of zoom or device pixel ratio.
  const d = cropper.getData(true);
  const finalX = Math.max(0, d.x);
  const finalY = Math.max(0, d.y);
  const finalW = Math.max(1, d.width);
  const finalH = Math.max(1, d.height);

  try {
    await invoke('crop_image', { path: state.currentPath, x: finalX, y: finalY, width: finalW, height: finalH });
  } catch (err) {
    notify("Crop failed: " + err);
    return; // keep the cropper active so the user can retry
  }
  destroyCropper();
  refreshCurrentImage();
  updateHistoryStats();
}

// Shortcuts & Settings
function renderShortcuts() {
  el.shortcutLegend.innerHTML = '';
  Object.entries(state.config.hotkeys).forEach(([key, path]) => {
    const li = document.createElement('li');
    li.className = 'shortcut-item';
    li.innerHTML = `<span class="shortcut-key">${key.toUpperCase()}</span> <span class="shortcut-path">${escapeHtml(basename(path))}</span>`;
    el.shortcutLegend.appendChild(li);
  });
}

function toggleSettings() {
  const closing = !el.modalSettings.classList.contains('hidden');
  if (closing) {
    closeModal(el.modalSettings);
    renderShortcuts();
    scheduleConfigSave(true);
  } else {
    openModal(el.modalSettings, document.getElementById('btn-close-settings'));
    renderSettingsList();
    document.getElementById('toggle-tray').checked = state.config.minimize_to_tray !== false;
    document.getElementById('toggle-convert-replace').checked = state.config.convert_replace === true;
    document.getElementById('toggle-convert-report').checked = state.config.convert_show_report !== false;
    document.getElementById('toggle-context-menu').checked = state.config.context_menu === true;
    el.convertMode.value = state.config.convert_replace ? 'replace' : 'next';
  }
}

// --- Metadata modal (Ctrl+Q) -------------------------------------------

// Guards against stale responses when navigating quickly while the modal is
// open: every load bumps the sequence, older in-flight requests are dropped.
let metadataRequestSeq = 0;

async function toggleMetadataModal() {
  const opening = el.modalMetadata.classList.contains('hidden');
  if (!opening) {
    closeModal(el.modalMetadata);
    // Invalidate any in-flight metadata fetch for the now-hidden modal.
    metadataRequestSeq++;
    return;
  }
  if (!state.currentPath) {
    notify('No image selected');
    return;
  }
  openModal(el.modalMetadata, document.getElementById('btn-close-metadata'));
  await loadMetadata(true);
}

// Loads and renders metadata for the currently selected image. With
// `showLoading` (first open) the body is cleared to "Loading…"; on live
// refreshes (arrow-key navigation) the previous content stays visible until
// the new data arrives, avoiding a flicker on every keypress.
async function loadMetadata(showLoading = false) {
  if (!state.currentPath) return;
  if (showLoading) {
    el.metadataBody.innerHTML = '<div class="metadata-loading">Loading…</div>';
  }
  const seq = ++metadataRequestSeq;
  try {
    const meta = await invoke('get_metadata', { path: state.currentPath });
    if (seq !== metadataRequestSeq) return; // stale — a newer navigation won
    renderMetadata(meta);
  } catch (err) {
    if (seq !== metadataRequestSeq) return;
    el.metadataBody.innerHTML = `<div class="metadata-error">Failed to read metadata: ${escapeHtml(String(err))}</div>`;
  }
}

function renderMetadata(meta) {
  const fileName = document.getElementById('metadata-file');
  if (fileName) fileName.textContent = meta.name;
  const sections = [];

  // Copy-button payloads (prompts, raw ComfyUI JSON) can be tens of KB —
  // embedding them in data-* attributes would rebuild that HTML on every
  // navigation while the modal is open. They are kept here and referenced
  // by index instead.
  const metaPayloads = [];
  const payloadRef = (text) => metaPayloads.push(text) - 1;

  // File summary
  const date = meta.modified ? formatDateTime(meta.modified) : '—';
  sections.push(`
    <div class="metadata-section">
      <h3>File</h3>
      <table class="meta-table">
        <tr><td>Name</td><td>${escapeHtml(meta.name)}</td></tr>
        <tr><td>Format</td><td>${escapeHtml(meta.format.toUpperCase())}</td></tr>
        <tr><td>Size</td><td>${formatBytes(meta.size)}</td></tr>
        <tr><td>Modified</td><td>${escapeHtml(date)}</td></tr>
      </table>
      <div class="meta-copy-btns">
        <button class="btn-secondary" data-export="json">Export JSON</button>
        <button class="btn-secondary" data-export="txt">Export TXT</button>
      </div>
    </div>`);

  // AI prompt (if present) with copy actions
  if (meta.ai) {
    const ai = meta.ai;
    const promptBlock = ai.prompt
      ? `<pre class="meta-ai-prompt">${escapeHtml(ai.prompt)}</pre>`
      : '';
    const negativeBlock = ai.negative_prompt
      ? `<h4>Negative</h4><pre class="meta-ai-prompt meta-ai-negative">${escapeHtml(ai.negative_prompt)}</pre>`
      : '';
    sections.push(`
      <div class="metadata-section metadata-ai">
        <div class="metadata-ai-head">
          <h3>AI Generation <span class="meta-ai-source">${escapeHtml(ai.source)}</span></h3>
          <div class="meta-copy-btns">
            ${ai.prompt ? `<button class="btn-secondary" data-copy-idx="${payloadRef(ai.prompt)}" title="Copy prompt">Copy prompt</button>` : ''}
            ${ai.negative_prompt ? `<button class="btn-secondary" data-copy-idx="${payloadRef(ai.negative_prompt)}" title="Copy negative prompt">Copy negative</button>` : ''}
            <button class="btn-secondary" data-copy-idx="${payloadRef(ai.raw)}" title="Copy raw payload">Copy raw</button>
          </div>
        </div>
        ${promptBlock}
        ${negativeBlock}
        ${ai.params ? `<h4>Parameters</h4><pre class="meta-ai-params">${escapeHtml(ai.params)}</pre>` : ''}
      </div>`);
  } else {
    sections.push(`<div class="metadata-section"><div class="metadata-noai">No AI generation prompt detected.</div></div>`);
  }

  // PNG text chunks (prompts not otherwise categorized)
  if (meta.text_chunks && meta.text_chunks.length) {
    const rows = meta.text_chunks
      .map(c => `<tr><td>${escapeHtml(c.key)}</td><td class="meta-cell-wrap">${escapeHtml(c.value)}</td></tr>`)
      .join('');
    sections.push(`<div class="metadata-section"><h3>PNG Text Chunks</h3><table class="meta-table">${rows}</table></div>`);
  }

  // JPEG comments
  if (meta.comments && meta.comments.length) {
    const rows = meta.comments.map(c => `<tr><td>Comment</td><td class="meta-cell-wrap">${escapeHtml(c)}</td></tr>`).join('');
    sections.push(`<div class="metadata-section"><h3>JPEG Comments</h3><table class="meta-table">${rows}</table></div>`);
  }

  // EXIF
  if (meta.exif && meta.exif.length) {
    const rows = meta.exif
      .map(c => `<tr><td>${escapeHtml(c.key)}</td><td class="meta-cell-wrap">${escapeHtml(c.value)}</td></tr>`)
      .join('');
    sections.push(`<div class="metadata-section"><h3>EXIF (${meta.exif.length})</h3><table class="meta-table">${rows}</table></div>`);
  } else {
    sections.push(`<div class="metadata-section"><div class="metadata-noai">No EXIF data.</div></div>`);
  }

  el.metadataBody.innerHTML = sections.join('');

  // Copy buttons
  el.metadataBody.querySelectorAll('[data-copy-idx]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      try {
        await invoke('copy_text', { text: metaPayloads[Number(btn.dataset.copyIdx)] });
        notify('Copied to clipboard', false);
      } catch (err) {
        notify('Copy failed: ' + err);
      }
    });
  });

  // Export buttons: "Export TXT" writes the readable dump; "Export JSON"
  // writes the ComfyUI workflow graph for AI images (droppable into
  // ComfyUI), falling back to the full metadata dump for plain photos.
  el.metadataBody.querySelectorAll('[data-export]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      try {
        const outPath = await invoke('export_metadata', {
          path: state.currentPath,
          format: btn.dataset.export
        });
        notify('Exported: ' + basename(outPath), false);
      } catch (err) {
        notify('Export failed: ' + err);
      }
    });
  });
}

function renderSettingsList() {
  el.hotkeyEditList.innerHTML = '';
  const keys = 'abcdefghijklmnopqrstuvwxyz'.split('');
  const assigned = keys.filter(k => state.config.hotkeys[k]);
  const unassigned = keys.filter(k => !state.config.hotkeys[k]);

  const counter = document.createElement('div');
  counter.className = 'hotkey-counter';
  counter.textContent = `${assigned.length} of ${keys.length} assigned`;
  el.hotkeyEditList.appendChild(counter);

  // Assigned keys first — the state the user actually maintains.
  assigned.forEach(key => el.hotkeyEditList.appendChild(hotkeyRow(key)));

  // Free keys as a compact grid instead of 21 "Not set" rows.
  if (unassigned.length) {
    const caption = document.createElement('div');
    caption.className = 'keycap-caption';
    caption.textContent = 'Available keys';
    el.hotkeyEditList.appendChild(caption);

    const grid = document.createElement('div');
    grid.className = 'keycap-grid';
    unassigned.forEach(key => {
      const keycap = document.createElement('button');
      keycap.className = 'keycap';
      keycap.textContent = key.toUpperCase();
      keycap.title = `Assign a folder to “${key.toUpperCase()}”`;
      keycap.addEventListener('click', async () => {
        const selected = await open({ directory: true });
        if (selected) {
          state.config.hotkeys[key] = selected;
          renderSettingsList();
        }
      });
      grid.appendChild(keycap);
    });
    el.hotkeyEditList.appendChild(grid);
  }
}

// One assigned-hotkey row: key, folder, change, clear.
function hotkeyRow(key) {
  const row = document.createElement('div');
  row.className = 'hotkey-row';

  const kbd = document.createElement('kbd');
  kbd.textContent = key.toUpperCase();

  const path = state.config.hotkeys[key];
  const span = document.createElement('span');
  span.textContent = path;
  span.title = path;

  const change = document.createElement('button');
  change.className = 'btn-secondary';
  change.textContent = 'Change';
  change.addEventListener('click', async () => {
    const selected = await open({ directory: true });
    if (selected) {
      state.config.hotkeys[key] = selected;
      renderSettingsList();
    }
  });

  const clear = document.createElement('button');
  clear.className = 'hotkey-clear';
  clear.textContent = '✕';
  clear.title = 'Clear this hotkey';
  clear.setAttribute('aria-label', `Clear hotkey ${key.toUpperCase()}`);
  clear.addEventListener('click', () => {
    delete state.config.hotkeys[key];
    scheduleConfigSave(true);
    renderSettingsList();
    renderShortcuts();
  });

  row.append(kbd, span, change, clear);
  return row;
}

init();
