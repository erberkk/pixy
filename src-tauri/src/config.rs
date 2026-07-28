use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::Manager;

// One named LLM connection profile — the chat UI lets the user pick between
// several of these instead of the widget only ever knowing a single model.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct LlmProfile {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    // Whether to let the model reason/"think" before answering. Defaults to
    // off: for hybrid-thinking models this reliably produced faster, more
    // complete answers for structured tasks (the GitHub digest) than
    // leaving thinking enabled did.
    #[serde(default)]
    pub think: bool,
    #[serde(default)]
    pub max_tokens: u32,
}

// Speech-to-text connection profile — no voice/rate knobs, those are a
// TTS-only concept.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct SttProfile {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    // ISO-639-1 code ("tr", "en", …), or empty to let the server auto-detect.
    //
    // Worth setting for anything but English: auto-detection gets roughly one
    // second of speech to work from, and whisper's prior is heavily English —
    // asking it "nasılsın" without this produced confident English nonsense,
    // which then went to the model as if it were the question.
    #[serde(default)]
    pub language: String,
}

// Text-to-speech connection profile — same shape as SttProfile plus
// `voice`, since which voice to speak in is a near-universal TTS knob
// (OpenAI TTS, ElevenLabs, Piper, ... all expose one).
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct TtsProfile {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub voice: String,
}

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
    // Multiple named profiles (e.g. "Local Ollama", "GPT-4") so the chat UI
    // can switch between them instead of the widget only ever knowing one
    // model.
    #[serde(default)]
    pub llm_profiles: Vec<LlmProfile>,
    // Which profile's id the chat UI (and background digest/issue-analysis
    // calls) should use by default — None/unresolvable falls back to the
    // first entry in llm_profiles.
    #[serde(default)]
    pub llm_active_profile_id: Option<String>,
    // If set, spawned in the background on widget startup (only if nothing
    // is already listening) so the user doesn't have to manually start their
    // LLM runtime every time — e.g. "ollama serve". Global, not per-profile:
    // it starts whichever local runtime process the user configured,
    // independent of which profile ends up talking to it.
    #[serde(default)]
    pub llm_autostart: bool,
    #[serde(default)]
    pub llm_start_command: Option<String>,
    // Pre-multi-profile single-endpoint fields — kept only so
    // llm::migrate_legacy_config can fold an already-configured connection
    // (from before llm_profiles existed) into a real profile instead of
    // silently losing it the first time this version of the app runs.
    // Never read anywhere else; safe to remove once migration has run once
    // for every user.
    #[serde(default)]
    pub llm_base_url: Option<String>,
    #[serde(default)]
    pub llm_model: Option<String>,
    #[serde(default)]
    pub llm_api_key: Option<String>,
    #[serde(default)]
    pub llm_think: Option<bool>,
    #[serde(default)]
    pub llm_max_tokens: Option<u32>,

    // Speech-to-text / text-to-speech connection profiles — same
    // multi-profile shape as llm_profiles. Driven by the voice assistant
    // (voice_enabled below): STT transcribes what you said, TTS speaks the
    // reply.
    #[serde(default)]
    pub stt_profiles: Vec<SttProfile>,
    #[serde(default)]
    pub stt_active_profile_id: Option<String>,
    #[serde(default)]
    pub stt_autostart: bool,
    #[serde(default)]
    pub stt_start_command: Option<String>,
    #[serde(default)]
    pub tts_profiles: Vec<TtsProfile>,
    #[serde(default)]
    pub tts_active_profile_id: Option<String>,
    #[serde(default)]
    pub tts_autostart: bool,
    #[serde(default)]
    pub tts_start_command: Option<String>,

    // Custom instructions ("personality") applied to every chat — see
    // ai/chat.rs's get/save_chat_instructions.
    #[serde(default)]
    pub chat_instructions: Option<String>,
    // GitHub personal access token, entered by the user in the GitHub
    // settings modal — used to read issues/PRs assigned to or opened by them.
    #[serde(default)]
    pub github_token: Option<String>,
    // Local calendar date ("YYYY-MM-DD") of the last successful daily GitHub
    // digest run — prevents re-running (and re-notifying) more than once
    // per day around the scheduled morning time.
    #[serde(default)]
    pub last_digest_date: Option<String>,
    // Extra roots to scan for memory files (content/memory.rs), on top of the
    // default ~/.claude/projects — lets a user whose Claude Code config
    // lives somewhere else (CLAUDE_CONFIG_DIR set, a different OS user
    // profile, a synced copy from another machine, ...) still see those
    // memories instead of only ever seeing an empty graph.
    #[serde(default)]
    pub memory_extra_roots: Vec<String>,

    // Voice assistant: say the wake word while the widget is up and it
    // records you, transcribes via the STT profile, answers with the active
    // LLM profile and speaks the reply through the TTS profile
    // (ai/voice.rs + the mascot's voice/ modules).
    //
    // Off by default, and deliberately not inferred from "the STT and TTS
    // profiles are both filled in" — turning this on holds the microphone
    // open for as long as the widget runs, which is not something to opt a
    // user into as a side effect of having configured a transcription server.
    #[serde(default)]
    pub voice_enabled: bool,
    // Wake-word score (0..1) above which the wake word counts as heard.
    // Exposed rather than hardcoded because the bundled model was trained to
    // a loose false-positive target, so the useful threshold depends on the
    // user's mic and room more than on the model.
    #[serde(default)]
    pub voice_threshold: Option<f32>,
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
