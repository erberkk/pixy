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
import { setAmbient, pushEvent } from "./pipstate.js";

const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;

const CALL_APP_RE = /teams|zoom|discord|slack/i;
const STREAM_APP_RE = /obs/i;
const FORGOTTEN_THRESHOLD_SECS = 300; // 5 minutes unanswered
const SLEEP_THRESHOLD_SECS = 900; // 15 minutes with no keyboard/mouse input
const LOW_BATTERY_PERCENT = 20;
const ACTIVE_STALE_SECS = 60; // last_activity older than this no longer counts as "coding"

let spotifyPlaying = false;
// Timestamp of the last Claude Code hook event of ANY kind (see server.rs's
// claude-hook-activity emit) — list_agent_sessions only sees activity in
// this app's OWN pooled terminals, so a `claude` session running in some
// other window would never otherwise register as "coding".
let lastHookActivitySince = null;
const HOOK_ACTIVITY_CODING_WINDOW_MS = 30000;
// Counts backend events (merge/CI/review/issue) that happened while ambient
// was 'sleeping' — surfaced once as a single 'welcome_back' pulse the moment
// the user is active again, instead of several separate pulses landing the
// instant the pill wakes up.
let eventsSinceSleep = 0;
let wasSleeping = false;

export async function computeAmbient() {
  const [sessions, audioSessions, micMuted, power, idleSecs] = await Promise.all([
    invoke("list_agent_sessions").catch(() => []),
    invoke("list_audio_sessions").catch(() => []),
    invoke("system_mic_get_muted").catch(() => true),
    invoke("get_power_status").catch(() => null),
    invoke("get_idle_seconds").catch(() => 0),
  ]);

  const audioNames = audioSessions.map((s) => s.name || "");
  const onCall = !micMuted && audioNames.some((n) => CALL_APP_RE.test(n));
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
  const recentHookActivity =
    lastHookActivitySince !== null && Date.now() - lastHookActivitySince < HOOK_ACTIVITY_CODING_WINDOW_MS;

  let next;
  if (onCall) next = "call";
  else if (streaming) next = "streaming";
  else if (active.length >= 2) next = "juggling";
  else if (forgotten) next = "forgotten";
  else if (pending.length > 0) next = "waiting";
  else if (active.length === 1 || recentHookActivity) next = "coding";
  else if (spotifyPlaying) next = "listening";
  else if (lowPower) next = "lowpower";
  else if (sleeping) next = "sleeping";
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
