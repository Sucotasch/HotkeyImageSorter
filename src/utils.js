// Pure helper functions shared by the frontend, kept free of Tauri/window
// dependencies so they can be unit-tested with Node's built-in test runner.

export function basename(path) {
  return path.split(/[\\/]/).pop();
}
