use tauri::Manager;

// Creating a webview window at runtime (after the event loop has started)
// reliably hangs in this environment — WRY's WebView2 controller init never
// completes for a window built from inside a command/menu handler (0% CPU,
// truly stuck waiting, not a busy loop; reproduced even deferring the build
// via run_on_main_thread to the next loop iteration). So every window is
// instead declared statically in tauri.conf.json (all `visible: false`,
// shown/hidden here) and never built dynamically.
#[tauri::command]
pub fn open_workspace(app: tauri::AppHandle) {
    show_window(&app, "workspace");
    if let Some(window) = app.get_webview_window("workspace") {
        let _ = window.unminimize();
    }
}

#[tauri::command]
pub fn hide_workspace(app: tauri::AppHandle) {
    hide_window(&app, "workspace");
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

// The one "open" here that isn't one of our own windows: the user's normal
// terminal.
//
// This replaces a pool of sixteen PTY-backed terminal windows the app used to
// own. Those existed so it could watch Claude Code by reading its rendered
// screen; the hooks made that unnecessary, and they fire from any terminal —
// so all the pool did was spawn sixteen cmd.exe and sixteen conhost.exe at
// every launch whether or not one was ever opened.
//
// Windows Terminal first because it is what a Windows 11 user almost certainly
// means by "terminal", with plain cmd as the fallback for a machine that
// doesn't have it. Started detached, so closing it has nothing to do with us —
// which was the other half of the old design's problem.
#[tauri::command]
pub fn open_system_terminal() {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".to_string());
    if std::process::Command::new("wt.exe")
        .current_dir(&home)
        .spawn()
        .is_ok()
    {
        return;
    }
    // `start` is a cmd builtin, not an executable, hence going through cmd /c.
    // The empty "" is start's title argument — without it, start treats the
    // next quoted token as the title and opens nothing.
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", "cmd"])
        .current_dir(&home)
        .spawn();
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
