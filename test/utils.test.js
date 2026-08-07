import { test } from 'node:test';
import assert from 'node:assert/strict';
import { basename, escapeHtml, computeResizeRect, computeVisibleRange, compareImages } from '../src/utils.js';

test('basename extracts the file name from a forward-slash path', () => {
  assert.equal(basename('C:/Users/me/pics/photo.jpg'), 'photo.jpg');
});

test('basename extracts the file name from a backslash path', () => {
  assert.equal(basename('C:\\Users\\me\\pics\\photo.jpg'), 'photo.jpg');
});

test('basename returns a bare file name unchanged', () => {
  assert.equal(basename('photo.jpg'), 'photo.jpg');
});

test('basename returns an empty string for a trailing slash', () => {
  assert.equal(basename('/a/b/'), '');
});

test('escapeHtml escapes angle brackets', () => {
  assert.equal(escapeHtml('<img src=x onerror=alert(1)>'), '&lt;img src=x onerror=alert(1)&gt;');
});

test('escapeHtml escapes quotes and ampersands', () => {
  assert.equal(escapeHtml('a&b"c\'d'), 'a&amp;b&quot;c&#39;d');
});

test('escapeHtml leaves plain text untouched', () => {
  assert.equal(escapeHtml('plain path C:/pics/1.jpg'), 'plain path C:/pics/1.jpg');
});

// Image rect spanning x 300..900, y 100..700 (window coords)
const IMG = { left: 300, top: 100, right: 900, bottom: 700 };

function resize(handle, start, dx, dy, minSize = 20) {
  return computeResizeRect(IMG, start, dx, dy, handle, minSize);
}

test('west handle dragged left at the image edge keeps the right edge fixed (regression)', () => {
  // Region flush with the image left edge: dragging west beyond it must NOT
  // grow the region past the image on the right side.
  const r = resize('w', { left: 300, top: 200, width: 600, height: 300 }, -50, 0);
  assert.equal(r.left, 300);
  assert.equal(r.width, 600);
  assert.equal(r.left + r.width, 900, 'right edge must stay at 900');
});

test('west handle dragged left mid-image grows leftward only, right edge fixed', () => {
  const r = resize('w', { left: 400, top: 200, width: 300, height: 300 }, -50, 0);
  assert.equal(r.left, 350);
  assert.equal(r.width, 350);
  assert.equal(r.left + r.width, 700, 'right edge must stay at 700');
});

test('west handle dragged right shrinks to min size in place, right edge fixed', () => {
  const r = resize('w', { left: 400, top: 200, width: 300, height: 300 }, 400, 0);
  assert.equal(r.width, 20);
  assert.equal(r.left, 680, 'left edge stops at fixedRight - minSize');
  assert.equal(r.left + r.width, 700, 'right edge stays fixed at 700');
});

test('east handle dragged right clamps at the image edge', () => {
  const r = resize('e', { left: 400, top: 200, width: 300, height: 300 }, 400, 0);
  assert.equal(r.left, 400);
  assert.equal(r.width, 500);
  assert.equal(r.left + r.width, 900);
});

test('east handle dragged left shrinks to min size in place, left edge fixed', () => {
  const r = resize('e', { left: 400, top: 200, width: 300, height: 300 }, -400, 0);
  assert.equal(r.width, 20);
  assert.equal(r.left, 400);
  assert.equal(r.left + r.width, 420);
});

test('north handle dragged up at the top edge keeps the bottom edge fixed (regression)', () => {
  const r = resize('n', { left: 400, top: 100, width: 300, height: 400 }, 0, -60);
  assert.equal(r.top, 100);
  assert.equal(r.height, 400);
  assert.equal(r.top + r.height, 500, 'bottom edge must stay at 500');
});

// Virtualized list window helpers
const ROW_H = 32;

test('computeVisibleRange covers the top rows with overscan at scroll 0', () => {
  const r = computeVisibleRange(0, 320, ROW_H, 1000, 10);
  assert.equal(r.start, 0);
  assert.equal(r.end, 20); // 10 viewport rows + 10 overscan
});

test('computeVisibleRange centers the window around the scrolled position', () => {
  // scrollTop 32*500 = row 500 at the top; viewport 10 rows, overscan 10
  const r = computeVisibleRange(500 * ROW_H, 10 * ROW_H, ROW_H, 1000, 10);
  assert.equal(r.start, 490);
  assert.equal(r.end, 520);
});

test('computeVisibleRange clamps at the bottom of the list', () => {
  const r = computeVisibleRange(995 * ROW_H, 10 * ROW_H, ROW_H, 1000, 10);
  assert.equal(r.start, 985);
  assert.equal(r.end, 1000);
});

test('computeVisibleRange handles a list shorter than the viewport', () => {
  const r = computeVisibleRange(0, 500, ROW_H, 5, 10);
  assert.equal(r.start, 0);
  assert.equal(r.end, 5);
});

test('computeVisibleRange returns empty for an empty list', () => {
  const r = computeVisibleRange(0, 320, ROW_H, 0, 10);
  assert.equal(r.start, 0);
  assert.equal(r.end, 0);
});

test('computeVisibleRange never returns start after end', () => {
  // pathological: viewport smaller than a row, top of the list
  const r = computeVisibleRange(2, 4, ROW_H, 100, 0);
  assert.ok(r.start <= r.end);
});

// --- Name sorting ---
const img = (name) => ({ name, modified: 0, size: 0 });

test('sort names case-insensitively: lowercase "a" before uppercase "Z"', () => {
  const list = [img('Zulu.jpg'), img('apple.jpg'), img('mango.jpg')];
  const sorted = [...list].sort((a, b) => compareImages(a, b, 'name', 1));
  assert.deepEqual(
    sorted.map((i) => i.name),
    ['apple.jpg', 'mango.jpg', 'Zulu.jpg']
  );
});

test('sort names is a single alphabet pass (no uppercase/lowercase split)', () => {
  // Case is ignored (sensitivity base): lowercase and uppercase interleave in
  // one alphabet pass instead of sorting every uppercase name before every
  // lowercase one. Within an equal-name group the original order is kept
  // (stable sort).
  const list = ['Zulu.jpg', 'apple.jpg', 'mango.jpg', 'Mango.jpg', 'Apricot.jpg'].map(img);
  const sorted = [...list].sort((a, b) => compareImages(a, b, 'name', 1));
  assert.deepEqual(
    sorted.map((i) => i.name),
    ['apple.jpg', 'Apricot.jpg', 'mango.jpg', 'Mango.jpg', 'Zulu.jpg']
  );
});

test('sort names ignores case entirely (a before Z)', () => {
  const sorted = ['Z.jpg', 'a.jpg'].map(img).sort((x, y) => compareImages(x, y, 'name', 1));
  assert.deepEqual(sorted.map((i) => i.name), ['a.jpg', 'Z.jpg']);
});

test('sort names by plain character order (no numeric interpretation)', () => {
  // No natural-number sorting: digit runs are compared as characters, so
  // "img10.jpg" < "img2.jpg" ('1' < '2'). This is the deliberate trade-off
  // for keeping UUID-style filenames in textual order.
  const list = ['img10.jpg', 'img2.jpg', 'img1.jpg'].map(img);
  const sorted = [...list].sort((a, b) => compareImages(a, b, 'name', 1));
  assert.deepEqual(
    sorted.map((i) => i.name),
    ['img1.jpg', 'img10.jpg', 'img2.jpg']
  );
});

test('sort names: leading-zero name stays at the top (0 < 1 < 3)', () => {
  // Regression: natural sort read "03e66bd5" as the number 3 and placed the
  // file between "3d…" and "4a…". Plain character order puts the '0' file
  // before the '1'-prefixed ones.
  const list = ['3deafb68.png', '1d50e8c0.png', '03e66bd5.png', '4a4cf769.png'].map(img);
  const sorted = [...list].sort((a, b) => compareImages(a, b, 'name', 1));
  assert.deepEqual(
    sorted.map((i) => i.name),
    ['03e66bd5.png', '1d50e8c0.png', '3deafb68.png', '4a4cf769.png']
  );
});

test('sort names descending flips the order', () => {
  const list = ['b.jpg', 'A.jpg', 'c.jpg'].map(img);
  const sorted = [...list].sort((a, b) => compareImages(a, b, 'name', -1));
  assert.deepEqual(
    sorted.map((i) => i.name),
    ['c.jpg', 'b.jpg', 'A.jpg']
  );
});

test('sort returns 0 for equal names (stable sort requirement)', () => {
  assert.equal(compareImages(img('x.jpg'), img('x.jpg'), 'name', 1), 0);
});

test('sort by size and modified use numeric comparison', () => {
  const a = { name: 'a', size: 10, modified: 100 };
  const b = { name: 'b', size: 20, modified: 50 };
  assert.equal(compareImages(a, b, 'size', 1), -1);
  assert.equal(compareImages(a, b, 'size', -1), 1);
  assert.equal(compareImages(a, b, 'modified', 1), 1);
  assert.equal(compareImages(a, b, 'modified', -1), -1);
});

test('north-west corner drag clamps both axes independently', () => {
  const r = resize('nw', { left: 400, top: 200, width: 300, height: 300 }, -200, -200);
  assert.equal(r.left, 300);
  assert.equal(r.width, 400, 'right edge fixed at 700');
  assert.equal(r.top, 100);
  assert.equal(r.height, 400, 'bottom edge fixed at 500');
  assert.equal(r.left + r.width, 700);
  assert.equal(r.top + r.height, 500);
});
