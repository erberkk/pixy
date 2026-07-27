// Polls CI status for the user's own OPEN pull requests and notifies the
// mascot on a success<->failure transition — mirrors poll_for_merges'
// cache-diff shape in github.rs (see that file's comment for why a fresh
// cache never falsely fires on the first observation).
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use tauri::{Emitter, Manager};

use crate::config::read_config;
use crate::github::{minutes_since, search_issues, USER_AGENT};

fn debug_log_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("github-ci-debug.log")
}

fn append_debug_log(app: &tauri::AppHandle, entry: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(debug_log_path(app)) {
        let _ = writeln!(file, "{entry}");
    }
}

fn ci_cache_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app.path().app_data_dir().expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("github-ci-cache.json")
}

fn read_ci_cache(app: &tauri::AppHandle) -> HashMap<String, String> {
    std::fs::read_to_string(ci_cache_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_ci_cache(app: &tauri::AppHandle, cache: &HashMap<String, String>) {
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(ci_cache_path(app), json);
    }
}

fn fetch_head_sha(token: &str, repo: &str, number: u64) -> Result<String, String> {
    let resp = reqwest::blocking::Client::new()
        .get(format!("https://api.github.com/repos/{repo}/pulls/{number}"))
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .timeout(Duration::from_secs(15))
        .send()
        .map_err(|e| format!("Couldn't connect: {e}"))?;
    let body: serde_json::Value = resp.json().map_err(|e| format!("Invalid response: {e}"))?;
    body.get("head")
        .and_then(|h| h.get("sha"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "no head sha in response".to_string())
}

// "failure" | "success" | "pending" | "none" (no check-runs reported at all
// — repos with no Actions/CI configured shouldn't be treated as either
// state, so callers skip caching/notifying for that case).
fn fetch_ci_conclusion(token: &str, repo: &str, sha: &str) -> Result<String, String> {
    let resp = reqwest::blocking::Client::new()
        .get(format!("https://api.github.com/repos/{repo}/commits/{sha}/check-runs"))
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .timeout(Duration::from_secs(15))
        .send()
        .map_err(|e| format!("Couldn't connect: {e}"))?;
    let body: serde_json::Value = resp.json().map_err(|e| format!("Invalid response: {e}"))?;
    let runs = body.get("check_runs").and_then(|r| r.as_array()).cloned().unwrap_or_default();
    if runs.is_empty() {
        return Ok("none".to_string());
    }
    let mut any_failed = false;
    let mut any_pending = false;
    for run in &runs {
        let status = run.get("status").and_then(|s| s.as_str()).unwrap_or("");
        if status != "completed" {
            any_pending = true;
            continue;
        }
        let conclusion = run.get("conclusion").and_then(|c| c.as_str()).unwrap_or("");
        if matches!(conclusion, "failure" | "timed_out" | "action_required") {
            any_failed = true;
        }
    }
    if any_failed {
        Ok("failure".to_string())
    } else if any_pending {
        Ok("pending".to_string())
    } else {
        Ok("success".to_string())
    }
}

fn poll_for_ci_status(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    let Some(token) = cfg.github_token.filter(|t| !t.trim().is_empty()) else {
        return;
    };
    let prs = match search_issues(&token, "is:pr is:open author:@me") {
        Ok(p) => p,
        Err(e) => {
            append_debug_log(app, &format!("ci-watcher: failed fetching open PRs — {e}"));
            return;
        }
    };

    let mut cache = read_ci_cache(app);
    for pr in &prs {
        let key = format!("{}#{}", pr.repo, pr.number);
        let sha = match fetch_head_sha(&token, &pr.repo, pr.number) {
            Ok(s) => s,
            Err(e) => {
                append_debug_log(app, &format!("ci-watcher: {key} — failed fetching head sha: {e}"));
                continue;
            }
        };
        let conclusion = match fetch_ci_conclusion(&token, &pr.repo, &sha) {
            Ok(c) => c,
            Err(e) => {
                append_debug_log(app, &format!("ci-watcher: {key} — failed fetching check-runs: {e}"));
                continue;
            }
        };
        if conclusion == "pending" || conclusion == "none" {
            continue; // don't cache/compare transient or CI-less states
        }

        let prior = cache.get(&key).cloned();
        // Same "fresh but already in this state within the poll window"
        // guard as poll_for_merges — otherwise a PR that's brand new to the
        // cache and already failing/passing would never notify since there's
        // no prior observation to diff against.
        let is_fresh = prior.is_none() && minutes_since(&pr.updated_at).map(|m| m <= 10).unwrap_or(false);

        if prior.as_deref() == Some("success") && conclusion == "failure" || (is_fresh && conclusion == "failure") {
            append_debug_log(app, &format!("ci-watcher: {key} CI failed — notifying"));
            let _ = app.emit(
                "github-ci",
                json!({ "kind": "failed", "title": pr.title, "repo": pr.repo, "number": pr.number, "url": pr.url }),
            );
        } else if prior.as_deref() == Some("failure") && conclusion == "success" {
            append_debug_log(app, &format!("ci-watcher: {key} CI recovered — notifying"));
            let _ = app.emit(
                "github-ci",
                json!({ "kind": "recovered", "title": pr.title, "repo": pr.repo, "number": pr.number, "url": pr.url }),
            );
        }
        cache.insert(key, conclusion);
    }
    write_ci_cache(app, &cache);
}

pub fn start_ci_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        poll_for_ci_status(&app);
        std::thread::sleep(Duration::from_secs(300));
    });
}
