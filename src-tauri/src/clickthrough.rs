use std::sync::Mutex;
use std::time::Duration;

// Window-relative, physical-pixel bounds of whatever's currently visible/
// interactive in the mascot window (the pill, an open quick menu, a notice
// card, ...) — reported from the frontend (see main.js's reportHotRect())
// every time the visible layout changes. Anywhere outside this rect, clicks
// should pass through the window to whatever's behind it instead of being
// swallowed by this always-on-top overlay's much larger, mostly-transparent
// window rectangle.
static HOT_RECT: Mutex<(f64, f64, f64, f64)> = Mutex::new((0.0, 0.0, 0.0, 0.0));

#[tauri::command]
pub fn set_hot_rect(x: f64, y: f64, width: f64, height: f64) {
    *HOT_RECT.lock().unwrap() = (x, y, width, height);
}

// The hot-rect approach works fine for click-triggered UI (a notice card,
// the quick-menu) since it only needs to be accurate at the instant of a
// click. A continuously-hovered, size-changing panel (the Spotify panel)
// is a different story: every geometry change (the pill growing) has to
// race this poll loop's ~40ms cadence to stay accurate, and any mismatch —
// even for a moment — flips the window to click-through right as the
// cursor sits over the newly-revealed area, which stops mouse events from
// reaching the webview entirely and reads as the whole panel flickering in
// and out. Pausing the hot-rect check for the duration of that one hover
// interaction (falling back to "always interactive") sidesteps the race
// completely instead of trying to win it.
static PAUSED: Mutex<bool> = Mutex::new(false);

#[tauri::command]
pub fn set_click_through_paused(paused: bool) {
    *PAUSED.lock().unwrap() = paused;
}

// CSS `pointer-events`/transparency only affects DOM hit-testing inside the
// webview — it does nothing to stop the OS from routing clicks to this
// window everywhere within its rectangle. The actual fix (the standard one
// for always-on-top overlay widgets) is toggling the native
// "ignore cursor events" flag based on where the real OS cursor is, polled
// from a background thread — the webview itself can't detect hover once
// the window starts ignoring cursor events, so this can't be done from JS
// mousemove listeners alone.
#[cfg(windows)]
pub fn start_click_through_watcher(window: tauri::WebviewWindow) {
    std::thread::spawn(move || {
        let mut currently_ignoring: Option<bool> = None;
        loop {
            if *PAUSED.lock().unwrap() {
                if currently_ignoring != Some(false) {
                    let _ = window.set_ignore_cursor_events(false);
                    currently_ignoring = Some(false);
                }
            } else if let (Ok(win_pos), Some((cx, cy))) = (window.outer_position(), cursor_pos()) {
                let (rx, ry, rw, rh) = *HOT_RECT.lock().unwrap();
                let local_x = (cx - win_pos.x) as f64;
                let local_y = (cy - win_pos.y) as f64;
                let inside =
                    local_x >= rx && local_x <= rx + rw && local_y >= ry && local_y <= ry + rh;
                let should_ignore = !inside;
                if currently_ignoring != Some(should_ignore) {
                    let _ = window.set_ignore_cursor_events(should_ignore);
                    currently_ignoring = Some(should_ignore);
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    });
}

#[cfg(windows)]
fn cursor_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point).ok()? };
    Some((point.x, point.y))
}

#[cfg(not(windows))]
pub fn start_click_through_watcher(_window: tauri::WebviewWindow) {}
