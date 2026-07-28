use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;

use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use tauri::Emitter;

const EVENT_PORT: u16 = 47623;

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
            // so only this one should feed the pip ambient system's
            // "coding" mood (signals.js) — treating every hook alike
            // previously left "coding" showing for a while right after a
            // Stop/Notification, which is backwards.
            "idle" => {
                let _ = app_handle.emit("claude-hook-activity", ());
                let _ = app_handle.emit("mascot-state", "idle");
            }
            // UserPromptSubmit — purely an ambient/pip-mood signal
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
    // Which pooled terminal (WIDGET_TERMINAL_LABEL) this came from, if
    // resolvable — lets resolve_decision clear that terminal's display flag
    // (agent/terminal.rs's clear_pending) once answered.
    label: Option<String>,
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
// answer from the widget with no PTY keystroke involved at all; `label`
// (WIDGET_TERMINAL_LABEL, set per pooled terminal in agent/terminal.rs, inherited
// down the process tree) is used only to flag that terminal's display state
// (agent/terminal.rs's mark_pending) and to focus/attribute the card to the right
// window — the decision itself doesn't need it.
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

    if let Some(label) = &label {
        crate::agent::terminal::mark_pending(app_handle, label, &tool_name);
    }

    pending().lock().unwrap().insert(
        request_id.clone(),
        PendingDecision {
            request,
            label: label.clone(),
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

    if let Some(label) = &entry.label {
        crate::agent::terminal::clear_pending(label);
    }
    Ok(())
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
// session correlation for requests with no resolvable terminal label (e.g.
// "External session" — Claude running outside this app's pooled terminals),
// so there's no reliable automatic signal to flush on. This command is the
// manual equivalent: a "Dismiss" affordance that discards the card and
// answers "deny" (a safe default — never silently allow something nobody
// actually reviewed) so the underlying connection is freed either way.
#[tauri::command]
pub fn dismiss_permission(request_id: String) -> Result<(), String> {
    resolve_decision(&request_id, "deny", None)
}

pub fn start_event_server(app_handle: tauri::AppHandle) {
    thread::spawn(move || {
        let addr = format!("127.0.0.1:{EVENT_PORT}");
        let server = match tiny_http::Server::http(&addr) {
            Ok(server) => server,
            Err(err) => {
                eprintln!("failed to bind mascot event server on {addr}: {err}");
                return;
            }
        };

        for request in server.incoming_requests() {
            let app_handle = app_handle.clone();
            thread::spawn(move || {
                if request.url().starts_with("/decide") {
                    handle_decide_request(request, &app_handle);
                } else {
                    handle_event_request(request, &app_handle);
                }
            });
        }
    });
}
