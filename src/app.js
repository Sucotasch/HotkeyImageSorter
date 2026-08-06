import { basename, escapeHtml, computeResizeRect, computeVisibleRange } from './utils.js';

const { invoke } = window.__TAURI__.core;
const { open } = window.__TAURI__.dialog;

// State management
let state = {
  images: [],
  currentIndex: -1,
  currentPath: '',
  folder: '',
  config: { hotkeys: {} },
  isCropActive: false,
  sort: { by: 'name', dir: 1 },
  selectedPaths: new Set()
};

let currentZoom = 1;

// Crop region tracked in image-fraction coordinates (0..1) so it survives
// zoom, navigation and window resizes without leaving the image.
let cropFrac = { x: 0.25, y: 0.25, w: 0.5, h: 0.5 };

// UI Elements
const el = {
  imageList: document.getElementById('image-list-container'),
  mainImage: document.getElementById('main-image'),
  filePath: document.getElementById('current-file-path'),
  fileMeta: document.getElementById('file-meta'),
  infoDimensions: document.getElementById('info-dimensions'),
  infoDate: document.getElementById('info-date'),
  infoSize: document.getElementById('info-size'),
  fileCount: document.getElementById('file-count'),
  btnSelectFolder: document.getElementById('btn-select-folder'),
  btnRotate: document.getElementById('btn-rotate'),
  btnCrop: document.getElementById('btn-crop'),
  shortcutLegend: document.getElementById('shortcut-legend'),
  cropOverlay: document.getElementById('crop-overlay'),
  cropRegion: document.getElementById('crop-region'),
  modalSettings: document.getElementById('modal-settings'),
  hotkeyEditList: document.getElementById('hotkey-edit-list'),
  cropLabel: document.getElementById('crop-label'),
  modalMetadata: document.getElementById('modal-metadata'),
  metadataBody: document.getElementById('metadata-body')
};

// True when any modal is open (settings or metadata). Used to suppress global
// hotkeys (Delete, Enter, A–Z, Esc) that must not fire behind a modal.
function anyModalOpen() {
  return !el.modalSettings.classList.contains('hidden') ||
         !el.modalMetadata.classList.contains('hidden');
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
  state.config = await invoke('get_config');
  if (state.config.sort_by) state.sort.by = state.config.sort_by;
  if (state.config.sort_dir) state.sort.dir = state.config.sort_dir;
  
  document.getElementById('sort-by').value = state.sort.by;
  updateSortButtonUI();

  renderShortcuts();
  setupEventListeners();
  if (state.config.last_folder) {
    state.folder = state.config.last_folder;
    await loadFiles(state.config.last_file);
  }
  
  if (state.config.minimize_to_tray !== false) {
    document.getElementById('toggle-tray').checked = true;
  }
  
  updateHistoryStats();
}

function setupEventListeners() {
  el.btnSelectFolder.addEventListener('click', selectFolder);
  document.getElementById('btn-config-hotkeys').addEventListener('click', toggleSettings);
  document.getElementById('btn-close-settings').addEventListener('click', toggleSettings);
  document.getElementById('btn-close-metadata').addEventListener('click', () => toggleMetadataModal());
  el.btnRotate.addEventListener('click', () => doRotate());
  el.btnCrop.addEventListener('click', () => toggleCrop());
  document.getElementById('btn-undo').addEventListener('click', () => doUndo());
  document.getElementById('btn-redo').addEventListener('click', () => doRedo());
  document.getElementById('btn-refresh').addEventListener('click', () => loadFiles(state.currentPath));
  
  document.getElementById('toggle-tray').addEventListener('change', (e) => {
    state.config.minimize_to_tray = e.target.checked;
    invoke('save_config', { config: state.config });
  });

  el.mainImage.addEventListener('wheel', (e) => {
    e.preventDefault();
    applyZoom(e.deltaY);
  }, { passive: false });
  // While crop is active the dark overlay covers the image, so it must forward
  // wheel events to zoom as well (the region stays glued via cropFrac).
  el.cropOverlay.addEventListener('wheel', (e) => {
    e.preventDefault();
    applyZoom(e.deltaY);
  }, { passive: false });

  // Reposition the crop region whenever the image (re)loads — covers
  // navigation, rotate, crop and undo/redo refreshes uniformly.
  el.mainImage.addEventListener('load', () => {
    if (state.isCropActive) positionCropRegion();
  });

  // Keep the crop region glued to the image when the window is resized
  window.addEventListener('resize', () => {
    if (state.isCropActive) positionCropRegion();
    scheduleRenderWindow();
  });
  
  // Virtual list: re-render the window on scroll
  el.imageList.addEventListener('scroll', () => scheduleRenderWindow(), { passive: true });

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
    invoke('save_config', { config: state.config });
    sortAndRender();
  });
  document.getElementById('btn-sort-dir').addEventListener('click', () => {
    state.sort.dir *= -1;
    state.config.sort_dir = state.sort.dir;
    invoke('save_config', { config: state.config });
    updateSortButtonUI();
    sortAndRender();
  });

  // Fullscreen: double-click the image or the file name
  el.mainImage.addEventListener('dblclick', toggleFullscreen);
  el.filePath.addEventListener('dblclick', toggleFullscreen);

  // Crop Interaction
  setupCropDrag();
  el.cropRegion.addEventListener('dblclick', (e) => {
    if (e.target.classList.contains('crop-handle')) return;
    executeCrop();
  });
}

function updateSortButtonUI() {
  const btn = document.getElementById('btn-sort-dir');
  if (state.sort.dir === 1) {
    btn.innerHTML = `<span style="color:var(--primary-color)">↑</span> <span style="color:var(--text-dim)">↓</span>`;
  } else {
    btn.innerHTML = `<span style="color:var(--text-dim)">↑</span> <span style="color:var(--primary-color)">↓</span>`;
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
    invoke('save_config', { config: state.config });
    // A brand-new folder starts at the top of the list
    el.imageList.scrollTop = 0;
    loadFiles();
  }
}

async function loadFiles(initialLoadPath = null) {
  try {
    state.images = await invoke('list_files', { dir: state.folder });
    // The folder changed (or files were moved/edited): prompt badges from the
    // previous listing are stale, so rebuild the cache from scratch.
    promptPaths.clear();
    promptChecked.clear();
    lastListSignature = '';
    sortAndRender(initialLoadPath);
  } catch (err) {
    state.images = [];
    promptPaths.clear();
    promptChecked.clear();
    renderList();
    el.fileCount.textContent = '0 images';
    notify("Cannot load folder: " + err);
  }
}

let lastListSignature = '';

function sortAndRender(initialLoadPath = null) {
  state.images.sort((a, b) => {
    let valA = a[state.sort.by];
    let valB = b[state.sort.by];
    return valA < valB ? -state.sort.dir : state.sort.dir;
  });
  // Avoid rebuilding the DOM when the list content/order is unchanged
  const signature = state.images.map(i => i.path).join('\n');
  if (signature !== lastListSignature) {
    renderList();
    lastListSignature = signature;
  }
  el.fileCount.textContent = `${state.images.length} images`;
  
  if (initialLoadPath) {
      const idx = state.images.findIndex(i => i.path === initialLoadPath);
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
        // Only re-render when something changed (and never while a rename
        // input is live — the rAF path checks this, the async .then must too).
        if (promptPaths.size !== before && renamingIndex === -1) {
          renderWindow();
        }
      })
      .catch(() => {});
  });
}

// Render the badge glyph for a row, if its path is known to have a prompt.
function hasPromptBadge(path) {
  return promptPaths.has(path);
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

function renderWindow() {
  const container = el.imageList;
  const { start, end } = computeListRange();
  if (start === listStart && end === listEnd) return;
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
      badge.title = 'Contains AI generation metadata — press Ctrl+I';
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
  state.currentIndex = index;
  const img = state.images[index];
  state.currentPath = img.path;
  
  state.config.last_file = img.path;
  invoke('save_config', { config: state.config });
  
  // Use Tauri's convertFileSrc for local images. The cache-buster forces the
  // webview to re-fetch from disk: without it, the same URL (this file was
  // already displayed before the last rotate/crop/undo/redo) can be served
  // from the webview cache and undo/redo appear to do nothing.
  const src = window.__TAURI__.core.convertFileSrc(img.path) + '?t=' + Date.now();
  el.mainImage.src = src;
  el.filePath.textContent = img.path;
  
  currentZoom = 1;
  el.mainImage.style.transform = 'scale(1)';
  
  // Update meta info
  const date = new Date(img.modified * 1000);
  el.infoDate.textContent = date.toLocaleDateString('ru-RU', { day:'2-digit', month:'2-digit', year:'numeric' }) + ' ' + date.toLocaleTimeString('ru-RU', { hour:'2-digit', minute:'2-digit' });
  const kb = img.size / 1024;
  el.infoSize.textContent = kb >= 1024 ? (kb / 1024).toFixed(1) + ' MB' : Math.round(kb) + ' KB';
  el.infoDimensions.textContent = '…px';
  el.fileMeta.classList.remove('hidden');
  // get natural dimensions after load
  el.mainImage.onload = () => {
    el.infoDimensions.textContent = `${el.mainImage.naturalWidth} × ${el.mainImage.naturalHeight} px`;
  };
  
  // Update active and selected statuses in the rendered window
  applyRowClasses();

  // Scroll into view if needed
  scrollIndexIntoView(index);
}

// Global Keys
async function handleGlobalKeydown(e) {
  const active = document.activeElement;
  if (active && (active.tagName === 'INPUT' || active.tagName === 'SELECT' || active.tagName === 'TEXTAREA' || active.isContentEditable)) return;

  const key = e.key.toLowerCase();
  const code = e.code;
  
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
  } else if ((code === 'Delete' || key === 'delete') && !e.ctrlKey && !e.metaKey && !e.altKey && !anyModalOpen()) {
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
  } else if (e.ctrlKey && (code === 'KeyI' || key === 'i')) {
    e.preventDefault();
    await toggleMetadataModal();
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
  } else if ((key === 'enter' || code === 'Enter') && !(active && active.tagName === 'BUTTON') && !anyModalOpen()) {
    e.preventDefault();
    await toggleFullscreen();
  } else if (code && code.startsWith('Key') && !e.ctrlKey && !e.metaKey && !e.altKey && !anyModalOpen()) {
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

function refreshCurrentImage() {
  // Add cache buster to URL
  const src = window.__TAURI__.core.convertFileSrc(state.currentPath);
  el.mainImage.src = src + '?t=' + Date.now();
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

// Crop Logic

// Displayed image rectangle in viewport coordinates (includes zoom transform)
function imageRect() {
  return el.mainImage.getBoundingClientRect();
}

// Wheel zoom; also keeps the crop region glued to the image when active.
function applyZoom(deltaY) {
  if (deltaY < 0) currentZoom = Math.min(10, currentZoom + 0.1);
  else currentZoom = Math.max(0.1, currentZoom - 0.1);
  el.mainImage.style.transform = `scale(${currentZoom})`;
  if (state.isCropActive) positionCropRegion();
}

// Position the region from cropFrac, keeping it inside the current image rect.
// The overlay does not start at window (0,0) (the sidebar shifts it), so the
// window-space rect is converted into overlay coordinates before styling.
function positionCropRegion() {
  const img = imageRect();
  if (!(img.width > 0 && img.height > 0)) return;
  const ov = el.cropOverlay.getBoundingClientRect();
  el.cropRegion.style.left = (img.left - ov.left + cropFrac.x * img.width) + 'px';
  el.cropRegion.style.top = (img.top - ov.top + cropFrac.y * img.height) + 'px';
  el.cropRegion.style.width = (cropFrac.w * img.width) + 'px';
  el.cropRegion.style.height = (cropFrac.h * img.height) + 'px';
  updateCropLabel();
}

// Record the region's position relative to the displayed image (0..1)
function captureCropFrac() {
  const img = imageRect();
  if (!(img.width > 0 && img.height > 0)) return;
  const r = el.cropRegion.getBoundingClientRect();
  cropFrac.x = (r.left - img.left) / img.width;
  cropFrac.y = (r.top - img.top) / img.height;
  cropFrac.w = r.width / img.width;
  cropFrac.h = r.height / img.height;
}

// Clamp a viewport-space rect (fixed size, e.g. when moving the region) so it
// stays fully inside the image.
function applyCropClamp(left, top, width, height) {
  const img = imageRect();
  if (!(img.width > 0 && img.height > 0)) return { left, top, width, height };
  width = Math.min(width, img.width);
  height = Math.min(height, img.height);
  left = Math.max(img.left, Math.min(left, img.right - width));
  top = Math.max(img.top, Math.min(top, img.bottom - height));
  return { left, top, width, height };
}

// Live readout of the crop size in image pixels
function updateCropLabel() {
  if (!el.cropLabel) return;
  const img = imageRect();
  if (!(img.width > 0 && img.height > 0) || !el.mainImage.naturalWidth) return;
  const scaleX = img.width / el.mainImage.naturalWidth;
  const r = el.cropRegion.getBoundingClientRect();
  const w = Math.round(r.width / scaleX);
  const h = Math.round(r.height / scaleX);
  el.cropLabel.textContent = `Double-click to crop — ${w} × ${h} px`;
}

function toggleCrop() {
  state.isCropActive = !state.isCropActive;
  el.cropOverlay.classList.toggle('hidden', !state.isCropActive);
  el.btnCrop.classList.toggle('active', state.isCropActive);
  if (state.isCropActive) {
    // Center a 300px box on the actual image (not the viewport)
    const img = imageRect();
    if (!(img.width > 0 && img.height > 0)) return; // image not loaded yet
    const size = 300;
    const w = Math.min(size, img.width);
    const h = Math.min(size, img.height);
    cropFrac = {
      x: (img.width - w) / 2 / img.width,
      y: (img.height - h) / 2 / img.height,
      w: w / img.width,
      h: h / img.height,
    };
    positionCropRegion();
  }
}

function setupCropDrag() {
  let isDragging = false;
  let startX, startY;
  const MIN_SIZE = 20;

  el.cropRegion.addEventListener('mousedown', (e) => {
    if (e.target.classList.contains('crop-handle')) return;
    if (e.button !== 0) return;
    const rect = el.cropRegion.getBoundingClientRect();
    startX = e.clientX - rect.left;
    startY = e.clientY - rect.top;
    isDragging = true;
  });

  window.addEventListener('mousemove', (e) => {
    if (!isDragging) return;
    const ov = el.cropOverlay.getBoundingClientRect();
    const clamped = applyCropClamp(
      e.clientX - startX,
      e.clientY - startY,
      el.cropRegion.offsetWidth,
      el.cropRegion.offsetHeight
    );
    el.cropRegion.style.left = (clamped.left - ov.left) + 'px';
    el.cropRegion.style.top = (clamped.top - ov.top) + 'px';
    captureCropFrac();
    updateCropLabel();
  });

  window.addEventListener('mouseup', () => isDragging = false);

  // Handle resizing: corners (nw/ne/sw/se) and edges (n/s/e/w)
  const handles = el.cropRegion.querySelectorAll('.crop-handle');
  handles.forEach(h => {
    h.addEventListener('mousedown', (e) => {
      e.stopPropagation();
      if (e.button !== 0) return;
      const handleClass = e.target.classList[1]; // nw, ne, sw, se, n, s, e, w
      const startX = e.clientX;
      const startY = e.clientY;
      const startW = el.cropRegion.offsetWidth;
      const startH = el.cropRegion.offsetHeight;
      const startRect = el.cropRegion.getBoundingClientRect();
      const startL = startRect.left; // window coords, matches applyCropClamp
      const startT = startRect.top;

      const onMouseMove = (me) => {
        const dx = me.clientX - startX;
        const dy = me.clientY - startY;
        const img = imageRect();
        if (!(img.width > 0 && img.height > 0)) return;
        const ov = el.cropOverlay.getBoundingClientRect();
        const clamped = computeResizeRect(
          img,
          { left: startL, top: startT, width: startW, height: startH },
          dx,
          dy,
          handleClass,
          MIN_SIZE
        );
        el.cropRegion.style.width = clamped.width + 'px';
        el.cropRegion.style.height = clamped.height + 'px';
        el.cropRegion.style.left = (clamped.left - ov.left) + 'px';
        el.cropRegion.style.top = (clamped.top - ov.top) + 'px';
        captureCropFrac();
        updateCropLabel();
      };

      const onMouseUp = () => {
        window.removeEventListener('mousemove', onMouseMove);
        window.removeEventListener('mouseup', onMouseUp);
      };

      window.addEventListener('mousemove', onMouseMove);
      window.addEventListener('mouseup', onMouseUp);
    });
  });
}

async function executeCrop() {
  const imgRect = el.mainImage.getBoundingClientRect();
  const regionRect = el.cropRegion.getBoundingClientRect();

  // getBoundingClientRect already includes the zoom transform, so this scale
  // (image pixels per displayed pixel) is correct at any currentZoom.
  const scale = imgRect.width / el.mainImage.naturalWidth;
  if (!(scale > 0)) return;

  const x = Math.round((regionRect.left - imgRect.left) / scale);
  const y = Math.round((regionRect.top - imgRect.top) / scale);
  const w = Math.round(regionRect.width / scale);
  const h = Math.round(regionRect.height / scale);
  
  const finalX = Math.max(0, Math.min(x, el.mainImage.naturalWidth - 1));
  const finalY = Math.max(0, Math.min(y, el.mainImage.naturalHeight - 1));
  const finalW = Math.max(1, Math.min(w, el.mainImage.naturalWidth - finalX));
  const finalH = Math.max(1, Math.min(h, el.mainImage.naturalHeight - finalY));
  
  await invoke('crop_image', { path: state.currentPath, x: finalX, y: finalY, width: finalW, height: finalH });
  toggleCrop();
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
  el.modalSettings.classList.toggle('hidden');
  if (!el.modalSettings.classList.contains('hidden')) renderSettingsList();
  else {
    renderShortcuts();
    invoke('save_config', { config: state.config });
  }
}

// --- Metadata modal (Ctrl+I) -------------------------------------------

async function toggleMetadataModal() {
  const opening = el.modalMetadata.classList.contains('hidden');
  if (!opening) {
    el.modalMetadata.classList.add('hidden');
    return;
  }
  if (!state.currentPath) {
    notify('No image selected');
    return;
  }
  el.modalMetadata.classList.remove('hidden');
  el.metadataBody.innerHTML = '<div class="metadata-loading">Loading…</div>';
  try {
    const meta = await invoke('get_metadata', { path: state.currentPath });
    renderMetadata(meta);
  } catch (err) {
    el.metadataBody.innerHTML = `<div class="metadata-error">Failed to read metadata: ${escapeHtml(String(err))}</div>`;
  }
}

function renderMetadata(meta) {
  const sections = [];

  // File summary
  const date = meta.modified ? new Date(meta.modified * 1000).toLocaleString() : '—';
  sections.push(`
    <div class="metadata-section">
      <h3>File</h3>
      <table class="meta-table">
        <tr><td>Name</td><td>${escapeHtml(meta.name)}</td></tr>
        <tr><td>Format</td><td>${escapeHtml(meta.format.toUpperCase())}</td></tr>
        <tr><td>Size</td><td>${formatBytes(meta.size)}</td></tr>
        <tr><td>Modified</td><td>${escapeHtml(date)}</td></tr>
      </table>
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
            ${ai.prompt ? `<button class="btn-secondary" data-copy="${escapeAttr(ai.prompt)}" title="Copy prompt">Copy prompt</button>` : ''}
            ${ai.negative_prompt ? `<button class="btn-secondary" data-copy="${escapeAttr(ai.negative_prompt)}" title="Copy negative prompt">Copy negative</button>` : ''}
            <button class="btn-secondary" data-copy="${escapeAttr(ai.raw)}" title="Copy raw payload">Copy raw</button>
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
  el.metadataBody.querySelectorAll('[data-copy]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      try {
        await invoke('copy_text', { text: btn.dataset.copy });
        notify('Copied to clipboard', false);
      } catch (err) {
        notify('Copy failed: ' + err);
      }
    });
  });
}

function formatBytes(bytes) {
  if (bytes < 1024) return bytes + ' B';
  const kb = bytes / 1024;
  if (kb < 1024) return kb.toFixed(1) + ' KB';
  return (kb / 1024).toFixed(1) + ' MB';
}

function escapeAttr(s) {
  return String(s).replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function renderSettingsList() {
  el.hotkeyEditList.innerHTML = '';
  "abcdefghijklmnopqrstuvwxyz".split('').forEach(key => {
    const row = document.createElement('div');
    row.style.display = 'contents';
    const currentPath = state.config.hotkeys[key] || 'Not set';
    
    const kbd = document.createElement('kbd');
    kbd.textContent = key.toUpperCase();
    
    const span = document.createElement('span');
    span.textContent = currentPath;
    
    const btn = document.createElement('button');
    btn.textContent = 'Set';
    btn.addEventListener('click', async () => {
      const selected = await open({ directory: true });
      if (selected) {
        state.config.hotkeys[key] = selected;
        renderSettingsList();
      }
    });

    row.appendChild(kbd);
    row.appendChild(span);
    row.appendChild(btn);
    el.hotkeyEditList.appendChild(row);
  });
}

init();
