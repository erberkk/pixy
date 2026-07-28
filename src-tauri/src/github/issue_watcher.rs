use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{Emitter, Manager};

use crate::config::read_config;
use crate::github::{fetch_username, minutes_since, search_issues, GithubItem, USER_AGENT};

// Mirrors github/api.rs's MERGE_FRESHNESS_MINUTES — a brand-new issue can be
// created AND self-assigned (or created already closed) in one action, so
// the very first time this watcher ever sees it, there's no "before"
// snapshot to diff against. Without this, that assignment/close would
// silently never notify, the same failure mode the merge watcher's
// freshness fallback exists to avoid.
const ISSUE_FRESHNESS_MINUTES: i64 = 10;

// Separate from github/api.rs's merge watcher (same polling pattern, different
// concern: issue lifecycle rather than "did one of my PRs get merged") so
// the two stay easy to reason about independently.

fn debug_log_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("issue-watcher-debug.log")
}

fn append_debug_log(app: &tauri::AppHandle, entry: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(debug_log_path(app))
    {
        use std::io::Write;
        let _ = writeln!(file, "{entry}\n");
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct IssueSnapshot {
    state: String,
    comments: u64,
    assigned_to_me: bool,
}

fn issue_cache_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("github-issue-cache.json")
}

fn read_issue_cache(app: &tauri::AppHandle) -> HashMap<String, IssueSnapshot> {
    std::fs::read_to_string(issue_cache_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_issue_cache(app: &tauri::AppHandle, cache: &HashMap<String, IssueSnapshot>) {
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(issue_cache_path(app), json);
    }
}

// Fetches just the LAST comment on an issue (not the whole thread) — the
// issue-comments endpoint has no "sort=desc" option, so this instead
// computes which page the last comment falls on from the already-known
// total comment count and requests only that page.
fn fetch_last_comment(token: &str, repo: &str, number: u64, total_comments: u64) -> Option<(String, String)> {
    let per_page: u64 = 100;
    let page = if total_comments == 0 { 1 } else { total_comments.div_ceil(per_page) };
    let url = format!("https://api.github.com/repos/{repo}/issues/{number}/comments");
    let resp = reqwest::blocking::Client::new()
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token)
        .query(&[("per_page", per_page.to_string()), ("page", page.to_string())])
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let comments: Vec<serde_json::Value> = resp.json().ok()?;
    let last = comments.last()?;
    let author = last.get("user").and_then(|u| u.get("login")).and_then(|l| l.as_str())?.to_string();
    let body = last.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string();
    Some((author, body))
}

fn item_key(item: &GithubItem) -> String {
    format!("{}#{}", item.repo, item.number)
}

fn poll_for_issue_updates(app: &tauri::AppHandle) {
    let cfg = read_config(app);
    let Some(token) = cfg.github_token.filter(|t| !t.trim().is_empty()) else {
        return;
    };

    // involves:@me is deliberately broader than assignee:@me — it also
    // covers issues the user authored, commented on, or was mentioned in,
    // matching "issues I'm involved with", not just ones assigned to me.
    // No is:open/is:closed filter, so both states come back — needed to
    // detect open->closed and closed->open transitions.
    let involved = match search_issues(&token, "is:issue involves:@me") {
        Ok(items) => items,
        Err(e) => {
            append_debug_log(app, &format!("issue-watcher: failed fetching involved issues — {e}"));
            return;
        }
    };
    // A second query just to know CURRENT assignment status per issue —
    // simpler than parsing the assignees array out of the raw search
    // response, and mirrors the existing digest job's pattern of separate
    // targeted queries.
    let assigned = match search_issues(&token, "is:issue assignee:@me") {
        Ok(items) => items,
        Err(e) => {
            append_debug_log(app, &format!("issue-watcher: failed fetching assigned issues — {e}"));
            return;
        }
    };
    let assigned_keys: std::collections::HashSet<String> = assigned.iter().map(item_key).collect();

    let username = match fetch_username(&token) {
        Ok(u) => u,
        Err(e) => {
            append_debug_log(app, &format!("issue-watcher: failed fetching username, skipping this poll — {e}"));
            return;
        }
    };

    let mut cache = read_issue_cache(app);
    for item in &involved {
        let key = item_key(item);
        let is_assigned_now = assigned_keys.contains(&key);
        let prior = cache.get(&key).cloned();

        // First time this issue has ever been seen — normally just record
        // its baseline and move on (without this, every issue the user is
        // already involved in at first launch would fire a notification the
        // moment the watcher starts, which isn't "new" from the user's
        // perspective). EXCEPT when the issue itself is very recent
        // (ISSUE_FRESHNESS_MINUTES) — creating an issue and self-assigning
        // (or self-closing) it in one action means the very first poll
        // already sees the "after" state with nothing to diff against, so
        // that combination would otherwise silently never notify.
        let Some(prior) = prior else {
            let is_fresh = minutes_since(&item.updated_at).map(|m| m <= ISSUE_FRESHNESS_MINUTES).unwrap_or(false);
            if is_fresh && is_assigned_now {
                append_debug_log(app, &format!("issue-watcher: {key} newly assigned to you (fresh, unseen before)"));
                let _ = app.emit(
                    "github-issue-update",
                    json!({ "kind": "assigned", "title": item.title, "number": item.number, "repo": item.repo, "url": item.url }),
                );
            }
            if is_fresh && item.state == "closed" {
                append_debug_log(app, &format!("issue-watcher: {key} closed (fresh, unseen before)"));
                let _ = app.emit(
                    "github-issue-update",
                    json!({ "kind": "closed", "title": item.title, "number": item.number, "repo": item.repo, "url": item.url }),
                );
            }
            cache.insert(
                key,
                IssueSnapshot {
                    state: item.state.clone(),
                    comments: item.comments,
                    assigned_to_me: is_assigned_now,
                },
            );
            continue;
        };

        if !prior.assigned_to_me && is_assigned_now {
            append_debug_log(app, &format!("issue-watcher: {key} newly assigned to you"));
            let _ = app.emit(
                "github-issue-update",
                json!({ "kind": "assigned", "title": item.title, "number": item.number, "repo": item.repo, "url": item.url }),
            );
        }

        if item.comments > prior.comments {
            match fetch_last_comment(&token, &item.repo, item.number, item.comments) {
                Some((author, body)) if author != username => {
                    append_debug_log(app, &format!("issue-watcher: {key} new comment by {author}"));
                    let snippet: String = body.chars().take(400).collect();
                    let _ = app.emit(
                        "github-issue-update",
                        json!({
                            "kind": "comment",
                            "title": item.title,
                            "number": item.number,
                            "repo": item.repo,
                            "url": item.url,
                            "detail": format!("{author}: {snippet}"),
                        }),
                    );
                }
                Some(_) => {
                    // Last comment was the user's own — per explicit
                    // request, the user's own comments don't self-notify.
                }
                None => {
                    append_debug_log(app, &format!("issue-watcher: {key} comment count grew but failed to fetch last comment"));
                }
            }
        }

        if prior.state == "open" && item.state == "closed" {
            append_debug_log(app, &format!("issue-watcher: {key} closed"));
            let _ = app.emit(
                "github-issue-update",
                json!({ "kind": "closed", "title": item.title, "number": item.number, "repo": item.repo, "url": item.url }),
            );
        } else if prior.state == "closed" && item.state == "open" {
            append_debug_log(app, &format!("issue-watcher: {key} reopened"));
            let _ = app.emit(
                "github-issue-update",
                json!({ "kind": "reopened", "title": item.title, "number": item.number, "repo": item.repo, "url": item.url }),
            );
        }

        cache.insert(
            key,
            IssueSnapshot {
                state: item.state.clone(),
                comments: item.comments,
                assigned_to_me: is_assigned_now,
            },
        );
    }
    write_issue_cache(app, &cache);
}

// Polling (not push/webhook) means any state round-trip that fully
// completes BETWEEN two polls is invisible by construction — e.g. close
// then reopen an issue within one interval, and the poll right after only
// ever sees "open" both before and after, with nothing to diff against.
// 90s (vs. the merge watcher's 300s) shrinks that blind window without
// meaningfully touching the GitHub API rate limit — at ~3 requests/poll
// this is still only ~120 requests/hour, well under the 5000/hour cap for
// an authenticated token.
const POLL_INTERVAL_SECS: u64 = 90;

pub fn start_issue_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        poll_for_issue_updates(&app);
        std::thread::sleep(Duration::from_secs(POLL_INTERVAL_SECS));
    });
}
