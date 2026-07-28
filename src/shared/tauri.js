// Tauri's injected globals, surfaced as ES module bindings so every window
// script imports them the same way instead of re-deriving them from
// `window.__TAURI__` in each file.
export const { invoke } = window.__TAURI__.core;
export const { listen } = window.__TAURI__.event;
export const currentWindow = () => window.__TAURI__.window.getCurrentWindow();
export const currentWebview = () => window.__TAURI__.webview.getCurrentWebview();
