use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{Emitter, Manager};

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    // Set by mark_pending when Claude Code's PermissionRequest hook fires for
    // this terminal, cleared by clear_pending once the mascot's Approve/Deny/
    // Submit answers the hook directly (see server.rs's resolve_decision —
    // the actual decision now travels back as the hook's HTTP response body,
    // never as a PTY keystroke; this is purely a display flag for "show all
    // agents"/the ambient system).
    pending_tool_name: Option<String>,
    // Most recent non-chrome line visible on screen — lets the "show all
    // agents" list summarize what every pooled terminal is doing right now,
    // not just the ones currently blocked on a decision.
    last_activity: Option<String>,
    // When last_activity last actually CHANGED (not just when this session
    // was last polled — terminal.js reports a screen snapshot on every
    // render tick regardless of whether anything moved, so using call-time
    // here would never go stale even for a terminal sitting on a static
    // idle shell prompt). Lets the pip ambient system's "coding" mood
    // (signals.js) tell real, ongoing activity apart from a terminal
    // window that was opened once and then left alone.
    last_activity_at: Option<std::time::Instant>,
    // When the current pending prompt first appeared — lets the mascot tell
    // "just asked" apart from "been sitting unanswered for a while" (the
    // `forgotten` ambient state) without the frontend having to track
    // wall-clock time itself across permission-card re-renders.
    pending_since: Option<std::time::Instant>,
}

// Keyed by terminal window label ("terminal", "terminal2", ...). Dynamic
// window creation hangs in this environment (see windows.rs), so the pool is
// a fixed set of pre-declared windows rather than one spawned per session.
// HashMap::new() isn't const, so a OnceLock lazily builds the map on first
// use instead of a plain `static ... = Mutex::new(HashMap::new())`.
static SESSIONS: OnceLock<Mutex<HashMap<String, PtySession>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, PtySession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn debug_log_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("terminal-debug.log")
}

fn append_debug_log(app: &tauri::AppHandle, entry: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(debug_log_path(app))
    {
        let _ = writeln!(file, "{entry}\n");
    }
}

// Box-drawing borders and spinner glyphs are pure rendering chrome, not
// activity content — kept only for the "show all agents" list's
// last-activity snippet, not for any decision-making anymore.
const BORDER_CHARS: &[char] = &[
    '─', '│', '╭', '╮', '╰', '╯', '├', '┤', '┬', '┴', '┼', '═', '║', '✢', '✶', '✻', '✽', '✿', '◐',
    '◑', '◒', '◓', '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '❯',
];

// Fixed status-line chrome (spinner word + elapsed time/tokens, mode
// indicators) rather than real content — dropped from the last-activity
// snippet regardless of the random spinner verb Claude Code is using this
// frame ("9s · ↓ 433 tokens · thought for 2s" etc. all use "·" as a
// separator, which real text essentially never contains).
fn is_chrome_line(line: &str) -> bool {
    let l = line.to_lowercase();
    l == "waiting…"
        || l.contains('·')
        || l.contains("esc to cancel")
        || l.contains("to auto-approve") // "shift+tab to auto-approve file edits" hint
        || l.starts_with("↑/↓") // "↑/↓ Navigate" menu hint
        || l.ends_with('…') // trailing "<RandomVerb>…" spinner fragment with nothing else on the line
}

fn clean_lines(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        let stripped: String = line.chars().filter(|c| !BORDER_CHARS.contains(c)).collect();
        let mut trimmed = stripped.trim();
        // Drop a trailing "(ctrl+o to expand)"-style affordance hint Claude
        // appends to tool-call lines — UI chrome, not part of the activity text.
        if let Some(idx) = trimmed.rfind('(') {
            if trimmed[idx..].to_lowercase().contains("to expand") {
                trimmed = trimmed[..idx].trim_end();
            }
        }
        if trimmed.is_empty() || is_chrome_line(trimmed) {
            continue;
        }
        if out.last().map(|s| s.as_str()) == Some(trimmed) {
            continue; // dedup consecutive repeats (redraw noise)
        }
        out.push(trimmed.to_string());
    }
    out
}

#[tauri::command]
pub fn start_terminal_session(app: tauri::AppHandle, window: tauri::WebviewWindow) -> Result<(), String> {
    let label = window.label().to_string();
    {
        let guard = sessions().lock().unwrap();
        if guard.contains_key(&label) {
            append_debug_log(&app, &format!("start_terminal_session[{label}]: already exists, reusing"));
            return Ok(()); // already running — reuse it
        }
    }
    append_debug_log(&app, &format!("start_terminal_session[{label}]: no existing session, spawning new one"));

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("failed to open pty: {e}"))?;

    let mut cmd = CommandBuilder::new_default_prog();
    // Lets a Claude Code PermissionRequest hook running INSIDE this shell
    // report back which pooled terminal it came from (see server.rs's
    // handle_decide_request / SETUP.md's hook command) — inherited down the
    // process tree from this shell to `claude` to the hook's own child
    // process, no IPC needed to establish the mapping.
    cmd.env("WIDGET_TERMINAL_LABEL", &label);
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to spawn shell: {e}"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("failed to clone pty reader: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("failed to take pty writer: {e}"))?;

    {
        let mut guard = sessions().lock().unwrap();
        guard.insert(
            label.clone(),
            PtySession {
                master: pair.master,
                writer,
                child,
                pending_tool_name: None,
                last_activity: None,
                last_activity_at: None,
                pending_since: None,
            },
        );
    }

    append_debug_log(&app, &format!("start_terminal_session[{label}]: spawned shell, reader thread starting"));

    let app_for_reader = app.clone();
    let label_for_reader = label.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => {
                    append_debug_log(&app_for_reader, &format!("reader[{label_for_reader}]: EOF, shell exited"));
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    append_debug_log(&app_for_reader, &format!("reader[{label_for_reader}]: read error: {e}"));
                    break;
                }
            };
            // Just forwards raw bytes for xterm.js to render — the widget
            // never reconstructs terminal state from this raw byte stream
            // itself (see report_terminal_text for the rendered-screen
            // snapshot, used only for the last-activity snippet + deny-digit
            // lookup now, not for detecting permission prompts).
            //
            // The event NAME itself is suffixed with the window label
            // (rather than relying on emit_to's target-based filtering with
            // a shared event name) — every pooled terminal window's JS
            // calls the plain global `listen()` with no explicit target
            // option, which defaults to matching ANY emit target, so
            // emit_to(label, "terminal-output", ...) was still reaching
            // every terminal window's listener, not just that session's own
            // — this is what caused terminal windows to mirror each
            // other's keystrokes/output live. A per-window event name sidesteps
            // that filtering question entirely: only the matching window's
            // listener is even registered for this exact name.
            let _ = app_for_reader.emit(&format!("terminal-output:{label_for_reader}"), buf[..n].to_vec());
        }
        let _ = app_for_reader.emit(&format!("terminal-closed:{label_for_reader}"), ());
    });

    Ok(())
}

// Called from terminal.js after each render settles, with a plain-text
// snapshot of xterm.js's current visible screen (already correctly handles
// cursor movement/carriage-return redraws/erasure — the raw PTY byte stream
// alone can't give us that without reimplementing a terminal emulator).
// Only feeds the last-activity display snippet and the raw snapshot used at
// deny-click time now — actual permission decisions come from
// record_permission_request (the Claude Code hook), not from scanning this.
#[tauri::command]
pub fn report_terminal_text(window: tauri::WebviewWindow, text: String) -> Result<(), String> {
    let label = window.label().to_string();
    let mut guard = sessions().lock().unwrap();
    let session = guard.get_mut(&label).ok_or("no active terminal session")?;

    let lines: Vec<&str> = text.lines().collect();
    let cleaned = clean_lines(&lines);
    if let Some(last) = cleaned.last() {
        if session.last_activity.as_deref() != Some(last.as_str()) {
            session.last_activity_at = Some(std::time::Instant::now());
        }
        session.last_activity = Some(last.clone());
    }

    Ok(())
}

// Called by server.rs as soon as Claude Code's PermissionRequest hook fires
// for a known pooled terminal — purely a display-state flag now (see
// PtySession's pending_tool_name doc comment). The actual decision travels
// back to Claude Code as the hook's own HTTP response body (server.rs's
// resolve_decision), never through this terminal's PTY. Always overwrites
// any existing pending marker for this session: PermissionRequest only fires
// for a tool that's genuinely blocked on a decision right now, so the newest
// one is always the one that matters.
pub fn mark_pending(app: &tauri::AppHandle, label: &str, tool_name: &str) {
    let mut guard = sessions().lock().unwrap();
    let Some(session) = guard.get_mut(label) else {
        append_debug_log(app, &format!("permission-request: unknown terminal label '{label}', dropped"));
        return;
    };
    session.pending_tool_name = Some(tool_name.to_string());
    session.pending_since = Some(std::time::Instant::now());
}

// Called by server.rs's resolve_decision once the hook's HTTP response has
// actually been sent (approve, deny, or an AskUserQuestion answer) — clears
// the display flag set by mark_pending. Silently a no-op for an unknown or
// already-cleared label (e.g. the request had no resolvable terminal label
// to begin with).
pub fn clear_pending(label: &str) {
    let mut guard = sessions().lock().unwrap();
    if let Some(session) = guard.get_mut(label) {
        session.pending_tool_name = None;
        session.pending_since = None;
    }
}

#[tauri::command]
pub fn write_to_pty(window: tauri::WebviewWindow, input: String) -> Result<(), String> {
    let label = window.label().to_string();
    let mut guard = sessions().lock().unwrap();
    let session = guard.get_mut(&label).ok_or("no active terminal session")?;
    session
        .writer
        .write_all(input.as_bytes())
        .map_err(|e| format!("write failed: {e}"))?;
    session.writer.flush().map_err(|e| format!("flush failed: {e}"))
}

#[tauri::command]
pub fn resize_pty(window: tauri::WebviewWindow, cols: u16, rows: u16) -> Result<(), String> {
    let label = window.label().to_string();
    let guard = sessions().lock().unwrap();
    let session = guard.get(&label).ok_or("no active terminal session")?;
    session
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("resize failed: {e}"))
}

#[derive(Serialize, Clone)]
pub struct SessionSummary {
    session_id: String,
    agent: Option<String>,
    activity: Option<String>,
    has_pending: bool,
    // Seconds the current permission prompt has been sitting unanswered —
    // None when there's no pending prompt. Lets the frontend distinguish a
    // fresh ask ("waiting") from one that's been ignored a while
    // ("forgotten") without tracking wall-clock time itself.
    pending_since_secs: Option<u64>,
    // Seconds since `activity` last actually changed — lets the frontend's
    // "coding" ambient mood (signals.js) tell a terminal with genuinely
    // ongoing output apart from one that was opened once and then left
    // sitting on a static prompt.
    last_activity_secs: Option<u64>,
}

// Backs the mascot's "Show all agents" list — every pooled terminal window
// eagerly starts its own PTY session as soon as its (hidden) webview loads
// (see terminal.js), so the SESSIONS map alone would include slots the user
// never actually opened. Filtered down to windows that are currently VISIBLE
// on screen, matching what the user actually thinks of as "the terminals
// I've opened" rather than every pre-spawned pool slot.
#[tauri::command]
pub fn list_agent_sessions(app: tauri::AppHandle) -> Vec<SessionSummary> {
    let guard = sessions().lock().unwrap();
    let mut out: Vec<SessionSummary> = guard
        .iter()
        .filter(|(label, _)| {
            app.get_webview_window(label)
                .map(|w| w.is_visible().unwrap_or(false))
                .unwrap_or(false)
        })
        .map(|(label, s)| SessionSummary {
            session_id: label.clone(),
            // Single-agent app now — this just means "something's happened
            // in this terminal" rather than picking Claude out of several
            // possible CLIs.
            agent: s.last_activity.as_ref().map(|_| "claude".to_string()),
            activity: s.last_activity.clone(),
            has_pending: s.pending_tool_name.is_some(),
            pending_since_secs: s.pending_since.map(|t| t.elapsed().as_secs()),
            last_activity_secs: s.last_activity_at.map(|t| t.elapsed().as_secs()),
        })
        .collect();
    out.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    out
}

// Called on app quit (see tray.rs) — kills every pooled session's shell
// child process so none of them linger after the widget exits.
pub fn stop_terminal_session() {
    let mut guard = sessions().lock().unwrap();
    for (_, mut session) in guard.drain() {
        let _ = session.child.kill();
    }
}
