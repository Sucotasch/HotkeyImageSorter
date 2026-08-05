import { test } from 'node:test';
import assert from 'node:assert/strict';
import { basename, escapeHtml } from '../src/utils.js';

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
