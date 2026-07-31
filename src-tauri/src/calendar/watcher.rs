// Announces a meeting shortly before it starts.
//
// Unlike the mail and GitHub watchers there is no "don't replay history on first
// run" guard here, and deliberately so: this only ever looks at a window a few
// minutes wide starting now, so there is no history to replay. A meeting that is
// about to start is worth mentioning whether or not the widget was running a
// minute ago.
use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::json;
use tauri::{Emitter, Manager};

use crate::google::calendar::{self, CalendarEvent};
use crate::google::oauth;
use crate::tunables;

/// How long an announced occurrence is remembered.
///
/// Only long enough that a still-running meeting cannot be announced twice.
/// Occurrence ids are unique per instance (they carry the start time), so this
/// map would otherwise grow for the life of the install.
const REMEMBER_SECS: i64 = 2 * 24 * 60 * 60;

fn cache_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app.path().app_data_dir().expect("app data dir must be resolvable");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("calendar-announced-cache.json")
}

fn read_cache(app: &tauri::AppHandle) -> HashMap<String, i64> {
    std::fs::read_to_string(cache_path(app))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_cache(app: &tauri::AppHandle, cache: &HashMap<String, i64>) {
    if let Ok(json) = serde_json::to_string_pretty(cache) {
        let _ = std::fs::write(cache_path(app), json);
    }
}

/// Drops entries old enough that the occurrence they name is long over.
pub fn prune(cache: &mut HashMap<String, i64>, now: i64) {
    cache.retain(|_, announced_at| now - *announced_at < REMEMBER_SECS);
}

/// Whether this occurrence is worth a notice right now.
pub fn should_announce(event: &CalendarEvent, include_all_day: bool, already_announced: bool) -> bool {
    if already_announced {
        return false;
    }
    // An all-day entry starts at local midnight, so announcing one means waking
    // the user at 00:00 for a public holiday. They still appear on the morning
    // card either way.
    if event.all_day && !include_all_day {
        return false;
    }
    true
}

/// Scopes an occurrence id to the account it came from.
///
/// Two calendars can legitimately hold the same event — a meeting invited to both
/// a personal and a work address arrives with the same series id on each. Keyed
/// by occurrence alone, whichever account was polled first would silence the
/// other, which is only correct by accident.
fn cache_key(account_id: &str, event_id: &str) -> String {
    format!("{account_id}:{event_id}")
}

fn poll(app: &tauri::AppHandle) {
    let accounts = oauth::accounts(app);
    if accounts.is_empty() {
        return;
    }
    let show_account = accounts.len() > 1;

    let lead = tunables::int(app, tunables::CALENDAR_REMIND_MINUTES).max(1);
    let include_all_day = tunables::toggle(app, tunables::CALENDAR_INCLUDE_ALL_DAY);
    let now_epoch = chrono::Utc::now().timestamp();
    let now_local = chrono::Local::now();

    let mut cache = read_cache(app);
    prune(&mut cache, now_epoch);

    for account in &accounts {
        let events = match calendar::starting_soon(app, &account.id, lead) {
            Ok(events) => events,
            Err(error) => {
                // One calendar failing must not stop the others being checked.
                eprintln!("calendar-watcher [{}]: {}", account.email, error.message());
                continue;
            }
        };

        for event in &events {
            let key = cache_key(&account.id, &event.id);
            if !should_announce(event, include_all_day, cache.contains_key(&key)) {
                continue;
            }
            let _ = app.emit(
                "calendar-soon",
                json!({
                    "title": event.title,
                    "clock": event.clock(),
                    // Can be zero or negative for a meeting that just began — the
                    // fetch window reaches slightly into the past on purpose, so a
                    // poll landing just after the hour still says something.
                    "starts_in_minutes": event.minutes_away(now_local),
                    "location": event.location,
                    "url": event.url,
                    "all_day": event.all_day,
                    "account": if show_account { account.email.clone() } else { String::new() },
                }),
            );
            cache.insert(key, now_epoch);
        }
    }

    write_cache(app, &cache);
}

pub fn start_calendar_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        poll(&app);
        std::thread::sleep(tunables::secs(&app, tunables::CALENDAR_POLL));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Local};

    fn event(all_day: bool) -> CalendarEvent {
        CalendarEvent {
            id: "abc_20260730T090000Z".to_string(),
            title: "Standup".to_string(),
            location: String::new(),
            url: String::new(),
            start: Local::now() + Duration::minutes(5),
            all_day,
        }
    }

    #[test]
    fn a_meeting_is_announced_once() {
        assert!(should_announce(&event(false), false, false));
        assert!(!should_announce(&event(false), false, true));
    }

    // The default that keeps the widget from waking someone at midnight for a
    // birthday, without hiding it from the morning card.
    #[test]
    fn all_day_entries_are_opt_in() {
        assert!(!should_announce(&event(true), false, false));
        assert!(should_announce(&event(true), true, false));
    }

    #[test]
    fn stale_entries_are_forgotten_but_recent_ones_are_kept() {
        let now = 1_800_000_000;
        let mut cache = HashMap::from([
            ("old".to_string(), now - REMEMBER_SECS - 1),
            ("recent".to_string(), now - 60),
        ]);
        prune(&mut cache, now);
        assert!(!cache.contains_key("old"));
        assert!(cache.contains_key("recent"));
    }
}
