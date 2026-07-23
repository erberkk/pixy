// Spotify now-playing panel — kept in its own module/stylesheet
// (spotify.css) separate from the general mascot state machine (main.js),
// since it's a fully independent concern (not part of the hook-driven state
// machine) that only happens to render inside the same #mascot element.
// Opened with a single click on the pill (double-click still opens the
// terminal — see main.js). Closed by the cursor LEAVING the pill/panel
// area, not by a second click — an earlier "click anywhere else to close"
// version was unreliable because that document-level click listener only
// ever sees clicks that land inside the mascot's own (small, mostly
// transparent) OS window; a click anywhere else on the desktop never
// reaches this webview's JS at all, so the panel could get stuck open.
// mouseleave doesn't have that problem — it fires off the OS's normal
// mouse-move tracking as soon as the cursor exits the window, regardless of
// where it goes next.
import { isNoticeLocked, reportHotRectSoon } from "./main.js";

const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;

// null = no Spotify session (or not yet known); otherwise { title, artist,
// is_playing }, kept in sync by the "spotify-now-playing" watcher event
// (see media.rs) plus a one-shot fetch the first time the user opens the
// panel, so it doesn't wait out the watcher's ~2s poll interval.
let spotifyState = null;
// A real click and the first half of a double-click look identical until
// the second click either does or doesn't arrive — this holds the pending
// "was it just a single click?" decision (see the click listener below).
let clickTimer = null;
// Debounces mouseleave — moving the cursor from the pill onto one of the
// panel's own buttons/sliders technically leaves-then-re-enters #mascot for
// an instant, so closing immediately on the first mouseleave would slam the
// panel shut while trying to use it.
let closeTimer = null;

// Native <input type=range> has no built-in "filled" look — the track
// color is one flat color regardless of value — so the white/gray split
// is painted manually as an inline gradient every time the value changes.
function updateVolumeFill(slider) {
  slider.style.setProperty("--fill", `${slider.value}%`);
}

// Real mute toggle (backed by the endpoint's actual Mute flag) — used for
// the speaker/mic buttons in the toolbar. Re-reads the current muted state
// from the backend rather than tracking it locally, so it stays correct
// even if something else (a hardware mute key, Windows' own flyout, the
// matching row in the session list below) changed it since the panel opened.
function bindMuteToggle(elId, getMutedCmd, setMutedCmd) {
  const el = document.getElementById(elId);
  el.addEventListener("click", () => {
    invoke(getMutedCmd)
      .then((muted) => {
        const next = !muted;
        return invoke(setMutedCmd, { muted: next }).then(() => {
          el.classList.toggle("muted", next);
        });
      })
      .catch(() => {});
  });
}

function initToggleButton(elId, getMutedCmd) {
  invoke(getMutedCmd)
    .then((muted) => {
      document.getElementById(elId).classList.toggle("muted", muted);
    })
    .catch(() => {});
}

// One row per currently adjustable app session on the PC (Windows' own
// Volume Mixer list), plus a synthetic "Master" row (pid 0, using the
// system-wide endpoint commands) pinned first — rebuilt from scratch on
// every reveal since which apps are playing audio changes independently of
// anything this widget watches.
function renderAudioSessions(sessions) {
  const list = document.getElementById("audio-session-list");
  list.innerHTML = "";

  const rows = [{ pid: 0, name: "Master", volume: null, muted: null, isMaster: true }, ...sessions];

  for (const session of rows) {
    const row = document.createElement("div");
    row.className = "audio-row";

    const name = document.createElement("span");
    name.className = "audio-session-name";
    name.textContent = session.name;
    row.appendChild(name);

    const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    icon.setAttribute("viewBox", "0 0 24 24");
    icon.setAttribute("fill", "none");
    icon.setAttribute("stroke", "currentColor");
    icon.setAttribute("stroke-width", "2");
    icon.classList.add("panel-icon");
    icon.innerHTML =
      '<path d="M11 5 6 9H2v6h4l5 4z"/><path d="M15.5 8.5a5 5 0 010 7"/>';
    row.appendChild(icon);

    const slider = document.createElement("input");
    slider.type = "range";
    slider.className = "panel-slider";
    slider.min = "0";
    slider.max = "100";
    row.appendChild(slider);

    const getVolumeCmd = session.isMaster ? "system_speaker_get_volume" : null;
    const setVolumeCmd = session.isMaster ? "system_speaker_set_volume" : "set_session_volume";
    const getMutedCmd = session.isMaster ? "system_speaker_get_muted" : null;
    const setMutedCmd = session.isMaster ? "system_speaker_set_muted" : "set_session_muted";
    const extraArgs = session.isMaster ? {} : { pid: session.pid };

    if (session.isMaster) {
      invoke(getVolumeCmd)
        .then((v) => {
          slider.value = Math.round(v * 100);
          updateVolumeFill(slider);
        })
        .catch(() => {});
      invoke(getMutedCmd)
        .then((muted) => icon.classList.toggle("muted", muted))
        .catch(() => {});
    } else {
      slider.value = Math.round((session.volume ?? 0) * 100);
      updateVolumeFill(slider);
      icon.classList.toggle("muted", !!session.muted);
    }

    let throttled = false;
    let pendingLevel = null;
    const send = (level) => invoke(setVolumeCmd, { level, ...extraArgs }).catch(() => {});
    slider.addEventListener("input", (e) => {
      updateVolumeFill(e.target);
      const level = Number(e.target.value) / 100;
      if (throttled) {
        pendingLevel = level;
        return;
      }
      throttled = true;
      send(level);
      setTimeout(() => {
        throttled = false;
        if (pendingLevel !== null) {
          const toSend = pendingLevel;
          pendingLevel = null;
          send(toSend);
        }
      }, 80);
    });
    slider.addEventListener("change", (e) => {
      pendingLevel = null;
      send(Number(e.target.value) / 100);
    });

    icon.addEventListener("click", () => {
      const next = !icon.classList.contains("muted");
      invoke(setMutedCmd, { muted: next, ...extraArgs })
        .then(() => icon.classList.toggle("muted", next))
        .catch(() => {});
    });

    list.appendChild(row);
  }
}

function refreshAudioSessions() {
  invoke("list_audio_sessions")
    .then((sessions) => renderAudioSessions(sessions))
    .catch(() => renderAudioSessions([]));
}

// Keeps the panel roughly in sync with Windows' own Volume Mixer / mic
// button while it's open — there's no push-notification API this widget
// hooks into (that would mean implementing custom COM callback objects via
// windows-rs' #[implement]), so this polls instead, same idea as the
// Spotify now-playing watcher elsewhere in the app. Skips a tick while the
// user's actively focused on one of the list's own sliders, since rebuilding
// the DOM out from under an in-progress drag would yank focus/interrupt it.
let audioSyncTimer = null;
function startAudioSync() {
  stopAudioSync();
  audioSyncTimer = setInterval(() => {
    const list = document.getElementById("audio-session-list");
    if (document.activeElement && list.contains(document.activeElement)) return;
    refreshAudioSessions();
    initToggleButton("system-speaker-toggle", "system_speaker_get_muted");
    initToggleButton("system-mic-toggle", "system_mic_get_muted");
  }, 1500);
}
function stopAudioSync() {
  if (audioSyncTimer) {
    clearInterval(audioSyncTimer);
    audioSyncTimer = null;
  }
}

function renderSpotifyPanel() {
  if (!spotifyState) return;
  document.getElementById("spotify-title").textContent = spotifyState.title || "";
  document.getElementById("spotify-artist").textContent = spotifyState.artist || "";
  const btn = document.getElementById("spotify-play-pause");
  btn.querySelector(".icon-play").style.display = spotifyState.is_playing ? "none" : "";
  btn.querySelector(".icon-pause").style.display = spotifyState.is_playing ? "" : "none";

  const art = document.getElementById("spotify-art");
  if (spotifyState.art) {
    art.src = spotifyState.art;
    art.classList.add("has-art");
  } else {
    art.removeAttribute("src");
    art.classList.remove("has-art");
  }
}

// The panel lives INSIDE #mascot (not a separate overlay element like the
// quick-menu), so #mascot's own bounding-rect naturally covers it once
// expanded — no extra hot-rect plumbing needed beyond the existing
// reportHotRectSoon() reading #mascot's rect. Also gated on the quick-menu
// NOT being open — right-click's menu is positioned assuming the plain
// small pill, so letting both be active at once makes them visually
// collide (see main.js's contextmenu handler, which collapses this first).
// The "spotify-hover" CSS class name predates the switch to click-toggling
// and just means "panel is open" now — kept as-is to avoid churning every
// selector in spotify.css for a rename with no behavior change.
function openSpotifyPanel() {
  if (isNoticeLocked() || document.body.className !== "state-idle") return;
  if (document.getElementById("quick-menu").classList.contains("visible")) return;
  const mascotEl = document.getElementById("mascot");

  const reveal = () => {
    if (!spotifyState) return; // nothing playing — stays the plain idle pill

    // The panel's size (and its buttons/sliders) differs from the tiny
    // idle pill's click-through hot-rect — pausing click-through for the
    // duration this is open sidesteps any brief mismatch during that
    // resize (clickthrough.rs's poll, ~40ms cadence) turning the window
    // click-through right as the panel's controls appear, rather than
    // trying to win that race.
    invoke("set_click_through_paused", { paused: true });

    renderSpotifyPanel();
    mascotEl.classList.add("spotify-hover");
    reportHotRectSoon();
    initToggleButton("system-speaker-toggle", "system_speaker_get_muted");
    initToggleButton("system-mic-toggle", "system_mic_get_muted");
    refreshAudioSessions();
    startAudioSync();
  };

  if (spotifyState === null) {
    invoke("spotify_get_state")
      .then((state) => {
        spotifyState = state;
        reveal();
      })
      .catch(() => {});
  } else {
    reveal();
  }
}

function closeSpotifyPanel() {
  document.getElementById("mascot").classList.remove("spotify-hover");
  invoke("set_click_through_paused", { paused: false });
  stopAudioSync();
  reportHotRectSoon();
}

function toggleSpotifyPanel() {
  if (document.getElementById("mascot").classList.contains("spotify-hover")) {
    closeSpotifyPanel();
  } else {
    openSpotifyPanel();
  }
}

// Called by main.js from the double-click (open terminal) and right-click
// (quick-menu) handlers — both need this panel out of the way immediately,
// cancelling any pending single-click toggle too, not just closing an
// already-open panel.
export function cancelSpotifyPanel() {
  if (clickTimer) {
    clearTimeout(clickTimer);
    clickTimer = null;
  }
  if (closeTimer) {
    clearTimeout(closeTimer);
    closeTimer = null;
  }
  closeSpotifyPanel();
}

window.addEventListener("DOMContentLoaded", () => {
  listen("spotify-now-playing", (event) => {
    spotifyState = event.payload;
    if (document.getElementById("mascot").classList.contains("spotify-hover")) {
      if (spotifyState) {
        renderSpotifyPanel();
      } else {
        closeSpotifyPanel(); // Spotify closed/session ended while the panel was open
      }
    }
  });

  const mascotEl = document.getElementById("mascot");

  // Single click toggles the panel; double-click still opens the terminal
  // (see main.js). The two look identical up through the first click, so
  // this waits out the standard double-click window before committing to
  // "that was a real single click" — if a second click lands within it,
  // this is that second click of a dblclick, so it's swallowed here and
  // main.js's dblclick handler (which also fired) takes over instead.
  mascotEl.addEventListener("click", (e) => {
    if (e.target.closest(".spotify-panel")) return; // let the panel's own controls handle their own clicks
    // Any other overlay (agent permission card, GitHub digest, etc.) owns
    // this click instead — checked up front, not just inside
    // openSpotifyPanel(), because clicking e.g. Approve can revert the state
    // back to idle by the time the 300ms single-click timer below fires,
    // which would otherwise open the Spotify panel as an unintended side
    // effect of a click that had nothing to do with it.
    if (isNoticeLocked() || document.body.className !== "state-idle") return;
    if (clickTimer) {
      clearTimeout(clickTimer);
      clickTimer = null;
      return;
    }
    clickTimer = setTimeout(() => {
      clickTimer = null;
      toggleSpotifyPanel();
    }, 300);
  });

  // Cursor leaving the pill/panel closes it (with a short grace period —
  // moving onto one of the panel's own buttons/sliders technically leaves
  // then re-enters #mascot for an instant, so closing on the very first
  // mouseleave would slam it shut mid-interaction).
  mascotEl.addEventListener("mouseleave", () => {
    if (!mascotEl.classList.contains("spotify-hover")) return;
    closeTimer = setTimeout(() => {
      closeTimer = null;
      closeSpotifyPanel();
    }, 250);
  });
  mascotEl.addEventListener("mouseenter", () => {
    if (closeTimer) {
      clearTimeout(closeTimer);
      closeTimer = null;
    }
  });

  document.getElementById("spotify-play-pause").addEventListener("click", () => {
    invoke("spotify_play_pause").catch(() => {});
  });
  document.getElementById("spotify-prev").addEventListener("click", () => {
    invoke("spotify_previous").catch(() => {});
  });
  document.getElementById("spotify-next").addEventListener("click", () => {
    invoke("spotify_next").catch(() => {});
  });

  // Speaker + mic are both instant mute/unmute only (system-wide default
  // endpoints, not scoped to one app) — actual levels, including Spotify's
  // own, live in the session list below instead.
  bindMuteToggle("system-speaker-toggle", "system_speaker_get_muted", "system_speaker_set_muted");
  bindMuteToggle("system-mic-toggle", "system_mic_get_muted", "system_mic_set_muted");
});
