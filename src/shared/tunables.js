// The frontend's read side of the per-machine settings registry declared in
// src-tauri/src/tunables.rs.
//
// The one rule this file exists to enforce: no default lives here. Every value
// comes from the backend, which merges the user's overrides onto the compiled
// defaults before answering — so there is exactly one place a number is written
// down, and a frontend copy can never drift from the Rust one.
//
// That means `t()` throws rather than guessing when values haven't loaded. A
// module that reads a tunable must await `loadTunables()` first; failing loudly
// on the second line of startup is far better than a silent NaN threshold that
// makes the microphone behave strangely for a week.
import { invoke, listen } from "./tauri.js";

let values = null;
let loading = null;

/**
 * Loads (once) the effective values for every tunable. Safe to call from
 * several modules — they share the one in-flight request.
 */
export function loadTunables() {
  if (!loading) {
    loading = invoke("get_tunables")
      .then((payload) => {
        values = payload.values;
        return values;
      })
      .catch((err) => {
        // Not cached as a failure: this is a local IPC call, so a failure is
        // either a transient startup race or a broken build, and a retry on the
        // next caller is the useful behaviour.
        loading = null;
        throw err;
      });
  }
  return loading;
}

/** One tunable's current value, by the id declared in tunables.rs. */
export function t(id) {
  if (values === null) {
    throw new Error(`Tunables read before loading (${id}) — await loadTunables() first.`);
  }
  const value = values[id];
  if (value === undefined) {
    // An id the backend doesn't know: a typo, or a setting removed from the
    // registry without its readers being updated.
    throw new Error(`Unknown tunable: ${id}`);
  }
  return value;
}

/** A `names` tunable as an array of lowercase substrings. */
export function tNames(id) {
  return String(t(id))
    .split(",")
    .map((part) => part.trim().toLowerCase())
    .filter(Boolean);
}

// Settings runs in its own webview, so saving there can't call into this one.
// The backend re-broadcasts instead, and every window that reads tunables picks
// the new values up without a restart — which is what lets all but the event
// port be marked as taking effect immediately.
listen("tunables-changed", () => {
  invoke("get_tunables")
    .then((payload) => {
      values = payload.values;
    })
    .catch(() => {
      // Keep the values we already have: stale-but-working beats throwing on
      // every subsequent read because one refresh failed.
    });
});
