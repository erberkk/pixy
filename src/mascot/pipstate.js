// Two-tier priority store for the Pip sprite's mood (NOT the box-layout
// state — that's still body.className, owned entirely by main.js/spotify.js
// and untouched by this file; see pip.js's header comment for why the two
// are kept separate).
//
//   ambient — persistent, recomputed from live signals (signals.js): what
//             Pip is doing right now with nothing more urgent going on.
//   event   — transient (a few seconds), pushed by one-off happenings
//             (a PR merged, CI failed, someone mentioned you). The most
//             recent event always wins over ambient until it expires, then
//             ambient resumes exactly where it left off.
//
// When a pinned card (agent-permission / github-digest) or any of the
// existing transient notice states owns body.className, .pip-row is hidden
// by CSS (same mechanism .face used to be hidden by) — this store keeps
// running underneath regardless, so whatever it resolves to is simply
// invisible until the pill returns to view. No coordination with
// noticeLocked is needed for that reason.
import { initPip, setPipState } from "./pip.js";
import { reportHotRectSoon } from "./main.js";

let ambient = "idle";
let eventName = null;
let eventTimer = null;

// True idle (mascot alone, 58px circle) and every other pip state (wider
// pill with text) are DIFFERENT sizes — see styles.css's
// `.state-idle #mascot[data-pip-state="idle"]` vs `:not(...)` rules. Every
// setPipState() call can therefore change #mascot's actual on-screen box,
// and the click-through hot-rect (main.js/clickthrough.rs) only updates
// when told to — without this, switching pip states left it pointed at
// whatever box shape was current the last time something else happened to
// call reportHotRectSoon(), silently breaking clicks on the new shape.
function resolve() {
  if (eventName) {
    setPipState(eventName);
  } else {
    setPipState(ambient);
  }
  reportHotRectSoon();
}

export function setAmbient(name) {
  if (ambient === name) return;
  ambient = name;
  if (!eventName) resolve();
}

export function pushEvent(name, ms = 8000) {
  if (eventTimer) clearTimeout(eventTimer);
  eventName = name;
  resolve();
  eventTimer = setTimeout(() => {
    eventTimer = null;
    eventName = null;
    resolve();
  }, ms);
}

export function currentAmbient() {
  return ambient;
}

window.addEventListener("DOMContentLoaded", () => {
  initPip({
    canvas: document.getElementById("pip-canvas"),
    root: document.getElementById("mascot"),
    ind: document.getElementById("pip-ind"),
    title: document.getElementById("pip-title"),
    sub: document.getElementById("pip-sub"),
  });
  // initPip() switches #mascot from the raw HTML's pre-JS layout to the
  // idle circle — main.js's own initial reportHotRectSoon() (from its
  // DOMContentLoaded listener, which runs before this one) measured the
  // OLD shape, so it needs to be told about the new one.
  reportHotRectSoon();
});
