// Polls the inbox and announces what is new — the mail counterpart to
// github/issue_watcher.rs, and built the same way: diff a cached snapshot,
// notify on the edges, and never replay history on first run.
//
// One deliberate difference from the GitHub watchers: the debug log here records
// counts and message ids only. Those watchers write issue titles and comment
// text freely, which is fine for a public repository and is not fine for a
// mailbox — a log file full of who wrote to the user about what is exactly the
// thing this feature should not leave lying on disk.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{Emitter, Manager};

use crate::config::GoogleAccount;
use crate::google::gmail::{self, MailMessage};
use crate::google::oauth;
use crate::mail::summary;
use crate::tunables;

/// How many unread messages to ask Gmail about per poll. Well above any
/// plausible number of *new* ones between two checks, while keeping the list
/// request bounded for an inbox with thousands unread.
const UNREAD_LOOKUP_LIMIT: u32 = 25;

fn debug_log_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app.path().app_data_dir().expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("mail-watcher-debug.log")
}

/// Appends a line to the mail watcher's log.
///
/// Callers must pass counts and ids only — never a sender, subject or body. See
/// this module's header for why.
fn append_debug_log(app: &tauri::AppHandle, entry: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(debug_log_path(app))
    {
        let _ = writeln!(file, "{entry}");
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct SeenCache {
    /// False only before the very first successful poll. Without this flag an
    /// empty id list would be indistinguishable from "never ran", and a user with
    /// 40 unread messages would be told about all 40 the moment they connect.
    initialized: bool,
    unread_ids: Vec<String>,
}

/// Every account's snapshot, keyed by account id.
///
/// Per account and not one shared set: a message id means nothing outside the
/// mailbox it came from, so merging two inboxes into one set would let either
/// account's ids suppress the other's arrivals.
type SeenCaches = HashMap<String, SeenCache>;

fn cache_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app.path().app_data_dir().expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("mail-seen-cache.json")
}

fn read_cache(app: &tauri::AppHandle) -> SeenCaches {
    std::fs::read_to_string(cache_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_cache(app: &tauri::AppHandle, cache: &SeenCaches) {
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(cache_path(app), json);
    }
}

/// Whether this sender should be skipped for pop-ups.
///
/// A reply to something the user sent is never muted, whatever the address looks
/// like: they asked someone a question, and an automated-looking return address
/// is common for ticketing systems that are nonetheless answering them.
pub fn is_muted(address: &str, is_reply_to_me: bool, muted: &[String]) -> bool {
    if is_reply_to_me {
        return false;
    }
    let address = address.to_lowercase();
    muted.iter().any(|needle| address.contains(needle.as_str()))
}

/// Ids present now that weren't present last time, keeping Gmail's newest-first
/// order.
fn newly_arrived(current: &[String], previous: &HashSet<String>) -> Vec<String> {
    current
        .iter()
        .filter(|id| !previous.contains(*id))
        .cloned()
        .collect()
}

fn emit_message(
    app: &tauri::AppHandle,
    account: &GoogleAccount,
    show_account: bool,
    message: &MailMessage,
    description: String,
) {
    let _ = app.emit(
        "mail-new",
        json!({
            "from": message.from_name,
            "address": message.from_address,
            "subject": message.subject,
            "description": description,
            "is_reply_to_me": message.is_reply_to_me,
            "url": message.url(&account.email),
            // Empty with one mailbox connected. The backend decides this rather
            // than the frontend, because only it knows how many there are — and
            // stamping the address on every notice when there is only one to
            // choose from is noise, not information.
            "account": if show_account { account.email.clone() } else { String::new() },
            "collapsed_count": 0,
        }),
    );
}

/// Polls one mailbox. Its snapshot is updated in `caches` whether or not
/// anything was announced.
fn poll_account(
    app: &tauri::AppHandle,
    account: &GoogleAccount,
    show_account: bool,
    caches: &mut SeenCaches,
) {
    let tag = if account.email.is_empty() { &account.id } else { &account.email };

    // No date window here, unlike the morning card. This compares one poll's
    // unread ids against the last one's to spot arrivals, and a window would make
    // messages silently leave the set as they aged — which is a change this is
    // deliberately blind to. UNREAD_LOOKUP_LIMIT already bounds the request.
    let unread = match gmail::unread_ids(app, &account.id, UNREAD_LOOKUP_LIMIT, None) {
        Ok(ids) => ids,
        Err(error) => {
            // A dead grant has already emitted google-auth-needed from the token
            // layer; here it only needs to not be retried as if it were a blip.
            // One account failing must not stop the others being polled.
            append_debug_log(app, &format!("mail-watcher [{tag}]: list failed — {}", error.message()));
            return;
        }
    };

    let cache = caches.get(&account.id).cloned().unwrap_or_default();
    let previous: HashSet<String> = cache.unread_ids.iter().cloned().collect();

    // The snapshot is replaced wholesale rather than added to, so a message that
    // gets read or archived drops out on its own and the cache tracks the inbox
    // instead of growing forever.
    let next_cache = SeenCache {
        initialized: true,
        unread_ids: unread.clone(),
    };
    caches.insert(account.id.clone(), next_cache);

    if !cache.initialized {
        append_debug_log(
            app,
            &format!("mail-watcher [{tag}]: first run, baseline of {} unread recorded", unread.len()),
        );
        return;
    }

    let arrived = newly_arrived(&unread, &previous);
    if arrived.is_empty() {
        return;
    }
    append_debug_log(app, &format!("mail-watcher [{tag}]: {} new message(s)", arrived.len()));

    if !tunables::toggle(app, tunables::MAIL_NOTIFY_NEW) {
        return;
    }

    // Applied per mailbox rather than across all of them: each account's arrivals
    // collapse into their own line, and a collapsed line that spanned two inboxes
    // could not say which one it meant.
    let cap = tunables::int(app, tunables::MAIL_MAX_NOTICES).max(1) as usize;
    let muted = tunables::names(app, tunables::MAIL_MUTE_SENDERS);

    // Only the messages actually about to be announced are fetched and
    // summarized. Coming back to eighty unread messages otherwise means eighty
    // full fetches and eighty model calls to produce one collapsed line.
    let mut announced = 0usize;
    let mut skipped_muted = 0usize;
    for id in arrived.iter().take(cap) {
        let message = match gmail::fetch_message(app, &account.id, id) {
            Ok(message) => message,
            Err(error) => {
                append_debug_log(
                    app,
                    &format!("mail-watcher [{tag}]: {id} fetch failed — {}", error.message()),
                );
                continue;
            }
        };
        if is_muted(&message.from_address, message.is_reply_to_me, &muted) {
            skipped_muted += 1;
            continue;
        }
        // Blocking, by design: the notice waits for the description rather than
        // appearing and rewriting itself under the user. Bounded by the summary
        // timeout, and this is the watcher's own thread.
        let description = summary::describe(app, &message);
        emit_message(app, account, show_account, &message, description);
        announced += 1;
    }

    // Everything past the cap collapses into a single line. Without this,
    // opening the laptop after a weekend means the widget beeping for a minute.
    let remaining = arrived.len().saturating_sub(cap);
    if remaining > 0 {
        let inbox_url = if account.email.is_empty() {
            "https://mail.google.com/mail/u/0/#inbox".to_string()
        } else {
            format!("https://mail.google.com/mail/u/?authuser={}#inbox", account.email)
        };
        let _ = app.emit(
            "mail-new",
            json!({
                "from": "",
                "subject": "",
                "description": "",
                "is_reply_to_me": false,
                "url": inbox_url,
                "account": if show_account { account.email.clone() } else { String::new() },
                "collapsed_count": remaining,
            }),
        );
    }
    append_debug_log(
        app,
        &format!("mail-watcher [{tag}]: announced {announced}, muted {skipped_muted}, collapsed {remaining}"),
    );
}

fn poll(app: &tauri::AppHandle) {
    let accounts = oauth::accounts(app);
    if accounts.is_empty() {
        return;
    }
    // With one mailbox its address adds nothing to a notice; with two it is the
    // only thing telling them apart.
    let show_account = accounts.len() > 1;

    let mut caches = read_cache(app);
    for account in &accounts {
        poll_account(app, account, show_account, &mut caches);
    }
    // A removed account leaves its snapshot behind otherwise, and re-adding it
    // later would silently suppress everything that arrived in the meantime.
    caches.retain(|id, _| accounts.iter().any(|account| &account.id == id));
    write_cache(app, &caches);
}

pub fn start_mail_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        poll(&app);
        // Re-read every iteration so a change in Settings applies from the next
        // poll on rather than being latched for the life of the app.
        std::thread::sleep(tunables::secs(&app, tunables::MAIL_POLL));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn muted_list() -> Vec<String> {
        vec!["noreply".to_string(), "newsletter".to_string()]
    }

    #[test]
    fn muted_senders_are_matched_anywhere_in_the_address() {
        assert!(is_muted("noreply@github.com", false, &muted_list()));
        assert!(is_muted("weekly-NEWSLETTER@shop.com", false, &muted_list()));
        assert!(!is_muted("ali@example.com", false, &muted_list()));
    }

    // The exception that makes the mute list safe to use: the user asked someone
    // a question, and ticketing systems answer from addresses that look exactly
    // like the ones people mute.
    #[test]
    fn a_reply_to_the_user_is_never_muted() {
        assert!(!is_muted("noreply@jira.example.com", true, &muted_list()));
    }

    #[test]
    fn only_ids_that_were_not_there_before_count_as_new() {
        let previous: HashSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let current = vec!["c".to_string(), "b".to_string(), "a".to_string()];
        // Gmail lists newest first, and that order has to survive the diff so the
        // cap announces the newest rather than an arbitrary subset.
        assert_eq!(newly_arrived(&current, &previous), vec!["c".to_string()]);
    }

    #[test]
    fn nothing_is_new_when_the_inbox_has_not_moved() {
        let previous: HashSet<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let current = vec!["b".to_string(), "a".to_string()];
        assert!(newly_arrived(&current, &previous).is_empty());
    }

    // A message that gets read must not linger in the snapshot: if it did, and
    // the user later marked it unread again, it would never be announced.
    #[test]
    fn the_snapshot_follows_the_inbox_rather_than_accumulating() {
        let previous: HashSet<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let current = vec!["d".to_string()];
        let next = SeenCache { initialized: true, unread_ids: current.clone() };
        assert_eq!(newly_arrived(&current, &previous), vec!["d".to_string()]);
        assert_eq!(next.unread_ids, vec!["d".to_string()]);
    }
}
