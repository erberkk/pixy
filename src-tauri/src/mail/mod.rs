// Mail: the inbox watcher (watcher), the one-or-two lines shown for a message
// (summary), and the once-a-day card (brief).
//
// The Google API client itself lives in google/gmail.rs — this module is the
// part with opinions in it: what counts as new, what is worth announcing, and
// what a message should say when no model is available to compress it.
pub mod brief;
pub mod summary;
pub mod watcher;

use std::path::PathBuf;

use tauri::Manager;

fn debug_log_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app.path().app_data_dir().expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("mail-watcher-debug.log")
}

/// Appends a line to the mail log.
///
/// Callers must pass counts, ids and reasons only — never a sender, subject or
/// body. See this module's header for why.
///
/// Shared by the watcher and the summarizer rather than owned by the watcher,
/// because the thing hardest to diagnose here turned out to be a summary that
/// silently did not happen: the model timed out, the description fell back to the
/// message's own opening lines, and nothing anywhere said so. A preview and a
/// summary look similar enough that the difference is invisible from outside.
pub(crate) fn debug_log(app: &tauri::AppHandle, entry: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(debug_log_path(app))
    {
        let _ = writeln!(file, "{entry}");
    }
}
