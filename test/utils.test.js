import { test } from 'node:test';
import assert from 'node:assert/strict';
import { basename, escapeHtml, computeVisibleRange, compareImages, formatBytes } from '../src/utils.js';

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

// --- Byte formatting ---

test('formatBytes renders bytes below 1 KB', () => {
  assert.equal(formatBytes(0), '0 B');
  assert.equal(formatBytes(512), '512 B');
});

test('formatBytes renders kilobytes with one decimal', () => {
  assert.equal(formatBytes(1024), '1.0 KB');
  assert.equal(formatBytes(1536), '1.5 KB');
  assert.equal(formatBytes(58 * 1024), '58.0 KB');
});

test('formatBytes scales to megabytes and gigabytes', () => {
  assert.equal(formatBytes(1024 * 1024), '1.0 MB');
  assert.equal(formatBytes(2.5 * 1024 * 1024), '2.5 MB');
  assert.equal(formatBytes(3 * 1024 ** 3), '3.0 GB');
});

// --- Virtualized list window helpers ---
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
