// The agent permission card: renders a Claude Code PreToolUse/AskUserQuestion
// hook payload (see agent/server.rs) into an approve/deny — or multiple-choice —
// prompt, one card per waiting request. Pinned open via the notice lock until
// every pending request is resolved.
import { invoke } from "../../shared/tauri.js";
import { beep } from "../lib/sound.js";
import { reportHotRectSoon } from "../lib/hotrect.js";
import { clearRevertTimer, closePinnedCard, lockNotice } from "../notice/notice.js";
import { refreshAmbient } from "../pip/signals.js";

// Whatever the hook chose to call itself, for the card header. `?label=` in the
// hook URL is optional and free-form: it used to name one of this app's own
// terminal windows, and those are gone, so most requests now arrive without one.
function sourceDisplayName(sessionId) {
  return sessionId || "Claude Code";
}

// Pinned open like the digest card (see noticeLocked) — several Claude Code
// sessions can be blocked on a decision at once, so this is a LIST of pending
// requests, one .permission-card per entry, stacked vertically instead of shown
// one-at-a-time. Keyed by request_id, not session_id: most requests carry no
// label at all, and two unlabeled ones must still be told apart.
let pendingPermissions = [];

// tool_name/tool_input come straight from Claude Code's own PermissionRequest
// hook payload (see agent/terminal.rs's record_permission_request) — real
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
// as this same PermissionRequest hook's `updatedInput` (see agent/server.rs's
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
    source.textContent = sourceDisplayName(req.session_id);
    header.appendChild(source);

    const text = document.createElement("span");
    text.className = "permission-header-text";
    text.textContent = req.questions ? "asked" : "needs approval";
    header.appendChild(text);

    // There's no automatic way to tell "still genuinely pending" apart from
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
      invoke("dismiss_permission", { requestId: req.request_id })
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
}

// There used to be a "Show all agents" section here, listing every terminal this
// app had opened with its detected CLI and last visible activity line, click to
// focus that window. It was only ever able to show sessions running inside the
// app's own terminal pool — which no longer exists, because the hooks report
// from wherever Claude actually runs. Nothing replaced it: the useful half (a
// request is waiting) is the card itself, and "which window is it in" is not
// something we can answer for a terminal we didn't launch.

export function showAgentPermissionNotice({ session_id, request_id, tool_name, tool_input, questions }) {
  clearRevertTimer();
  lockNotice();
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
    // Not a plain unlockNotice(): this card preempts whatever was on screen, so
    // on the way out it has to hand the area to anything that queued up behind
    // it rather than just going idle (see closePinnedCard).
    closePinnedCard();
  } else {
    renderPermissionList();
  }
  reportHotRectSoon();
  refreshAmbient(); // don't wait a whole poll interval for "waiting" to clear once this resolves
}

function resolveAgentPermission(requestId, approve) {
  invoke("respond_permission", { requestId, approve, updatedInput: null })
    .finally(() => finishResolvedPermission(requestId));
}

// AskUserQuestion answers travel back as the SAME PermissionRequest hook's
// updatedInput (agent/server.rs's resolve_decision) — echoing the original
// `questions` array back alongside `answers` mirrors the shape Claude Code's
// own AskUserQuestion tool expects when a hook supplies the answer, matching
// AgentGlance's proven contract rather than guessing a new one.
function resolveQuestionPermission(requestId, questions, answers) {
  invoke("respond_permission", { requestId, approve: true, updatedInput: { questions, answers } })
    .finally(() => finishResolvedPermission(requestId));
}
