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
  cropRegion: { x: 50, y: 50, w: 200, h: 200 },
  sort: { by: 'name', dir: 1 },
  selectedPaths: new Set()
};

let currentZoom = 1;

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
  hotkeyEditList: document.getElementById('hotkey-edit-list')
};

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
    if (e.deltaY < 0) currentZoom = Math.min(10, currentZoom + 0.1);
    else currentZoom = Math.max(0.1, currentZoom - 0.1);
    el.mainImage.style.transform = `scale(${currentZoom})`;
  }, { passive: false });
  
  // Navigation
  window.addEventListener('keydown', handleGlobalKeydown);
  
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

  // Crop Interaction
  setupCropDrag();
  el.cropRegion.addEventListener('dblclick', executeCrop);
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
    loadFiles();
  }
}

async function loadFiles(initialLoadPath = null) {
  state.images = await invoke('list_files', { dir: state.folder });
  sortAndRender(initialLoadPath);
}

function sortAndRender(initialLoadPath = null) {
  state.images.sort((a, b) => {
    let valA = a[state.sort.by];
    let valB = b[state.sort.by];
    return valA < valB ? -state.sort.dir : state.sort.dir;
  });
  renderList();
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
         document.querySelectorAll('.list-item').forEach((e, i) => e.classList.toggle('active', i === newIndex));
         const activeItem = el.imageList.children[newIndex];
         if (activeItem) activeItem.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
     } else {
         selectImage(0);
     }
  } else if (state.images.length > 0) {
      selectImage(0);
  }
}

// Virtual List Rendering (Simple Version)
function renderList() {
  el.imageList.innerHTML = '';
  state.images.forEach((img, index) => {
    const div = document.createElement('div');
    const isActive = index === state.currentIndex ? 'active' : '';
    const isSelected = state.selectedPaths.has(img.path) ? 'selected' : '';
    
    div.className = `list-item ${isActive} ${isSelected}`;
    div.textContent = img.name;
    div.addEventListener('click', (e) => handleItemClick(e, index));
    el.imageList.appendChild(div);
  });
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
  
  // Use Tauri's convertFileSrc for local images
  const src = window.__TAURI__.core.convertFileSrc(img.path);
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
  
  // Update active and selected statuses in list
  document.querySelectorAll('.list-item').forEach((elem, i) => {
    elem.classList.toggle('active', i === index);
    elem.classList.toggle('selected', state.selectedPaths.has(state.images[i].path));
  });
  
  // Scroll into view if needed
  const activeDot = el.imageList.children[index];
  if (activeDot) activeDot.scrollIntoView({ block: 'nearest' });
}

// Global Keys
async function handleGlobalKeydown(e) {
  if (document.activeElement.tagName === 'INPUT') return;

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
    } else if (!el.modalSettings.classList.contains('hidden')) {
      toggleSettings();
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
  } else if (code === 'Delete' || key === 'delete') {
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
      } catch (err) { console.error("Failed to delete", err); }
    }
  } else if (e.ctrlKey && (code === 'KeyR' || key === 'r')) {
    await doRotate();
  } else if (e.ctrlKey && (code === 'KeyC' || key === 'c')) {
    toggleCrop();
  } else if (e.ctrlKey && (code === 'KeyZ' || key === 'z')) {
    e.preventDefault();
    await doUndo();
  } else if (e.ctrlKey && (code === 'KeyX' || key === 'x')) {
    e.preventDefault();
    await doRedo();
  } else if (code && code.startsWith('Key')) {
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
        } catch (err) { console.error("Move failed:", err); }
    }
  }
}

async function doRotate() {
  if (!state.currentPath) return;
  await invoke('rotate_image', { path: state.currentPath, degrees: 90 });
  refreshCurrentImage();
  updateHistoryStats();
}

async function doUndo() {
  try {
    const undonePath = await invoke('undo_last_move');
    if (undonePath) {
      await loadFiles();
      refreshCurrentImage();
      updateHistoryStats();
    }
  } catch(err) { console.warn("Undo failed:", err); }
}

async function doRedo() {
  try {
    const redonePath = await invoke('redo_last_move');
    if (redonePath) {
      await loadFiles();
      refreshCurrentImage();
      updateHistoryStats();
    }
  } catch(err) { console.warn("Redo failed:", err); }
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

// Crop Logic
function toggleCrop() {
  state.isCropActive = !state.isCropActive;
  el.cropOverlay.classList.toggle('hidden', !state.isCropActive);
  el.btnCrop.classList.toggle('active', state.isCropActive);
  if (state.isCropActive) {
    const imgRect = el.cropOverlay.getBoundingClientRect();
    const size = 300;
    el.cropRegion.style.width = size + 'px';
    el.cropRegion.style.height = size + 'px';
    el.cropRegion.style.left = (imgRect.width / 2 - size / 2) + 'px';
    el.cropRegion.style.top = (imgRect.height / 2 - size / 2) + 'px';
  }
}

function setupCropDrag() {
  let isDragging = false;
  let startX, startY;

  el.cropRegion.addEventListener('mousedown', (e) => {
    if (e.target.classList.contains('crop-handle')) return;
    isDragging = true;
    startX = e.clientX - el.cropRegion.offsetLeft;
    startY = e.clientY - el.cropRegion.offsetTop;
  });

  window.addEventListener('mousemove', (e) => {
    if (!isDragging) return;
    el.cropRegion.style.left = (e.clientX - startX) + 'px';
    el.cropRegion.style.top = (e.clientY - startY) + 'px';
  });

  window.addEventListener('mouseup', () => isDragging = false);
  
  // Handle resizing
  const handles = el.cropRegion.querySelectorAll('.crop-handle');
  handles.forEach(h => {
    h.addEventListener('mousedown', (e) => {
      e.stopPropagation();
      const handleClass = e.target.classList[1]; // nw, ne, sw, se
      const startX = e.clientX;
      const startY = e.clientY;
      const startW = el.cropRegion.offsetWidth;
      const startH = el.cropRegion.offsetHeight;
      const startL = el.cropRegion.offsetLeft;
      const startT = el.cropRegion.offsetTop;

      const onMouseMove = (me) => {
        const dx = me.clientX - startX;
        const dy = me.clientY - startY;

        if (handleClass.includes('e')) el.cropRegion.style.width = (startW + dx) + 'px';
        if (handleClass.includes('s')) el.cropRegion.style.height = (startH + dy) + 'px';
        if (handleClass.includes('w')) {
          el.cropRegion.style.width = (startW - dx) + 'px';
          el.cropRegion.style.left = (startL + dx) + 'px';
        }
        if (handleClass.includes('n')) {
          el.cropRegion.style.height = (startH - dy) + 'px';
          el.cropRegion.style.top = (startT + dy) + 'px';
        }
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
  
  const scale = imgRect.width / el.mainImage.naturalWidth;
  
  let relativeX = regionRect.left - imgRect.left;
  let relativeY = regionRect.top - imgRect.top;
  
  const x = Math.round(relativeX / scale);
  const y = Math.round(relativeY / scale);
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
    li.innerHTML = `<span class="shortcut-key">${key.toUpperCase()}</span> <span class="shortcut-path">${basename(path)}</span>`;
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

function basename(path) {
  return path.split(/[\\/]/).pop();
}

init();
