// Mascot window bootstrap. Owns no feature logic — it only wires the backend's
// events and the pill's own mouse interactions to the modules that do:
// notice.js (state machine), github-notice.js, agent-notice.js, quick-menu.js.
import { listen, currentWindow } from "../shared/tauri.js";
import { cancelSpotifyPanel } from "./spotify/spotify.js";
import { setState } from "./notice/notice.js";
import { reportHotRectSoon } from "./lib/hotrect.js";
import {
  closeGithubDigestNotice,
  showGithubDigestNotice,
  showGithubIssueNotice,
  showGithubMergeNotice,
} from "./github/github-notice.js";
import { showAgentPermissionNotice } from "./agent/agent-notice.js";
import { hideMascot, hideQuickMenu, openSettings, openTerminal, openWorkspace } from "./quick-menu/quick-menu.js";

window.addEventListener("DOMContentLoaded", () => {
  setState("idle");

  listen("mascot-state", (event) => {
    setState(event.payload);
  });

  listen("github-merge", (event) => {
    showGithubMergeNotice(event.payload);
  });

  listen("github-issue-update", (event) => {
    showGithubIssueNotice(event.payload);
  });

  listen("github-digest", (event) => {
    showGithubDigestNotice(event.payload.summary);
  });

  listen("mascot-permission-request", (event) => {
    showAgentPermissionNotice(event.payload);
  });

  const mascotEl = document.getElementById("mascot");
  const quickMenu = document.getElementById("quick-menu");

  mascotEl.addEventListener("dblclick", () => {
    cancelSpotifyPanel();
    openTerminal();
  });

  // Window dragging, implemented by hand instead of the native
  // data-tauri-drag-region attribute — that attribute hijacks the window
  // move on the very first mousedown unconditionally, which silently
  // swallowed the "click" event for a plain stationary click (confirmed by
  // simulating a real OS-level click with and without the attribute
  // present — this is what broke the Spotify hover panel's single-click
  // open). Tracking movement ourselves and only calling startDragging()
  // once the cursor has actually moved past a small deadzone means a real
  // click (mousedown+mouseup with no/negligible movement) never touches
  // this at all, leaving click/dblclick/contextmenu completely unaffected.
  const DRAG_THRESHOLD_PX = 4;
  let dragStart = null;
  mascotEl.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return; // left button only
    if (e.target.closest("button, input, .permission-btn, .spotify-btn, .toggle-btn")) return;
    dragStart = { x: e.clientX, y: e.clientY };
  });
  window.addEventListener("mousemove", (e) => {
    if (!dragStart) return;
    const dx = e.clientX - dragStart.x;
    const dy = e.clientY - dragStart.y;
    if (Math.hypot(dx, dy) > DRAG_THRESHOLD_PX) {
      dragStart = null;
      currentWindow().startDragging();
    }
  });
  window.addEventListener("mouseup", () => {
    dragStart = null;
  });

  // Right-click opens a small round-icon menu below the pill instead of the
  // OS's default rectangular context menu (more items — Mail — will land in
  // here later). Collapses the spotify panel first (see spotify.js) — the
  // two are mutually exclusive, otherwise the menu (positioned assuming the
  // plain small pill) visually collides with the enlarged panel.
  mascotEl.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    cancelSpotifyPanel();
    quickMenu.classList.toggle("visible");
    reportHotRectSoon();
  });

  document.getElementById("quick-menu-workspace").addEventListener("click", () => {
    openWorkspace();
    hideQuickMenu();
  });

  document.getElementById("quick-menu-settings").addEventListener("click", () => {
    openSettings();
    hideQuickMenu();
  });

  document.getElementById("quick-menu-hide").addEventListener("click", () => {
    hideMascot();
    hideQuickMenu();
  });

  document.getElementById("digest-close-btn").addEventListener("click", () => {
    closeGithubDigestNotice();
  });

  document.addEventListener("click", (e) => {
    if (!e.target.closest("#quick-menu")) hideQuickMenu();
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") hideQuickMenu();
  });
});
