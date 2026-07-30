use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::Emitter;

use crate::config::{read_config, write_config, LlmProfile};
use crate::ai::process::{autostart_if_needed, is_reachable, spawn_detached, stop_tracked};

const DEFAULT_LLM_BASE_URL: &str = "http://localhost:11434/v1";

// Everything the chat UI (and Settings' LLM section) needs in one round
// trip: the full profile list, which one is active, and the (global, not
// per-profile) autostart settings.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct LlmSettings {
    pub profiles: Vec<LlmProfile>,
    pub active_profile_id: String,
    pub autostart: bool,
    pub start_command: String,
}

// Folds a pre-multi-profile single-endpoint config (base_url/model/api_key
// set directly on AppConfig, from before llm_profiles existed) into a real
// profile — otherwise upgrading to this version would silently drop
// whatever the user already had configured. Runs at most once: it clears
// the legacy fields after migrating so this is a no-op on every later call.
fn migrate_legacy_llm_config(app: &tauri::AppHandle) {
    let mut cfg = read_config(app);
    let Some(model) = cfg.llm_model.clone().filter(|m| !m.trim().is_empty()) else {
        return;
    };
    if !cfg.llm_profiles.is_empty() {
        // Already migrated (or the user already has profiles some other
        // way) — just clear the stale legacy fields without touching
        // llm_profiles.
        cfg.llm_model = None;
        write_config(app, &cfg);
        return;
    }

    let profile = LlmProfile {
        id: uuid::Uuid::new_v4().to_string(),
        label: "Migrated profile".to_string(),
        base_url: cfg
            .llm_base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_LLM_BASE_URL.to_string()),
        model,
        api_key: cfg.llm_api_key.clone().unwrap_or_default(),
        think: cfg.llm_think.unwrap_or(false),
        max_tokens: cfg.llm_max_tokens.unwrap_or(2048),
    };
    cfg.llm_active_profile_id = Some(profile.id.clone());
    cfg.llm_profiles.push(profile);
    cfg.llm_base_url = None;
    cfg.llm_model = None;
    cfg.llm_api_key = None;
    cfg.llm_think = None;
    cfg.llm_max_tokens = None;
    write_config(app, &cfg);
}

#[tauri::command]
pub fn get_llm_settings(app: tauri::AppHandle) -> LlmSettings {
    migrate_legacy_llm_config(&app);
    let cfg = read_config(&app);
    let active_profile_id = cfg
        .llm_active_profile_id
        .filter(|id| cfg.llm_profiles.iter().any(|p| &p.id == id))
        .or_else(|| cfg.llm_profiles.first().map(|p| p.id.clone()))
        .unwrap_or_default();
    LlmSettings {
        profiles: cfg.llm_profiles,
        active_profile_id,
        autostart: cfg.llm_autostart,
        start_command: cfg.llm_start_command.unwrap_or_default(),
    }
}

#[tauri::command]
pub fn save_llm_settings(app: tauri::AppHandle, settings: LlmSettings) {
    let mut cfg = read_config(&app);
    cfg.llm_profiles = settings.profiles;
    cfg.llm_active_profile_id = Some(settings.active_profile_id);
    cfg.llm_autostart = settings.autostart;
    cfg.llm_start_command = Some(settings.start_command);
    write_config(&app, &cfg);
}

// Cheap, frequent update from the chat UI's model dropdown — avoids
// resending the whole profile list (with API keys) just to remember which
// one was last selected.
#[tauri::command]
pub fn set_active_llm_profile(app: tauri::AppHandle, profile_id: String) {
    let mut cfg = read_config(&app);
    cfg.llm_active_profile_id = Some(profile_id);
    write_config(&app, &cfg);
}

// Used by background callers (the GitHub digest/issue-analysis watchers)
// that need "the" LLM to talk to rather than a user-picked one — falls back
// to the first configured profile, or an empty profile (every caller here
// already treats an empty `model` as "not configured, skip").
pub fn get_active_llm_profile(app: tauri::AppHandle) -> LlmProfile {
    let settings = get_llm_settings(app);
    settings
        .profiles
        .into_iter()
        .find(|p| p.id == settings.active_profile_id)
        .unwrap_or_default()
}

// PID of the LLM server process we spawned ourselves (if any), so "Quit
// (also stop LLM server)" in the tray menu can kill exactly that process —
// never a server the user already had running before the widget started.
static LLM_CHILD_PID: Mutex<Option<u32>> = Mutex::new(None);

// Called once at widget startup.
pub fn maybe_autostart(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    if !cfg.llm_autostart {
        return;
    }
    let Some(command) = cfg.llm_start_command.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let base_url = cfg
        .llm_active_profile_id
        .as_ref()
        .and_then(|id| cfg.llm_profiles.iter().find(|p| &p.id == id))
        .or_else(|| cfg.llm_profiles.first())
        .map(|p| p.base_url.clone())
        .unwrap_or_else(|| DEFAULT_LLM_BASE_URL.to_string());
    autostart_if_needed(&base_url, &command, &LLM_CHILD_PID);
}

// Only kills a process this widget spawned itself (see LLM_CHILD_PID above)
// — a no-op if autostart was off or the server was already running.
pub fn stop_autostarted() {
    stop_tracked(&LLM_CHILD_PID);
}

// The chat UI's (or Settings') "Start now" button for any of the three
// sections (LLM/STT/TTS) — same reachability-first-then-spawn logic as
// autostart_if_needed, but on demand rather than only at widget launch, and
// without PID tracking (a server started this way is the user's own
// responsibility to stop; only the at-launch autostart path is cleaned up
// by "Quit (also stop LLM server)").
#[tauri::command]
pub async fn start_server_now(base_url: String, start_command: String) -> Result<String, String> {
    crate::offload(move || {
        if start_command.trim().is_empty() {
            return Err("No start command configured.".to_string());
        }
        if is_reachable(&base_url) {
            return Ok("Already running.".to_string());
        }
        match spawn_detached(&start_command) {
            Some(_) => Ok("Starting… give it a few seconds, then test the connection.".to_string()),
            None => Err("Couldn't launch that command — check it's a valid path.".to_string()),
        }
})
    .await
}

// --- what a given server+model turned out not to accept ---------------------
//
// "OpenAI-compatible" is a family of dialects, not one API, and the differences
// that matter here are per *model*, not per provider: on api.openai.com,
// gpt-4o takes `max_tokens` while the reasoning models reject it and demand
// `max_completion_tokens` instead. So this is learned from rejections rather
// than guessed from the URL or the model name — a name-matching heuristic
// ("starts with o1/o3/gpt-5…") would be wrong the day any provider ships a new
// family, and wrong in a way that shows up as a broken chat rather than a
// warning.
//
// Keyed by base_url + model, and separate from NO_OLLAMA_NATIVE below because
// the scopes genuinely differ: whether a route exists is a property of the
// server, whether a parameter is accepted is a property of the model.
#[derive(Default, Clone, Copy)]
struct ModelQuirks {
    wants_max_completion_tokens: bool,
    rejects_reasoning_effort: bool,
}

static MODEL_QUIRKS: OnceLock<Mutex<HashMap<String, ModelQuirks>>> = OnceLock::new();

fn quirks_key(base_url: &str, model: &str) -> String {
    format!("{base_url}|{model}")
}

fn quirks_for(key: &str) -> ModelQuirks {
    MODEL_QUIRKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .get(key)
        .copied()
        .unwrap_or_default()
}

fn record_quirk(key: &str, apply: impl FnOnce(&mut ModelQuirks)) {
    let mut map = MODEL_QUIRKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    apply(map.entry(key.to_string()).or_default());
}

// A rejection that names the parameter it wants instead is the server telling us
// how to fix the request — the only kind worth retrying automatically. Anything
// else (bad key, missing model, rate limit) is for the user to see.
enum ParamFix {
    UseMaxCompletionTokens,
    DropReasoningEffort,
    None,
}

fn diagnose_param_error(status: reqwest::StatusCode, body: &str) -> ParamFix {
    if !status.is_client_error() {
        return ParamFix::None;
    }
    // Matching the parameter name in the message, not the prose around it: the
    // wording differs between providers but the name is what they all cite.
    if body.contains("max_completion_tokens") {
        return ParamFix::UseMaxCompletionTokens;
    }
    if body.contains("reasoning_effort") {
        return ParamFix::DropReasoningEffort;
    }
    ParamFix::None
}

// Adds the two parameters whose spelling and support vary, according to what
// this server+model has already told us it accepts.
//
// `think` is what Settings' "Allow model thinking/reasoning" switch controls.
// On Ollama's native endpoint it maps to that API's own `think` flag; here the
// nearest equivalent is `reasoning_effort`, which only reasoning models take —
// hence the learned drop. Switching it off deliberately sends nothing rather
// than asking for minimal effort: a reasoning model reasons regardless, and
// pretending otherwise would be a promise this can't keep.
fn apply_model_params(payload: &mut serde_json::Value, q: &ModelQuirks, max_tokens: Option<u32>, think: bool) {
    if let Some(max) = max_tokens.filter(|m| *m > 0) {
        let field = if q.wants_max_completion_tokens {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        payload[field] = json!(max);
    }
    if think && !q.rejects_reasoning_effort {
        payload["reasoning_effort"] = json!("medium");
    }
}

// Extracts the human-readable half of an OpenAI-style error body.
fn error_message(status: reqwest::StatusCode, body: &str) -> String {
    let msg = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .map(|e| e.get("message").unwrap_or(e).to_string())
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    format!("HTTP {status}: {msg}")
}

// POSTs a chat request, retrying once if the server rejects it by naming a
// parameter it wants spelled differently (or not sent at all). The retry is
// bounded by construction: each fix is recorded before rebuilding, and
// apply_model_params can then only produce a request without that mistake, so
// the same rejection cannot recur.
fn post_chat_request(
    url: &str,
    api_key: &str,
    timeout_secs: u64,
    key: &str,
    build: impl Fn(&ModelQuirks) -> serde_json::Value,
) -> Result<reqwest::blocking::Response, String> {
    for attempt in 0..2 {
        let payload = build(&quirks_for(key));
        let mut req = reqwest::blocking::Client::new()
            .post(url)
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .json(&payload);
        if !api_key.trim().is_empty() {
            req = req.bearer_auth(api_key);
        }

        let resp = req.send().map_err(|e| format!("Couldn't connect: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }

        let body = resp.text().unwrap_or_default();
        if attempt == 0 {
            match diagnose_param_error(status, &body) {
                ParamFix::UseMaxCompletionTokens => {
                    record_quirk(key, |q| q.wants_max_completion_tokens = true);
                    continue;
                }
                ParamFix::DropReasoningEffort => {
                    record_quirk(key, |q| q.rejects_reasoning_effort = true);
                    continue;
                }
                ParamFix::None => {}
            }
        }
        return Err(error_message(status, &body));
    }
    unreachable!("the loop returns on both the success and the give-up path")
}

// Both single-turn request shapes below (OpenAI-compatible and Ollama native)
// take the same role/content message objects, so the only thing that differs
// between them is the surrounding payload — build the array once here rather
// than inlining the optional system turn in two places.
fn single_turn(system: Option<&str>, user_content: &str) -> serde_json::Value {
    let mut msgs = Vec::new();
    if let Some(system) = system.filter(|s| !s.trim().is_empty()) {
        msgs.push(json!({"role": "system", "content": system}));
    }
    msgs.push(json!({"role": "user", "content": user_content}));
    json!(msgs)
}

// Shared by test_llm_connection, summarize_github_activity and the voice
// assistant's reply. Proxied through Rust (not fetch() from a webview) so it
// works against ANY OpenAI-compatible server regardless of that server's CORS
// config — reqwest isn't a browser and isn't subject to CORS at all.
fn chat_completion(
    base_url: &str,
    model: &str,
    api_key: &str,
    system: Option<&str>,
    user_content: &str,
    timeout_secs: u64,
    max_tokens: Option<u32>,
    think: bool,
) -> Result<String, String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let key = quirks_key(base_url, model);
    let resp = post_chat_request(&url, api_key, timeout_secs, &key, |q| {
        let mut payload = json!({
            "model": model,
            "messages": single_turn(system, user_content),
            "stream": false
        });
        // max_tokens caps how long a model can ramble/hallucinate for — without
        // it, a "thinking" or unstable model can run for minutes and degrade
        // into fabricated content the longer it goes (seen firsthand: a small
        // test prompt was fine, but a longer one produced invented issues, fake
        // dates, and eventually a nonsensical self-generated question).
        apply_model_params(&mut payload, q, max_tokens, think);
        payload
    })?;

    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Invalid response from server: {e}"))?;

    let raw = body["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| format!("Unexpected response format: {body}"))?;

    Ok(strip_model_artifacts(raw))
}

#[tauri::command]
pub async fn test_llm_connection(
    base_url: String,
    model: String,
    api_key: String,
    think: bool,
    max_tokens: u32,
) -> Result<String, String> {
    crate::offload(move || {
        let reply = run_chat(&base_url, &model, &api_key, None, "Reply with just the word OK.", 30, think, max_tokens)?;
        if reply.is_empty() {
            return Ok("(empty reply, but the connection and model are working)".to_string());
        }
        Ok(truncate(&reply, 200))
})
    .await
}

// Judges ONE issue's full thread in isolation — a far more tractable task
// for a small local model than reasoning over 20+ issues in a single call
// (which reliably produced "nothing needs attention" or an empty reply).
// Backend-only, called once per issue from github/api.rs's
// enrich_with_issue_analysis — this is genuinely slow for a big backlog,
// which is accepted since it only runs once a day.
pub fn judge_issue_thread(
    base_url: String,
    model: String,
    api_key: String,
    issue_title: String,
    username: String,
    thread: String,
    think: bool,
    max_tokens: u32,
) -> Result<Option<String>, String> {
    let prompt = format!(
        "Below is the full thread (description + all comments, in order) of a \
         GitHub issue assigned to {username}. Comments authored by the assignee \
         themselves are labeled \"YOU\" in the transcript — every other label is \
         someone else. Read the whole thread in order, not just the last message.\n\n\
         Decide: does YOU still owe an action here right now (e.g. answer a question \
         directed at them, fix something reported, react to a test result, or \
         respond to a new request)? Pay close attention to whether YOU's own last \
         relevant message already handed this off to someone else (e.g. \"fix is \
         live, please test\") — if so, the ball is in the OTHER person's court, not \
         YOU's, and this is NOT an action item for YOU even if no one has replied \
         since.\n\n\
         Reply with EXACTLY one of:\n\
         - \"ACTION: <what YOU still needs to do, under 15 words>\"\n\
         - \"NO_ACTION\"\n\
         Nothing else — no explanation, no restating the thread.\n\n\
         Issue: {issue_title}\n\n\
         --- thread ---\n{thread}"
    );

    let reply = run_chat(&base_url, &model, &api_key, None, &prompt, 60, think, max_tokens)?;
    let trimmed = reply.trim();
    match trimmed.strip_prefix("ACTION:").map(|s| s.trim()) {
        Some(note) if !note.is_empty() => Ok(Some(note.to_string())),
        _ => Ok(None),
    }
}

// Ollama's OpenAI-compatibility shim (/v1/chat/completions) ignores the
// "think" parameter for hybrid-thinking models — confirmed firsthand: the
// identical request against Ollama's own native /api/chat with
// "think": false answers cleanly in ~1s, while the /v1 shim still burns the
// entire token budget on an internal reasoning field and returns empty
// content. When base_url looks like Ollama's default (ends in "/v1"), try
// the native endpoint first so the user's think/max_tokens choice actually
// takes effect; fall back to the generic OpenAI-compatible path for any
// other server (LM Studio, llama.cpp, ...) or if the native attempt fails.
fn run_chat(
    base_url: &str,
    model: &str,
    api_key: &str,
    system: Option<&str>,
    user_content: &str,
    timeout_secs: u64,
    think: bool,
    max_tokens: u32,
) -> Result<String, String> {
    if let Some(result) = ollama_native_chat(base_url, model, api_key, system, user_content, timeout_secs, think) {
        if let Ok(reply) = &result {
            if !reply.is_empty() {
                return Ok(reply.clone());
            }
        }
    }
    chat_completion(base_url, model, api_key, system, user_content, timeout_secs, Some(max_tokens), think)
}

// Base URLs already known to have no /api/chat route.
//
// The native-endpoint probe below is worth it against Ollama, but any other
// OpenAI-compatible server — including OpenAI, Anthropic and Gemini, whose base
// URLs also end in /v1 — simply doesn't have that route. Measured against a
// stand-in provider: without this, every single message paid for a 404 round
// trip before the real request. Remembering the miss makes it once per server
// per app run instead of once per message.
//
// Only a 404 marks a server: a refused connection or a timeout might be
// transient, and shouldn't permanently disable the fast path for a local Ollama
// that happened to be restarting.
static NO_OLLAMA_NATIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn no_ollama_native() -> &'static Mutex<HashSet<String>> {
    NO_OLLAMA_NATIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

fn ollama_native_known_missing(base_url: &str) -> bool {
    no_ollama_native().lock().unwrap().contains(base_url)
}

fn mark_ollama_native_missing(base_url: &str) {
    no_ollama_native().lock().unwrap().insert(base_url.to_string());
}

fn ollama_native_chat(
    base_url: &str,
    model: &str,
    api_key: &str,
    system: Option<&str>,
    user_content: &str,
    timeout_secs: u64,
    think: bool,
) -> Option<Result<String, String>> {
    if ollama_native_known_missing(base_url) {
        return None;
    }
    let root = base_url.trim_end_matches('/').strip_suffix("/v1")?;
    let url = format!("{root}/api/chat");

    let result = (|| -> Result<String, String> {
        let mut req = reqwest::blocking::Client::new()
            .post(&url)
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .json(&json!({
                "model": model,
                "messages": single_turn(system, user_content),
                "stream": false,
                "think": think
            }));
        if !api_key.trim().is_empty() {
            req = req.bearer_auth(api_key);
        }

        let resp = req.send().map_err(|e| format!("Couldn't connect: {e}"))?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .map_err(|e| format!("Invalid response from server: {e}"))?;

        if !status.is_success() {
            if status == reqwest::StatusCode::NOT_FOUND {
                mark_ollama_native_missing(base_url);
            }
            let msg = body.get("error").and_then(|e| e.as_str()).unwrap_or("unknown error");
            return Err(format!("HTTP {status}: {msg}"));
        }

        let raw = body["message"]["content"]
            .as_str()
            .ok_or_else(|| format!("Unexpected response format: {body}"))?;
        Ok(strip_model_artifacts(raw))
    })();

    Some(result)
}

/// What a model turned out to be able to do, as far as its server will say.
///
/// `known` is the important field. Only Ollama reports this (through its native
/// /api/show), so against any other OpenAI-compatible server the answer is
/// "no idea" — and a warning shown on a setup that actually works is worse than
/// no warning at all, because the user learns to ignore it.
#[derive(Serialize, Clone, Copy, Default)]
pub struct ModelCapabilities {
    pub vision: bool,
    pub tools: bool,
    pub known: bool,
}

static MODEL_CAPABILITIES: OnceLock<Mutex<HashMap<String, ModelCapabilities>>> = OnceLock::new();

fn model_capabilities_cache() -> &'static Mutex<HashMap<String, ModelCapabilities>> {
    MODEL_CAPABILITIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Asks the server what a model can do. Cached per server+model per app run:
/// the answer only changes if the user re-pulls the model, and the chat UI asks
/// every time an image is attached.
#[tauri::command]
pub async fn get_model_capabilities(
    base_url: String,
    model: String,
    api_key: String,
) -> ModelCapabilities {
    crate::offload(move || {
        let key = quirks_key(&base_url, &model);
        if let Some(cached) = model_capabilities_cache().lock().ok().and_then(|c| c.get(&key).copied())
        {
            return cached;
        }
        let found = ask_ollama_capabilities(&base_url, &model, &api_key).unwrap_or_default();
        if let Ok(mut cache) = model_capabilities_cache().lock() {
            cache.insert(key, found);
        }
        found
    })
    .await
}

fn ask_ollama_capabilities(base_url: &str, model: &str, api_key: &str) -> Option<ModelCapabilities> {
    if ollama_native_known_missing(base_url) {
        return None;
    }
    let root = base_url.trim_end_matches('/').strip_suffix("/v1")?;
    let mut request = reqwest::blocking::Client::new()
        .post(format!("{root}/api/show"))
        .timeout(std::time::Duration::from_secs(8))
        .json(&json!({ "model": model }));
    if !api_key.trim().is_empty() {
        request = request.bearer_auth(api_key);
    }
    let response = request.send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: serde_json::Value = response.json().ok()?;
    capabilities_from_show(&body)
}

/// Reads the `capabilities` list out of an /api/show response.
///
/// Returns None rather than an all-false result when the field is missing: an
/// older server that does not report capabilities at all must come out as
/// "unknown", not as "this model can do nothing" — the second would put a
/// wrong warning in front of a model that works.
fn capabilities_from_show(body: &serde_json::Value) -> Option<ModelCapabilities> {
    let listed = body.get("capabilities")?.as_array()?;
    let has = |name: &str| listed.iter().any(|c| c.as_str() == Some(name));
    Some(ModelCapabilities {
        vision: has("vision"),
        tools: has("tools"),
        known: true,
    })
}

#[derive(Deserialize, Clone)]
pub struct ChatTurn {
    pub role: String,
    // Usually a plain string, but the chat UI's image-attachment path sends
    // an OpenAI-style content-parts array instead
    // (`[{"type":"text",...},{"type":"image_url",...}]`) — accepting any
    // JSON value here and passing it straight through to whichever backend
    // we call means this struct doesn't need two shapes.
    pub content: serde_json::Value,
}

fn turn_has_image(turn: &serde_json::Value) -> bool {
    turn["content"].is_array()
}

/// Which server to talk to and how, for the streaming path.
///
/// These five always travel together and are never chosen independently — they
/// come from one profile the user selected. Passing them as one value keeps the
/// stream functions down to what actually varies between calls (the messages,
/// the tools, where the tokens go).
pub(crate) struct ChatEndpoint<'a> {
    pub base_url: &'a str,
    pub model: &'a str,
    pub api_key: &'a str,
    pub think: bool,
    pub max_tokens: u32,
}

/// What one streamed request produced.
///
/// A turn is either an answer or a request to call tools — never usefully
/// both, in every response observed — but the two are carried together rather
/// than as an enum because a model that emits a sentence of preamble alongside
/// its call should not have that sentence thrown away before the caller can
/// decide what to do with it.
pub(crate) struct StreamOutcome {
    pub text: String,
    pub calls: Vec<crate::ai::tools::ToolCall>,
}

/// Cap on how many times one message may bounce through tools before the model
/// has to answer with what it has.
///
/// Without a cap a model that keeps rewording the same failing search never
/// terminates. Five is enough for "search, then read two of the results" and
/// short enough that a runaway costs seconds rather than minutes — each round
/// is a full request whose prompt has grown by the previous round's result.
const MAX_TOOL_ROUNDS: usize = 5;

// The chat UI's send button. Streams tokens back to the SAME window that
// invoked it (never broadcast app-wide — a second chat conversation open
// elsewhere has no business seeing these tokens) as they arrive over
// chat-stream-chunk, then exactly one terminal event: chat-stream-done with
// the final (artifact-stripped) text, or chat-stream-error.
#[tauri::command]
pub async fn send_chat_message(
    app: tauri::AppHandle,
    window: tauri::Window,
    chat_id: String,
    base_url: String,
    model: String,
    api_key: String,
    think: bool,
    max_tokens: u32,
    messages: Vec<ChatTurn>,
) {
    crate::offload(move || {
        // Converted once, here, because the tool loop appends message shapes
        // ChatTurn cannot express (an assistant turn carrying `tool_calls`, a
        // `role: "tool"` result). Everything below this line works in the wire
        // shape the servers actually take.
        let mut wire: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();

        let endpoint = ChatEndpoint {
            base_url: &base_url,
            model: &model,
            api_key: &api_key,
            think,
            max_tokens,
        };
        let specs = crate::ai::tools::specs(&app);
        let offered: Vec<serde_json::Value> = specs.iter().map(|s| s.to_wire()).collect();

        let mut answer = String::new();
        for round in 0..=MAX_TOOL_ROUNDS {
            // On the final round the tools are withheld. Leaving them offered
            // would let the model spend its last turn asking for another call
            // that will never run, and the user would get nothing at all —
            // taking them away forces it to answer from what it has gathered.
            let tools_this_round: &[serde_json::Value] =
                if round == MAX_TOOL_ROUNDS { &[] } else { &offered };

            let result = run_chat_stream(
                &endpoint,
                &wire,
                tools_this_round,
                &mut |delta| {
                    let _ = window
                        .emit("chat-stream-chunk", json!({ "chat_id": &chat_id, "delta": delta }));
                },
            );

            let outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    let _ = window
                        .emit("chat-stream-error", json!({ "chat_id": chat_id, "error": error }));
                    return;
                }
            };

            if outcome.calls.is_empty() {
                answer = outcome.text;
                break;
            }

            wire.push(crate::ai::tools::assistant_call_message(&outcome.calls));
            for call in &outcome.calls {
                // The UI needs this to say what is happening and to drop the
                // preamble streamed alongside the call — a tool run is seconds
                // of silence otherwise, which reads as a hang.
                let _ = window.emit(
                    "chat-tool-start",
                    json!({ "chat_id": &chat_id, "tool": &call.name, "arguments": &call.arguments }),
                );
                let outcome = crate::ai::tools::execute(&app, call);
                let _ = window.emit(
                    "chat-tool-done",
                    json!({
                        "chat_id": &chat_id,
                        "tool": &call.name,
                        "sources": &outcome.sources,
                    }),
                );
                wire.push(crate::ai::tools::tool_result_message(call, &outcome.text));
            }
        }

        let _ = window.emit(
            "chat-stream-done",
            json!({ "chat_id": chat_id, "full_text": strip_model_artifacts(&answer) }),
        );
    })
    .await
}

// Mirrors run_chat's Ollama-native-first, OpenAI-compatible-fallback
// strategy (see its comment) but streaming token-by-token instead of
// waiting for one full response. If the native attempt streamed nothing at
// all (wrong server, thinking model producing an empty visible answer) we
// fall through exactly like run_chat does — the small risk of emitting a
// handful of chunks from a native attempt that then errors out mid-stream
// (rather than failing before any output) is accepted as it mirrors the
// same probe-then-fallback tradeoff the non-streaming path already makes.
pub(crate) fn run_chat_stream(
    endpoint: &ChatEndpoint<'_>,
    messages: &[serde_json::Value],
    tools: &[serde_json::Value],
    on_delta: &mut dyn FnMut(&str),
) -> Result<StreamOutcome, String> {
    // Ollama's native /api/chat takes images via a separate per-message
    // `images` array, not inline in `content` — rather than juggling two
    // request shapes, an attachment just skips straight to the
    // OpenAI-compatible path, which accepts image_url content parts
    // directly and Ollama's own /v1 shim already understands for
    // vision-capable models.
    let has_image = messages.iter().any(turn_has_image);
    if !has_image {
        if let Some(Ok(outcome)) = ollama_native_chat_stream(endpoint, messages, tools, on_delta)
        {
            // A turn that asked for a tool is a real result even though it
            // carries no text — checking only the text would send it down the
            // fallback path and run the whole request a second time.
            if !outcome.text.is_empty() || !outcome.calls.is_empty() {
                return Ok(outcome);
            }
        }
    }
    openai_chat_stream(endpoint, messages, tools, on_delta)
}

fn ollama_native_chat_stream(
    endpoint: &ChatEndpoint<'_>,
    messages: &[serde_json::Value],
    tools: &[serde_json::Value],
    on_delta: &mut dyn FnMut(&str),
) -> Option<Result<StreamOutcome, String>> {
    let ChatEndpoint { base_url, model, api_key, think, .. } = *endpoint;
    if ollama_native_known_missing(base_url) {
        return None;
    }
    let root = base_url.trim_end_matches('/').strip_suffix("/v1")?;
    let url = format!("{root}/api/chat");

    let result = (|| -> Result<StreamOutcome, String> {
        let mut payload =
            json!({ "model": model, "messages": messages, "stream": true, "think": think });
        // Omitted entirely when there is nothing to offer — an empty array is
        // not the same as no tools to every server, and describing nothing
        // still costs prompt tokens.
        if !tools.is_empty() {
            payload["tools"] = json!(tools);
        }
        let mut req = reqwest::blocking::Client::new()
            .post(&url)
            .timeout(std::time::Duration::from_secs(120))
            .json(&payload);
        if !api_key.trim().is_empty() {
            req = req.bearer_auth(api_key);
        }
        let resp = req.send().map_err(|e| format!("Couldn't connect: {e}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                mark_ollama_native_missing(base_url);
            }
            let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
            let msg = body.get("error").and_then(|e| e.as_str()).unwrap_or("unknown error");
            return Err(format!("HTTP {status}: {msg}"));
        }

        let mut full = String::new();
        let mut calls = Vec::new();
        for line in BufReader::new(resp).lines() {
            let line = line.map_err(|e| format!("stream read error: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(delta) = obj["message"]["content"].as_str() {
                if !delta.is_empty() {
                    on_delta(delta);
                    full.push_str(delta);
                }
            }
            // Unlike text, a tool call is not streamed piece by piece here:
            // measured against a live server, the whole call arrives complete
            // in one chunk (with `done: false`), followed by a final empty
            // `done: true` chunk. So it can simply be parsed where it lands —
            // no accumulation, no partial-JSON handling.
            calls.extend(crate::ai::tools::parse_tool_calls(&obj["message"]));
            if obj["done"].as_bool() == Some(true) {
                break;
            }
        }
        Ok(StreamOutcome { text: full, calls })
    })();

    Some(result)
}

// OpenAI-compatible `stream: true` — server-sent events, each line
// `data: {...}` (or the literal `data: [DONE]` sentinel).
fn openai_chat_stream(
    endpoint: &ChatEndpoint<'_>,
    messages: &[serde_json::Value],
    tools: &[serde_json::Value],
    on_delta: &mut dyn FnMut(&str),
) -> Result<StreamOutcome, String> {
    let ChatEndpoint { base_url, model, api_key, think, max_tokens } = *endpoint;
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let key = quirks_key(base_url, model);
    // The parameter retry happens before a single SSE line is read, so a
    // rejected request never produces a half-streamed answer the user has to
    // watch get discarded.
    let resp = post_chat_request(&url, api_key, 120, &key, |q| {
        let mut payload = json!({ "model": model, "messages": messages, "stream": true });
        apply_model_params(&mut payload, q, Some(max_tokens), think);
        if !tools.is_empty() {
            payload["tools"] = json!(tools);
        }
        payload
    })?;

    let mut full = String::new();
    let mut partial = PartialToolCalls::default();
    for line in BufReader::new(resp).lines() {
        let line = line.map_err(|e| format!("stream read error: {e}"))?;
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let Ok(obj) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        let delta = &obj["choices"][0]["delta"];
        if let Some(text) = delta["content"].as_str() {
            if !text.is_empty() {
                on_delta(text);
                full.push_str(text);
            }
        }
        partial.absorb(delta);
    }
    Ok(StreamOutcome {
        text: full,
        calls: partial.finish(),
    })
}

/// Accumulator for tool calls arriving over SSE.
///
/// This is the one place the two endpoints genuinely differ in difficulty.
/// Ollama's native stream hands over a whole call in a single chunk, but the
/// OpenAI-compatible format splits one call across many `delta.tool_calls`
/// fragments: the first carries the id and function name, and the argument
/// JSON dribbles in as string pieces that are only valid once concatenated.
/// The `index` field — not the id, which later fragments omit — is what ties
/// the pieces of one call together when several are requested at once.
///
/// Reconstructed from the documented format rather than from a captured
/// response: the shim under test emitted its tool call without splitting it,
/// so the fragmented path here has not been seen firsthand. It degrades to the
/// single-chunk case correctly, which is what that server does.
#[derive(Default)]
struct PartialToolCalls {
    /// Keyed by `index` and ordered by it, so several calls in one turn come
    /// back in the order the model asked for them.
    by_index: std::collections::BTreeMap<i64, (String, String, String)>,
}

impl PartialToolCalls {
    fn absorb(&mut self, delta: &serde_json::Value) {
        let Some(fragments) = delta["tool_calls"].as_array() else {
            return;
        };
        for fragment in fragments {
            let index = fragment["index"].as_i64().unwrap_or(0);
            let entry = self.by_index.entry(index).or_default();
            if let Some(id) = fragment["id"].as_str() {
                if !id.is_empty() {
                    entry.0 = id.to_string();
                }
            }
            if let Some(name) = fragment["function"]["name"].as_str() {
                if !name.is_empty() {
                    entry.1 = name.to_string();
                }
            }
            match &fragment["function"]["arguments"] {
                serde_json::Value::String(piece) => entry.2.push_str(piece),
                // Not the documented streaming shape, but a server that sends
                // the arguments whole as an object should not be discarded for
                // being easier than expected.
                object @ serde_json::Value::Object(_) => entry.2 = object.to_string(),
                _ => {}
            }
        }
    }

    fn finish(self) -> Vec<crate::ai::tools::ToolCall> {
        let calls: Vec<serde_json::Value> = self
            .by_index
            .into_iter()
            .filter(|(_, (_, name, _))| !name.is_empty())
            .map(|(_, (id, name, arguments))| {
                json!({
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": arguments },
                })
            })
            .collect();
        if calls.is_empty() {
            return Vec::new();
        }
        // Reuses the same parser the non-streaming shapes go through, so the
        // string-vs-object argument handling has exactly one implementation.
        crate::ai::tools::parse_tool_calls(&json!({ "tool_calls": calls }))
    }
}

// "Thinking" models leak their whole internal reasoning (<think>...</think>)
// and some custom merges leak raw special tokens (<|endoftext|>, <|im_start|>)
// straight into message.content. None of that is useful to show the user,
// so strip it out — but never truncate here, callers decide their own
// length limit (a one-line connection test vs. a multi-sentence digest
// need very different limits).
fn strip_model_artifacts(raw: &str) -> String {
    let mut text = raw.to_string();

    while let Some(start) = text.find("<think>") {
        if let Some(end) = text[start..].find("</think>") {
            text.replace_range(start..start + end + "</think>".len(), "");
        } else {
            // Unterminated thinking block (truncated stream) — drop the rest.
            text.truncate(start);
        }
    }

    // Special tokens mark the end of any real content — cut there.
    if let Some(idx) = text.find("<|") {
        text.truncate(idx);
    }

    text.trim().to_string()
}

fn truncate(text: &str, max_len: usize) -> String {
    if text.chars().count() > max_len {
        let truncated: String = text.chars().take(max_len).collect();
        format!("{truncated}…")
    } else {
        text.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact list a live Ollama returned for the model this app ships
    // against, and for one that has no vision.
    #[test]
    fn capabilities_are_read_from_what_the_server_listed() {
        let seeing = json!({"capabilities": ["completion", "vision", "tools", "thinking"]});
        let caps = capabilities_from_show(&seeing).expect("should have parsed");
        assert!(caps.vision);
        assert!(caps.tools);
        assert!(caps.known);

        let blind = json!({"capabilities": ["completion"]});
        let caps = capabilities_from_show(&blind).expect("should have parsed");
        assert!(!caps.vision);
        assert!(!caps.tools);
        assert!(caps.known);
    }

    // The distinction the warning depends on: a server that says nothing must
    // not read as "cannot do anything", or every model behind a non-Ollama
    // endpoint gets warned about.
    #[test]
    fn a_server_that_reports_nothing_stays_unknown() {
        assert!(capabilities_from_show(&json!({"model": "x"})).is_none());
        assert!(capabilities_from_show(&json!({"capabilities": "not a list"})).is_none());
    }

    fn build(q: &ModelQuirks, max_tokens: Option<u32>, think: bool) -> serde_json::Value {
        let mut payload = json!({"model": "m"});
        apply_model_params(&mut payload, q, max_tokens, think);
        payload
    }

    #[test]
    fn spells_the_token_cap_the_way_the_model_wants() {
        let default = ModelQuirks::default();
        assert_eq!(build(&default, Some(512), false)["max_tokens"], json!(512));
        assert!(build(&default, Some(512), false).get("max_completion_tokens").is_none());

        let reasoning = ModelQuirks {
            wants_max_completion_tokens: true,
            ..Default::default()
        };
        assert_eq!(build(&reasoning, Some(512), false)["max_completion_tokens"], json!(512));
        // The old spelling must be gone, not merely accompanied — sending both is
        // itself a 400 on OpenAI.
        assert!(build(&reasoning, Some(512), false).get("max_tokens").is_none());
    }

    #[test]
    fn omits_the_token_cap_when_unset_or_zero() {
        let q = ModelQuirks::default();
        assert!(build(&q, None, false).get("max_tokens").is_none());
        // 0 is what an unconfigured profile carries (u32 default), and it would
        // otherwise be sent as a cap of zero tokens.
        assert!(build(&q, Some(0), false).get("max_tokens").is_none());
    }

    #[test]
    fn asks_for_reasoning_only_while_the_switch_is_on_and_accepted() {
        let q = ModelQuirks::default();
        assert_eq!(build(&q, None, true)["reasoning_effort"], json!("medium"));
        // Off means send nothing, not "ask for the least" — see apply_model_params.
        assert!(build(&q, None, false).get("reasoning_effort").is_none());

        let refused = ModelQuirks {
            rejects_reasoning_effort: true,
            ..Default::default()
        };
        assert!(build(&refused, None, true).get("reasoning_effort").is_none());
    }

    #[test]
    fn only_retries_when_the_server_names_the_parameter() {
        use reqwest::StatusCode;
        let openai_reasoning = r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.","type":"invalid_request_error"}}"#;
        assert!(matches!(
            diagnose_param_error(StatusCode::BAD_REQUEST, openai_reasoning),
            ParamFix::UseMaxCompletionTokens
        ));

        let no_reasoning = r#"{"error":{"message":"Unrecognized request argument supplied: reasoning_effort"}}"#;
        assert!(matches!(
            diagnose_param_error(StatusCode::BAD_REQUEST, no_reasoning),
            ParamFix::DropReasoningEffort
        ));

        // Things the user has to fix themselves must NOT be retried.
        assert!(matches!(
            diagnose_param_error(StatusCode::UNAUTHORIZED, r#"{"error":{"message":"Incorrect API key"}}"#),
            ParamFix::None
        ));
        assert!(matches!(
            diagnose_param_error(StatusCode::NOT_FOUND, r#"{"error":{"message":"The model does not exist"}}"#),
            ParamFix::None
        ));
        // A server-side fault is not a request we can repair by rewording it,
        // even if the body happens to mention the parameter.
        assert!(matches!(
            diagnose_param_error(StatusCode::INTERNAL_SERVER_ERROR, "max_completion_tokens exploded"),
            ParamFix::None
        ));
    }

    #[test]
    fn surfaces_the_servers_own_message() {
        use reqwest::StatusCode;
        let msg = error_message(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
        );
        assert!(msg.contains("400"), "{msg}");
        assert!(msg.contains("Incorrect API key provided"), "{msg}");
        // A non-JSON body (an HTML error page from a proxy) still has to produce
        // something readable rather than an empty string.
        let plain = error_message(StatusCode::BAD_GATEWAY, "<html>bad gateway</html>");
        assert!(plain.contains("502") && plain.contains("bad gateway"), "{plain}");
    }
}
