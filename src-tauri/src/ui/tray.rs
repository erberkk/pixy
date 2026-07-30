use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;

use crate::github;
use crate::ai::llm;
use crate::system::media;
use crate::ui::windows::{open_system_terminal, open_workspace};

pub fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItemBuilder::with_id("show", "Show mascot").build(app)?;
    let hide_item = MenuItemBuilder::with_id("hide", "Hide mascot").build(app)?;
    let workspace_item = MenuItemBuilder::with_id("workspace", "Open Workspace").build(app)?;
    let terminal_item = MenuItemBuilder::with_id("terminal", "Open Terminal").build(app)?;
    let github_digest_item =
        MenuItemBuilder::with_id("github_digest_now", "Run GitHub Digest Now").build(app)?;
    // Temporary — risk-spike probe for the GSMTC Spotify integration (see
    // plan doc), remove once the feature is confirmed working end-to-end.
    let spotify_probe_item =
        MenuItemBuilder::with_id("spotify_probe", "Debug: Spotify Probe").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let quit_stop_llm_item =
        MenuItemBuilder::with_id("quit_stop_llm", "Quit (also stop local servers)").build(app)?;
    let tray_menu = MenuBuilder::new(app)
        .items(&[
            &show_item,
            &hide_item,
            &workspace_item,
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
            "workspace" => open_workspace(app.clone()),
            "terminal" => open_system_terminal(),
            "github_digest_now" => github::run_github_digest_now(app.clone()),
            "spotify_probe" => media::probe_spotify(app.clone()),
            "quit" => app.exit(0),
            "quit_stop_llm" => {
                llm::stop_autostarted();
                crate::ai::speech::stop_autostarted();
                crate::ai::images::stop_autostarted();
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}
