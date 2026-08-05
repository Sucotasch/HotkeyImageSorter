import { test } from 'node:test';
import assert from 'node:assert/strict';
import { basename } from './utils.js';

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
