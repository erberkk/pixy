// Notes mode: the plain-text editor, its note list, preview, find/replace,
// command palette and trash. Reads and writes files through content/notes.rs.
import { invoke, currentWebview } from "../../shared/tauri.js";
import { escapeHtml, markdownToHtml } from "../../shared/markdown.js";
import { timeAgo } from "../../shared/format.js";
import { showToast } from "../lib/toast.js";
import {
  el,
  notesList,
  notesCount,
  titleInput,
  editor,
  gutter,
  preview,
  emptyState,
  editorWrap,
  searchInput,
  saveDot,
  wordCount,
  charCount,
  lineCount,
  readTime,
  curLine,
  curCol,
  selectionStat,
  selWords,
  selChars,
  sidebar,
  findPanel,
  findInput,
  replaceInput,
  app,
  overlayBackdrop,
  cmdk,
  cmdkInput,
  cmdkList,
  trashModal,
  trashList,
} from "../lib/dom.js";

let notes = [];
let activePath = null;
let saveTimer = null;
let useMono = localStorage.getItem("notepad_mono") === "1";
let showLines = localStorage.getItem("notepad_lines") === "1";
let previewMode = "off"; // off | split | full
let focusMode = false;
let fontSize = parseFloat(localStorage.getItem("notepad_fontsize") || "14.5");

export function activeNotes() {
  return notes.filter((n) => !n.trashed);
}
function trashedNotes() {
  return notes.filter((n) => n.trashed);
}

function defaultExt() {
  return localStorage.getItem("notepad_default_ext") || "txt";
}

function noteExt(note) {
  const m = /\.([^.\\/]+)$/.exec(note.file_path);
  return m ? m[1].toLowerCase() : defaultExt();
}

function persistNote(note, extOverride) {
  const wasActive = note.file_path === activePath;
  invoke("save_note", { note, ext: extOverride || null }).then((saved) => {
    const idx = notes.findIndex((n) => n.file_path === note.file_path);
    if (idx !== -1) notes[idx] = saved;
    else notes.push(saved);
    // A title edit (or an extension conversion) renames the underlying
    // file, which changes its path — keep "this is the open note" tracking
    // in sync with the new path.
    if (wasActive) {
      activePath = saved.file_path;
      updateExtBadge();
    }
    renderList();
  });
}

export function sortedNotes() {
  return [...activeNotes()].sort((a, b) => {
    if (!!a.pinned !== !!b.pinned) return a.pinned ? -1 : 1;
    return b.updated_at - a.updated_at;
  });
}

export function renderList() {
  const q = searchInput.value.trim().toLowerCase();
  const list = sortedNotes().filter(
    (n) => !q || n.title.toLowerCase().includes(q) || n.content.toLowerCase().includes(q)
  );
  notesCount.textContent = activeNotes().length + (activeNotes().length === 1 ? " note" : " notes");

  if (list.length === 0) {
    notesList.innerHTML =
      '<div class="empty-list">' +
      (activeNotes().length === 0 ? "No notes yet.<br>Create your first one." : "No matches found.") +
      "</div>";
    return;
  }

  notesList.innerHTML = list
    .map((n, i) => {
      return (
        '<div class="note-item ' +
        (n.file_path === activePath ? "active" : "") +
        '" data-i="' +
        i +
        '">' +
        '<div class="note-item-top">' +
        '<div class="note-title-row">' +
        '<span class="note-title">' +
        escapeHtml(n.title || "Untitled note") +
        "</span></div>" +
        '<div class="note-meta-row">' +
        (n.pinned ? '<span class="pin-dot"></span>' : "") +
        '<button class="note-delete" data-i="' +
        i +
        '" title="Delete"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M3 6h18M8 6V4a2 2 0 012-2h4a2 2 0 012 2v2m3 0l-1 14a2 2 0 01-2 2H8a2 2 0 01-2-2L5 6h14z"/></svg></button>' +
        "</div>" +
        "</div>" +
        '<span class="note-preview">' +
        (escapeHtml(n.content.slice(0, 60).replace(/\n/g, " ")) || "No additional text") +
        "</span>" +
        '<span class="note-time">' +
        timeAgo(n.updated_at) +
        "</span>" +
        "</div>"
      );
    })
    .join("");

  notesList.querySelectorAll(".note-item").forEach((item) => {
    item.addEventListener("click", (e) => {
      if (e.target.closest(".note-delete")) return;
      selectNote(list[parseInt(item.dataset.i, 10)].file_path);
    });
  });
  notesList.querySelectorAll(".note-delete").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      trashNote(list[parseInt(btn.dataset.i, 10)].file_path);
    });
  });
}

function renderPreview() {
  // Always render the live textarea content, not the last-saved note (which
  // only catches up after the autosave debounce) — otherwise preview lags
  // a beat behind what's actually being typed.
  preview.innerHTML = markdownToHtml(editor.value);
}

// Bound once (event delegation) instead of re-querying and re-attaching a
// listener per link/checkbox on every single re-render — with the preview
// re-rendering on every keystroke, per-element rebinding was the main cost.
preview.addEventListener("click", (e) => {
  const link = e.target.closest(".wiki-link");
  if (link) {
    const title = link.dataset.note;
    let target = activeNotes().find((x) => x.title.toLowerCase() === title.toLowerCase());
    if (target) selectNote(target.file_path);
    else createNote({ title }).then((created) => selectNote(created.file_path));
    return;
  }

  const checkbox = e.target.closest(".md-check input");
  if (checkbox) {
    e.stopPropagation();
    const lineIdx = parseInt(checkbox.dataset.line, 10);
    const lines = editor.value.split("\n");
    const l = lines[lineIdx];
    if (/\[ \]/.test(l)) lines[lineIdx] = l.replace("[ ]", "[x]");
    else lines[lineIdx] = l.replace(/\[x\]/i, "[ ]");
    editor.value = lines.join("\n");
    scheduleSave(true);
    updateStats();
    updateGutter();
    renderPreview();
  }
});

function applyPreviewMode() {
  editorWrap.classList.remove("split", "preview-only");
  if (previewMode === "split") editorWrap.classList.add("split");
  if (previewMode === "full") editorWrap.classList.add("preview-only");
  el("previewBtn").classList.toggle("accent", previewMode !== "off");
  if (previewMode !== "off") renderPreview();
}

/* ---------------- note selection / CRUD ---------------- */
function currentNote() {
  return notes.find((n) => n.file_path === activePath);
}

export function selectNote(filePath) {
  activePath = filePath;
  const n = notes.find((x) => x.file_path === filePath);
  if (!n) return;
  titleInput.value = n.title;
  editor.value = n.content;
  editorWrap.style.display = "flex";
  emptyState.style.display = "none";
  updateGutter();
  updateStats();
  updateCursorPos();
  renderList();
  el("pinBtn").classList.toggle("accent", !!n.pinned);
  updateExtBadge();
  if (previewMode !== "off") renderPreview();
  setTimeout(() => editor.focus(), 50);
}

function updateExtBadge() {
  const n = currentNote();
  const badge = el("extBadge");
  if (!n) {
    badge.textContent = "";
    return;
  }
  badge.textContent = "." + noteExt(n);
}

async function createNote(overrides) {
  const base = {
    file_path: "",
    title: "",
    content: "",
    pinned: false,
    trashed: false,
    created_at: 0,
    updated_at: 0,
  };
  const created = await invoke("save_note", { note: { ...base, ...overrides }, ext: defaultExt() });
  notes.unshift(created);
  return created;
}

async function newNote() {
  const created = await createNote({});
  renderList();
  selectNote(created.file_path);
  titleInput.focus();
}

async function duplicateNote() {
  const n = currentNote();
  if (!n) return;
  const created = await createNote({
    title: n.title + " (copy)",
    content: n.content,
    pinned: false,
  });
  renderList();
  selectNote(created.file_path);
  showToast("Note duplicated");
}

function trashNote(filePath) {
  const n = notes.find((x) => x.file_path === filePath);
  if (!n) return;
  n.trashed = true;
  persistNote(n);
  if (activePath === filePath) {
    activePath = null;
    const rest = sortedNotes();
    if (rest.length) {
      selectNote(rest[0].file_path);
    } else {
      editorWrap.style.display = "none";
      emptyState.style.display = "flex";
    }
  }
  renderList();
  showToast("Moved to trash");
}

function restoreNote(filePath) {
  const n = notes.find((x) => x.file_path === filePath);
  if (!n) return;
  n.trashed = false;
  persistNote(n);
  renderList();
  renderTrash();
  showToast("Note restored");
}

function purgeNote(filePath) {
  invoke("delete_note", { filePath });
  notes = notes.filter((n) => n.file_path !== filePath);
  renderTrash();
}

function emptyTrash() {
  trashedNotes().forEach((n) => invoke("delete_note", { filePath: n.file_path }));
  notes = notes.filter((n) => !n.trashed);
  renderTrash();
  showToast("Trash emptied");
}

/* ---------------- save / stats ---------------- */
function scheduleSave(immediate) {
  const n = currentNote();
  if (!n) return;
  saveDot.className = "save-dot saving";
  clearTimeout(saveTimer);
  const doSave = () => {
    n.title = titleInput.value;
    n.content = editor.value;
    persistNote(n);
    saveDot.className = "save-dot saved";
    renderList();
  };
  if (immediate) doSave();
  else saveTimer = setTimeout(doSave, 350);
}

function updateStats() {
  const text = editor.value;
  const words = text.trim() ? text.trim().split(/\s+/).length : 0;
  wordCount.textContent = words;
  charCount.textContent = text.length;
  lineCount.textContent = text.split("\n").length;
  readTime.textContent = Math.max(1, Math.round(words / 200)) + " min";
}

function updateGutter() {
  if (!showLines) return;
  const lines = editor.value.split("\n").length;
  let out = "";
  for (let i = 1; i <= lines; i++) out += i + "\n";
  gutter.textContent = out.trim() ? out : "1";
}

function updateCursorPos() {
  const pos = editor.selectionStart;
  const upto = editor.value.slice(0, pos);
  const linesUpto = upto.split("\n");
  curLine.textContent = linesUpto.length;
  curCol.textContent = linesUpto[linesUpto.length - 1].length + 1;

  const selLen = editor.selectionEnd - editor.selectionStart;
  if (selLen > 0) {
    const selected = editor.value.slice(editor.selectionStart, editor.selectionEnd);
    selChars.textContent = selLen;
    selWords.textContent = selected.trim() ? selected.trim().split(/\s+/).length : 0;
    selectionStat.style.display = "flex";
  } else {
    selectionStat.style.display = "none";
  }
}

export function applyFontMode() {
  editor.classList.toggle("mono", useMono);
  el("fontToggle").classList.toggle("accent", useMono);
}
export function applyLinesMode() {
  gutter.classList.toggle("show", showLines);
  el("linesToggle").classList.toggle("accent", showLines);
  updateGutter();
}
export function applyFontSize() {
  document.documentElement.style.setProperty("--editor-size", fontSize + "px");
  localStorage.setItem("notepad_fontsize", String(fontSize));
}

function countMatches(needle) {
  if (!needle) return 0;
  let count = 0;
  let idx = editor.value.indexOf(needle);
  while (idx !== -1) {
    count++;
    idx = editor.value.indexOf(needle, idx + needle.length);
  }
  return count;
}

function updateFindCount() {
  const f = findInput.value;
  el("findCount").textContent = f ? countMatches(f) + " matches" : "";
}

function findNext() {
  const f = findInput.value;
  updateFindCount();
  if (!f) return;
  const start = editor.selectionEnd || 0;
  let idx = editor.value.indexOf(f, start);
  if (idx === -1) idx = editor.value.indexOf(f, 0);
  if (idx === -1) return;
  editor.focus();
  editor.setSelectionRange(idx, idx + f.length);
  updateCursorPos();
}

/* ---------------- trash modal ---------------- */
function renderTrash() {
  const items = trashedNotes();
  if (items.length === 0) {
    trashList.innerHTML = '<div class="trash-empty">Trash is empty</div>';
    return;
  }
  trashList.innerHTML = items
    .map(
      (n, i) =>
        '<div class="trash-item" data-i="' +
        i +
        '"><span class="t-title">' +
        escapeHtml(n.title || "Untitled note") +
        '</span><div class="t-actions">' +
        '<button class="mini-btn restore-btn" data-i="' +
        i +
        '">Restore</button>' +
        '<button class="mini-btn danger purge-btn" data-i="' +
        i +
        '">Delete</button></div></div>'
    )
    .join("");
  trashList
    .querySelectorAll(".restore-btn")
    .forEach((b) => b.addEventListener("click", () => restoreNote(items[parseInt(b.dataset.i, 10)].file_path)));
  trashList
    .querySelectorAll(".purge-btn")
    .forEach((b) => b.addEventListener("click", () => purgeNote(items[parseInt(b.dataset.i, 10)].file_path)));
}

function openOverlay(panel) {
  overlayBackdrop.classList.add("open");
  panel.classList.add("open");
}
function closeOverlays() {
  overlayBackdrop.classList.remove("open");
  cmdk.classList.remove("open");
  trashModal.classList.remove("open");
}
overlayBackdrop.addEventListener("click", closeOverlays);

/* ---------------- command palette ---------------- */
let cmdkActiveIndex = 0;
function baseCommands() {
  const cmds = [
    { label: "New note", tag: "Ctrl+N", run: newNote },
    { label: "Duplicate current note", tag: "", run: duplicateNote },
    { label: "Toggle Markdown preview (split)", tag: "Ctrl+E", run: togglePreview },
    { label: "Toggle full-screen Markdown preview", tag: "Ctrl+Shift+E", run: toggleFullPreview },
    { label: "Toggle focus mode", tag: "Ctrl+.", run: toggleFocus },
    {
      label: "Toggle line numbers",
      tag: "",
      run: () => {
        showLines = !showLines;
        localStorage.setItem("notepad_lines", showLines ? "1" : "0");
        applyLinesMode();
      },
    },
    {
      label: "Toggle mono font",
      tag: "",
      run: () => {
        useMono = !useMono;
        localStorage.setItem("notepad_mono", useMono ? "1" : "0");
        applyFontMode();
      },
    },
    { label: "Save now", tag: "Ctrl+S", run: () => scheduleSave(true) },
    { label: "Convert note to .txt / .md", tag: "", run: () => el("extBadge").click() },
    {
      label: "Increase font size",
      tag: "Ctrl+=",
      run: () => {
        fontSize = Math.min(24, fontSize + 1);
        applyFontSize();
      },
    },
    {
      label: "Decrease font size",
      tag: "Ctrl+-",
      run: () => {
        fontSize = Math.max(11, fontSize - 1);
        applyFontSize();
      },
    },
    {
      label: "Open trash",
      tag: "",
      run: () => {
        closeOverlays();
        renderTrash();
        openOverlay(trashModal);
      },
    },
    { label: "Move current note to trash", tag: "", run: () => activePath && trashNote(activePath) },
  ];
  return cmds;
}

function renderCmdk() {
  const q = cmdkInput.value.trim().toLowerCase();
  const cmds = baseCommands().map((c) => ({ type: "cmd", label: c.label, tag: c.tag, run: c.run }));
  const noteMatches = activeNotes()
    .filter((n) => !q || n.title.toLowerCase().includes(q))
    .slice(0, 8)
    .map((n) => ({
      type: "note",
      label: n.title || "Untitled note",
      tag: "note",
      run: () => {
        closeOverlays();
        selectNote(n.file_path);
      },
    }));
  let all = [...noteMatches, ...cmds];
  if (q) all = all.filter((c) => c.label.toLowerCase().includes(q));
  cmdkActiveIndex = 0;
  if (all.length === 0) {
    cmdkList.innerHTML = '<div class="cmdk-empty">No results</div>';
    cmdkList._items = [];
    return;
  }
  cmdkList.innerHTML = all
    .map(
      (c, i) =>
        '<div class="cmdk-item ' +
        (i === 0 ? "active" : "") +
        '" data-i="' +
        i +
        '"><span>' +
        escapeHtml(c.label) +
        '</span><span class="tag">' +
        c.tag +
        "</span></div>"
    )
    .join("");
  cmdkList._items = all;
  cmdkList.querySelectorAll(".cmdk-item").forEach((it) => {
    it.addEventListener("click", () => all[parseInt(it.dataset.i, 10)].run());
    it.addEventListener("mouseenter", () => {
      cmdkActiveIndex = parseInt(it.dataset.i, 10);
      highlightCmdk();
    });
  });
}
function highlightCmdk() {
  cmdkList.querySelectorAll(".cmdk-item").forEach((it, i) => it.classList.toggle("active", i === cmdkActiveIndex));
  const activeEl = cmdkList.querySelector(".cmdk-item.active");
  if (activeEl) activeEl.scrollIntoView({ block: "nearest" });
}
function openCmdk() {
  closeOverlays();
  cmdkInput.value = "";
  renderCmdk();
  openOverlay(cmdk);
  setTimeout(() => cmdkInput.focus(), 50);
}
cmdkInput.addEventListener("input", renderCmdk);
cmdkInput.addEventListener("keydown", (e) => {
  const items = cmdkList._items || [];
  if (e.key === "ArrowDown") {
    e.preventDefault();
    cmdkActiveIndex = Math.min(items.length - 1, cmdkActiveIndex + 1);
    highlightCmdk();
  }
  if (e.key === "ArrowUp") {
    e.preventDefault();
    cmdkActiveIndex = Math.max(0, cmdkActiveIndex - 1);
    highlightCmdk();
  }
  if (e.key === "Enter") {
    e.preventDefault();
    if (items[cmdkActiveIndex]) items[cmdkActiveIndex].run();
  }
  if (e.key === "Escape") closeOverlays();
});

/* ---------------- focus / preview toggles ---------------- */
function togglePreview() {
  previewMode = previewMode === "off" ? "split" : "off";
  applyPreviewMode();
}
function toggleFullPreview() {
  previewMode = previewMode === "full" ? "off" : "full";
  applyPreviewMode();
}
function exitPreview() {
  if (previewMode !== "off") {
    previewMode = "off";
    applyPreviewMode();
    return true;
  }
  return false;
}
function toggleFocus() {
  focusMode = !focusMode;
  app.classList.toggle("focus-mode", focusMode);
  el("focusBtn").classList.toggle("accent", focusMode);
}
function exitFocus() {
  if (focusMode) {
    focusMode = false;
    app.classList.remove("focus-mode");
    el("focusBtn").classList.remove("accent");
    return true;
  }
  return false;
}

/* ---------------- auto-continue lists ---------------- */
editor.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    const val = editor.value;
    const pos = editor.selectionStart;
    const lineStart = val.lastIndexOf("\n", pos - 1) + 1;
    const line = val.slice(lineStart, pos);
    const checkM = line.match(/^(\s*)-\s\[( |x|X)\]\s/);
    const ulM = line.match(/^(\s*)([-*])\s/);
    const olM = line.match(/^(\s*)(\d+)\.\s/);
    let prefix = null;
    if (checkM) prefix = checkM[1] + "- [ ] ";
    else if (ulM) prefix = ulM[1] + ulM[2] + " ";
    else if (olM) prefix = olM[1] + (parseInt(olM[2], 10) + 1) + ". ";
    if (prefix) {
      if (line.trim() === prefix.trim()) {
        e.preventDefault();
        const before = val.slice(0, lineStart);
        const after = val.slice(pos);
        editor.value = before + after;
        editor.setSelectionRange(lineStart, lineStart);
      } else {
        e.preventDefault();
        document.execCommand("insertText", false, "\n" + prefix);
      }
      scheduleSave();
      updateStats();
      updateGutter();
    }
  }
});

/* ---------------- events ---------------- */
el("newNoteBtn").addEventListener("click", newNote);
el("deleteBtn").addEventListener("click", () => {
  if (activePath) trashNote(activePath);
});
el("pinBtn").addEventListener("click", () => {
  const n = currentNote();
  if (!n) return;
  n.pinned = !n.pinned;
  persistNote(n);
  el("pinBtn").classList.toggle("accent", n.pinned);
  renderList();
});
titleInput.addEventListener("input", () => scheduleSave());
let previewFrame = null;
function schedulePreviewRender() {
  if (previewMode === "off") return;
  // Coalesce into the next animation frame so a burst of keystrokes (fast
  // typing) triggers one re-render per frame instead of one per keypress —
  // that per-keystroke full markdown re-parse + innerHTML rebuild was what
  // made the preview feel like it was stuttering behind the typing.
  if (previewFrame) return;
  previewFrame = requestAnimationFrame(() => {
    previewFrame = null;
    renderPreview();
  });
}

editor.addEventListener("input", () => {
  scheduleSave();
  updateStats();
  updateGutter();
  schedulePreviewRender();
});
editor.addEventListener("scroll", () => {
  gutter.scrollTop = editor.scrollTop;
});
editor.addEventListener("keyup", updateCursorPos);
editor.addEventListener("click", updateCursorPos);
editor.addEventListener("select", updateCursorPos);

el("fontToggle").addEventListener("click", () => {
  useMono = !useMono;
  localStorage.setItem("notepad_mono", useMono ? "1" : "0");
  applyFontMode();
});
el("linesToggle").addEventListener("click", () => {
  showLines = !showLines;
  localStorage.setItem("notepad_lines", showLines ? "1" : "0");
  applyLinesMode();
});
el("previewBtn").addEventListener("click", togglePreview);
el("focusBtn").addEventListener("click", toggleFocus);
el("exitFocusBtn").addEventListener("click", exitFocus);
el("trashToggle").addEventListener("click", () => {
  closeOverlays();
  renderTrash();
  openOverlay(trashModal);
});
el("closeTrashBtn").addEventListener("click", closeOverlays);
el("emptyTrashBtn").addEventListener("click", emptyTrash);

el("collapseBtn").addEventListener("click", () => {
  sidebar.classList.add("collapsed");
  el("openSidebarBtn").style.display = "flex";
});
el("openSidebarBtn").addEventListener("click", () => {
  sidebar.classList.remove("collapsed");
  el("openSidebarBtn").style.display = "none";
});

el("findBtn").addEventListener("click", () => {
  findPanel.classList.toggle("open");
  if (findPanel.classList.contains("open")) {
    findInput.focus();
    updateFindCount();
  }
});
el("closeFindBtn").addEventListener("click", () => findPanel.classList.remove("open"));
findInput.addEventListener("input", updateFindCount);
el("findNextBtn").addEventListener("click", findNext);
el("replaceAllBtn").addEventListener("click", () => {
  const f = findInput.value;
  if (!f) return;
  const r = replaceInput.value;
  editor.value = editor.value.split(f).join(r);
  scheduleSave(true);
  updateStats();
  updateGutter();
  updateFindCount();
  if (previewMode !== "off") renderPreview();
  showToast("Replaced all matches");
});

document.addEventListener("keydown", (e) => {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && e.key.toLowerCase() === "n") {
    e.preventDefault();
    newNote();
  }
  if (mod && e.key.toLowerCase() === "f") {
    e.preventDefault();
    findPanel.classList.add("open");
    findInput.focus();
  }
  if (mod && e.key.toLowerCase() === "s") {
    e.preventDefault();
    scheduleSave(true);
  }
  if (mod && e.key.toLowerCase() === "k") {
    e.preventDefault();
    openCmdk();
  }
  if (mod && e.shiftKey && e.key.toLowerCase() === "e") {
    e.preventDefault();
    toggleFullPreview();
  } else if (mod && e.key.toLowerCase() === "e") {
    e.preventDefault();
    togglePreview();
  }
  if (mod && e.key === ".") {
    e.preventDefault();
    toggleFocus();
  }
  if (mod && (e.key === "=" || e.key === "+")) {
    e.preventDefault();
    fontSize = Math.min(24, fontSize + 1);
    applyFontSize();
  }
  if (mod && e.key === "-") {
    e.preventDefault();
    fontSize = Math.max(11, fontSize - 1);
    applyFontSize();
  }
  if (e.key === "Escape") {
    // Exiting focus mode is handled FIRST and separately from find/overlay
    // handling below: focus mode hides the toolbar (including the focus
    // toggle button itself), so previously Escape/keyboard-only was the sole
    // way out, and Escape wasn't wired to it at all — that was the "can't
    // get back out of fullscreen" bug.
    if (exitFocus()) return;
    findPanel.classList.remove("open");
    const overlayWasOpen = overlayBackdrop.classList.contains("open");
    closeOverlays();
    if (!overlayWasOpen) exitPreview();
  }
});

/* ---------------- storage folder ---------------- */
export async function refreshFolderDisplay() {
  const dir = await invoke("get_notes_dir");
  const folderPathEl = el("folderPath");
  folderPathEl.textContent = dir;
  el("folderRow").title = dir;
}

el("changeFolderBtn").addEventListener("click", async () => {
  const dir = await invoke("choose_notes_dir");
  if (!dir) return; // user cancelled the picker
  await refreshFolderDisplay();
  notes = await invoke("list_notes");
  renderList();
  activePath = null;
  if (activeNotes().length) {
    selectNote(sortedNotes()[0].file_path);
  } else {
    editorWrap.style.display = "none";
    emptyState.style.display = "flex";
  }
  showToast("Notes folder changed");
});

function registerOpenedNote(note) {
  const idx = notes.findIndex((n) => n.file_path === note.file_path);
  if (idx !== -1) notes[idx] = note;
  else notes.unshift(note);
  renderList();
  selectNote(note.file_path);
  showToast("Opened " + note.title);
}

el("openFileBtn").addEventListener("click", async () => {
  const note = await invoke("open_external_file");
  if (!note) return; // user cancelled the picker
  registerOpenedNote(note);
});

/* ---------------- default save format ---------------- */
export function applyFormatToggle() {
  const ext = defaultExt();
  document.querySelectorAll(".format-toggle-opt").forEach((btn) => {
    btn.classList.toggle("active", btn.dataset.ext === ext);
  });
}
document.querySelectorAll(".format-toggle-opt").forEach((btn) => {
  btn.addEventListener("click", () => {
    localStorage.setItem("notepad_default_ext", btn.dataset.ext);
    applyFormatToggle();
  });
});

el("extBadge").addEventListener("click", () => {
  const n = currentNote();
  if (!n) return;
  const next = noteExt(n) === "md" ? "txt" : "md";
  persistNote(n, next);
});

/* ---------------- drag & drop a .txt/.md file to open it ---------------- */
// Uses Tauri's native drag-drop event, not the HTML5 File API — the browser
// API never exposes a dropped file's real filesystem path, which we need so
// edits save straight back to the original file.
currentWebview().onDragDropEvent((event) => {
  const dropOverlay = el("dropOverlay");
  if (event.payload.type === "enter" || event.payload.type === "over") {
    dropOverlay.classList.add("visible");
    return;
  }
  if (event.payload.type === "leave") {
    dropOverlay.classList.remove("visible");
    return;
  }
  if (event.payload.type !== "drop") return;
  dropOverlay.classList.remove("visible");
  event.payload.paths.forEach(async (path) => {
    const note = await invoke("open_path", { path });
    if (note) registerOpenedNote(note);
    else showToast("Unsupported file type");
  });
});

// Loads the note list from disk. The array itself stays private to this module
// so the shell never has to hold (or accidentally stale-cache) notes state.
export async function loadNotes() {
  notes = await invoke("list_notes");
  renderList();
  return notes.length;
}
