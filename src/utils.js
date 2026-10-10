// Pure helper functions shared by the frontend, kept free of Tauri/window
// dependencies so they can be unit-tested with Node's built-in test runner.

export function basename(path) {
  return path.split(/[\\/]/).pop();
}

/**
 * Compute the visible range of rows for a virtualized list.
 *
 * @param {number} scrollTop   Scroll offset of the container (px)
 * @param {number} viewportH   Visible container height (px)
 * @param {number} rowH        Fixed row height (px)
 * @param {number} total       Total number of rows
 * @param {number} overscan    Extra rows to render above/below the viewport
 * @returns {{start: number, end: number}} Half-open range [start, end)
 */
export function computeVisibleRange(scrollTop, viewportH, rowH, total, overscan) {
  if (total <= 0) return { start: 0, end: 0 };
  const first = Math.max(0, Math.floor(scrollTop / rowH) - overscan);
  const last = Math.min(total, Math.ceil((scrollTop + viewportH) / rowH) + overscan);
  return { start: first, end: Math.max(first, last) };
}

/**
 * Compare two list entries for sorting.
 *
 * Names use a case-insensitive, plain character-order comparison ("apple"
 * before "Zebra"; "03e66bd5…" before "1d50e8c0…" because '0' < '1'). A
 * numeric-aware natural sort would read the leading "03" as the number 3 and
 * push the file into the '3' group — confusing for UUID-style filenames.
 * Equal values return 0 (required for a stable sort).
 *
 * @param {object} a  ImageInfo entry
 * @param {object} b  ImageInfo entry
 * @param {string} by Sort key: 'name' | 'modified' | 'size'
 * @param {number} dir 1 (ascending) or -1 (descending)
 */
export function compareImages(a, b, by, dir) {
  const valA = a[by];
  const valB = b[by];
  let cmp;
  if (by === 'name') {
    cmp = compareNames(String(valA), String(valB));
  } else if (valA < valB) {
    cmp = -1;
  } else if (valA > valB) {
    cmp = 1;
  } else {
    cmp = 0;
  }
  return cmp === 0 ? 0 : cmp * dir;
}

/**
 * Case-insensitive plain character-order comparison (no numeric
 * interpretation of digit runs).
 */
function compareNames(a, b) {
  const na = a.toLowerCase();
  const nb = b.toLowerCase();
  return na < nb ? -1 : na > nb ? 1 : 0;
}

export function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (c) => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  })[c]);
}

/**
 * Human-readable byte size: B → KB → MB → GB, one decimal past bytes.
 *
 * @param {number} bytes
 * @returns {string}
 */
export function formatBytes(bytes) {
  if (bytes < 1024) return bytes + ' B';
  const kb = bytes / 1024;
  if (kb < 1024) return kb.toFixed(1) + ' KB';
  const mb = kb / 1024;
  if (mb < 1024) return mb.toFixed(1) + ' MB';
  return (mb / 1024).toFixed(1) + ' GB';
}
