// The tray icon and its menu — the only way to reach the widget once the mascot
// is hidden, so everything here has to be reachable with no window open.
use tauri::image::Image;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;

use crate::ai::llm;
use crate::github;
use crate::ui::windows::{open_settings, open_system_terminal, open_workspace};

pub fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItemBuilder::with_id("show", "Show mascot").build(app)?;
    let hide_item = MenuItemBuilder::with_id("hide", "Hide mascot").build(app)?;
    let workspace_item = MenuItemBuilder::with_id("workspace", "Open Workspace").build(app)?;
    let settings_item = MenuItemBuilder::with_id("settings", "Open Settings").build(app)?;
    let terminal_item = MenuItemBuilder::with_id("terminal", "Open Terminal").build(app)?;
    let github_digest_item =
        MenuItemBuilder::with_id("github_digest_now", "Run GitHub Digest Now").build(app)?;
    let brief_item =
        MenuItemBuilder::with_id("morning_brief_now", "Run Morning Brief Now").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
    let quit_stop_servers_item =
        MenuItemBuilder::with_id("quit_stop_servers", "Quit and stop local servers").build(app)?;

    // Grouped by what the item does to the machine, which is the distinction that
    // matters when you are aiming at a small target next to the clock: showing a
    // window, opening one, making the widget do work now, or leaving. Flat, the
    // list put "Quit" directly under a debug probe.
    let tray_menu = MenuBuilder::new(app)
        .items(&[&show_item, &hide_item])
        .separator()
        .items(&[&workspace_item, &settings_item, &terminal_item])
        .separator()
        .items(&[&github_digest_item, &brief_item])
        .separator()
        .items(&[&quit_item, &quit_stop_servers_item])
        .build()?;

    TrayIconBuilder::new()
        // The mascot rather than the default window icon (which is still the
        // stock Tauri mark). Same pixy sprite the widget draws itself, in the
        // muted grey it wears on the chat screen — see icons/tray.png.
        .icon(Image::from_bytes(include_bytes!("../../icons/tray.png"))?)
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
            "settings" => open_settings(app.clone()),
            "terminal" => open_system_terminal(),
            "github_digest_now" => github::run_github_digest_now(app.clone()),
            "morning_brief_now" => crate::mail::brief::run_brief_now(app.clone()),
            "quit" => app.exit(0),
            "quit_stop_servers" => {
                // Each of these shells out to netstat and taskkill, so it is not
                // instant — but it has to finish before the exit, or the process
                // dies mid-kill and leaves exactly the orphan this item exists to
                // prevent.
                llm::stop_server(app);
                crate::ai::speech::stop_servers(app);
                crate::ai::images::stop_server(app);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}
