use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct AppConfig {
    pub notes_dir: Option<String>,
    // Files opened from outside notes_dir (via "Open file" or drag-drop) —
    // tracked here so they keep showing up in the sidebar across restarts
    // instead of only appearing for the session they were opened in.
    #[serde(default)]
    pub external_files: Vec<String>,
    // LLM connection settings, entered by the user in the settings modal —
    // deliberately not hardcoded anywhere, so any OpenAI-compatible local
    // runtime (Ollama, LM Studio, llama.cpp server, ...) can be pointed at.
    #[serde(default)]
    pub llm_base_url: Option<String>,
    #[serde(default)]
    pub llm_model: Option<String>,
    #[serde(default)]
    pub llm_api_key: Option<String>,
    // If set, spawned in the background on widget startup (only if nothing
    // is already listening) so the user doesn't have to manually start their
    // LLM runtime every time — e.g. "ollama serve".
    #[serde(default)]
    pub llm_autostart: bool,
    #[serde(default)]
    pub llm_start_command: Option<String>,
    // Whether to let the model reason/"think" before answering. Defaults to
    // off: for hybrid-thinking models this reliably produced faster, more
    // complete answers for structured tasks (the GitHub digest) than
    // leaving thinking enabled did.
    #[serde(default)]
    pub llm_think: Option<bool>,
    #[serde(default)]
    pub llm_max_tokens: Option<u32>,
    // GitHub personal access token, entered by the user in the GitHub
    // settings modal — used to read issues/PRs assigned to or opened by them.
    #[serde(default)]
    pub github_token: Option<String>,
    // Local calendar date ("YYYY-MM-DD") of the last successful daily GitHub
    // digest run — prevents re-running (and re-notifying) more than once
    // per day around the scheduled morning time.
    #[serde(default)]
    pub last_digest_date: Option<String>,
}

pub fn config_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = fs::create_dir_all(&dir);
    dir.join("config.json")
}

pub fn read_config(app: &tauri::AppHandle) -> AppConfig {
    fs::read_to_string(config_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn write_config(app: &tauri::AppHandle, cfg: &AppConfig) {
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(config_path(app), json);
    }
}
