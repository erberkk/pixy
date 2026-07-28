// Workspace window shell. Owns only what is genuinely shared between the three
// modes: which one is showing, the sidebar/search surfaces they swap in and out
// of, the title bar, and first-load init. Each mode's own logic lives in
// notes.js / memory.js / chat.js.
import { invoke, currentWindow } from "../shared/tauri.js";
import { getMode, setModeState } from "./lib/mode.js";
import {
  el,
  notesList,
  editorWrap,
  emptyState,
  searchInput,
  modeTabs,
  notesSidebarExtras,
  notesMainEl,
  memoryFiltersEl,
  memoryListEl,
  memoryMainEl,
  searchWrapEl,
  chatSidebarExtras,
  chatListEl,
  chatMainEl,
} from "./lib/dom.js";
import {
  activeNotes,
  applyFontMode,
  applyFontSize,
  applyFormatToggle,
  applyLinesMode,
  loadNotes,
  refreshFolderDisplay,
  renderList,
  selectNote,
  sortedNotes,
} from "./notes/notes.js";
import { loadMemories, renderMemoryList, resizeMemoryCanvas } from "./memory/memory.js";
import { loadChatMode } from "./chat/chat.js";

function setMode(mode) {
  setModeState(mode);
  modeTabs.querySelectorAll(".mode-tab").forEach((btn) => btn.classList.toggle("active", btn.dataset.mode === mode));
  notesMainEl.style.display = mode === "notes" ? "flex" : "none";
  memoryMainEl.style.display = mode === "memory" ? "flex" : "none";
  chatMainEl.style.display = mode === "chat" ? "flex" : "none";
  notesSidebarExtras.style.display = mode === "notes" ? "" : "none";
  el("notesTools").style.display = mode === "notes" ? "" : "none";
  el("formatRow").style.display = mode === "notes" ? "" : "none";
  memoryFiltersEl.style.display = mode === "memory" ? "flex" : "none";
  chatSidebarExtras.style.display = mode === "chat" ? "" : "none";
  notesList.style.display = mode === "notes" ? "" : "none";
  memoryListEl.style.display = mode === "memory" ? "" : "none";
  chatListEl.style.display = mode === "chat" ? "" : "none";
  searchWrapEl.style.display = mode === "chat" ? "none" : "";
  searchInput.placeholder = mode === "memory" ? "Search memory… (Ctrl+K)" : "Search notes… (Ctrl+K)";

  if (mode === "memory") {
    resizeMemoryCanvas();
    loadMemories();
  } else if (mode === "chat") {
    loadChatMode();
  } else {
    renderList();
  }
}

modeTabs.querySelectorAll(".mode-tab").forEach((btn) => {
  btn.addEventListener("click", () => setMode(btn.dataset.mode));
});

// One search box serves both list modes — it filters whichever one is showing.
searchInput.addEventListener("input", () => {
  if (getMode() === "memory") renderMemoryList();
  else renderList();
});

el("titlebar-close").addEventListener("click", () => {
  invoke("hide_workspace");
});
el("titlebar-minimize").addEventListener("click", () => {
  currentWindow().minimize();
});
el("titlebar-maximize").addEventListener("click", () => {
  currentWindow().toggleMaximize();
});

window.addEventListener("DOMContentLoaded", async () => {
  applyFontMode();
  applyLinesMode();
  applyFontSize();
  applyFormatToggle();
  refreshFolderDisplay();
  await loadNotes();
  if (activeNotes().length) {
    selectNote(sortedNotes()[0].file_path);
  } else {
    editorWrap.style.display = "none";
    emptyState.style.display = "flex";
  }
  resizeMemoryCanvas();
  // Chat is the default mode. Going through setMode rather than relying on the
  // markup's initial display values keeps one source of truth for which panels,
  // sidebar sections and list are showing — and it's what loads the chat's own
  // data (profiles + conversation list), which static markup can't do.
  setMode("chat");
});
