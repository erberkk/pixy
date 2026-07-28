// Connection settings for speech-to-text / text-to-speech providers — same
// multi-profile shape as ai/llm.rs's LlmSettings, including the same
// autostart/start_command convention (global per section, not per-profile —
// it starts whichever local server process the user configured).
//
// This module is only the connection settings and server supervision. The
// code that actually sends audio to these servers is ai/voice.rs, which reads
// the profiles saved here.
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::config::{read_config, write_config, SttProfile, TtsProfile};
use crate::ai::process::{autostart_if_needed, stop_tracked};

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

static STT_CHILD_PID: Mutex<Option<u32>> = Mutex::new(None);
static TTS_CHILD_PID: Mutex<Option<u32>> = Mutex::new(None);

pub fn maybe_autostart_stt(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    if !cfg.stt_autostart {
        return;
    }
    let Some(command) = cfg.stt_start_command.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let Some(base_url) = cfg
        .stt_active_profile_id
        .as_ref()
        .and_then(|id| cfg.stt_profiles.iter().find(|p| &p.id == id))
        .or_else(|| cfg.stt_profiles.first())
        .map(|p| p.base_url.clone())
    else {
        return;
    };
    autostart_if_needed(&base_url, &command, &STT_CHILD_PID);
}

pub fn maybe_autostart_tts(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    if !cfg.tts_autostart {
        return;
    }
    let Some(command) = cfg.tts_start_command.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let Some(base_url) = cfg
        .tts_active_profile_id
        .as_ref()
        .and_then(|id| cfg.tts_profiles.iter().find(|p| &p.id == id))
        .or_else(|| cfg.tts_profiles.first())
        .map(|p| p.base_url.clone())
    else {
        return;
    };
    autostart_if_needed(&base_url, &command, &TTS_CHILD_PID);
}

pub fn stop_autostarted() {
    stop_tracked(&STT_CHILD_PID);
    stop_tracked(&TTS_CHILD_PID);
}
