// The round-icon menu that replaces the OS context menu on right-click, plus
// the window-opening commands its items invoke.
import { invoke } from "../../shared/tauri.js";
import { reportHotRectSoon } from "../lib/hotrect.js";

export function openWorkspace() {
  invoke("open_workspace");
}

export function openSettings() {
  invoke("open_settings");
}

// Your normal terminal, not one of ours — the app used to run Claude inside its
// own PTY windows so it could watch the screen, and the hooks replaced that.
export function openTerminal() {
  invoke("open_system_terminal");
}

// System-tray-style minimize, not app.exit() — background watchers (GitHub
// polling, the hook event server) keep running; bring it back via the tray icon.
export function hideMascot() {
  invoke("hide_mascot");
}

export function hideQuickMenu() {
  document.getElementById("quick-menu").classList.remove("visible");
  reportHotRectSoon();
}
