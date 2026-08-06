import { test } from 'node:test';
import assert from 'node:assert/strict';
import { basename, escapeHtml, computeResizeRect, computeVisibleRange } from '../src/utils.js';

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

test('north-west corner drag clamps both axes independently', () => {
  const r = resize('nw', { left: 400, top: 200, width: 300, height: 300 }, -200, -200);
  assert.equal(r.left, 300);
  assert.equal(r.width, 400, 'right edge fixed at 700');
  assert.equal(r.top, 100);
  assert.equal(r.height, 400, 'bottom edge fixed at 500');
  assert.equal(r.left + r.width, 700);
  assert.equal(r.top + r.height, 500);
});
