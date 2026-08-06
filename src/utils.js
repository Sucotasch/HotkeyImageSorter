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
 * Compute the crop-region rect after a resize drag, clamped to the displayed
 * image rect. The edge opposite the dragged handle stays fixed, and the size
 * is derived from that fixed edge — so dragging a left/top handle against the
 * image edge cannot make the region grow past the image on the other side.
 *
 * @param {object} img   Image rect in the same coordinate space:
 *                       { left, top, right, bottom }
 * @param {object} start Start rect: { left, top, width, height }
 * @param {number} dx    Pointer delta since mousedown (horizontal)
 * @param {number} dy    Pointer delta since mousedown (vertical)
 * @param {string} handle One of 'nw', 'ne', 'sw', 'se', 'n', 's', 'e', 'w'
 * @param {number} minSize Minimum region size in px
 */
export function computeResizeRect(img, start, dx, dy, handle, minSize) {
  const minW = Math.min(minSize, img.right - img.left);
  const minH = Math.min(minSize, img.bottom - img.top);
  let left = start.left;
  let top = start.top;
  let width = start.width;
  let height = start.height;

  if (handle.includes('w')) {
    // Right edge stays fixed. The moving left edge is clamped to [img.left,
    // fixedRight - minW] so the region shrinks to minW in place instead of
    // sliding away, and can never grow past the image on the right.
    const fixedRight = start.left + start.width;
    left = Math.max(img.left, Math.min(start.left + dx, fixedRight - minW));
    width = fixedRight - left;
  } else if (handle.includes('e')) {
    // Left edge stays fixed. The moving right edge is clamped to
    // [start.left + minW, img.right].
    const right = Math.min(img.right, Math.max(start.left + start.width + dx, start.left + minW));
    width = right - start.left;
  }

  if (handle.includes('n')) {
    // Bottom edge stays fixed; symmetric to the west handle.
    const fixedBottom = start.top + start.height;
    top = Math.max(img.top, Math.min(start.top + dy, fixedBottom - minH));
    height = fixedBottom - top;
  } else if (handle.includes('s')) {
    // Top edge stays fixed; symmetric to the east handle.
    const bottom = Math.min(img.bottom, Math.max(start.top + start.height + dy, start.top + minH));
    height = bottom - start.top;
  }

  return { left, top, width, height };
}
