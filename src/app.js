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
  sort: { by: 'name', dir: 1 } // 1: asc, -1: desc
};

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
}

function setupEventListeners() {
  el.btnSelectFolder.addEventListener('click', selectFolder);
  document.getElementById('btn-config-hotkeys').addEventListener('click', toggleSettings);
  document.getElementById('btn-close-settings').addEventListener('click', toggleSettings);
  el.btnRotate.addEventListener('click', () => doRotate());
  el.btnCrop.addEventListener('click', () => toggleCrop());
  
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
    div.className = `list-item ${index === state.currentIndex ? 'active' : ''}`;
    div.textContent = img.name;
    div.addEventListener('click', () => selectImage(index));
    el.imageList.appendChild(div);
  });
}

async function selectImage(index) {
  if (index < 0 || index >= state.images.length) return;
  state.currentIndex = index;
  const img = state.images[index];
  state.currentPath = img.path;
  
  state.config.last_file = img.path;
  invoke('save_config', { config: state.config });
  
  // Use Tauri's convertFileSrc for local images (Tauri 2.0 uses a different scheme)
  // Actually, in Tauri 2.0, use window.__TAURI__.core.convertFileSrc
  const src = window.__TAURI__.core.convertFileSrc(img.path);
  el.mainImage.src = src;
  el.filePath.textContent = img.path;
  
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
  
  // Update active status in list
  document.querySelectorAll('.list-item').forEach((el, i) => {
    el.classList.toggle('active', i === index);
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
    selectImage(state.currentIndex + 1);
  } else if (code === 'ArrowUp' || key === 'arrowup') {
    e.preventDefault();
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
  } else if (code === 'Delete' || key === 'delete') {
    if (state.currentPath) {
      try {
        await invoke('trash_image', { path: state.currentPath });
        state.images.splice(state.currentIndex, 1);
        renderList();
        el.fileCount.textContent = `${state.images.length} images`;
        if (state.images.length > 0) {
          selectImage(Math.min(state.currentIndex, state.images.length - 1));
        } else {
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
    try {
      const undonePath = await invoke('undo_last_move');
      if (undonePath) {
        await loadFiles();
        refreshCurrentImage();
      }
    } catch(err) { console.warn("Undo failed:", err); }
  } else if (code && code.startsWith('Key')) {
    const char = code.replace('Key', '').toLowerCase();
    const targetDir = state.config.hotkeys[char];
    if (targetDir) {
      const newPath = await invoke('move_image', { source: state.currentPath, targetDir });
      state.images.splice(state.currentIndex, 1);
      renderList();
      el.fileCount.textContent = `${state.images.length} images`;
      if (state.images.length > 0) {
        selectImage(Math.min(state.currentIndex, state.images.length - 1));
      } else {
        el.mainImage.src = '';
        state.currentPath = '';
      }
    }
  }
}

async function doRotate() {
  if (!state.currentPath) return;
  await invoke('rotate_image', { path: state.currentPath, degrees: 90 });
  refreshCurrentImage();
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
  
  const fitScale = Math.min(imgRect.width / el.mainImage.naturalWidth, imgRect.height / el.mainImage.naturalHeight);
  const scale = Math.min(1, fitScale);
  
  const renderW = el.mainImage.naturalWidth * scale;
  const renderH = el.mainImage.naturalHeight * scale;
  const offsetX = (imgRect.width - renderW) / 2;
  const offsetY = (imgRect.height - renderH) / 2;
  
  let relativeX = (regionRect.left - imgRect.left) - offsetX;
  let relativeY = (regionRect.top - imgRect.top) - offsetY;
  
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
