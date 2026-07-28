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
// `gaming` is intentionally not wired here (no reliable signal yet — see the
// plan discussion); it still exists in pip.js/pip.css for manual use.
// `working`/`reviewing`/`writing` (splitting "coding" by the active tool's
// name) were considered and rejected too: tool_name changes multiple times
// per turn, so this would flicker between moods every tool call instead of
// reading as one continuous ambient pose — "coding" stays the single
// umbrella state for any real tool activity.
import { setAmbient, pushEvent } from "./pipstate.js";

import { invoke, listen } from "../../shared/tauri.js";
// The voice assistant holds the microphone open the whole time it is listening
// for the wake word, which trips the same is_mic_capture_active signal a call
// does — see the onCall calculation below.
import { isOpen as voiceHoldsMic } from "../voice/mic.js";

const CALL_APP_RE = /teams|zoom|discord|slack/i;
const STREAM_APP_RE = /obs/i;
const FORGOTTEN_THRESHOLD_SECS = 300; // 5 minutes unanswered
const BREAK_THRESHOLD_SECS = 240; // 4 minutes idle — a short "stepped away" pause, distinct from full sleep
const SLEEP_THRESHOLD_SECS = 900; // 15 minutes with no keyboard/mouse input
const LOW_BATTERY_PERCENT = 20;
const ACTIVE_STALE_SECS = 60; // last_activity older than this no longer counts as "coding"

let spotifyPlaying = false;
// Timestamp of the last Claude Code hook event of ANY kind (see agent/server.rs's
// claude-hook-activity emit) — list_agent_sessions only sees activity in
// this app's OWN pooled terminals, so a `claude` session running in some
// other window would never otherwise register as "coding".
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
  const [sessions, audioSessions, micMuted, micCaptureActive, power, idleSecs] = await Promise.all([
    invoke("list_agent_sessions").catch(() => []),
    invoke("list_audio_sessions").catch(() => []),
    invoke("system_mic_get_muted").catch(() => true),
    // Actual mic-capture activity (Windows' AudioSessionStateActive on the
    // default INPUT device), not just "an app with a known call-app name is
    // making sound" — the old CALL_APP_RE-only check never fired for a
    // browser-based call (Google Meet et al show up as chrome.exe/
    // msedge.exe, indistinguishable by name from that browser just having
    // some unrelated tab open). This generalizes past any specific app.
    invoke("is_mic_capture_active").catch(() => false),
    invoke("get_power_status").catch(() => null),
    invoke("get_idle_seconds").catch(() => 0),
  ]);

  const audioNames = audioSessions.map((s) => s.name || "");
  // While the voice assistant is on, micCaptureActive carries no information —
  // it is true continuously because *we* are the app capturing, so trusting it
  // would pin the mascot to "call" for as long as the wake word is armed.
  // Falling back to the app-name heuristic means a browser-based call is missed
  // while voice is enabled, which is the lesser of the two: a permanently wrong
  // ambient state is worse than one missed heuristic.
  const foreignCapture = micCaptureActive && !voiceHoldsMic();
  const onCall = !micMuted && (foreignCapture || audioNames.some((n) => CALL_APP_RE.test(n)));
  const streaming = audioNames.some((n) => STREAM_APP_RE.test(n));

  const pending = sessions.filter((s) => s.has_pending);
  const forgotten = pending.some((s) => (s.pending_since_secs || 0) > FORGOTTEN_THRESHOLD_SECS);
  // last_activity never clears itself (it just holds whatever the last
  // visible line was) — without a recency check, a terminal opened once and
  // then left alone would claim "coding" forever. ACTIVE_STALE_SECS is
  // deliberately generous (a human reading a long tool output before
  // reacting shouldn't flip the mascot back to idle mid-thought) but still
  // bounded.
  const active = sessions.filter((s) => s.agent && (s.last_activity_secs ?? Infinity) < ACTIVE_STALE_SECS);

  const lowPower = !!power && power.has_battery && !power.charging && power.percent <= LOW_BATTERY_PERCENT;
  const sleeping = idleSecs > SLEEP_THRESHOLD_SECS;
  // Checked AFTER sleeping in the chain below so a long-idle machine settles
  // into the deeper "sleeping" pose instead of getting stuck on "break" —
  // idleSecs growing past SLEEP_THRESHOLD_SECS is also, technically, past
  // BREAK_THRESHOLD_SECS, so the ordering (not a range check here) is what
  // actually stratifies the two.
  const onBreak = idleSecs > BREAK_THRESHOLD_SECS;
  const recentHookActivity =
    lastHookActivitySince !== null && Date.now() - lastHookActivitySince < HOOK_ACTIVITY_CODING_WINDOW_MS;
  const recentPromptSubmit =
    lastPromptSubmitSince !== null && Date.now() - lastPromptSubmitSince < THINKING_WINDOW_MS;

  let next;
  if (onCall) next = "call";
  else if (streaming) next = "streaming";
  else if (active.length >= 2) next = "juggling";
  else if (forgotten) next = "forgotten";
  else if (pending.length > 0) next = "waiting";
  else if (active.length === 1 || recentHookActivity) next = "coding";
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

window.addEventListener("DOMContentLoaded", () => {
  computeAmbient();
  setInterval(computeAmbient, 5000);

  listen("spotify-now-playing", (event) => {
    spotifyPlaying = !!(event.payload && event.payload.is_playing);
  });

  listen("claude-hook-activity", () => {
    lastHookActivitySince = Date.now();
    computeAmbient();
  });

  listen("claude-user-prompt-submit", () => {
    lastPromptSubmitSince = Date.now();
    computeAmbient();
  });

  // A pending decision is the highest-priority, most time-sensitive ambient
  // signal there is — recomputing immediately instead of waiting for the
  // next 5s poll tick is what makes the mascot's mood inside the permission
  // card itself (see styles.css's mini pip icon) switch to "waiting" right
  // away instead of still showing whatever it was doing a moment before
  // (e.g. "listening") for up to 5 seconds.
  listen("mascot-permission-request", () => {
    computeAmbient();
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
});
