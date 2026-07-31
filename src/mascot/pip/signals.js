// Wires real, already-available backend signals to the Pip sprite's mood
// (see pipstate.js) — this module never touches body.className / the
// existing notice system in main.js, it only decides what Pip's ambient
// pose + occasional event pulse should be. Two independent halves:
//
//   1. An ambient poll loop (every 5s): terminal sessions, audio/mic state,
//      battery, OS-wide idle time -> setAmbient(name).
//   2. Passive listeners on events the backend already emits (merge, CI,
//      review-requested, issue updates, digest ready) -> pushEvent(name).
//
// `gaming` and `juggling` are intentionally not wired here; both still exist in
// pip.js/pip.css for manual use. `gaming` has no reliable signal. `juggling`
// (two or more Claude sessions at once) used to be counted from this app's own
// pooled terminals, which are gone — the hooks carry a session_id that could
// bring it back for real terminals, but that isn't wired yet.
// `working`/`reviewing`/`writing` (splitting "coding" by the active tool's
// name) were considered and rejected too: tool_name changes multiple times
// per turn, so this would flicker between moods every tool call instead of
// reading as one continuous ambient pose — "coding" stays the single
// umbrella state for any real tool activity.
import { setAmbient, pushEvent } from "./pipstate.js";

import { invoke, listen } from "../../shared/tauri.js";
// Every threshold below is a setting rather than a constant — how long "away
// from the keyboard" means, which apps count as a call, when a battery is low:
// none of that is the same for two people. See src-tauri/src/tunables.rs.
import { loadTunables, t, tNames } from "../../shared/tunables.js";
// The voice assistant holds the microphone open the whole time it is listening
// for the wake word, which trips the same is_mic_capture_active signal a call
// does — see the onCall calculation below.
import { isOpen as voiceHoldsMic } from "../voice/mic.js";

let spotifyPlaying = false;
// Timestamp of the last Claude Code hook event of ANY kind (see agent/server.rs's
// claude-hook-activity emit). This is now the ONLY source of "coding": the app
// used to also infer it from its own pooled terminals by reading their rendered
// screens, but those are gone, and the hooks fire from whichever terminal the
// user actually runs Claude in — which is the case that matters.
let lastHookActivitySince = null;
const HOOK_ACTIVITY_CODING_WINDOW_MS = 30000;
// Timestamp of the last UserPromptSubmit hook (see agent/server.rs/SETUP.md) — the
// gap between the human submitting a prompt and Claude either calling a tool
// (which flips recentHookActivity/"coding" on, taking priority below) or
// just replying with plain text and going quiet again. Deliberately a short,
// self-expiring window rather than something explicitly cleared by Stop —
// that gap is normally a few seconds at most, so a fixed timeout is enough
// padding without needing a second hook wired just to cancel it early.
let lastPromptSubmitSince = null;
const THINKING_WINDOW_MS = 25000;
// Counts backend events (merge/CI/review/issue) that happened while ambient
// was 'sleeping' — surfaced once as a single 'welcome_back' pulse the moment
// the user is active again, instead of several separate pulses landing the
// instant the pill wakes up.
let eventsSinceSleep = 0;
let wasSleeping = false;

export async function computeAmbient() {
  // Awaited here rather than only at startup, because each listener below is
  // another entry point into this function and any of them can fire before the
  // first load resolves. Memoized, so this costs nothing after the first call.
  await loadTunables();

  const [pending, audioSessions, micMuted, micCaptureActive, power, idleSecs] = await Promise.all([
    // Straight from the held-open PermissionRequest hooks (agent/server.rs), so
    // it counts a request from any terminal. It used to be a flag on one of this
    // app's own terminal sessions, keyed by WIDGET_TERMINAL_LABEL — which meant
    // a permission request from the user's own shell showed a card but never
    // moved the mascot, because there was no session of ours to flag.
    invoke("pending_permissions").catch(() => ({ count: 0, oldest_secs: 0 })),
    invoke("list_audio_sessions").catch(() => []),
    invoke("system_mic_get_muted").catch(() => true),
    // Actual mic-capture activity (Windows' AudioSessionStateActive on the
    // default INPUT device), not just "an app with a known call-app name is
    // making sound" — the old app-name-only check never fired for a
    // browser-based call (Google Meet et al show up as chrome.exe/
    // msedge.exe, indistinguishable by name from that browser just having
    // some unrelated tab open). This generalizes past any specific app.
    invoke("is_mic_capture_active").catch(() => false),
    invoke("get_power_status").catch(() => null),
    invoke("get_idle_seconds").catch(() => 0),
  ]);

  const audioNames = audioSessions.map((s) => (s.name || "").toLowerCase());
  const matchesAny = (patterns) => audioNames.some((name) => patterns.some((p) => name.includes(p)));
  // While the voice assistant is on, micCaptureActive carries no information —
  // it is true continuously because *we* are the app capturing, so trusting it
  // would pin the mascot to "call" for as long as the wake word is armed.
  // Falling back to the app-name heuristic means a browser-based call is missed
  // while voice is enabled, which is the lesser of the two: a permanently wrong
  // ambient state is worse than one missed heuristic.
  const foreignCapture = micCaptureActive && !voiceHoldsMic();
  const onCall = !micMuted && (foreignCapture || matchesAny(tNames("presence.call_apps")));
  const streaming = matchesAny(tNames("presence.stream_apps"));

  const forgotten = pending.count > 0 && pending.oldest_secs > t("presence.forgotten_secs");

  const lowPower =
    !!power && power.has_battery && !power.charging && power.percent <= t("presence.low_battery_percent");
  const sleeping = idleSecs > t("presence.sleep_secs");
  // Checked AFTER sleeping in the chain below so a long-idle machine settles
  // into the deeper "sleeping" pose instead of getting stuck on "break" — a
  // machine idle past the sleep threshold is also, technically, past the break
  // threshold, so the ordering (not a range check here) is what actually
  // stratifies the two — which also means the ordering, not the numbers, decides
  // what happens if a user sets the sleep threshold *below* the break one: sleep
  // is tested first, so the break pose simply never appears. Harmless, and said
  // plainly in that setting's help text rather than guarded against.
  const onBreak = idleSecs > t("presence.break_secs");
  const recentHookActivity =
    lastHookActivitySince !== null && Date.now() - lastHookActivitySince < HOOK_ACTIVITY_CODING_WINDOW_MS;
  const recentPromptSubmit =
    lastPromptSubmitSince !== null && Date.now() - lastPromptSubmitSince < THINKING_WINDOW_MS;

  let next;
  if (onCall) next = "call";
  else if (streaming) next = "streaming";
  else if (forgotten) next = "forgotten";
  else if (pending.count > 0) next = "waiting";
  else if (recentHookActivity) next = "coding";
  // Real tool activity (coding, just above) always outranks "still
  // thinking" — once a tool actually runs, that's more informative than the
  // anticipatory pose from the moment the prompt was submitted.
  else if (recentPromptSubmit) next = "thinking";
  else if (spotifyPlaying) next = "listening";
  else if (lowPower) next = "lowpower";
  else if (sleeping) next = "sleeping";
  else if (onBreak) next = "break";
  else next = "idle";

  if (wasSleeping && next !== "sleeping" && eventsSinceSleep > 0) {
    pushEvent("welcome_back");
    eventsSinceSleep = 0;
  }
  wasSleeping = next === "sleeping";

  setAmbient(next);
}

function markEventDuringSleep() {
  if (wasSleeping) eventsSinceSleep++;
}

// The entry point for everything that isn't the poll loop. Logging rather than
// surfacing the failure is deliberate: presence is ambient decoration, so the
// wrong face is a far smaller interruption than an error notice over the whole
// widget — and an uncaught one here would be an unhandled rejection per event.
export function refreshAmbient() {
  return computeAmbient().catch((err) => console.error("presence signals:", err));
}

// A self-rescheduling timer rather than setInterval, because the interval is
// itself a setting: setInterval latches its period at creation, so changing it
// would need the timer torn down and rebuilt. Reading it per tick means the very
// next gap reflects a change. Only ever scheduled after a successful compute, so
// the read below cannot be the thing that throws.
function scheduleAmbientPoll() {
  setTimeout(() => {
    computeAmbient()
      .then(scheduleAmbientPoll)
      .catch((err) => console.error("presence signals stopped:", err));
  }, t("presence.poll_secs") * 1000);
}

window.addEventListener("DOMContentLoaded", () => {
  computeAmbient()
    .then(scheduleAmbientPoll)
    .catch((err) => console.error("presence signals never started:", err));

  listen("spotify-now-playing", (event) => {
    spotifyPlaying = !!(event.payload && event.payload.is_playing);
  });

  listen("claude-hook-activity", () => {
    lastHookActivitySince = Date.now();
    refreshAmbient();
  });

  listen("claude-user-prompt-submit", () => {
    lastPromptSubmitSince = Date.now();
    refreshAmbient();
  });

  // A pending decision is the highest-priority, most time-sensitive ambient
  // signal there is — recomputing immediately instead of waiting for the
  // next poll tick is what makes the mascot's mood inside the permission
  // card itself (see styles.css's mini pip icon) switch to "waiting" right
  // away instead of still showing whatever it was doing a moment before
  // (e.g. "listening") for as long as the poll interval.
  listen("mascot-permission-request", () => {
    refreshAmbient();
  });

  listen("github-merge", () => {
    markEventDuringSleep();
    pushEvent("happy");
  });

  listen("github-ci", (event) => {
    markEventDuringSleep();
    pushEvent(event.payload.kind === "failed" ? "alert" : "happy");
  });

  listen("github-review-requested", () => {
    markEventDuringSleep();
    pushEvent("review_requested");
  });

  listen("github-issue-update", (event) => {
    markEventDuringSleep();
    const kind = event.payload && event.payload.kind;
    if (kind === "assigned") pushEvent("assigned");
    else if (kind === "comment") pushEvent("mentioned");
  });

  listen("github-digest", () => {
    markEventDuringSleep();
    pushEvent("digest_ready", 2000); // brief pulse — the actual digest card takes over right after (main.js)
  });

  listen("mail-new", (event) => {
    markEventDuringSleep();
    // A reply to something you sent is the one kind of mail you were already
    // waiting on, so it gets its own pose rather than the generic arrival one.
    // 11s to match the notice's own lifetime (MAIL_NOTICE_MS): the postman
    // should be on screen for exactly as long as the message it delivered, not
    // vanish out from under it at the 8s default.
    pushEvent(event.payload && event.payload.is_reply_to_me ? "mail_reply" : "mail_new", 11000);
  });

  listen("calendar-soon", () => {
    markEventDuringSleep();
    pushEvent("meeting_soon", 12000); // CALENDAR_NOTICE_MS
  });

  listen("daily-brief", () => {
    markEventDuringSleep();
    // The brief card is pinned — it has no timer, so neither can its pose. Held
    // for far longer than anyone leaves the card up and ended explicitly when
    // the card closes (see main.js), the same way the voice states do it.
    pushEvent("brief_ready", 30 * 60 * 1000);
  });
});
