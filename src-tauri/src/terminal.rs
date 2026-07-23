use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{Emitter, Manager};

struct PendingPrompt {
    agent: String,
    preview: Vec<String>,
    approve_keys: Vec<u8>,
    deny_keys: Vec<u8>,
}

struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    pending: Option<PendingPrompt>,
    // Last agent CLI identified in this session (set once a prompt is first
    // detected) and the most recent non-chrome line visible on screen — lets
    // the "show all agents" list summarize what EVERY pooled terminal is
    // doing right now, not just the ones currently blocked on a decision.
    last_agent: Option<String>,
    last_activity: Option<String>,
    // Last screen snapshot that LOOKED like an interactive question but
    // matched none of detect_prompt's known patterns — e.g. Codex/Cursor
    // wording we couldn't verify ourselves. Deduped against so the debug log
    // doesn't fill up with the same unmatched screen repeated on every
    // render; only a genuinely new unmatched prompt gets logged.
    last_unmatched_snapshot: Option<String>,
}

// Keyed by terminal window label ("terminal", "terminal2", ...) — the user
// can run a different coding-agent CLI in each pooled window at the same
// time (Claude in one, Antigravity in another, etc.). Dynamic window
// creation hangs in this environment (see windows.rs), so the pool is a
// fixed set of pre-declared windows rather than one spawned per session.
// HashMap::new() isn't const, so a OnceLock lazily builds the map on first
// use instead of a plain `static ... = Mutex::new(HashMap::new())`.
static SESSIONS: OnceLock<Mutex<HashMap<String, PtySession>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, PtySession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Serialize, Clone)]
struct PermissionRequestPayload {
    // Which terminal window/session this came from — the mascot can have
    // several of these pending at once (one per pooled terminal window),
    // so approve/deny need to say which one they're resolving.
    session_id: String,
    agent: String,
    // The frontend renders line 0 as a "tool" badge and the rest as a
    // monospace body — an array instead of one flat string so the UI can
    // give the tool header actual visual hierarchy instead of everything
    // reading as one undifferentiated blob of stacked text.
    preview: Vec<String>,
}

fn debug_log_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("terminal-debug.log")
}

// Same pattern as github.rs's debug log — records every detected prompt
// (which profile matched + the raw preview text) so the still-uncertain
// Codex/Cursor/Antigravity patterns can be corrected from real captured
// output instead of guessed twice (see plan doc's confidence table).
fn append_debug_log(app: &tauri::AppHandle, entry: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(debug_log_path(app))
    {
        let _ = writeln!(file, "{entry}\n");
    }
}

// Finds the highest "N. " option number in the buffer — Claude Code's
// permission prompt option count varies (2 or 3 depending on tool type), so
// the deny option's digit is parsed from the actual visible text instead of
// hardcoded.
fn highest_numbered_option(text: &str) -> Option<u32> {
    let mut max_n = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let rest = &trimmed[digits.len()..];
        if rest.starts_with('.') {
            if let Ok(n) = digits.parse::<u32>() {
                max_n = Some(max_n.map_or(n, |m: u32| m.max(n)));
            }
        }
    }
    max_n
}

// Box-drawing borders and spinner glyphs are pure rendering chrome, not tool
// content — this text comes from xterm.js's own rendered screen buffer (see
// terminal.js's reportSnapshot), so it's already a correct single frame with
// no carriage-return redraw duplication; only per-line noise remains to
// filter (borders, spinner glyphs, status chrome lines below).
const BORDER_CHARS: &[char] = &[
    '─', '│', '╭', '╮', '╰', '╯', '├', '┤', '┬', '┴', '┼', '═', '║', '✢', '✶', '✻', '✽', '✿', '◐',
    '◑', '◒', '◓', '⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏', '❯',
];

// Fixed status-line chrome (spinner word + elapsed time/tokens, mode
// indicators) rather than actual tool content — always safe to drop
// entirely regardless of which CLI/random spinner verb produced it. Claude
// Code's status lines ("9s · ↓ 433 tokens · thought for 2s", "manual mode on
// · esc to interrupt", "medium · /effort") all use "·" as a visual
// separator, which real command text/descriptions essentially never
// contains — a much more robust single signal than enumerating every
// possible status phrase (they vary with model/effort/reasoning settings).
fn is_chrome_line(line: &str) -> bool {
    let l = line.to_lowercase();
    l == "waiting…"
        || l.contains('·')
        || l.contains("esc to cancel")
        || l.contains("to auto-approve") // "shift+tab to auto-approve file edits" hint
        || l == "pending edit" // Antigravity status line above the diff block
        || l.starts_with("↑/↓") // "↑/↓ Navigate" menu hint
        || l.ends_with('…') // trailing "<RandomVerb>…" spinner fragment with nothing else on the line
}

// A full-width horizontal rule (or the top/bottom edge of a box) that these
// CLIs draw immediately around an approval block. This is the single most
// reliable boundary between the block that needs a decision and the terminal
// scrollback above it (shell banner, CLI splash art, earlier turns) — a real
// content line is essentially never ≥70% rule characters.
fn is_divider_line(line: &str) -> bool {
    const RULE_CHARS: &[char] = &[
        '─', '═', '━', '—', '–', '-', '╭', '╮', '╰', '╯', '┏', '┓', '┗', '┛', '┄', '┅', '┈', '┉',
    ];
    let trimmed = line.trim();
    let rule = trimmed.chars().filter(|c| RULE_CHARS.contains(c)).count();
    // At least 6 rule chars AND nothing but rule chars / spaces — this cleanly
    // covers both a solid "──────" rule and a spaced "- - - -" dashed one
    // (Claude's edit prompt draws the latter above its question), while never
    // matching a real content line that merely happens to contain a dash.
    rule >= 6 && trimmed.chars().all(|c| c == ' ' || RULE_CHARS.contains(&c))
}

// Isolates just the approval block out of the full screen snapshot, using the
// divider(s) drawn around it. Two layouts are handled from the one rule:
//   * a single divider ABOVE the block, question below it (Antigravity) →
//     take everything between that divider and the question;
//   * the block fully WRAPPED in a box, question below the box (Claude) → the
//     last divider before the question is the box's BOTTOM edge with nothing
//     but the question under it, so fall back to the box interior (between the
//     previous divider and this one).
// With no divider at all, returns the whole range so the caller's tool-header
// trim still applies (unverified agents that don't draw one).
fn block_bounds(all_lines: &[&str], boundary: usize) -> (usize, usize) {
    let dividers: Vec<usize> = all_lines[..boundary]
        .iter()
        .enumerate()
        .filter(|(_, l)| is_divider_line(l))
        .map(|(i, _)| i)
        .collect();
    match dividers.last() {
        None => (0, boundary),
        Some(&last) => {
            let has_content_below = !clean_lines(&all_lines[last + 1..boundary]).is_empty();
            if has_content_below {
                (last + 1, boundary)
            } else if dividers.len() >= 2 {
                (dividers[dividers.len() - 2] + 1, last)
            } else {
                (0, last)
            }
        }
    }
}

fn clean_lines(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        let stripped: String = line.chars().filter(|c| !BORDER_CHARS.contains(c)).collect();
        let mut trimmed = stripped.trim();
        // Drop a trailing "(ctrl+o to expand)"-style affordance hint the CLIs
        // append to tool-call lines — it's UI chrome, not part of the action.
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

// Cuts away the assistant's preceding prose/reasoning ("I'll trigger a tool
// call that...") and keeps only from the actual tool header onward (e.g.
// "Bash command" + the command itself) — without this, the preview shows
// the whole chat turn's narration instead of just the thing needing a
// decision.
fn is_tool_header(line_lower: &str) -> bool {
    const EXACT: &[&str] = &[
        "bash command",
        "write",
        "edit",
        "multiedit",
        "read",
        "webfetch",
        "file access",
        "task",
        "search",
    ];
    EXACT.contains(&line_lower) || line_lower.starts_with("requested permission")
}

fn trim_to_tool_header(lines: &[String]) -> &[String] {
    for (i, line) in lines.iter().enumerate() {
        if is_tool_header(&line.to_lowercase()) {
            return &lines[i..];
        }
    }
    lines
}

// Caps both the number of lines and each individual line's length — kept as
// separate lines (not one joined/truncated blob) so the frontend can give
// the first line (tool header) its own styling instead of everything
// reading as flat stacked text.
fn cap_lines(lines: &[String], max_lines: usize, max_line_len: usize) -> Vec<String> {
    let start = lines.len().saturating_sub(max_lines);
    lines[start..]
        .iter()
        .map(|l| {
            if l.chars().count() > max_line_len {
                let truncated: String = l.chars().take(max_line_len).collect();
                format!("{truncated}…")
            } else {
                l.clone()
            }
        })
        .collect()
}

fn preview_tail(text: &str, max_lines: usize, max_line_len: usize) -> Vec<String> {
    let all_lines: Vec<&str> = text.lines().collect();
    let (top, bottom) = block_bounds(&all_lines, all_lines.len());
    let cleaned = clean_lines(&all_lines[top..bottom]);
    let trimmed: &[String] = if top == 0 { trim_to_tool_header(&cleaned) } else { &cleaned };
    cap_lines(trimmed, max_lines, max_line_len)
}

// Stops the preview right before the prompt's own question line (e.g. "Do
// you want to proceed?") since that question + its numbered options are
// redundant with our own Approve/Deny buttons — only the tool/command
// context above it is useful. That context is further narrowed to just the
// approval block (see block_bounds) so shell banners / CLI splash art /
// earlier turns above the block never leak into the preview.
fn preview_before(buffer: &str, needle_lower: &str, max_lines: usize, max_line_len: usize) -> Vec<String> {
    let lower = buffer.to_lowercase();
    let lower_lines: Vec<&str> = lower.lines().collect();
    let all_lines: Vec<&str> = buffer.lines().collect();
    let boundary = lower_lines
        .iter()
        .rposition(|l| l.contains(needle_lower))
        .unwrap_or(all_lines.len())
        .min(all_lines.len());
    let (top, bottom) = block_bounds(&all_lines, boundary);
    let cleaned = clean_lines(&all_lines[top..bottom]);
    // When a divider bounded the top, the block is already tight — a further
    // tool-header trim risks wrongly cutting into it, so only apply that trim
    // in the no-divider fallback.
    let trimmed: &[String] = if top == 0 { trim_to_tool_header(&cleaned) } else { &cleaned };
    cap_lines(trimmed, max_lines, max_line_len)
}

// Claude and Antigravity both use the same "❯ N. Option text" numbered-menu
// convention (common across Ink/blessed-style CLI UIs) — same handling for
// both: send the digit key directly (no Enter needed, confirmed for Claude;
// Antigravity unverified but shares the identical menu style so assumed to
// match — refine from the debug log if wrong), deny by the highest visible
// option number rather than a hardcoded one since option count varies.
fn numbered_prompt(buffer: &str, needle_lower: &str, agent: &str) -> PendingPrompt {
    let deny_n = highest_numbered_option(buffer).unwrap_or(2);
    PendingPrompt {
        agent: agent.to_string(),
        // Capped generously (was 6) — the preview now renders as a real
        // diff view (see main.js) with its own internal scrollbar, so a
        // multi-line Edit diff no longer needs to have its earlier "-"
        // removal rows tail-truncated away just to fit a small flat text box.
        preview: preview_before(buffer, needle_lower, 16, 120),
        approve_keys: b"1".to_vec(),
        deny_keys: deny_n.to_string().into_bytes(),
    }
}

// Identifies which agent CLI is running from whatever's currently on screen,
// independent of a permission prompt ever appearing — a session sitting idle
// or just working (no prompt yet) would otherwise show as "Idle" in the
// "show all agents" list forever, even though a real CLI is clearly running.
// Scans the FULL buffer (not just the last line) since these signals are
// typically a persistent status-footer/banner rather than the most recent
// line specifically (e.g. Antigravity's model name stays pinned in its
// footer every frame — see detect_prompt's antigravity/claude split above).
fn detect_agent_hint(lower: &str) -> Option<&'static str> {
    if lower.contains("gemini") || lower.contains("antigravity") {
        Some("antigravity")
    } else if lower.contains("claude code") {
        Some("claude")
    } else if lower.contains("codex") {
        Some("codex")
    } else if lower.contains("cursor-agent") || lower.contains("cursor agent") {
        Some("cursor")
    } else {
        None
    }
}

// Per-agent prompt detection — literal substring checks (lowercased) rather
// than regex, since the exact wording per tool is uncertain for 3 of the 4
// supported CLIs (see plan doc's confidence table) and hand-rolled matching
// keeps the dependency list lean. Order matters: more specific patterns are
// checked before the generic y/n fallback so a Codex/Cursor-style prompt
// isn't accidentally swallowed by a broader check or vice versa.
fn detect_prompt(buffer: &str) -> Option<PendingPrompt> {
    let lower = buffer.to_lowercase();

    if lower.contains("allow access to this file?") {
        return Some(numbered_prompt(buffer, "allow access to this file?", "antigravity"));
    }
    // Antigravity's actual file-edit confirmation ("Accept this file edit?",
    // "1. Yes, accept this change" / "2. No, reject this change") — confirmed
    // from real captured output, replacing an earlier guessed wording that
    // never matched. Matched by prefix/suffix rather than the exact string so
    // sibling prompts sharing the same shape (e.g. a command-accept variant)
    // aren't missed if the exact wording differs slightly.
    if let Some(needle) = lower
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("accept this") && l.ends_with('?'))
    {
        return Some(numbered_prompt(buffer, needle, "antigravity"));
    }
    if lower.contains("run without sandbox restrictions") {
        return Some(numbered_prompt(buffer, "run without sandbox restrictions", "antigravity"));
    }
    if lower.contains("run in sandbox") {
        return Some(numbered_prompt(buffer, "run in sandbox", "antigravity"));
    }

    // Claude & Antigravity end an interactive approval with a "Do you want
    // to ...?" question — the wording varies by tool: "proceed?" for a
    // command, "make this edit to X?" for a file edit, "create X?" for a new
    // file, etc. Matching the whole family by the "do you want to" opener
    // (rather than one exact phrasing) is why Claude's *edit* prompt was
    // being missed before while its *bash* prompt worked. Searched bottom-up
    // so the actual trailing question is used as the preview cut-point, not
    // an earlier mention of the same words in the scrollback.
    if let Some(needle) = lower
        .lines()
        .map(str::trim)
        .rev()
        .find(|l| l.starts_with("do you want to") && l.ends_with('?'))
    {
        // Antigravity always shows its model name ("Gemini ...") pinned in
        // its status footer every frame, which is used to tell the two apart
        // instead of assuming Claude.
        let agent = if lower.contains("gemini") || lower.contains("antigravity") {
            "antigravity"
        } else {
            "claude"
        };
        return Some(numbered_prompt(buffer, needle, agent));
    }

    // "Codex wants to run <command>" with "Yes" / "Always" / "No, provide
    // feedback" options — sourced from a real user's pasted terminal output
    // in openai/codex issue #2860 (second-hand, never captured directly by
    // this app), so treated with the same numbered-menu convention as
    // Claude/Antigravity as a best guess, refine from the debug log once
    // actually exercised. Checked before the older "allow command?"/y-n
    // guess so this more specific (and better-sourced) match wins if both
    // somehow appear.
    if lower.contains("codex wants to run") {
        return Some(numbered_prompt(buffer, "codex wants to run", "codex"));
    }

    if lower.contains("allow command?") || lower.contains("[y/n") {
        return Some(PendingPrompt {
            agent: "codex".to_string(),
            preview: preview_before(buffer, "allow command?", 16, 120),
            approve_keys: b"y\r".to_vec(),
            deny_keys: b"n\r".to_vec(),
        });
    }

    // Generic fallback — catches Cursor's plain y/n prompt and anything else
    // not specifically recognized above.
    for line in buffer.lines().rev().take(5) {
        let l = line.to_lowercase();
        if l.contains('?') && l.contains("y/n") {
            return Some(PendingPrompt {
                agent: "cli".to_string(),
                preview: preview_tail(buffer, 16, 120),
                approve_keys: b"y\r".to_vec(),
                deny_keys: b"n\r".to_vec(),
            });
        }
    }

    None
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

    let cmd = CommandBuilder::new_default_prog();
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
                pending: None,
                last_agent: None,
                last_activity: None,
                last_unmatched_snapshot: None,
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
            // Just forwards raw bytes for xterm.js to render — prompt
            // detection runs on xterm's own rendered screen buffer instead
            // (see report_terminal_text), not on this raw stream, since
            // reconstructing correct on-screen text from raw ANSI bytes
            // ourselves would mean reimplementing a terminal emulator.
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
#[tauri::command]
pub fn report_terminal_text(app: tauri::AppHandle, window: tauri::WebviewWindow, text: String) -> Result<(), String> {
    let label = window.label().to_string();
    let mut guard = sessions().lock().unwrap();
    let session = guard.get_mut(&label).ok_or("no active terminal session")?;

    // Refreshed on every render regardless of pending-prompt state, so "show
    // all agents" has something current to display for sessions that are
    // just working (not currently blocked on a decision) too.
    let lines: Vec<&str> = text.lines().collect();
    let cleaned = clean_lines(&lines);
    if let Some(last) = cleaned.last() {
        session.last_activity = Some(last.clone());
    }
    // Only fills in an agent identity that isn't already known — a prompt
    // actually being detected (below) is a stronger signal than this
    // heuristic and should never be overwritten by it.
    if session.last_agent.is_none() {
        if let Some(agent) = detect_agent_hint(&text.to_lowercase()) {
            session.last_agent = Some(agent.to_string());
        }
    }

    if session.pending.is_some() {
        return Ok(()); // already showing one, don't re-detect until resolved
    }
    if let Some(prompt) = detect_prompt(&text) {
        session.last_agent = Some(prompt.agent.clone());
        let payload = PermissionRequestPayload {
            session_id: label.clone(),
            agent: prompt.agent.clone(),
            preview: prompt.preview.clone(),
        };
        let log_line = format!("[{label}] detected agent={} preview=\n{}", prompt.agent, prompt.preview.join("\n"));
        session.pending = Some(prompt);
        drop(guard);
        append_debug_log(&app, &log_line);
        // Broadcast — only the mascot window listens for this event, so
        // emit (not emit_to) is fine; the payload's session_id is what lets
        // the mascot tell multiple concurrent requests apart.
        let _ = app.emit("mascot-permission-request", payload);
    } else if let Some(last) = cleaned.last() {
        // No known pattern matched, but this still looks like it could be an
        // interactive question (ends in '?', or shows a y/n-style hint) —
        // log it (deduped so an unchanging screen doesn't spam every render)
        // so an unrecognized Codex/Cursor/etc. prompt leaves a real trace to
        // fix from, instead of silently vanishing with no card AND no record.
        let looks_like_prompt = last.trim_end().ends_with('?') || last.to_lowercase().contains("y/n");
        if looks_like_prompt && session.last_unmatched_snapshot.as_deref() != Some(last.as_str()) {
            session.last_unmatched_snapshot = Some(last.clone());
            append_debug_log(
                &app,
                &format!("[{label}] unmatched possible prompt (no card shown): {last}"),
            );
        }
    }
    Ok(())
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

fn write_keys(session_id: &str, keys: &[u8]) -> Result<(), String> {
    let mut guard = sessions().lock().unwrap();
    let session = guard.get_mut(session_id).ok_or("no active terminal session")?;
    session
        .writer
        .write_all(keys)
        .map_err(|e| format!("write failed: {e}"))?;
    session.writer.flush().map_err(|e| format!("flush failed: {e}"))
}

// approve/deny are invoked from the MASCOT window, not the terminal window
// itself, so (unlike write_to_pty/resize_pty) there's no calling terminal
// window to infer the session from — the mascot passes back the session_id
// it received in the original mascot-permission-request payload instead.
#[tauri::command]
pub fn approve_permission(app: tauri::AppHandle, session_id: String) -> Result<(), String> {
    let keys = {
        let mut guard = sessions().lock().unwrap();
        let session = guard.get_mut(&session_id).ok_or("no active terminal session")?;
        let pending = session.pending.take().ok_or("no pending permission request")?;
        append_debug_log(&app, &format!("[{session_id}] approved agent={}", pending.agent));
        pending.approve_keys
    };
    write_keys(&session_id, &keys)
}

#[tauri::command]
pub fn deny_permission(app: tauri::AppHandle, session_id: String) -> Result<(), String> {
    let keys = {
        let mut guard = sessions().lock().unwrap();
        let session = guard.get_mut(&session_id).ok_or("no active terminal session")?;
        let pending = session.pending.take().ok_or("no pending permission request")?;
        append_debug_log(&app, &format!("[{session_id}] denied agent={}", pending.agent));
        pending.deny_keys
    };
    write_keys(&session_id, &keys)
}

#[derive(Serialize, Clone)]
pub struct SessionSummary {
    session_id: String,
    agent: Option<String>,
    activity: Option<String>,
    has_pending: bool,
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
            agent: s.last_agent.clone(),
            activity: s.last_activity.clone(),
            has_pending: s.pending.is_some(),
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
