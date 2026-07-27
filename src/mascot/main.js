import { cancelSpotifyPanel } from "./spotify.js";
import { computeAmbient } from "./signals.js";

const { listen } = window.__TAURI__.event;

const VALID_STATES = ["idle", "waiting_permission", "waiting_input", "turn_done"];

const NOTICE_TEXT = {
  waiting_permission: "Claude is waiting for your approval",
  waiting_input: "Claude is waiting for you",
};

let revertTimer = null;

// While true, some pinned notice (GitHub digest card, or an agent-terminal
// permission prompt) is locked open — nothing else (hook events, merge
// notices) is allowed to touch document.body.className until the user
// explicitly resolves it via its own button(s).
let noticeLocked = false;

// The window is always-on-top and much bigger than the visible pill (extra
// room for the digest card/quick-menu), and Windows routes clicks to
// whatever window sits at that screen point regardless of CSS transparency
// — so without this, the widget silently blocks clicks on anything behind
// its full (mostly invisible) rectangle. Rust polls the real cursor
// position against whatever rect we report here and makes the window
// click-through outside of it (see clickthrough.rs).
function reportHotRect() {
  const scale = window.devicePixelRatio || 1;
  const rects = [document.getElementById("mascot").getBoundingClientRect()];
  const quickMenu = document.getElementById("quick-menu");
  if (quickMenu.classList.contains("visible")) {
    // quick-menu itself is a 0x0 positioning anchor — its buttons are placed
    // via transform outside that box, so union each button instead.
    document.querySelectorAll(".quick-menu-item").forEach((btn) => rects.push(btn.getBoundingClientRect()));
  }
  const left = Math.min(...rects.map((r) => r.left));
  const top = Math.min(...rects.map((r) => r.top));
  const right = Math.max(...rects.map((r) => r.right));
  const bottom = Math.max(...rects.map((r) => r.bottom));
  window.__TAURI__.core.invoke("set_hot_rect", {
    x: left * scale,
    y: top * scale,
    width: (right - left) * scale,
    height: (bottom - top) * scale,
  });
}

// Box sizes animate over ~0.2-0.3s (CSS transitions) — report both the
// starting size and, after the transition settles, the final size, rather
// than tracking every intermediate frame.
// Exported for spotify.js — that panel lives inside #mascot too and needs
// the same hot-rect bookkeeping.
export function reportHotRectSoon() {
  reportHotRect();
  setTimeout(reportHotRect, 320);
}

// Exported (as a function, not a live binding) for spotify.js to check
// before expanding its own hover panel — the two must never show at once.
export function isNoticeLocked() {
  return noticeLocked;
}

function setState(state) {
  if (noticeLocked) return;
  const applied = VALID_STATES.includes(state) ? state : "idle";
  console.log("mascot-state received:", state, "-> applying:", applied);
  document.body.className = `state-${applied}`;

  const notice = document.getElementById("notice");
  notice.textContent = NOTICE_TEXT[applied] || "";

  playSound(applied);

  if (revertTimer) {
    clearTimeout(revertTimer);
    revertTimer = null;
  }

  // turn_done is a momentary pulse, not a persistent state — settle back to idle after it plays.
  if (applied === "turn_done") {
    revertTimer = setTimeout(() => {
      document.body.className = "state-idle";
      reportHotRectSoon();
    }, 1100);
  }

  // Safety net: there's no reliable hook for "a human clicked Deny in the
  // interactive prompt" (PermissionDenied only fires for auto-mode/classifier
  // decisions, confirmed against the Claude Code binary) — so a denial never
  // sends an event at all. Falling back to Stop (end of turn) would leave
  // this expanded for a while, so keep the timeout short instead.
  if (applied === "waiting_permission" || applied === "waiting_input") {
    revertTimer = setTimeout(() => {
      document.body.className = "state-idle";
      reportHotRectSoon();
    }, 6000);
  }

  reportHotRectSoon();
}

let audioCtx = null;

function getAudioCtx() {
  if (!audioCtx) {
    audioCtx = new (window.AudioContext || window.webkitAudioContext)();
  }
  return audioCtx;
}

function beep({ freq, duration, gain = 0.05, delay = 0 }) {
  const ctx = getAudioCtx();
  if (ctx.state === "suspended") {
    ctx.resume();
  }
  const startAt = ctx.currentTime + delay;

  const oscillator = ctx.createOscillator();
  const gainNode = ctx.createGain();

  oscillator.type = "sine";
  oscillator.frequency.setValueAtTime(freq, startAt);

  gainNode.gain.setValueAtTime(0, startAt);
  gainNode.gain.linearRampToValueAtTime(gain, startAt + 0.01);
  gainNode.gain.linearRampToValueAtTime(0, startAt + duration);

  oscillator.connect(gainNode);
  gainNode.connect(ctx.destination);

  oscillator.start(startAt);
  oscillator.stop(startAt + duration + 0.02);
}

function playSound(state) {
  switch (state) {
    case "waiting_permission":
      // urgent double beep: Claude needs an explicit yes/no decision
      beep({ freq: 880, duration: 0.15, gain: 0.2 });
      beep({ freq: 880, duration: 0.15, gain: 0.2, delay: 0.2 });
      break;
    case "waiting_input":
      // softer single chime: Claude is idle, waiting for your next message
      beep({ freq: 660, duration: 0.2, gain: 0.15 });
      break;
    case "turn_done":
      // quiet tick: Claude just finished a turn
      beep({ freq: 440, duration: 0.12, gain: 0.1 });
      break;
    default:
      break;
  }
}

// These bypass setState()/mascot-state entirely — they're one-off notices
// pushed from github.rs (merge watcher / daily digest), each with its own
// dynamic text and dismiss timing, not part of the fixed hook-state machine.
// content may be a plain string (textContent) or a DOM node (appended as-is)
// — the merge notice needs an icon+text row, which textContent can't express.
function showTransientNotice(stateClass, content, revertAfterMs, onNotice) {
  if (revertTimer) {
    clearTimeout(revertTimer);
    revertTimer = null;
  }
  document.body.className = stateClass;
  const notice = document.getElementById("notice");
  notice.innerHTML = "";
  if (content instanceof Node) {
    notice.appendChild(content);
  } else {
    notice.textContent = content;
  }
  notice.onclick = onNotice || null;
  reportHotRectSoon();

  revertTimer = setTimeout(() => {
    document.body.className = "state-idle";
    notice.onclick = null;
    reportHotRectSoon();
  }, revertAfterMs);
}

function buildIconSvg(pathD, { stroke = false } = {}) {
  const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  icon.setAttribute("viewBox", "0 0 24 24");
  if (stroke) {
    icon.setAttribute("fill", "none");
    icon.setAttribute("stroke", "currentColor");
    icon.setAttribute("stroke-width", "2");
    icon.setAttribute("stroke-linecap", "round");
    icon.setAttribute("stroke-linejoin", "round");
  } else {
    icon.setAttribute("fill", "currentColor");
  }
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", pathD);
  icon.appendChild(path);
  return icon;
}

// GitHub's own "merged" octicon (fill-based, 16x16 viewBox) — matches the
// purple merge icon GitHub shows on a merged PR, instead of a generic
// feather-style stroke icon.
const MERGE_ICON_PATH =
  "M5.45 5.154A4.25 4.25 0 0 0 9.25 7.5h1.378a2.251 2.251 0 1 1 0 1.5H9.25A5.734 5.734 0 0 1 5 7.123v3.505a2.25 2.25 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.95-.218ZM4.25 13.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5ZM3.5 3.75a.75.75 0 1 1 1.5 0 .75.75 0 0 1-1.5 0Zm8.5.75a.75.75 0 1 0 0-1.5.75.75 0 0 0 0 1.5Z";

function buildMergeIcon() {
  const icon = buildIconSvg(MERGE_ICON_PATH);
  icon.setAttribute("viewBox", "0 0 16 16");
  return icon;
}

function showGithubMergeNotice({ title, url }) {
  if (noticeLocked) return; // the pinned digest card takes priority

  const row = document.createElement("div");
  row.className = "merge-notice";
  row.appendChild(buildMergeIcon());
  const text = document.createElement("span");
  text.textContent = `"${title}" merged`;
  row.appendChild(text);

  showTransientNotice("state-github_merge", row, 8000, () => {
    if (url) window.__TAURI__.core.invoke("open_in_browser", { url });
  });
  beep({ freq: 720, duration: 0.14, gain: 0.18 });
  beep({ freq: 960, duration: 0.18, gain: 0.18, delay: 0.15 });
}

// Same one-line transient banner as the merge notice (see issue_watcher.rs
// for what actually triggers each kind) — just a different icon/text per
// event kind rather than a whole new box treatment.
function buildStrokeIcon(paths, viewBox = "0 0 24 24") {
  const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  icon.setAttribute("viewBox", viewBox);
  icon.setAttribute("fill", "none");
  icon.setAttribute("stroke", "currentColor");
  icon.setAttribute("stroke-width", "2");
  icon.setAttribute("stroke-linecap", "round");
  icon.setAttribute("stroke-linejoin", "round");
  for (const d of paths) {
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", d);
    icon.appendChild(path);
  }
  return icon;
}

const ISSUE_ICONS = {
  assigned: {
    paths: ["M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2", "M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z"],
    color: "#58a6ff",
  },
  comment: {
    paths: [
      "M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5Z",
    ],
    color: "#8b949e",
  },
  closed: { paths: ["M22 11.08V12a10 10 0 1 1-5.93-9.14", "M22 4 12 14.01l-3-3"], color: "#a371f7" },
  reopened: { paths: ["M1 4v6h6", "M3.51 15a9 9 0 1 0 2.13-9.36L1 10"], color: "#3fb950" },
};

const ISSUE_KIND_TEXT = {
  assigned: (title, number) => `Assigned: "${title}"${number ? ` #${number}` : ""}`,
  comment: (title, number) => `New comment: "${title}"${number ? ` #${number}` : ""}`,
  closed: (title, number) => `Closed: "${title}"${number ? ` #${number}` : ""}`,
  reopened: (title, number) => `Reopened: "${title}"${number ? ` #${number}` : ""}`,
};

function showGithubIssueNotice({ kind, title, number, url, detail, repo }) {
  if (noticeLocked) return; // the pinned digest card takes priority

  const spec = ISSUE_ICONS[kind] || ISSUE_ICONS.comment;
  const row = document.createElement("div");
  row.className = "merge-notice";
  if (detail) row.title = detail; // hover tooltip — e.g. who commented and a snippet
  const icon = buildStrokeIcon(spec.paths);
  icon.style.color = spec.color;
  row.appendChild(icon);

  // Two lines — a small repo label above the action text — since with
  // several repos in play "Assigned: \"title\"" alone doesn't say WHERE.
  const textWrap = document.createElement("div");
  textWrap.className = "issue-notice-text";
  if (repo) {
    const repoLine = document.createElement("span");
    repoLine.className = "issue-notice-repo";
    repoLine.textContent = repo;
    textWrap.appendChild(repoLine);
  }
  const actionLine = document.createElement("span");
  actionLine.className = "issue-notice-action";
  actionLine.textContent = (ISSUE_KIND_TEXT[kind] || ISSUE_KIND_TEXT.comment)(title, number);
  textWrap.appendChild(actionLine);
  row.appendChild(textWrap);

  showTransientNotice("state-github_merge", row, 8000, () => {
    if (url) window.__TAURI__.core.invoke("open_in_browser", { url });
  });
  beep({ freq: 720, duration: 0.14, gain: 0.18 });
  beep({ freq: 960, duration: 0.18, gain: 0.18, delay: 0.15 });
}

const GITHUB_ICON_PATH =
  "M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.91.57.1.79-.25.79-.55 0-.27-.01-1.16-.02-2.11-3.2.7-3.88-1.36-3.88-1.36-.52-1.33-1.28-1.68-1.28-1.68-1.04-.71.08-.7.08-.7 1.15.08 1.76 1.18 1.76 1.18 1.03 1.76 2.7 1.25 3.36.96.1-.75.4-1.25.73-1.54-2.55-.29-5.23-1.28-5.23-5.68 0-1.25.45-2.28 1.18-3.08-.12-.29-.51-1.46.11-3.04 0 0 .96-.31 3.15 1.18a10.9 10.9 0 015.74 0c2.19-1.49 3.15-1.18 3.15-1.18.62 1.58.23 2.75.11 3.04.74.8 1.18 1.83 1.18 3.08 0 4.41-2.69 5.38-5.25 5.67.42.36.78 1.07.78 2.16 0 1.56-.01 2.82-.01 3.2 0 .31.21.66.8.55A10.99 10.99 0 0023.5 12C23.5 5.65 18.35.5 12 .5z";

// Categorizes an "Attention" line by its content so the dot color carries
// meaning at a glance (red = someone's blocking you, green = ready to go,
// gray = just old) instead of every row looking identical.
function digestRowCategory(line) {
  if (/changes requested/i.test(line)) return "danger";
  if (/ready to merge|approved/i.test(line)) return "ok";
  if (/review\/approval/i.test(line)) return "info";
  if (/stale/i.test(line)) return "stale";
  return "action";
}

// Renders the Workload line as a small stat header, then each "Attention"
// item as its own row with a category-colored dot — a plain text blob was
// hard to scan at a glance.
function renderDigestList(container, summary) {
  container.innerHTML = "";

  const header = document.createElement("div");
  header.className = "digest-header";
  const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  icon.setAttribute("viewBox", "0 0 24 24");
  icon.setAttribute("fill", "currentColor");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", GITHUB_ICON_PATH);
  icon.appendChild(path);
  const title = document.createElement("span");
  title.textContent = "GitHub Digest";
  header.appendChild(icon);
  header.appendChild(title);
  container.appendChild(header);

  const lines = summary
    .split("\n")
    .map((l) => l.replace(/^[-•]\s*/, "").trim())
    .filter(Boolean);

  const workloadLine = lines.find((l) => /^workload:/i.test(l));
  const attentionLines = lines
    .filter((l) => /^attention:/i.test(l))
    .map((l) => l.replace(/^attention:\s*/i, ""));

  if (workloadLine) {
    const stat = document.createElement("div");
    stat.className = "digest-workload";
    stat.textContent = workloadLine.replace(/^workload:\s*/i, "");
    container.appendChild(stat);
  }

  const list = document.createElement("div");
  list.className = "digest-list";

  for (const line of attentionLines) {
    const row = document.createElement("div");
    row.className = `digest-row digest-row--${digestRowCategory(line)}`;

    const dot = document.createElement("span");
    dot.className = "digest-dot";
    const text = document.createElement("span");
    text.textContent = line;

    row.appendChild(dot);
    row.appendChild(text);
    list.appendChild(row);
  }
  container.appendChild(list);
}

// Unlike every other notice, this one has NO auto-revert timer — it stays
// up until the user clicks the dedicated Close button, and noticeLocked
// blocks every other state change (setState/merge notice) from touching it
// in the meantime.
function showGithubDigestNotice(summary) {
  if (revertTimer) {
    clearTimeout(revertTimer);
    revertTimer = null;
  }
  noticeLocked = true;
  document.body.className = "state-github_digest";
  renderDigestList(document.getElementById("notice"), summary);
  beep({ freq: 600, duration: 0.15, gain: 0.15 });
  reportHotRectSoon();
}

function closeGithubDigestNotice() {
  noticeLocked = false;
  document.body.className = "state-idle";
  reportHotRectSoon();
}

function openNotepad() {
  window.__TAURI__.core.invoke("open_notepad");
}

function openSettings() {
  window.__TAURI__.core.invoke("open_settings");
}

function openTerminal() {
  window.__TAURI__.core.invoke("open_terminal");
}

// Single-agent app — Claude Code only (see SETUP.md for the hook wiring
// this depends on). Still an object (not a bare string) since
// buildAgentSessionsList indexes it by whatever's in SessionSummary.agent.
const AGENT_LABELS = {
  claude: "Claude",
};

// terminalN -> "Terminal N" (plain "terminal" is slot 1) — used by both the
// permission card header (implicitly, via session_id) and the "show all
// agents" list rows. A null session_id means the hook fired with no
// resolvable WIDGET_TERMINAL_LABEL (Claude running outside this app's pooled
// terminals) — there's no window to name or focus, so say so instead of
// guessing "Terminal 1".
function terminalDisplayName(sessionId) {
  if (!sessionId) return "External session";
  const match = /(\d+)$/.exec(sessionId);
  return match ? `Terminal ${match[1]}` : "Terminal 1";
}

// Pinned open like the digest card (see noticeLocked) — each pooled terminal
// (terminal.rs) can have its own Claude Code session independently blocked on
// a decision, so this is a LIST of pending requests, one .permission-card per
// entry, stacked vertically instead of shown one-at-a-time. Keyed by
// request_id (not session_id — a request with no resolvable terminal label
// has session_id: null, and two such requests must still be told apart).
let pendingPermissions = [];

// Toggled by the "Show all agents" button — independent of pendingPermissions
// so the expanded list survives across re-renders (e.g. a new prompt arriving
// from another terminal) until the user explicitly collapses it again.
let allSessionsVisible = false;

// tool_name/tool_input come straight from Claude Code's own PermissionRequest
// hook payload (see terminal.rs's record_permission_request) — real
// structured data, not text scraped off the rendered terminal screen, so
// this renders each known tool shape directly instead of trying to parse
// meaning out of a pre-rendered preview.
function buildCodeBlock(text, extraClass = "") {
  const box = document.createElement("div");
  box.className = `permission-code ${extraClass}`.trim();
  box.textContent = text;
  return box;
}

function buildFilePathLine(path) {
  const line = document.createElement("div");
  line.className = "permission-file-path";
  line.textContent = path;
  return line;
}

// Simple line-level before/after (not a real LCS diff) — old_string/
// new_string are exact, so even a flat "every old line removed, every new
// line added" rendering is already far more accurate than the old
// screen-scraped preview ever was, using the same add/remove row styling.
function buildEditDiff(oldString, newString) {
  const box = document.createElement("div");
  box.className = "permission-preview";
  for (const line of (oldString || "").split("\n")) {
    box.appendChild(buildDiffRow("-", line));
  }
  for (const line of (newString || "").split("\n")) {
    box.appendChild(buildDiffRow("+", line));
  }
  return box;
}

function buildDiffRow(marker, content) {
  const row = document.createElement("div");
  row.className = `diff-row ${marker === "+" ? "diff-row--add" : "diff-row--remove"}`;
  const markerCol = document.createElement("span");
  markerCol.className = "diff-marker";
  markerCol.textContent = marker;
  row.appendChild(markerCol);
  const code = document.createElement("span");
  code.className = "diff-code";
  code.textContent = content;
  row.appendChild(code);
  return row;
}

// Per-tool renderers — each returns an array of DOM nodes to append into the
// card body. Falls back to pretty-printed JSON for any tool shape not
// specifically handled below (new/renamed tools, MCP tools, etc.) so nothing
// ever renders as a blank card.
const TOOL_RENDERERS = {
  Bash(input) {
    const nodes = [buildCodeBlock(input.command || "")];
    if (input.description) {
      const desc = document.createElement("div");
      desc.className = "permission-tool-desc";
      desc.textContent = input.description;
      nodes.unshift(desc);
    }
    return nodes;
  },
  Write(input) {
    return [buildFilePathLine(input.file_path || ""), buildCodeBlock(input.content || "")];
  },
  Edit(input) {
    return [buildFilePathLine(input.file_path || ""), buildEditDiff(input.old_string, input.new_string)];
  },
  MultiEdit(input) {
    const nodes = [buildFilePathLine(input.file_path || "")];
    for (const edit of input.edits || []) {
      nodes.push(buildEditDiff(edit.old_string, edit.new_string));
    }
    return nodes;
  },
  Read(input) {
    return [buildFilePathLine(input.file_path || "")];
  },
  Glob(input) {
    return [buildCodeBlock(`${input.pattern || ""}${input.path ? "  in " + input.path : ""}`)];
  },
  Grep(input) {
    return [buildCodeBlock(`${input.pattern || ""}${input.path ? "  in " + input.path : ""}`)];
  },
  WebFetch(input) {
    return [buildCodeBlock(input.url || "")];
  },
  Task(input) {
    const nodes = [];
    if (input.subagent_type) nodes.push(buildCodeBlock(input.subagent_type, "permission-tool-desc"));
    if (input.description) nodes.push(buildCodeBlock(input.description));
    return nodes;
  },
};

function renderToolInput(toolName, toolInput) {
  const input = toolInput && typeof toolInput === "object" ? toolInput : {};
  const renderer = TOOL_RENDERERS[toolName];
  if (renderer) return renderer(input);
  return [buildCodeBlock(JSON.stringify(toolInput, null, 2))];
}

// Renders Claude Code's AskUserQuestion tool the same way its own TUI does —
// one or more questions, each with tappable option chips — instead of a
// plain Approve/Deny card. Answers are collected client-side and sent back
// as this same PermissionRequest hook's `updatedInput` (see server.rs's
// resolve_decision / respond_permission), the same mechanism AgentGlance
// uses on macOS: no keystrokes are simulated, Claude Code receives the
// answer as if the user had picked it in its own prompt.
function buildQuestionUI(req) {
  const wrap = document.createElement("div");
  wrap.className = "question-block";
  const selections = new Map(); // question text -> Set of selected option labels

  for (const q of req.questions) {
    const qKey = q.question || q.header || "";
    selections.set(qKey, new Set());

    const qEl = document.createElement("div");
    qEl.className = "question-item";
    if (q.header) {
      const qHeader = document.createElement("div");
      qHeader.className = "question-header";
      qHeader.textContent = q.header;
      qEl.appendChild(qHeader);
    }
    const qText = document.createElement("div");
    qText.className = "question-text";
    qText.textContent = q.question || "";
    qEl.appendChild(qText);

    const optsWrap = document.createElement("div");
    optsWrap.className = "question-options";
    for (const opt of q.options || []) {
      const chip = document.createElement("button");
      chip.type = "button";
      chip.className = "question-chip";
      chip.textContent = opt.label;
      chip.addEventListener("click", () => {
        const set = selections.get(qKey);
        if (q.multiSelect) {
          if (set.has(opt.label)) {
            set.delete(opt.label);
            chip.classList.remove("selected");
          } else {
            set.add(opt.label);
            chip.classList.add("selected");
          }
        } else {
          set.clear();
          set.add(opt.label);
          for (const sibling of optsWrap.querySelectorAll(".question-chip")) {
            sibling.classList.remove("selected");
          }
          chip.classList.add("selected");
        }
      });
      optsWrap.appendChild(chip);
    }
    qEl.appendChild(optsWrap);
    wrap.appendChild(qEl);
  }

  const submitBtn = document.createElement("button");
  submitBtn.className = "permission-btn permission-btn--approve question-submit";
  submitBtn.textContent = "Submit";
  submitBtn.addEventListener("click", () => {
    const answers = {};
    for (const [qKey, set] of selections) {
      if (set.size === 0) return; // every question needs at least one pick before submitting
      answers[qKey] = set.size === 1 ? [...set][0] : [...set];
    }
    resolveQuestionPermission(req.request_id, req.questions, answers);
  });
  wrap.appendChild(submitBtn);

  return wrap;
}

function renderPermissionList() {
  const notice = document.getElementById("notice");
  notice.innerHTML = "";

  for (const req of pendingPermissions) {
    const card = document.createElement("div");
    card.className = "permission-card";

    const header = document.createElement("div");
    header.className = "permission-header";

    const badge = document.createElement("span");
    badge.className = "agent-badge agent-badge--claude";
    badge.textContent = "Claude";
    header.appendChild(badge);

    const source = document.createElement("span");
    source.className = "permission-header-source";
    source.textContent = terminalDisplayName(req.session_id);
    header.appendChild(source);

    const text = document.createElement("span");
    text.className = "permission-header-text";
    text.textContent = req.questions ? "asked" : "needs approval";
    header.appendChild(text);

    // For requests with no resolvable terminal (session_id null — Claude
    // running outside this app's pooled terminals, see terminalDisplayName),
    // there's no automatic way to tell "still genuinely pending" apart from
    // "the hook that asked already gave up client-side and nobody will ever
    // answer this" (AgentGlance solves the equivalent case by auto-flushing
    // on the session's next forward-progress hook; this app doesn't track
    // those events or attempt session correlation for unlabeled requests).
    // Dismiss is the manual equivalent — discards the card and answers
    // "deny" so the connection doesn't sit open forever either way.
    const dismissBtn = document.createElement("button");
    dismissBtn.className = "permission-dismiss-btn";
    dismissBtn.textContent = "×";
    dismissBtn.title = "Dismiss (denies the request)";
    dismissBtn.addEventListener("click", () => {
      window.__TAURI__.core
        .invoke("dismiss_permission", { requestId: req.request_id })
        .finally(() => finishResolvedPermission(req.request_id));
    });
    header.appendChild(dismissBtn);

    card.appendChild(header);

    if (req.questions) {
      card.appendChild(buildQuestionUI(req));
    } else {
      const tool = document.createElement("div");
      tool.className = "permission-tool";
      tool.textContent = req.tool_name || "Unknown";
      card.appendChild(tool);

      for (const node of renderToolInput(req.tool_name, req.tool_input)) {
        card.appendChild(node);
      }

      const actions = document.createElement("div");
      actions.className = "permission-actions";
      const approveBtn = document.createElement("button");
      approveBtn.className = "permission-btn permission-btn--approve";
      approveBtn.textContent = "Approve";
      approveBtn.addEventListener("click", () => resolveAgentPermission(req.request_id, true));
      const denyBtn = document.createElement("button");
      denyBtn.className = "permission-btn permission-btn--deny";
      denyBtn.textContent = "Deny";
      denyBtn.addEventListener("click", () => resolveAgentPermission(req.request_id, false));
      actions.appendChild(approveBtn);
      actions.appendChild(denyBtn);
      card.appendChild(actions);
    }

    notice.appendChild(card);
  }

  appendAgentSessionsSection(notice);
}

// "Show all agents" — expands into a list of EVERY pooled terminal that's
// been opened this run (not just ones with a pending decision), each row
// showing its detected agent CLI + last visible activity line, click to
// focus that terminal window. Fetched fresh from the backend each time it's
// expanded rather than kept in sync live — this is a glanceable summary, not
// a real-time dashboard.
function appendAgentSessionsSection(notice) {
  const toggleBtn = document.createElement("button");
  toggleBtn.className = "show-all-agents-btn";
  toggleBtn.textContent = allSessionsVisible ? "Hide all agents" : "Show all agents";
  toggleBtn.addEventListener("click", () => {
    allSessionsVisible = !allSessionsVisible;
    renderPermissionList();
    reportHotRectSoon();
  });
  notice.appendChild(toggleBtn);

  if (!allSessionsVisible) return;

  const placeholder = document.createElement("div");
  placeholder.className = "agent-sessions-list";
  const loading = document.createElement("div");
  loading.className = "agent-session-empty";
  loading.textContent = "Loading…";
  placeholder.appendChild(loading);
  notice.appendChild(placeholder);

  window.__TAURI__.core
    .invoke("list_agent_sessions")
    .then((sessions) => {
      if (!allSessionsVisible || !placeholder.isConnected) return; // collapsed/re-rendered before this resolved
      placeholder.replaceWith(buildAgentSessionsList(sessions));
      reportHotRectSoon();
    })
    .catch(() => {
      if (!allSessionsVisible || !placeholder.isConnected) return;
      loading.textContent = "Couldn't load agent sessions";
    });
}

function buildAgentSessionsList(sessions) {
  const list = document.createElement("div");
  list.className = "agent-sessions-list";

  if (sessions.length === 0) {
    const empty = document.createElement("div");
    empty.className = "agent-session-empty";
    empty.textContent = "No agent terminals opened yet";
    list.appendChild(empty);
    return list;
  }

  for (const s of sessions) {
    const row = document.createElement("div");
    row.className = "agent-session-row";
    row.addEventListener("click", () => {
      window.__TAURI__.core.invoke("focus_terminal_session", { label: s.session_id });
    });

    const badge = document.createElement("span");
    badge.className = `agent-badge agent-badge--${s.agent || "idle"}`;
    badge.textContent = s.agent ? AGENT_LABELS[s.agent] || s.agent : "Idle";
    row.appendChild(badge);

    const info = document.createElement("div");
    info.className = "agent-session-info";
    const label = document.createElement("span");
    label.className = "agent-session-label";
    label.textContent = terminalDisplayName(s.session_id);
    info.appendChild(label);
    const activity = document.createElement("span");
    activity.className = "agent-session-activity";
    activity.textContent = s.activity || "No activity yet";
    info.appendChild(activity);
    row.appendChild(info);

    if (s.has_pending) {
      const dot = document.createElement("span");
      dot.className = "agent-session-pending-dot";
      dot.title = "Waiting for your approval";
      row.appendChild(dot);
    }

    list.appendChild(row);
  }
  return list;
}

function showAgentPermissionNotice({ session_id, request_id, tool_name, tool_input, questions }) {
  if (revertTimer) {
    clearTimeout(revertTimer);
    revertTimer = null;
  }
  noticeLocked = true;
  document.body.className = "state-agent_permission";

  const existing = pendingPermissions.findIndex((p) => p.request_id === request_id);
  const entry = { session_id, request_id, tool_name, tool_input, questions };
  if (existing >= 0) {
    pendingPermissions[existing] = entry;
  } else {
    pendingPermissions.push(entry);
  }

  renderPermissionList();
  beep({ freq: 880, duration: 0.15, gain: 0.2 });
  beep({ freq: 880, duration: 0.15, gain: 0.2, delay: 0.2 });
  reportHotRectSoon();
}

// Shared cleanup after any pending request resolves — pulled out since both
// a plain Approve/Deny and an AskUserQuestion Submit need the exact same
// card-list bookkeeping afterwards, just with a different Tauri command call.
function finishResolvedPermission(requestId) {
  pendingPermissions = pendingPermissions.filter((p) => p.request_id !== requestId);
  if (pendingPermissions.length === 0) {
    noticeLocked = false;
    allSessionsVisible = false;
    document.body.className = "state-idle";
  } else {
    renderPermissionList();
  }
  reportHotRectSoon();
  computeAmbient(); // don't wait up to 5s for "waiting" to clear once this resolves
}

function resolveAgentPermission(requestId, approve) {
  window.__TAURI__.core
    .invoke("respond_permission", { requestId, approve, updatedInput: null })
    .finally(() => finishResolvedPermission(requestId));
}

// AskUserQuestion answers travel back as the SAME PermissionRequest hook's
// updatedInput (server.rs's resolve_decision) — echoing the original
// `questions` array back alongside `answers` mirrors the shape Claude Code's
// own AskUserQuestion tool expects when a hook supplies the answer, matching
// AgentGlance's proven contract rather than guessing a new one.
function resolveQuestionPermission(requestId, questions, answers) {
  window.__TAURI__.core
    .invoke("respond_permission", { requestId, approve: true, updatedInput: { questions, answers } })
    .finally(() => finishResolvedPermission(requestId));
}


function hideQuickMenu() {
  document.getElementById("quick-menu").classList.remove("visible");
  reportHotRectSoon();
}

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
      window.__TAURI__.window.getCurrentWindow().startDragging();
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

  document.getElementById("quick-menu-notepad").addEventListener("click", () => {
    openNotepad();
    hideQuickMenu();
  });

  document.getElementById("quick-menu-settings").addEventListener("click", () => {
    openSettings();
    hideQuickMenu();
  });

  // System-tray-style minimize, not app.exit() — background watchers (GitHub
  // polling, terminal sessions) keep running; bring it back via the tray icon.
  document.getElementById("quick-menu-hide").addEventListener("click", () => {
    window.__TAURI__.core.invoke("hide_mascot");
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
