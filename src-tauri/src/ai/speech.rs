// Connection settings for speech-to-text / text-to-speech providers — same
// multi-profile shape as ai/llm.rs's LlmSettings, including the same
// autostart/start_command convention (global per section, not per-profile —
// it starts whichever local server process the user configured).
//
// This module is only the connection settings and server supervision. The
// code that actually sends audio to these servers is ai/voice.rs, which reads
// the profiles saved here.
use serde::{Deserialize, Serialize};

use crate::config::{read_config, write_config, SttProfile, TtsProfile};
use crate::ai::process::{autostart_if_needed, stop_local_server};

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SttSettings {
    pub profiles: Vec<SttProfile>,
    pub active_profile_id: String,
    pub autostart: bool,
    pub start_command: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct TtsSettings {
    pub profiles: Vec<TtsProfile>,
    pub active_profile_id: String,
    pub autostart: bool,
    pub start_command: String,
}

#[tauri::command]
pub fn get_stt_settings(app: tauri::AppHandle) -> SttSettings {
    let cfg = read_config(&app);
    let active_profile_id = cfg
        .stt_active_profile_id
        .filter(|id| cfg.stt_profiles.iter().any(|p| &p.id == id))
        .or_else(|| cfg.stt_profiles.first().map(|p| p.id.clone()))
        .unwrap_or_default();
    SttSettings {
        profiles: cfg.stt_profiles,
        active_profile_id,
        autostart: cfg.stt_autostart,
        start_command: cfg.stt_start_command.unwrap_or_default(),
    }
}

#[tauri::command]
pub fn save_stt_settings(app: tauri::AppHandle, settings: SttSettings) {
    let mut cfg = read_config(&app);
    cfg.stt_profiles = settings.profiles;
    cfg.stt_active_profile_id = Some(settings.active_profile_id);
    cfg.stt_autostart = settings.autostart;
    cfg.stt_start_command = Some(settings.start_command);
    write_config(&app, &cfg);
}

#[tauri::command]
pub fn get_tts_settings(app: tauri::AppHandle) -> TtsSettings {
    let cfg = read_config(&app);
    let active_profile_id = cfg
        .tts_active_profile_id
        .filter(|id| cfg.tts_profiles.iter().any(|p| &p.id == id))
        .or_else(|| cfg.tts_profiles.first().map(|p| p.id.clone()))
        .unwrap_or_default();
    TtsSettings {
        profiles: cfg.tts_profiles,
        active_profile_id,
        autostart: cfg.tts_autostart,
        start_command: cfg.tts_start_command.unwrap_or_default(),
    }
}

#[tauri::command]
pub fn save_tts_settings(app: tauri::AppHandle, settings: TtsSettings) {
    let mut cfg = read_config(&app);
    cfg.tts_profiles = settings.profiles;
    cfg.tts_active_profile_id = Some(settings.active_profile_id);
    cfg.tts_autostart = settings.autostart;
    cfg.tts_start_command = Some(settings.start_command);
    write_config(&app, &cfg);
}

// The endpoints the two speech servers are expected to answer on. Shared by the
// startup spawn and the tray's stop-and-quit so the two cannot disagree about
// which server they mean. Unlike the LLM there is no default to fall back on: a
// speech server nobody has configured has no address to look for.
fn stt_base_url(cfg: &crate::config::AppConfig) -> Option<String> {
    cfg.stt_active_profile_id
        .as_ref()
        .and_then(|id| cfg.stt_profiles.iter().find(|p| &p.id == id))
        .or_else(|| cfg.stt_profiles.first())
        .map(|p| p.base_url.clone())
}

fn tts_base_url(cfg: &crate::config::AppConfig) -> Option<String> {
    cfg.tts_active_profile_id
        .as_ref()
        .and_then(|id| cfg.tts_profiles.iter().find(|p| &p.id == id))
        .or_else(|| cfg.tts_profiles.first())
        .map(|p| p.base_url.clone())
}

pub fn maybe_autostart_stt(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    if !cfg.stt_autostart {
        return;
    }
    let Some(command) = cfg.stt_start_command.clone().filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let Some(base_url) = stt_base_url(&cfg) else {
        return;
    };
    autostart_if_needed(&base_url, &command);
}

pub fn maybe_autostart_tts(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    if !cfg.tts_autostart {
        return;
    }
    let Some(command) = cfg.tts_start_command.clone().filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let Some(base_url) = tts_base_url(&cfg) else {
        return;
    };
    autostart_if_needed(&base_url, &command);
}

/// Stops both speech servers for the tray's stop-and-quit, whoever started them —
/// see process::stop_local_server.
pub fn stop_servers(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    for base_url in [stt_base_url(&cfg), tts_base_url(&cfg)].into_iter().flatten() {
        stop_local_server(&base_url);
    }
}
