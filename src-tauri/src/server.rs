use std::thread;

use serde::Deserialize;
use serde_json::json;
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
        Ok(event) => {
            let _ = app_handle.emit("mascot-state", event.state);
        }
        Err(err) => {
            eprintln!("ignoring malformed mascot event payload: {err}");
        }
    }

    let _ = request.respond(tiny_http::Response::from_string("ok"));
}

// Notification-only: Claude Code's interactive PermissionRequest decision only
// seems to apply to headless/auto-mode runs, not sessions with a human
// answering in the editor/terminal directly. So this never blocks — it just
// surfaces the request in the widget and immediately defers back to whatever
// Claude Code would normally do ("ask" = show its own prompt as usual).
fn handle_decide_request(mut request: tiny_http::Request, app_handle: &tauri::AppHandle) {
    // The hook's stdin (tool name/input) is intentionally not parsed or shown —
    // a fixed "Claude is waiting for your permission" notice covers every case
    // instead of trying to render arbitrary command/file-path content.
    let mut body = String::new();
    let _ = request.as_reader().read_to_string(&mut body);

    let _ = app_handle.emit("mascot-state", "waiting_permission");

    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("valid header");
    let response = tiny_http::Response::from_string(json!({ "decision": "ask" }).to_string())
        .with_header(header);
    let _ = request.respond(response);
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
                if request.url() == "/decide" {
                    handle_decide_request(request, &app_handle);
                } else {
                    handle_event_request(request, &app_handle);
                }
            });
        }
    });
}
