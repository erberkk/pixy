use std::process::Command;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{read_config, write_config};

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub autostart: bool,
    pub start_command: String,
    pub think: bool,
    pub max_tokens: u32,
}

const DEFAULT_LLM_BASE_URL: &str = "http://localhost:11434/v1";
const DEFAULT_MAX_TOKENS: u32 = 600;

#[tauri::command]
pub fn get_llm_config(app: tauri::AppHandle) -> LlmConfig {
    let cfg = read_config(&app);
    LlmConfig {
        base_url: cfg.llm_base_url.unwrap_or_else(|| DEFAULT_LLM_BASE_URL.to_string()),
        model: cfg.llm_model.unwrap_or_default(),
        api_key: cfg.llm_api_key.unwrap_or_default(),
        autostart: cfg.llm_autostart,
        start_command: cfg.llm_start_command.unwrap_or_default(),
        think: cfg.llm_think.unwrap_or(false),
        max_tokens: cfg.llm_max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
    }
}

#[tauri::command]
pub fn save_llm_config(
    app: tauri::AppHandle,
    base_url: String,
    model: String,
    api_key: String,
    autostart: bool,
    start_command: String,
    think: bool,
    max_tokens: u32,
) {
    let mut cfg = read_config(&app);
    cfg.llm_base_url = Some(base_url);
    cfg.llm_model = Some(model);
    cfg.llm_api_key = Some(api_key);
    cfg.llm_autostart = autostart;
    cfg.llm_start_command = Some(start_command);
    cfg.llm_think = Some(think);
    cfg.llm_max_tokens = Some(max_tokens);
    write_config(&app, &cfg);
}

// PID of the LLM server process we spawned ourselves (if any), so "Quit
// (also stop LLM server)" in the tray menu can kill exactly that process —
// never a server the user already had running before the widget started.
static LLM_CHILD_PID: Mutex<Option<u32>> = Mutex::new(None);

fn is_reachable(base_url: &str) -> bool {
    reqwest::blocking::Client::new()
        .get(base_url.trim_end_matches('/'))
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .is_ok()
}

fn spawn_detached(command_line: &str) -> Option<u32> {
    let mut parts = command_line.split_whitespace();
    let program = parts.next()?;
    let mut cmd = Command::new(program);
    cmd.args(parts);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    cmd.spawn().ok().map(|child| child.id())
}

// Called once at widget startup. Only starts anything if the user opted in
// AND nothing is already answering on their configured base_url — never
// spawns a duplicate server, and never touches a server the user started
// themselves outside the widget.
pub fn maybe_autostart(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    if !cfg.llm_autostart {
        return;
    }
    let Some(command) = cfg.llm_start_command.filter(|s| !s.trim().is_empty()) else {
        return;
    };
    let base_url = cfg
        .llm_base_url
        .unwrap_or_else(|| DEFAULT_LLM_BASE_URL.to_string());
    if is_reachable(&base_url) {
        return;
    }
    if let Some(pid) = spawn_detached(&command) {
        *LLM_CHILD_PID.lock().unwrap() = Some(pid);
    }
}

// Only kills a process this widget spawned itself (see LLM_CHILD_PID above)
// — a no-op if autostart was off or the server was already running.
pub fn stop_autostarted() {
    let Some(pid) = LLM_CHILD_PID.lock().unwrap().take() else {
        return;
    };

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

// Shared by test_llm_connection and summarize_github_activity. Proxied
// through Rust (not fetch() from a webview) so it works against ANY
// OpenAI-compatible server regardless of that server's CORS config —
// reqwest isn't a browser and isn't subject to CORS at all.
fn chat_completion(
    base_url: &str,
    model: &str,
    api_key: &str,
    user_content: &str,
    timeout_secs: u64,
    max_tokens: Option<u32>,
) -> Result<String, String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut payload = json!({
        "model": model,
        "messages": [{"role": "user", "content": user_content}],
        "stream": false
    });
    // Caps how long a model can ramble/hallucinate for — without this, a
    // "thinking" or unstable model can run for minutes and degrade into
    // fabricated content the longer it goes (seen firsthand: a small test
    // prompt was fine, but a longer one produced invented issues, fake
    // dates, and eventually a nonsensical self-generated question).
    if let Some(max) = max_tokens {
        payload["max_tokens"] = json!(max);
    }
    let mut req = reqwest::blocking::Client::new()
        .post(&url)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .json(&payload);
    if !api_key.trim().is_empty() {
        req = req.bearer_auth(api_key);
    }

    let resp = req.send().map_err(|e| format!("Couldn't connect: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Invalid response from server: {e}"))?;

    if !status.is_success() {
        let msg = body
            .get("error")
            .and_then(|e| e.get("message").or(Some(e)))
            .map(|m| m.to_string())
            .unwrap_or_else(|| body.to_string());
        return Err(format!("HTTP {status}: {msg}"));
    }

    let raw = body["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| format!("Unexpected response format: {body}"))?;

    Ok(strip_model_artifacts(raw))
}

#[tauri::command]
pub fn test_llm_connection(
    base_url: String,
    model: String,
    api_key: String,
    think: bool,
    max_tokens: u32,
) -> Result<String, String> {
    let reply = run_chat(&base_url, &model, &api_key, "Reply with just the word OK.", 30, think, max_tokens)?;
    if reply.is_empty() {
        return Ok("(empty reply, but the connection and model are working)".to_string());
    }
    Ok(truncate(&reply, 200))
}

// Judges ONE issue's full thread in isolation — a far more tractable task
// for a small local model than reasoning over 20+ issues in a single call
// (which reliably produced "nothing needs attention" or an empty reply).
// Backend-only, called once per issue from github.rs's
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

    let reply = run_chat(&base_url, &model, &api_key, &prompt, 60, think, max_tokens)?;
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
    user_content: &str,
    timeout_secs: u64,
    think: bool,
    max_tokens: u32,
) -> Result<String, String> {
    if let Some(result) = ollama_native_chat(base_url, model, api_key, user_content, timeout_secs, think) {
        if let Ok(reply) = &result {
            if !reply.is_empty() {
                return Ok(reply.clone());
            }
        }
    }
    chat_completion(base_url, model, api_key, user_content, timeout_secs, Some(max_tokens))
}

fn ollama_native_chat(
    base_url: &str,
    model: &str,
    api_key: &str,
    user_content: &str,
    timeout_secs: u64,
    think: bool,
) -> Option<Result<String, String>> {
    let root = base_url.trim_end_matches('/').strip_suffix("/v1")?;
    let url = format!("{root}/api/chat");

    let result = (|| -> Result<String, String> {
        let mut req = reqwest::blocking::Client::new()
            .post(&url)
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .json(&json!({
                "model": model,
                "messages": [{"role": "user", "content": user_content}],
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
