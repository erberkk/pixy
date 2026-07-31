// The morning card: what is sitting unread, and what is on today.
//
// Assembled here in Rust and handed to the frontend as structured data, not as
// a paragraph written by a model. That is the lesson github/api.rs already
// learned the hard way (see build_final_digest's comment): asking one call to
// gather, order and de-duplicate a list reliably dropped items. The model's job
// here is strictly per-message compression, which it is good at; deciding what
// goes on the card is arithmetic, and arithmetic should not be guessed.
use std::time::Duration;

use chrono::{Local, Timelike};
use serde_json::json;
use tauri::Emitter;

use crate::config::{read_config, write_config, GoogleAccount};
use crate::google::calendar::{self, CalendarEvent};
use crate::google::gmail::{self, MailMessage};
use crate::google::oauth;
use crate::mail::summary;
use crate::tunables;

/// How many messages are fetched at once.
///
/// The per-message work is two requests — the message, then its thread to see
/// whether it is a reply — and done one after another that was the whole reason
/// the card took so long to appear: sixteen messages meant thirty-two round
/// trips in a row. They are independent, so they go out in parallel. Six at a
/// time rather than all of them, because the point is to stop waiting on latency,
/// not to open a hundred sockets at Google.
const FETCH_THREADS: usize = 6;

/// The unread mail worth putting on the card, pooled across every account.
///
/// Deliberately one merged list rather than a section per account: the question
/// the card answers is "what is waiting for me this morning", and that does not
/// become several questions because the mail arrived in two mailboxes. Each row
/// carries its own account label instead (only when there is more than one).
fn build_items(
    app: &tauri::AppHandle,
    accounts: &[GoogleAccount],
    show_account: bool,
    days: u32,
    max_items: u32,
) -> Vec<serde_json::Value> {
    // Which messages to fetch, gathered across every mailbox first so the actual
    // fetching can be done in one parallel pass rather than account by account.
    let mut wanted: Vec<(usize, String)> = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        match gmail::unread_ids(app, &account.id, max_items, Some(days)) {
            Ok(ids) => wanted.extend(ids.into_iter().map(|id| (index, id))),
            Err(error) => {
                // One mailbox failing must not empty the whole card.
                eprintln!(
                    "morning brief [{}]: couldn't list unread mail — {}",
                    account.email,
                    error.message()
                );
            }
        }
    }

    // Scoped threads so the borrowed app handle and account list need no cloning
    // or Arc; the scope cannot outlive them by construction.
    let chunk_size = wanted.len().div_ceil(FETCH_THREADS).max(1);
    let mut messages: Vec<(usize, MailMessage)> = std::thread::scope(|scope| {
        let handles: Vec<_> = wanted
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|(index, id)| {
                            gmail::fetch_message(app, &accounts[*index].id, id)
                                .ok()
                                .map(|message| (*index, message))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .flatten()
            .collect()
    });

    messages.sort_by_key(|(_, message)| card_order(message));

    messages
        .iter()
        .take(max_items as usize)
        .map(|(index, message)| {
            let account = &accounts[*index];
            json!({
                "from": message.from_name,
                "subject": message.subject,
                // Left sequential on purpose: this is the only step that may call
                // the model, and a local one answers one request at a time
                // anyway — running six at once would queue inside the server
                // instead of here, while costing six times the memory.
                "description": summary::describe(app, message),
                "is_reply_to_me": message.is_reply_to_me,
                "url": message.url(&account.email),
                "account": if show_account { account.email.clone() } else { String::new() },
            })
        })
        .collect()
}

/// Where one message ranks on the card: replies first, newest first within that.
///
/// A named function with a test behind it, because ordering the merged list by
/// the reply flag ALONE was a bug that looked like a simplification. The sort is
/// stable, so with no replies nothing moved and the list kept the order it was
/// built in — account by account. The first mailbox's eight candidates filled all
/// five rows and the second never appeared. Not occasionally: every morning,
/// unless the second one happened to hold a reply.
///
/// Comparing arrival times is what makes the merged list one list rather than two
/// concatenated ones.
fn card_order(message: &MailMessage) -> (bool, i64) {
    (!message.is_reply_to_me, -message.received_at_ms)
}

fn build_events(app: &tauri::AppHandle, accounts: &[GoogleAccount], show_account: bool) -> Vec<serde_json::Value> {
    let mut all: Vec<(&GoogleAccount, CalendarEvent)> = Vec::new();
    for account in accounts {
        match calendar::today(app, &account.id) {
            Ok(events) => all.extend(events.into_iter().map(|event| (account, event))),
            Err(error) => {
                // One half failing must not take the other down: a calendar the
                // account cannot read is no reason to withhold the unread mail.
                eprintln!(
                    "morning brief [{}]: couldn't read the calendar — {}",
                    account.email,
                    error.message()
                );
            }
        }
    }
    // Two calendars merged arrive interleaved by account, not by time — sorted
    // here so the day reads top to bottom whatever it was assembled from.
    all.sort_by_key(|(_, event)| event.start);

    all.iter()
        .map(|(account, event)| {
            json!({
                "title": event.title,
                "clock": event.clock(),
                "location": event.location,
                "url": event.url,
                "all_day": event.all_day,
                "account": if show_account { account.email.clone() } else { String::new() },
            })
        })
        .collect()
}

/// Builds and shows the card. Slow by nature — it fetches several messages and
/// may summarize each — so every caller runs it on its own thread.
pub fn run_brief(app: &tauri::AppHandle) {
    let accounts = oauth::accounts(app);
    if accounts.is_empty() {
        return;
    }
    let show_account = accounts.len() > 1;

    // One window for the whole card, read once so the headline count and the rows
    // below it cannot disagree about which week they are describing.
    let days = tunables::int(app, tunables::MAIL_BRIEF_DAYS).max(1) as u32;

    // Summed across mailboxes: "41 unread" is the number the user cares about, not
    // two numbers they have to add up themselves.
    let unread_count: u64 = accounts
        .iter()
        .map(|account| gmail::unread_count(app, &account.id, Some(days)).unwrap_or(0))
        .sum();
    let max_items = tunables::int(app, tunables::MAIL_BRIEF_MAX_ITEMS).max(1) as u32;
    let items = build_items(app, &accounts, show_account, days, max_items);
    let events = build_events(app, &accounts, show_account);

    // Marked done as soon as the work is done, whether or not there was anything
    // to show — an empty inbox and a clear calendar is a real answer, and
    // retrying every ten minutes for the rest of the day would not improve it.
    let mut cfg = read_config(app);
    cfg.last_brief_date = Some(Local::now().date_naive().to_string());
    write_config(app, &cfg);

    if items.is_empty() && events.is_empty() && unread_count == 0 {
        return;
    }

    let _ = app.emit(
        "daily-brief",
        json!({
            "unread_count": unread_count,
            // Sent so the card can say which window the count is over. A bare
            // number invites the reader to assume "everything", which is what it
            // used to mean and no longer does.
            "days": days,
            "items": items,
            "events": events,
        }),
    );
}

/// Fires once per calendar day, at or after the configured hour in local time.
///
/// Same shape as the GitHub digest watcher, and for the same reasons: checking
/// every ten minutes rather than sleeping until the hour means a machine that
/// was asleep at 9am still gets its card the first time it checks afterwards.
pub fn start_brief_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        let now = Local::now();
        let today = now.date_naive().to_string();
        let already_ran = read_config(&app).last_brief_date.as_deref() == Some(today.as_str());
        // Re-read per check so moving the hour earlier in Settings can still fire
        // today's card.
        let hour = tunables::int(&app, tunables::MAIL_BRIEF_HOUR) as u32;

        if !already_ran && now.hour() >= hour {
            run_brief(&app);
        }

        std::thread::sleep(Duration::from_secs(600));
    });
}

/// Bypasses the once-a-day gate, for the tray's "Run Morning Brief Now".
pub fn run_brief_now(app: tauri::AppHandle) {
    std::thread::spawn(move || run_brief(&app));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(subject: &str, received_at_ms: i64, is_reply_to_me: bool) -> MailMessage {
        MailMessage {
            thread_id: subject.to_string(),
            from_name: "Someone".to_string(),
            from_address: "someone@example.com".to_string(),
            subject: subject.to_string(),
            snippet: String::new(),
            body: String::new(),
            is_reply_to_me,
            received_at_ms,
        }
    }

    fn subjects_in_card_order(mut messages: Vec<MailMessage>) -> Vec<String> {
        messages.sort_by_key(card_order);
        messages.into_iter().map(|m| m.subject).collect()
    }

    /// The regression this function exists for. Built account-by-account, as
    /// build_items does: everything from mailbox A, then everything from B. With
    /// no replies to reshuffle anything, ordering on the reply flag alone left
    /// this list untouched and the card showed A's mail and nothing else.
    #[test]
    fn two_mailboxes_interleave_by_arrival_time() {
        let messages = vec![
            message("a-older", 1_000, false),
            message("a-oldest", 500, false),
            message("b-newest", 3_000, false),
            message("b-newer", 2_000, false),
        ];
        assert_eq!(
            subjects_in_card_order(messages),
            vec!["b-newest", "b-newer", "a-older", "a-oldest"]
        );
    }

    #[test]
    fn a_reply_outranks_newer_mail_that_is_not_one() {
        let messages = vec![
            message("new-stranger", 9_000, false),
            message("old-reply", 100, true),
        ];
        assert_eq!(subjects_in_card_order(messages), vec!["old-reply", "new-stranger"]);
    }

    #[test]
    fn replies_are_ordered_among_themselves_by_time() {
        let messages = vec![
            message("older-reply", 1_000, true),
            message("newer-reply", 5_000, true),
        ];
        assert_eq!(subjects_in_card_order(messages), vec!["newer-reply", "older-reply"]);
    }
}
