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

// Private now that closePinnedCard is the only way out of a pinned card —
// releasing the lock without draining the queue would strand whatever is
// waiting behind it, which is exactly the bug the queue exists to fix.
function unlockNotice() {
  noticeLocked = false;
}

// Pinned cards waiting for the screen. There is exactly one notice area, and a
// pinned card holds it until the user closes it — so before this queue existed,
// a second card arriving while the first was up hit the isNoticeLocked() guard
// and was dropped on the floor. That is not hypothetical: the GitHub digest and
// the morning mail brief are both scheduled for the morning, and a machine that
// was off at both their hours runs them minutes apart on the same catch-up.
const pinnedQueue = [];

// A card that has waited this long has stopped being news. Showing a "good
// morning, here is your day" card at 6pm because nobody closed the one in front
// of it is worse than not showing it at all.
const QUEUE_STALE_MS = 2 * 60 * 60 * 1000;

function present(card) {
  clearRevertTimer();
  lockNotice();
  document.body.className = card.stateClass;
  const notice = document.getElementById("notice");
  card.render(notice);
  // A card always opens at its top. Emptying and refilling a scrollable element
  // does NOT reset its scroll position: the browser's scroll anchoring sees
  // content appear above where it was looking and compensates by scrolling down
  // to match. Measured on the morning brief — the second time it opened, it came
  // up 163px down, with its own header off-screen above.
  notice.scrollTop = 0;
  reportHotRectSoon();
}

/// Shows a pinned card, or parks it until the screen is free.
///
/// `render` is given the notice element and is responsible for its own contents
/// and its own sound — a queued card must not beep when it was queued, only when
/// it actually appears.
export function showPinnedCard(stateClass, render) {
  const card = { stateClass, render, queuedAt: Date.now() };
  if (noticeLocked) {
    pinnedQueue.push(card);
    return false;
  }
  present(card);
  return true;
}

/// Closes whatever pinned card is showing and hands the screen to the next one.
///
/// Every path that dismisses a pinned card goes through here, including the
/// agent permission card — which takes the notice area by preemption rather than
/// by queueing (a held-open tool call cannot wait behind a digest), but still has
/// to drain the queue on the way out or a card parked behind it would never
/// appear at all.
export function closePinnedCard() {
  unlockNotice();

  const now = Date.now();
  while (pinnedQueue.length > 0) {
    const next = pinnedQueue.shift();
    if (now - next.queuedAt > QUEUE_STALE_MS) continue;
    present(next);
    return;
  }

  document.body.className = "state-idle";
  reportHotRectSoon();
}
