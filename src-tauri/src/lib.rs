// Feature-grouped modules — see each group's mod.rs for what it covers.
// `config` and `tunables` stay at the root because every group reads them:
// config is the single shared AppConfig file, and tunables is the registry of
// per-machine values that groups read instead of hardcoding.
mod agent;
mod ai;
mod config;
mod content;
mod github;
mod system;
mod tunables;
mod ui;
mod web;

use tauri::utils::config::Color;
use tauri::Manager;

use agent::server::{
    dismiss_permission, get_event_server_status, pending_permissions, respond_permission,
    start_event_server,
};
use ai::chat::{
    choose_chats_dir, delete_chat, get_chat_instructions, get_chats_dir, list_chats, load_chat,
    pick_chat_attachment, record_voice_turn, reset_chats_dir, save_chat, save_chat_instructions,
};
use ai::images::{generate_image, open_generated_image, read_generated_image};
use ai::llm::{
    get_llm_settings, get_model_capabilities, save_llm_settings, send_chat_message,
    set_active_llm_profile, start_server_now, test_llm_connection,
};
use ai::recall::{recall_context, recall_reindex, recall_status, search_chats};
use ai::speech::{get_stt_settings, get_tts_settings, save_stt_settings, save_tts_settings};
use ai::voice::{
    get_voice_readiness, set_voice_enabled, set_voice_threshold, voice_reply_stream, voice_speak, voice_transcribe,
};
use content::memory::{add_memory_root, get_memory_roots, list_memories, remove_memory_root};
use content::notes::{choose_notes_dir, delete_note, get_notes_dir, list_notes, open_external_file, open_path, save_note};
use github::{
    get_github_config, get_github_report, open_in_browser, save_github_config,
    test_github_connection,
};
use system::media::{
    is_mic_capture_active, list_audio_sessions, set_session_muted, set_session_volume, spotify_get_state,
    spotify_next, spotify_play_pause, spotify_previous, system_mic_get_muted, system_mic_set_muted,
    system_speaker_get_muted, system_speaker_get_volume, system_speaker_set_muted,
    system_speaker_set_volume,
};
use system::power::{get_idle_seconds, get_power_status};
use tunables::{get_tunables, save_tunables};
use ui::clickthrough::{set_click_through_paused, set_hot_rect};
use ui::windows::{
    hide_mascot, hide_settings, hide_workspace, open_settings, open_system_terminal, open_workspace,
    position_top_center,
};

/// Runs blocking work off the main thread, from an `async` command.
///
/// Two separate hazards make this necessary, and the middle ground between them
/// is narrow:
///
/// 1. A SYNC `#[tauri::command]` runs on the MAIN thread. Anything slow in one
///    stops the window pumping Win32 messages and Windows puts "Not Responding"
///    in the title bar. Measured: a command waiting on an HTTP request had
///    IsHungAppWindow() return true within two seconds.
/// 2. An `async` command runs on the Tokio runtime — but `reqwest::blocking`
///    builds its own runtime internally and cannot do that from inside another
///    one. Measured: after making the commands async, every HTTP command hung
///    forever while the SQLite ones still answered in milliseconds.
///
/// spawn_blocking is the answer to both: it is off the main thread, and it is a
/// context where blocking is allowed.
pub(crate) async fn offload<T, F>(work: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // A panic in the closure is a bug in the command, and there is no channel to
    // report it through for the commands that do not return Result — so it is
    // re-raised rather than swallowed into a wrong-looking success.
    tauri::async_runtime::spawn_blocking(work)
        .await
        .expect("a command's blocking body panicked")
}

// Hides (not destroys) a window on close — destroying it would require
// rebuilding it dynamically later, which hangs (see ui/windows.rs). Shared by
// every secondary window (workspace, settings, ...).
fn hide_on_close(window: &tauri::WebviewWindow) {
    let window_for_close = window.clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = window_for_close.hide();
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // Copying goes through here rather than through the webview's
        // navigator.clipboard, which rejects with NotAllowedError ("Document is
        // not focused") whenever the window does not hold OS focus — so a copy
        // button reported success and wrote nothing. Measured, not guessed:
        // writeText leaves the Windows clipboard untouched in that state.
        .plugin(tauri_plugin_clipboard_manager::init())
        .invoke_handler(tauri::generate_handler![
            list_notes,
            save_note,
            delete_note,
            get_notes_dir,
            choose_notes_dir,
            open_external_file,
            open_path,
            open_workspace,
            hide_workspace,
            get_llm_settings,
            get_model_capabilities,
            generate_image,
            read_generated_image,
            open_generated_image,
            save_llm_settings,
            set_active_llm_profile,
            send_chat_message,
            list_chats,
            load_chat,
            save_chat,
            delete_chat,
            get_chat_instructions,
            save_chat_instructions,
            pick_chat_attachment,
            record_voice_turn,
            get_stt_settings,
            save_stt_settings,
            get_tts_settings,
            save_tts_settings,
            get_voice_readiness,
            set_voice_enabled,
            set_voice_threshold,
            voice_transcribe,
            voice_reply_stream,
            voice_speak,
            start_server_now,
            test_llm_connection,
            open_settings,
            hide_settings,
            get_github_config,
            save_github_config,
            test_github_connection,
            get_github_report,
            open_in_browser,
            set_hot_rect,
            set_click_through_paused,
            open_system_terminal,
            hide_mascot,
            respond_permission,
            dismiss_permission,
            pending_permissions,
            spotify_get_state,
            spotify_play_pause,
            spotify_next,
            spotify_previous,
            system_speaker_get_volume,
            system_speaker_set_volume,
            system_speaker_get_muted,
            system_speaker_set_muted,
            system_mic_get_muted,
            system_mic_set_muted,
            list_audio_sessions,
            is_mic_capture_active,
            set_session_volume,
            set_session_muted,
            get_power_status,
            get_idle_seconds,
            list_memories,
            get_memory_roots,
            add_memory_root,
            remove_memory_root,
            get_tunables,
            save_tunables,
            get_event_server_status,
            recall_context,
            recall_reindex,
            recall_status,
            search_chats,
            get_chats_dir,
            choose_chats_dir,
            reset_chats_dir
        ])
        .setup(|app| {
            let window = app
                .get_webview_window("mascot")
                .expect("mascot window must exist");
            position_top_center(&window);
            // Windows/WebView2 can otherwise show a faint opaque rectangle behind
            // the rounded corners of a transparent window — force true alpha.
            let _ = window.set_background_color(Some(Color(0, 0, 0, 0)));

            // All secondary windows are declared `visible: false` in
            // tauri.conf.json and only shown on demand. Workspace used to be
            // `visible: true` (shown at launch) to work around a since-fixed
            // bug (the `additionalBrowserArgs` config property broke every
            // secondary window's WebView2 init, regardless of visibility —
            // see git history). That's resolved now, so a normal
            // hidden-until-shown window works fine here too.
            if let Some(workspace_window) = app.get_webview_window("workspace") {
                hide_on_close(&workspace_window);
            }
            if let Some(settings_window) = app.get_webview_window("settings") {
                hide_on_close(&settings_window);
                let _ = settings_window.set_background_color(Some(Color(0, 0, 0, 0)));
            }

            ui::tray::setup_tray(app)?;

            start_event_server(app.handle().clone());

            // The search index is a cache over the chat files, and those can
            // change while the app isn't running — edited, deleted, synced from
            // another machine — so it is reconciled at every startup rather than
            // trusted. Runs on its own thread: it reads every conversation.
            ai::recall::start_indexer(app.handle().clone());

            // Runs on its own thread — it may block briefly on a reachability
            // check/process spawn, and that must never delay window startup.
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                ai::llm::maybe_autostart(&app_handle);
                ai::speech::maybe_autostart_stt(&app_handle);
                ai::speech::maybe_autostart_tts(&app_handle);
                ai::images::maybe_autostart(&app_handle);
            });

            github::start_merge_watcher(app.handle().clone());
            github::start_daily_digest_watcher(app.handle().clone());
            github::issue_watcher::start_issue_watcher(app.handle().clone());
            github::ci_watcher::start_ci_watcher(app.handle().clone());

            system::media::start_spotify_watcher(app.handle().clone());

            ui::clickthrough::start_click_through_watcher(window);

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
