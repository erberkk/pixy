use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;

use crate::github;
use crate::llm;
use crate::media;
use crate::terminal;
use crate::windows::{open_notepad, open_terminal};

pub fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItemBuilder::with_id("show", "Show mascot").build(app)?;
    let hide_item = MenuItemBuilder::with_id("hide", "Hide mascot").build(app)?;
    let notepad_item = MenuItemBuilder::with_id("notepad", "Open Notepad").build(app)?;
    let terminal_item = MenuItemBuilder::with_id("terminal", "Open Agent Terminal").build(app)?;
    let github_digest_item =
        MenuItemBuilder::with_id("github_digest_now", "Run GitHub Digest Now").build(app)?;
    // Temporary — risk-spike probe for the GSMTC Spotify integration (see
    // plan doc), remove once the feature is confirmed working end-to-end.
    let spotify_probe_item =
        MenuItemBuilder::with_id("spotify_probe", "Debug: Spotify Probe").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let quit_stop_llm_item =
        MenuItemBuilder::with_id("quit_stop_llm", "Quit (also stop LLM server)").build(app)?;
    let tray_menu = MenuBuilder::new(app)
        .items(&[
            &show_item,
            &hide_item,
            &notepad_item,
            &terminal_item,
            &github_digest_item,
            &spotify_probe_item,
            &quit_item,
            &quit_stop_llm_item,
        ])
        .build()?;

    TrayIconBuilder::new()
        .icon(app.default_window_icon().cloned().unwrap())
        .menu(&tray_menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("mascot") {
                    let _ = window.show();
                }
            }
            "hide" => {
                if let Some(window) = app.get_webview_window("mascot") {
                    let _ = window.hide();
                }
            }
            "notepad" => open_notepad(app.clone()),
            "terminal" => open_terminal(app.clone()),
            "github_digest_now" => github::run_github_digest_now(app.clone()),
            "spotify_probe" => media::probe_spotify(app.clone()),
            "quit" => {
                terminal::stop_terminal_session();
                app.exit(0);
            }
            "quit_stop_llm" => {
                terminal::stop_terminal_session();
                llm::stop_autostarted();
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}
