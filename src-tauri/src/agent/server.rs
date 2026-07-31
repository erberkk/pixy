use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use tauri::Emitter;

#[derive(Deserialize)]
struct MascotEvent {
    state: String,
}

fn handle_event_request(mut request: tiny_http::Request, app_handle: &tauri::AppHandle) {
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    match serde_json::from_str::<MascotEvent>(&body) {
        Ok(event) => match event.state.as_str() {
            // Only PreToolUse (mapped to state:"idle" here, see SETUP.md's
            // hook table) actually means "a tool is running right now" —
            // Notification (waiting_input) and Stop (turn_done) mean the
            // opposite (Claude is waiting on the human, or just finished),
            // so only this one should feed the pixy ambient system's
            // "coding" mood (signals.js) — treating every hook alike
            // previously left "coding" showing for a while right after a
            // Stop/Notification, which is backwards.
            "idle" => {
                let _ = app_handle.emit("claude-hook-activity", ());
                let _ = app_handle.emit("mascot-state", "idle");
            }
            // UserPromptSubmit — purely an ambient/pixy-mood signal
            // (signals.js's "thinking"), never forwarded as "mascot-state":
            // that event feeds main.js's body.className notice/card system,
            // and a prompt being submitted has nothing to do with that
            // layer (no card should open/close just because the human hit
            // enter).
            "thinking" => {
                let _ = app_handle.emit("claude-user-prompt-submit", ());
            }
            other => {
                let _ = app_handle.emit("mascot-state", other);
            }
        },
        Err(err) => {
            eprintln!("ignoring malformed mascot event payload: {err}");
        }
    }

    let _ = request.respond(tiny_http::Response::from_string("ok"));
}

// Pulls "label" out of a raw request target like "/decide?label=terminal3" —
// tiny_http hands back the full path+query as one string with no built-in
// query parser, and this is the only param this endpoint needs.
fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

#[derive(Deserialize)]
struct DecidePayload {
    tool_name: Option<String>,
    tool_input: Option<JsonValue>,
}

// A PermissionRequest hook invocation held open pending a human decision.
// Confirmed against Claude Code's own docs (code.claude.com/docs/en/hooks)
// that this hook's JSON decision — {"hookSpecificOutput":{"decision":
// {"behavior":"allow"|"deny","updatedInput":...}}} — is honored in a normal
// INTERACTIVE terminal session too, not just headless/auto-mode as an
// earlier version of this file assumed (that assumption was wrong; see
// SETUP.md's design-decisions section for the correction). So the decision
// now travels back as this HTTP response's body — no PTY keystrokes at all.
struct PendingDecision {
    request: tiny_http::Request,
    // When the hook arrived. This registry is now the only source of "somebody
    // is waiting on you" — see pending_permissions below.
    since: std::time::Instant,
}

static PENDING: OnceLock<Mutex<HashMap<String, PendingDecision>>> = OnceLock::new();

fn pending() -> &'static Mutex<HashMap<String, PendingDecision>> {
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_request_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("req-{millis}-{n}")
}

// AskUserQuestion's tool_input carries a `questions` array (question/header/
// options[].label/multiSelect per question) — the exact shape Claude Code's
// own interactive multi-choice prompt renders from. Extracted here so the
// frontend can render the same chips instead of a generic Approve/Deny card;
// None for every other tool.
fn extract_questions(tool_name: &str, tool_input: &JsonValue) -> Option<JsonValue> {
    if tool_name != "AskUserQuestion" {
        return None;
    }
    tool_input.get("questions").cloned()
}

// Claude Code's PermissionRequest hook (see SETUP.md). Held open — not
// responded to here — until the mascot's Approve/Deny (or, for
// AskUserQuestion, its option-chip Submit) calls resolve_decision. Claude
// Code itself blocks the tool call on this HTTP response, so the human can
// answer from the widget with no PTY keystroke involved at all.
//
// `?label=` is still read, but only to caption the card: it used to name one of
// this app's own pooled terminals, and back when it did, it was also what marked
// that terminal as waiting. Those terminals are gone — Claude Code runs wherever
// the user runs it — so the label is now just a string the hook may or may not
// provide, and nothing depends on it.
fn handle_decide_request(mut request: tiny_http::Request, app_handle: &tauri::AppHandle) {
    let label = query_param(request.url(), "label").filter(|l| !l.is_empty());

    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);
    let payload: Option<DecidePayload> = serde_json::from_str(&body).ok();

    let tool_name = payload
        .as_ref()
        .and_then(|p| p.tool_name.clone())
        .unwrap_or_else(|| "Unknown".to_string());
    let tool_input = payload.and_then(|p| p.tool_input).unwrap_or(JsonValue::Null);
    let questions = extract_questions(&tool_name, &tool_input);

    let request_id = next_request_id();

    pending().lock().unwrap().insert(
        request_id.clone(),
        PendingDecision {
            request,
            since: std::time::Instant::now(),
        },
    );

    let _ = app_handle.emit(
        "mascot-permission-request",
        json!({
            "session_id": label,
            "request_id": request_id,
            "tool_name": tool_name,
            "tool_input": tool_input,
            "questions": questions,
        }),
    );
}

// Called from the two Tauri commands below (respond_permission / the
// AskUserQuestion submit path funnels through the same command with
// updated_input set) — builds Claude Code's PermissionRequest decision
// contract and finally responds to the hook's held-open HTTP request.
pub fn resolve_decision(request_id: &str, behavior: &str, updated_input: Option<JsonValue>) -> Result<(), String> {
    let entry = pending()
        .lock()
        .unwrap()
        .remove(request_id)
        .ok_or_else(|| "no pending permission request for this id".to_string())?;

    let mut decision = json!({ "behavior": behavior });
    if let Some(updated_input) = updated_input {
        decision["updatedInput"] = updated_input;
    }
    let body = json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision,
        }
    })
    .to_string();

    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("valid header");
    let response = tiny_http::Response::from_string(body).with_header(header);
    let _ = entry.request.respond(response);
    Ok(())
}

/// How many permission requests are waiting, and how long the oldest has been.
///
/// This is what drives the mascot's "waiting"/"forgotten" poses (pixy/signals.js).
/// It used to be derived from a flag on one of this app's own pooled terminal
/// sessions, keyed by WIDGET_TERMINAL_LABEL — which meant it only ever fired for
/// Claude running *inside* the widget. Reading the registry instead makes it work
/// for any terminal, which is the whole point of the hooks being the integration.
#[derive(serde::Serialize)]
pub struct PendingPermissions {
    count: usize,
    oldest_secs: u64,
}

#[tauri::command]
pub fn pending_permissions() -> PendingPermissions {
    let guard = pending().lock().unwrap();
    PendingPermissions {
        count: guard.len(),
        oldest_secs: guard
            .values()
            .map(|entry| entry.since.elapsed().as_secs())
            .max()
            .unwrap_or(0),
    }
}

// Single entry point for every way the mascot can answer a pending
// PermissionRequest hook: plain Approve/Deny (updated_input: None) and
// AskUserQuestion's Submit (updated_input: Some({"questions":...,
// "answers":...}), approve: true — mirrors AgentGlance's own answer shape).
#[tauri::command]
pub fn respond_permission(request_id: String, approve: bool, updated_input: Option<JsonValue>) -> Result<(), String> {
    resolve_decision(&request_id, if approve { "allow" } else { "deny" }, updated_input)
}

// A pending request whose OWN hook invocation already gave up client-side
// (its `timeout` in settings.json elapsed with nobody answering from the
// widget) leaves its tiny_http::Request sitting in PENDING forever — the
// connection is dead, but nothing tells this server that. AgentGlance avoids
// this by auto-flushing a session's queued decisions the moment a
// forward-progress hook (PostToolUse/Stop/UserPromptSubmit/SessionEnd)
// arrives for it; this app doesn't track those extra hook events or attempt
// session correlation, so there's no reliable automatic signal to flush on. This command is the
// manual equivalent: a "Dismiss" affordance that discards the card and
// answers "deny" (a safe default — never silently allow something nobody
// actually reviewed) so the underlying connection is freed either way.
#[tauri::command]
pub fn dismiss_permission(request_id: String) -> Result<(), String> {
    resolve_decision(&request_id, "deny", None)
}

// Whether the event server actually got its port, kept so the settings window
// can say so. Surfaced in the UI rather than only on stderr because a release
// build is a GUI-subsystem binary with no console attached: a failed bind there
// would be completely invisible, and the symptom — Claude Code hooks silently
// doing nothing — gives no hint of where to look. The likeliest cause is also
// the user having just changed the port, so the place they need to be told is
// the field they changed.
static BIND_STATUS: Mutex<Option<(u16, Option<String>)>> = Mutex::new(None);

#[derive(serde::Serialize)]
pub struct EventServerStatus {
    port: u16,
    listening: bool,
    error: Option<String>,
    /// False until the server thread has had its turn — the settings window can
    /// open before then, and "not listening" would be a lie in that instant.
    known: bool,
}

#[tauri::command]
pub fn get_event_server_status() -> EventServerStatus {
    match BIND_STATUS.lock().ok().and_then(|guard| guard.clone()) {
        Some((port, error)) => EventServerStatus {
            port,
            listening: error.is_none(),
            error,
            known: true,
        },
        None => EventServerStatus {
            port: 0,
            listening: false,
            error: None,
            known: false,
        },
    }
}

/// Whether a request came from a web page rather than from a hook.
///
/// This server binds loopback, which is often mistaken for a boundary. It is
/// not: any page open in any browser can POST to `127.0.0.1` cross-origin. The
/// response is unreadable to the page under CORS, but these endpoints are
/// side-effect endpoints — `/decide` puts a permission card on screen that is
/// indistinguishable from a real one, which is a phishing primitive, and `/`
/// drives mascot state.
///
/// Browsers announce themselves whether they mean to or not. `Origin` is
/// mandatory on a cross-origin POST, and every modern browser attaches
/// `Sec-Fetch-*` to *every* request it makes, same-origin included — and neither
/// can be set by page script, because both are forbidden header names. A hook
/// invocation from Claude Code or `curl` sends neither, so their presence is a
/// reliable signal without asking the user to reconfigure anything.
///
/// What this is not: authentication. It stops a browser, not another program
/// running as the same user. A shared token in the hook URL would cover that
/// too, at the cost of invalidating every existing hook configuration — worth
/// doing, deliberately not bundled into this change. See SECURITY.md.
fn is_browser_request(headers: &[tiny_http::Header]) -> bool {
    headers.iter().any(|header| {
        let name = header.field.as_str().as_str().to_ascii_lowercase();
        name == "origin" || name.starts_with("sec-fetch-")
    })
}

pub fn start_event_server(app_handle: tauri::AppHandle) {
    // Read once, here: the port is what the socket is bound to, so a change only
    // takes effect on the next launch — which is why its schema entry is marked
    // as needing a restart.
    let port = crate::tunables::int(&app_handle, crate::tunables::EVENT_PORT) as u16;
    thread::spawn(move || {
        let addr = format!("127.0.0.1:{port}");
        let server = match tiny_http::Server::http(&addr) {
            Ok(server) => server,
            Err(err) => {
                let message = err.to_string();
                eprintln!("failed to bind mascot event server on {addr}: {message}");
                if let Ok(mut guard) = BIND_STATUS.lock() {
                    *guard = Some((port, Some(message)));
                }
                return;
            }
        };
        if let Ok(mut guard) = BIND_STATUS.lock() {
            *guard = Some((port, None));
        }

        for request in server.incoming_requests() {
            let app_handle = app_handle.clone();
            thread::spawn(move || {
                if is_browser_request(request.headers()) {
                    let _ = request.respond(
                        tiny_http::Response::from_string(
                            "This endpoint is for Claude Code hooks, not for web pages.\n",
                        )
                        .with_status_code(403),
                    );
                    return;
                }
                if request.url().starts_with("/decide") {
                    handle_decide_request(request, &app_handle);
                } else {
                    handle_event_request(request, &app_handle);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Vec<tiny_http::Header> {
        pairs
            .iter()
            .map(|(name, value)| {
                tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes())
                    .expect("test header must be valid")
            })
            .collect()
    }

    /// What Claude Code and curl actually send. If this ever starts returning
    /// true the hook integration is broken, and the symptom would be every hook
    /// silently 403ing.
    #[test]
    fn a_hook_invocation_is_not_treated_as_a_browser() {
        assert!(!is_browser_request(&headers(&[
            ("Host", "127.0.0.1:7423"),
            ("User-Agent", "curl/8.4.0"),
            ("Content-Type", "application/json"),
            ("Accept", "*/*"),
        ])));
        // No headers at all is a legal HTTP/1.0 request.
        assert!(!is_browser_request(&headers(&[])));
    }

    #[test]
    fn a_cross_origin_post_from_a_page_is_recognised() {
        assert!(is_browser_request(&headers(&[
            ("Host", "127.0.0.1:7423"),
            ("Origin", "https://evil.example"),
            ("Content-Type", "text/plain"),
        ])));
    }

    /// The same-origin case, which `Origin` alone would miss: a page served from
    /// this very port sends no Origin on a same-origin POST, but still sends
    /// Sec-Fetch-*.
    #[test]
    fn a_same_origin_browser_request_is_recognised_by_sec_fetch() {
        assert!(is_browser_request(&headers(&[
            ("Host", "127.0.0.1:7423"),
            ("Sec-Fetch-Site", "same-origin"),
            ("Sec-Fetch-Mode", "cors"),
        ])));
    }

    /// Header names are case-insensitive on the wire, so the check must be too —
    /// otherwise it is bypassed by sending `origin:` in lower case.
    #[test]
    fn the_check_does_not_depend_on_header_capitalisation() {
        assert!(is_browser_request(&headers(&[("origin", "null")])));
        assert!(is_browser_request(&headers(&[("SEC-FETCH-MODE", "no-cors")])));
    }
}
