// The mascot's notice/state machine: which body state class is showing, the
// auto-revert timer behind transient notices, and the lock that lets a pinned
// card (GitHub digest, agent permission) block every other state change.
//
// revertTimer and noticeLocked are deliberately private with accessors — the
// github and agent notice modules need to clear the timer and take/release the
// lock, and an ES module import cannot be assigned to.
import { reportHotRectSoon } from "../lib/hotrect.js";
import { playSound } from "../lib/sound.js";

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

export function isNoticeLocked() {
  return noticeLocked;
}

export function setState(state) {
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

export function showTransientNotice(stateClass, content, revertAfterMs, onNotice) {
  clearRevertTimer();
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

export function clearRevertTimer() {
  if (revertTimer) {
    clearTimeout(revertTimer);
    revertTimer = null;
  }
  // Cancelling the revert always means "I'm taking over the notice area", so the
  // outgoing notice's click target has to go with it. Without this, a GitHub
  // notice's open-the-link handler stayed live on #notice underneath whatever
  // replaced it — and since the permission cards render *inside* #notice, every
  // Approve click bubbled up and reopened that link.
  const notice = document.getElementById("notice");
  if (notice) notice.onclick = null;
}

export function lockNotice() {
  noticeLocked = true;
}

export function unlockNotice() {
  noticeLocked = false;
}
