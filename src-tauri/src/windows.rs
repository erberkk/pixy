use tauri::Manager;

// Creating a webview window at runtime (after the event loop has started)
// reliably hangs in this environment — WRY's WebView2 controller init never
// completes for a window built from inside a command/menu handler (0% CPU,
// truly stuck waiting, not a busy loop; reproduced even deferring the build
// via run_on_main_thread to the next loop iteration). So every window is
// instead declared statically in tauri.conf.json (all `visible: false`,
// shown/hidden here) and never built dynamically.
#[tauri::command]
pub fn open_notepad(app: tauri::AppHandle) {
    show_window(&app, "notepad");
    if let Some(window) = app.get_webview_window("notepad") {
        let _ = window.unminimize();
    }
}

#[tauri::command]
pub fn hide_notepad(app: tauri::AppHandle) {
    hide_window(&app, "notepad");
}

fn show_window(app: &tauri::AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn hide_window(app: &tauri::AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.hide();
    }
}

// Hides the mascot window itself (system-tray-style minimize, same as the
// tray menu's "Hide mascot" item) — NOT app.exit(), so background watchers
// (github merge/issue polling, terminal sessions, etc.) keep running and the
// window can be brought back via the tray icon. For "I don't want to see
// this while gaming/watching something" without actually quitting.
#[tauri::command]
pub fn hide_mascot(app: tauri::AppHandle) {
    hide_window(&app, "mascot");
}

#[tauri::command]
pub fn open_settings(app: tauri::AppHandle) {
    show_window(&app, "settings");
}

#[tauri::command]
pub fn hide_settings(app: tauri::AppHandle) {
    hide_window(&app, "settings");
}

// Fixed pool of pre-declared terminal windows (see tauri.conf.json) — one
// per concurrently-running Claude Code session the user wants open at once.
// Dynamic window creation hangs in this environment (see module comment
// above), so "open another terminal" means "reveal the next not-yet-visible
// slot in this pool" rather than actually creating a new window. 16 is not
// a "real" limit meant to constrain usage — nobody realistically runs that
// many sessions at once — it's just how many hidden WebView2 instances get
// spawned at app startup (each has a real memory/CPU cost even while hidden), so
// the number is generous rather than unbounded. Bump this (and mirror the
// new labels into tauri.conf.json + capabilities/default.json) if it's
// ever actually hit.
const TERMINAL_POOL: &[&str] = &[
    "terminal", "terminal2", "terminal3", "terminal4", "terminal5", "terminal6", "terminal7",
    "terminal8", "terminal9", "terminal10", "terminal11", "terminal12", "terminal13", "terminal14",
    "terminal15", "terminal16",
];

#[tauri::command]
pub fn open_terminal(app: tauri::AppHandle) {
    for label in TERMINAL_POOL {
        if let Some(window) = app.get_webview_window(label) {
            if !window.is_visible().unwrap_or(false) {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
                return;
            }
        }
    }
    // All pool slots are already open — just bring the last one to the
    // front rather than silently doing nothing.
    if let Some(window) = app.get_webview_window(TERMINAL_POOL[TERMINAL_POOL.len() - 1]) {
        let _ = window.set_focus();
    }
}

// Invoked from the mascot's "show all agents" list — brings a specific
// pooled terminal window (by label) to the front, e.g. clicking a row for a
// session that isn't currently blocked on a permission decision.
#[tauri::command]
pub fn focus_terminal_session(app: tauri::AppHandle, label: String) {
    show_window(&app, &label);
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.unminimize();
    }
}

// Invoked from inside a terminal window itself (its own close button), so
// the calling window IS the one to hide — no need to look it up by a fixed
// label like the other hide_* commands, since there are several now.
#[tauri::command]
pub fn hide_terminal(window: tauri::WebviewWindow) {
    let _ = window.hide();
}

// The OS window is created at a fixed size (see tauri.conf.json) big enough
// for the largest expanded content and never resized at runtime — an earlier
// version called window.set_size() on every state change, but the outer
// window would visibly grow (confirmed via outer_size()) while the embedded
// webview surface silently kept rendering at the old size, leaving most of
// the "expanded" window blank/transparent. Collapsing/expanding is instead
// done entirely with CSS width/height transitions on the inner #mascot div,
// which sidesteps that native-resize/webview-repaint mismatch entirely.
pub fn position_top_center(window: &tauri::WebviewWindow) {
    if let Ok(Some(monitor)) = window.current_monitor() {
        let screen = monitor.size();
        let scale = monitor.scale_factor();
        if let Ok(win_size) = window.outer_size() {
            let margin_top = (10.0 * scale) as i32;
            let x = (screen.width as i32 - win_size.width as i32) / 2;
            let _ = window.set_position(tauri::PhysicalPosition::new(x.max(0), margin_top));
        }
    }
}
