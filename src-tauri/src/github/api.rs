use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{Local, Timelike};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{Emitter, Manager};

use crate::config::{read_config, write_config};

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct GithubConfig {
    token: String,
}

#[tauri::command]
pub fn get_github_config(app: tauri::AppHandle) -> GithubConfig {
    let cfg = read_config(&app);
    GithubConfig {
        token: cfg.github_token.unwrap_or_default(),
    }
}

#[tauri::command]
pub fn save_github_config(app: tauri::AppHandle, token: String) {
    let mut cfg = read_config(&app);
    cfg.github_token = Some(token);
    write_config(&app, &cfg);
}

// GitHub's API requires a User-Agent header on every request (rejects
// requests without one) and reqwest doesn't send one by default.
pub(crate) const USER_AGENT: &str = "widget-mascot-app";

pub(crate) fn fetch_username(token: &str) -> Result<String, String> {
    let resp = reqwest::blocking::Client::new()
        .get("https://api.github.com/user")
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token.trim())
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .map_err(|e| format!("Couldn't connect: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Invalid response from server: {e}"))?;

    if !status.is_success() {
        let msg = body
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(format!("HTTP {status}: {msg}"));
    }

    body.get("login")
        .and_then(|l| l.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("Unexpected response format: {body}"))
}

#[tauri::command]
pub async fn test_github_connection(token: String) -> Result<String, String> {
    crate::offload(move || {
        let login = fetch_username(&token)?;
        Ok(format!("Connected: @{login}"))
})
    .await
}

#[derive(Serialize, Clone)]
pub struct GithubItem {
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) repo: String,
    pub(crate) state: String, // "open" | "closed" | "merged"
    pub(crate) updated_at: String,
    pub(crate) comments: u64,
    // Only ever populated for PRs, and only along the digest path (see
    // enrich_with_reviews) — left None for the plain report list so opening
    // that window doesn't cost one extra API call per PR shown.
    #[serde(default)]
    review_state: Option<String>,
    #[serde(default)]
    review_body: Option<String>,
    // Only ever populated for issues, and only along the digest path (see
    // enrich_with_issue_analysis) — an LLM's verdict on whether this issue's
    // full thread still owes the assignee an action, and why.
    #[serde(default)]
    action_note: Option<String>,
}

#[derive(Serialize, Default)]
pub struct GithubReport {
    issues: Vec<GithubItem>,
    pull_requests: Vec<GithubItem>,
}

// GitHub's search API returns both issues and PRs from /search/issues (a PR
// shows up with an extra "pull_request" key) — used for both queries below.
pub(crate) fn search_issues(token: &str, query: &str) -> Result<Vec<GithubItem>, String> {
    let resp = reqwest::blocking::Client::new()
        .get("https://api.github.com/search/issues")
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .query(&[
            ("q", query),
            ("sort", "updated"),
            ("order", "desc"),
            ("per_page", "20"),
        ])
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .map_err(|e| format!("Couldn't connect: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("Invalid response from server: {e}"))?;

    if !status.is_success() {
        let msg = body
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(format!("HTTP {status}: {msg}"));
    }

    let items = body
        .get("items")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();

    Ok(items
        .iter()
        .map(|item| {
            let html_url = item
                .get("html_url")
                .and_then(|u| u.as_str())
                .unwrap_or_default()
                .to_string();
            let repo = html_url
                .strip_prefix("https://github.com/")
                .and_then(|rest| rest.split("/issues/").next())
                .and_then(|rest| rest.split("/pull/").next())
                .unwrap_or("")
                .to_string();
            let pull_request = item.get("pull_request");
            let merged = pull_request
                .and_then(|pr| pr.get("merged_at"))
                .map(|v| !v.is_null())
                .unwrap_or(false);
            let raw_state = item.get("state").and_then(|s| s.as_str()).unwrap_or("open");
            let state = if pull_request.is_some() && merged {
                "merged".to_string()
            } else {
                raw_state.to_string()
            };

            GithubItem {
                number: item.get("number").and_then(|n| n.as_u64()).unwrap_or(0),
                title: item.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                url: html_url,
                repo,
                state,
                updated_at: item.get("updated_at").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                comments: item.get("comments").and_then(|c| c.as_u64()).unwrap_or(0),
                review_state: None,
                review_body: None,
                action_note: None,
            }
        })
        .collect())
}

#[tauri::command]
pub async fn get_github_report(token: String) -> Result<GithubReport, String> {
    crate::offload(move || {
        let token = token.trim();
        if token.is_empty() {
            return Err("No GitHub token configured yet.".to_string());
        }
        let issues = search_issues(token, "is:issue is:open assignee:@me")?;
        let pull_requests = search_issues(token, "is:pr is:open author:@me")?;
        Ok(GithubReport { issues, pull_requests })
})
    .await
}

#[tauri::command]
pub fn open_in_browser(app: tauri::AppHandle, url: String) {
    use tauri_plugin_opener::OpenerExt;
    let _ = app.opener().open_url(url, None::<String>);
}

fn pr_cache_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("github-pr-cache.json")
}

fn read_pr_cache(app: &tauri::AppHandle) -> HashMap<String, String> {
    std::fs::read_to_string(pr_cache_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_pr_cache(app: &tauri::AppHandle, cache: &HashMap<String, String>) {
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(pr_cache_path(app), json);
    }
}

pub(crate) fn minutes_since(updated_at: &str) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(updated_at).ok()?;
    Some((chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_minutes())
}

// Polls the user's PRs (every state, not just open) and notifies the mascot
// the moment one flips from open to merged. The cache starts empty on first
// run, so nothing that was ALREADY merged before the widget noticed it gets
// (falsely) reported as "just merged" — EXCEPT a PR that's brand new to the
// cache but merged within the last poll cycle's worth of time, since that
// can only mean it went open->merged fast enough that we never observed the
// "open" snapshot in between (created and merged inside one 5-minute
// window). Without this, a PR merged that quickly would silently never
// notify, since there's no prior "open" entry to compare against. How wide
// that window is is the merge-freshness setting (tunables.rs).
fn poll_for_merges(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    let Some(token) = cfg.github_token.filter(|t| !t.trim().is_empty()) else {
        return;
    };
    let freshness_minutes = crate::tunables::int(app, crate::tunables::GITHUB_MERGE_FRESHNESS);
    let prs = match search_issues(&token, "is:pr author:@me") {
        Ok(p) => p,
        Err(e) => {
            append_debug_log(app, &format!("merge-watcher: failed fetching PRs — {e}"));
            return;
        }
    };

    let mut cache = read_pr_cache(app);
    for pr in &prs {
        let key = format!("{}#{}", pr.repo, pr.number);
        let prior = cache.get(&key).cloned();
        let was_open = prior.as_deref() == Some("open");
        let is_fresh_unseen_merge = prior.is_none()
            && pr.state == "merged"
            && minutes_since(&pr.updated_at).map(|m| m <= freshness_minutes).unwrap_or(false);

        if (was_open || is_fresh_unseen_merge) && pr.state == "merged" {
            append_debug_log(
                app,
                &format!(
                    "merge-watcher: {key} merged (prior={:?}, fresh_unseen={is_fresh_unseen_merge}) — notifying",
                    prior
                ),
            );
            let _ = app.emit(
                "github-merge",
                json!({ "title": pr.title, "repo": pr.repo, "url": pr.url }),
            );
        }
        cache.insert(key, pr.state.clone());
    }
    write_pr_cache(app, &cache);
}

fn review_cache_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("github-review-request-cache.json")
}

fn read_review_cache(app: &tauri::AppHandle) -> HashMap<String, bool> {
    std::fs::read_to_string(review_cache_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_review_cache(app: &tauri::AppHandle, cache: &HashMap<String, bool>) {
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(review_cache_path(app), json);
    }
}

// Notifies once per PR the moment it starts asking for the user's review —
// cache-diffed the same way as merges (false->true edge only), not every
// poll it's still pending, so this doesn't re-notify every 5 minutes for as
// long as the request sits unanswered.
fn poll_for_review_requests(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    let Some(token) = cfg.github_token.filter(|t| !t.trim().is_empty()) else {
        return;
    };
    let prs = match search_issues(&token, "is:pr is:open review-requested:@me") {
        Ok(p) => p,
        Err(e) => {
            append_debug_log(app, &format!("review-watcher: failed fetching review requests — {e}"));
            return;
        }
    };

    let cache = read_review_cache(app);
    let mut still_pending: HashMap<String, bool> = HashMap::new();
    for pr in &prs {
        let key = format!("{}#{}", pr.repo, pr.number);
        if !cache.get(&key).copied().unwrap_or(false) {
            append_debug_log(app, &format!("review-watcher: {key} — review requested, notifying"));
            let _ = app.emit(
                "github-review-requested",
                json!({ "title": pr.title, "repo": pr.repo, "number": pr.number, "url": pr.url }),
            );
        }
        still_pending.insert(key, true);
    }
    write_review_cache(app, &still_pending);
}

pub fn start_merge_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        poll_for_merges(&app);
        poll_for_review_requests(&app);
        std::thread::sleep(Duration::from_secs(300));
    });
}

fn truncate_str(text: &str, max_len: usize) -> String {
    if text.chars().count() > max_len {
        let truncated: String = text.chars().take(max_len).collect();
        format!("{truncated}…")
    } else {
        text.to_string()
    }
}

// Only the latest review per PR matters for "does this need my attention" —
// GitHub's reviews endpoint returns them in submission order, so the last
// one is the most recent verdict (a later re-review supersedes an earlier
// one from the same or a different reviewer). Pulls the review's own body
// text too — without it the model has nothing to reason about beyond the
// bare state, so it can only parrot "changes requested" back with no real
// explanation of *why*.
fn fetch_latest_review(token: &str, repo: &str, number: u64) -> Option<(String, String)> {
    let url = format!("https://api.github.com/repos/{repo}/pulls/{number}/reviews");
    let resp = reqwest::blocking::Client::new()
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let reviews: Vec<serde_json::Value> = resp.json().ok()?;
    let last = reviews.last()?;
    let state = last.get("state").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let body = last.get("body").and_then(|b| b.as_str()).unwrap_or("").trim().to_string();
    Some((state, body))
}

// The last comment alone can be meaningless without context (e.g. it
// references something earlier, or it's actually the assignee's OWN reply
// already closing the loop) — so this pulls the issue body plus the whole
// comment thread in order, and hands all of it to the model at once rather
// than judging a single message in isolation.
fn fetch_issue_thread(token: &str, repo: &str, number: u64) -> Option<(String, Vec<(String, String)>)> {
    let issue_url = format!("https://api.github.com/repos/{repo}/issues/{number}");
    let issue_resp = reqwest::blocking::Client::new()
        .get(&issue_url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .ok()?;
    let issue: serde_json::Value = issue_resp.json().ok()?;
    let body = issue.get("body").and_then(|b| b.as_str()).unwrap_or("").trim().to_string();

    let comments_url = format!("https://api.github.com/repos/{repo}/issues/{number}/comments");
    let comments_resp = reqwest::blocking::Client::new()
        .get(&comments_url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .query(&[("per_page", "100")])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .ok()?;
    let comments: Vec<serde_json::Value> = if comments_resp.status().is_success() {
        comments_resp.json().unwrap_or_default()
    } else {
        Vec::new()
    };

    // GitHub returns comments oldest-first already — that ordering (not just
    // the last entry) is what lets the model tell "still waiting on you"
    // apart from "you already answered this".
    let thread: Vec<(String, String)> = comments
        .iter()
        .map(|c| {
            let author = c
                .get("user")
                .and_then(|u| u.get("login"))
                .and_then(|l| l.as_str())
                .unwrap_or("unknown")
                .to_string();
            let text = c.get("body").and_then(|b| b.as_str()).unwrap_or("").trim().to_string();
            (author, text)
        })
        .collect();

    Some((body, thread))
}

// Relabels the assignee's own comments as "YOU" instead of leaving the
// model to match a GitHub login string against a username buried in the
// instructions — that string-matching step is exactly the kind of thing it
// got wrong (misread the assignee's own "confirm fix is live, please test"
// comment as if the fix wasn't done, instead of recognizing it as the
// assignee having already acted and handed it to someone else).
fn format_thread(issue_body: &str, username: &str, thread: &[(String, String)]) -> String {
    let mut out = String::new();
    if !issue_body.is_empty() {
        out.push_str(&format!("[issue description]\n{}\n\n", truncate_str(issue_body, 600)));
    }
    // Cap to the most recent comments — a very long-running issue could have
    // dozens, and only the recent tail matters for "is this still pending".
    const MAX_COMMENTS: usize = 15;
    let start = thread.len().saturating_sub(MAX_COMMENTS);
    for (author, text) in &thread[start..] {
        if text.is_empty() {
            continue;
        }
        let label = if author.eq_ignore_ascii_case(username) {
            "YOU".to_string()
        } else {
            format!("@{author}")
        };
        out.push_str(&format!("[{label}]: {}\n\n", truncate_str(text, 400)));
    }
    out
}

// A deterministic stand-in for the model's verdict, used whenever the model
// cannot be asked (none configured) or did not answer (the call failed).
//
// It deliberately claims less than the model would — "there is discussion here
// nobody has read for you" rather than "you owe an answer" — because nothing
// has actually judged it. Saying that much is still far better than what this
// replaces: an LLM failure used to drop the issue from the digest entirely, so
// an issue with twelve new comments simply never appeared. A digest that
// silently shrinks when a background call times out is worse than one that
// admits it did not analyze something.
fn unanalyzed_note(comments: u64, last_author: Option<&str>) -> String {
    let plural = if comments == 1 { "comment" } else { "comments" };
    match last_author {
        Some(author) => format!("{comments} {plural}, last from @{author} (not analyzed)"),
        None => format!("{comments} {plural} (not analyzed)"),
    }
}

// Reads the ENTIRE thread per issue (not just the latest message) and asks
// the LLM, in its own small isolated call, whether the assignee genuinely
// still owes an action here — a per-issue call over one thread is a far
// more tractable task for a small local model than asking one call to
// reason correctly over 20+ issues at once (which reliably produced
// "nothing needs attention" or an empty reply).
//
// `username` and `llm_cfg` are both optional because neither is required for
// the digest to be worth showing: without them every issue that has discussion
// still gets an unanalyzed_note, which is the whole point of this being a
// degradation rather than an early return.
fn enrich_with_issue_analysis(
    app: &tauri::AppHandle,
    token: &str,
    username: Option<&str>,
    llm_cfg: Option<&crate::config::LlmProfile>,
    issues: &mut [GithubItem],
) {
    for issue in issues.iter_mut() {
        let tag = format!("{} #{}", issue.repo, issue.number);
        if issue.comments == 0 {
            append_debug_log(app, &format!("issue {tag}: skipped (0 comments, description-only)"));
            continue;
        }

        // Nothing to ask, so don't pay for a thread fetch either — the comment
        // count is already in the search result, and it is all the fallback
        // note can honestly use.
        let (Some(llm_cfg), Some(username)) = (llm_cfg, username) else {
            append_debug_log(app, &format!("issue {tag}: unanalyzed (no model or no username)"));
            issue.action_note = Some(unanalyzed_note(issue.comments, None));
            continue;
        };

        let Some((body, thread)) = fetch_issue_thread(token, &issue.repo, issue.number) else {
            append_debug_log(app, &format!("issue {tag}: unanalyzed (failed to fetch thread)"));
            issue.action_note = Some(unanalyzed_note(issue.comments, None));
            continue;
        };
        let last_author = thread.last().map(|(author, _)| author.clone());
        let transcript = format_thread(&body, username, &thread);
        if transcript.trim().is_empty() {
            append_debug_log(app, &format!("issue {tag}: skipped (empty transcript)"));
            continue;
        }
        match crate::ai::llm::judge_issue_thread(
            llm_cfg.base_url.clone(),
            llm_cfg.model.clone(),
            llm_cfg.api_key.clone(),
            issue.title.clone(),
            username.to_string(),
            transcript,
            llm_cfg.think,
            llm_cfg.max_tokens,
        ) {
            Ok(Some(note)) => {
                append_debug_log(app, &format!("issue {tag}: ACTION — {note}"));
                issue.action_note = Some(note);
            }
            Ok(None) => {
                // The model read the thread and said no. That is a real
                // verdict, so it is trusted — no fallback note here, or the
                // analysis would never be able to remove anything.
                append_debug_log(app, &format!("issue {tag}: NO_ACTION"));
            }
            Err(e) => {
                append_debug_log(app, &format!("issue {tag}: LLM call failed — {e}"));
                // The one judgement worth making without a model, and the same
                // rule issue_watcher.rs already applies to comment notices: if
                // the last word in the thread was the user's own, this is not
                // sitting waiting on them.
                if last_author
                    .as_deref()
                    .map(|author| author.eq_ignore_ascii_case(username))
                    .unwrap_or(false)
                {
                    append_debug_log(app, &format!("issue {tag}: last comment is yours, not flagged"));
                } else {
                    issue.action_note = Some(unanalyzed_note(issue.comments, last_author.as_deref()));
                }
            }
        }
    }
}

// Only enriches PRs (issues have no review concept) — used only along the
// digest path, not the plain report list, so opening the GitHub panel
// doesn't cost one extra API call per PR just to render it.
fn enrich_with_reviews(token: &str, pull_requests: &mut [GithubItem]) {
    for pr in pull_requests.iter_mut() {
        if let Some((state, body)) = fetch_latest_review(token, &pr.repo, pr.number) {
            pr.review_state = Some(state);
            pr.review_body = if body.is_empty() { None } else { Some(body) };
        }
    }
}

// Staleness is computed here rather than left for the model to infer from list
// position — asking it to judge that from ordering alone (no real dates)
// reliably produced "nothing is urgent" even when items hadn't moved in months.
// The cutoff itself is a setting (tunables.rs).
fn days_since(updated_at: &str) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(updated_at).ok()?;
    Some((chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_days())
}

// Builds the final mascot-facing digest text directly in Rust — no LLM
// involved in this step at all. Earlier versions asked one big LLM call to
// assemble+order+dedupe everything, which reliably dropped items, mixed up
// ordering, and produced one line per stale item instead of a compact
// summary. Since we already know every fact (who has an action, who's
// approved, who's stale) precisely, there's nothing left for a model to
// usefully judge here — assembling it ourselves guarantees nothing is
// missing, issues come before PRs, and stale items collapse into one line.
fn build_final_digest(report: &GithubReport, stale_days: i64) -> String {
    let mut out = vec![format!(
        "- Workload: {} issues, {} pull requests open.",
        report.issues.len(),
        report.pull_requests.len()
    )];

    let mut attention: Vec<String> = Vec::new();

    // Issues with a genuine LLM-derived reason to act — the one thing in
    // this whole digest that actually benefits from a model's judgment.
    for item in &report.issues {
        if let Some(note) = &item.action_note {
            attention.push(format!("{} #{} — {}", item.repo, item.number, truncate_str(note, 400)));
        }
    }

    // Every PR always has *some* review status worth surfacing.
    for item in &report.pull_requests {
        let reason = match item.review_state.as_deref() {
            Some("CHANGES_REQUESTED") => match &item.review_body {
                Some(body) => format!("changes requested — \"{}\"", truncate_str(body, 400)),
                None => "changes requested, no comment given".to_string(),
            },
            Some("APPROVED") => "approved, ready to merge".to_string(),
            _ => "still waiting on a review/approval".to_string(),
        };
        attention.push(format!("{} #{} — {}", item.repo, item.number, reason));
    }

    // Stale items (issues or PRs) collapse into one combined line at the
    // end, rather than one bullet each.
    let stale: Vec<String> = report
        .issues
        .iter()
        .chain(report.pull_requests.iter())
        .filter(|i| days_since(&i.updated_at).map(|d| d >= stale_days).unwrap_or(false))
        .map(|i| format!("{} #{}", i.repo, i.number))
        .collect();
    if !stale.is_empty() {
        attention.push(format!(
            "{} item(s) stale ({stale_days}+ days, no update): {}",
            stale.len(),
            stale.join(", ")
        ));
    }

    if attention.is_empty() {
        out.push("- Attention: nothing urgent.".to_string());
    } else {
        out.extend(attention.into_iter().map(|a| format!("- Attention: {a}")));
    }

    out.join("\n")
}

fn debug_log_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("github-digest-debug.log")
}

// Temporary: appends every digest run's input/output to a plain file so we
// can inspect what actually got sent to the LLM and what came back, without
// building any on-screen debug UI. Remove once the feature is trusted.
fn append_debug_log(app: &tauri::AppHandle, entry: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(debug_log_path(app))
    {
        let _ = writeln!(file, "{entry}\n");
    }
}

// The actual daily-digest job: fetch current issues/PRs, ask the configured
// LLM for one short summary, and surface it via the mascot. Runs on a
// background thread (see start_daily_digest_watcher) once per day, or
// on-demand from the tray's "Run GitHub Digest Now" (for testing without
// waiting for the schedule).
pub fn run_daily_digest(app: &tauri::AppHandle) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cfg = read_config(app);
    let Some(token) = cfg.github_token.clone().filter(|t| !t.trim().is_empty()) else {
        append_debug_log(app, &format!("===== {ts} =====\nskipped: no GitHub token configured"));
        return;
    };

    let mut issues = match search_issues(&token, "is:issue is:open assignee:@me") {
        Ok(i) => i,
        Err(e) => {
            append_debug_log(app, &format!("===== {ts} =====\nfailed fetching issues: {e}"));
            return;
        }
    };
    let mut pull_requests = match search_issues(&token, "is:pr is:open author:@me") {
        Ok(p) => p,
        Err(e) => {
            append_debug_log(app, &format!("===== {ts} =====\nfailed fetching pull requests: {e}"));
            return;
        }
    };
    enrich_with_reviews(&token, &mut pull_requests);

    // Mark today as done as soon as the GitHub fetch succeeds — no need to
    // retry until tomorrow, whether or not there was anything to summarize.
    let mut cfg = cfg;
    cfg.last_digest_date = Some(Local::now().date_naive().to_string());
    write_config(app, &cfg);

    if issues.is_empty() && pull_requests.is_empty() {
        append_debug_log(app, &format!("===== {ts} =====\nskipped: no open issues or PRs"));
        return;
    }

    // A missing model no longer abandons the whole digest. Everything below
    // except the per-issue notes — the workload line, every PR's review state,
    // the stale roll-up — is computed here in Rust and never needed a model at
    // all, so returning early threw most of the digest away in order to skip
    // one optional enrichment.
    let llm_cfg = crate::ai::llm::get_active_llm_profile(app.clone());
    let llm_cfg = (!llm_cfg.model.trim().is_empty()).then_some(llm_cfg);
    if llm_cfg.is_none() {
        append_debug_log(
            app,
            &format!("===== {ts} =====\nno LLM model configured — issue threads go unanalyzed, digest still runs"),
        );
    }

    // Per-issue thread analysis — each is its own LLM call (see
    // enrich_with_issue_analysis), so this can genuinely take a while for a
    // big backlog. That's accepted: this only runs once a day.
    let username = fetch_username(&token).ok();
    if username.is_none() {
        append_debug_log(app, &format!("===== {ts} =====\nfailed to fetch username, issues go unanalyzed"));
    }
    enrich_with_issue_analysis(app, &token, username.as_deref(), llm_cfg.as_ref(), &mut issues);

    let report = GithubReport { issues, pull_requests };
    let summary = build_final_digest(
        &report,
        crate::tunables::int(app, crate::tunables::GITHUB_STALE_DAYS),
    );
    append_debug_log(app, &format!("===== {ts} =====\n--- final digest ---\n{summary}"));
    let _ = app.emit("github-digest", json!({ "summary": summary }));
}

// Checks every 10 minutes; fires once per calendar day, only at/after the
// configured digest hour in local time (so it lands "in the morning", not the
// instant midnight ticks over). If the PC was off/asleep at that hour, this
// still catches up the first time it checks afterward that day (whatever time
// that ends up being) — it only cares "did today already run", not "is it
// exactly that hour right now". Every check (run or skipped) gets a log
// line so it's visible this is actually alive, not just running silently.
pub fn start_daily_digest_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        let now = Local::now();
        let stamp = now.format("%Y-%m-%d %H:%M:%S");
        let today = now.date_naive().to_string();
        let already_ran_today = read_config(&app).last_digest_date.as_deref() == Some(today.as_str());
        // Re-read per check rather than once outside the loop, so moving the
        // hour earlier in Settings can still fire the digest today.
        let digest_hour = crate::tunables::int(&app, crate::tunables::GITHUB_DIGEST_HOUR) as u32;

        if already_ran_today {
            append_debug_log(&app, &format!("[{stamp}] watcher check: already ran today, skipping"));
        } else if now.hour() < digest_hour {
            append_debug_log(
                &app,
                &format!("[{stamp}] watcher check: not yet {digest_hour}:00 local time, skipping for now"),
            );
        } else {
            append_debug_log(
                &app,
                &format!("[{stamp}] watcher check: not run today yet and past {digest_hour}:00 — running now"),
            );
            run_daily_digest(&app);
        }

        std::thread::sleep(Duration::from_secs(600));
    });
}

// Bypasses the once-a-day gate — for manually testing the digest right now
// instead of waiting for the schedule (wired to the tray menu, not any
// on-screen button; this isn't meant to be something the user babysits).
pub fn run_github_digest_now(app: tauri::AppHandle) {
    std::thread::spawn(move || run_daily_digest(&app));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn item(repo: &str, number: u64, comments: u64) -> GithubItem {
        GithubItem {
            number,
            title: format!("Issue {number}"),
            url: format!("https://github.com/{repo}/issues/{number}"),
            repo: repo.to_string(),
            state: "open".to_string(),
            updated_at: fresh(),
            comments,
            review_state: None,
            review_body: None,
            action_note: None,
        }
    }

    #[test]
    fn unanalyzed_note_reads_as_a_count_not_a_verdict() {
        assert_eq!(unanalyzed_note(1, None), "1 comment (not analyzed)");
        assert_eq!(unanalyzed_note(3, None), "3 comments (not analyzed)");
        assert_eq!(
            unanalyzed_note(3, Some("ali")),
            "3 comments, last from @ali (not analyzed)"
        );
    }

    // The defect this whole change exists to fix: when the model could not be
    // asked, an issue with discussion on it used to vanish from the digest
    // entirely. The workload line and the PR rows never needed a model at all.
    #[test]
    fn digest_is_complete_without_any_model_verdicts() {
        let mut issue = item("me/app", 7, 3);
        issue.action_note = Some(unanalyzed_note(3, Some("ali")));
        let report = GithubReport {
            issues: vec![issue],
            pull_requests: vec![item("me/app", 9, 0)],
        };

        let digest = build_final_digest(&report, 30);

        assert!(digest.contains("Workload: 1 issues, 1 pull requests open."));
        assert!(digest.contains("me/app #7 — 3 comments, last from @ali (not analyzed)"));
        // Every PR is always worth a line, whatever its review state.
        assert!(digest.contains("me/app #9 — still waiting on a review/approval"));
        assert!(!digest.contains("nothing urgent"));
    }

    #[test]
    fn an_issue_the_model_cleared_stays_out_of_the_attention_list() {
        let report = GithubReport {
            issues: vec![item("me/app", 7, 3)], // action_note: None == NO_ACTION
            pull_requests: vec![],
        };
        let digest = build_final_digest(&report, 30);
        assert!(digest.contains("Attention: nothing urgent."));
        assert!(!digest.contains("#7"));
    }

    #[test]
    fn stale_items_collapse_into_one_line() {
        let mut old_issue = item("me/app", 1, 0);
        old_issue.updated_at = "2020-01-01T00:00:00Z".to_string();
        let mut old_pr = item("me/app", 2, 0);
        old_pr.updated_at = "2020-01-01T00:00:00Z".to_string();

        let report = GithubReport {
            issues: vec![old_issue],
            pull_requests: vec![old_pr],
        };
        let digest = build_final_digest(&report, 30);

        assert!(digest.contains("2 item(s) stale (30+ days, no update): me/app #1, me/app #2"));
    }

    #[test]
    fn a_requested_change_is_quoted_so_the_reason_survives() {
        let mut pr = item("me/app", 4, 0);
        pr.review_state = Some("CHANGES_REQUESTED".to_string());
        pr.review_body = Some("please rename the flag".to_string());

        let report = GithubReport {
            issues: vec![],
            pull_requests: vec![pr],
        };
        let digest = build_final_digest(&report, 30);

        assert!(digest.contains("changes requested — \"please rename the flag\""));
    }
}
