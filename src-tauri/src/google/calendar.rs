// The slice of the Google Calendar API this widget uses: what is on today, and
// what is about to start.
//
// `singleEvents=true` is the reason this is a small file. It makes Google expand
// recurrence rules server-side, so a weekly standup arrives as one concrete
// occurrence with a real start time instead of an RRULE this app would have to
// implement RFC 5545 to understand — and time zones, DST and EXDATEs with it.
use chrono::{DateTime, Duration as ChronoDuration, Local, NaiveDate, TimeZone, Utc};
use serde_json::Value;

use crate::google::oauth::TokenError;

const API: &str = "https://www.googleapis.com/calendar/v3/calendars/primary/events";

/// A ceiling on one request. The widget only ever looks at a day or two, and
/// a calendar with more than this on it in that span is not going to be read off
/// a mascot pill anyway.
const MAX_RESULTS: u32 = 50;

#[derive(Clone)]
pub struct CalendarEvent {
    /// Unique per occurrence, not per series: with singleEvents=true Google
    /// returns "{seriesId}_{originalStart}" for each instance. That is what makes
    /// it safe to use as the "already announced" key for a daily standup.
    pub id: String,
    pub title: String,
    pub location: String,
    pub url: String,
    pub start: DateTime<Local>,
    /// True for entries with a date but no time. They start at local midnight,
    /// which is why announcing them is opt-in (see CALENDAR_INCLUDE_ALL_DAY).
    pub all_day: bool,
}

impl CalendarEvent {
    /// Whole minutes from now until this starts. Negative once it has begun.
    pub fn minutes_away(&self, now: DateTime<Local>) -> i64 {
        (self.start - now).num_minutes()
    }

    /// "09:30", or "all day" for a dateless entry.
    pub fn clock(&self) -> String {
        if self.all_day {
            "all day".to_string()
        } else {
            self.start.format("%H:%M").to_string()
        }
    }
}

/// Reads one entry from the events list, or None if it is not something to show.
///
/// Split out from fetching so the filtering rules can be tested against captured
/// JSON — they are the part with judgement in them.
pub fn parse_event(item: &Value) -> Option<CalendarEvent> {
    // A cancelled instance of a recurring series still comes back in the list;
    // it is a tombstone, not a meeting.
    if item.get("status").and_then(Value::as_str) == Some("cancelled") {
        return None;
    }

    // An invitation the user already declined is not their meeting. Only their
    // own attendee row matters here — everyone else's response is their business.
    let declined = item
        .get("attendees")
        .and_then(Value::as_array)
        .map(|attendees| {
            attendees.iter().any(|attendee| {
                attendee.get("self").and_then(Value::as_bool).unwrap_or(false)
                    && attendee.get("responseStatus").and_then(Value::as_str) == Some("declined")
            })
        })
        .unwrap_or(false);
    if declined {
        return None;
    }

    let start_node = item.get("start")?;
    let (start, all_day) = if let Some(date_time) = start_node.get("dateTime").and_then(Value::as_str) {
        let parsed = DateTime::parse_from_rfc3339(date_time).ok()?;
        (parsed.with_timezone(&Local), false)
    } else {
        // An all-day entry is a bare "2026-07-30" with no zone: it means that
        // calendar day where the user is, so it resolves against local midnight.
        let date = start_node.get("date").and_then(Value::as_str)?;
        let naive = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?.and_hms_opt(0, 0, 0)?;
        (Local.from_local_datetime(&naive).single()?, true)
    };

    Some(CalendarEvent {
        id: item.get("id")?.as_str()?.to_string(),
        title: item
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("(no title)")
            .trim()
            .to_string(),
        location: item
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        url: item
            .get("htmlLink")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        start,
        all_day,
    })
}

fn fetch(
    app: &tauri::AppHandle,
    account_id: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<CalendarEvent>, TokenError> {
    let mut url = reqwest::Url::parse(API).expect("static URL parses");
    url.query_pairs_mut()
        // Without this a recurring series arrives once, as a rule to evaluate.
        .append_pair("singleEvents", "true")
        .append_pair("orderBy", "startTime")
        .append_pair("timeMin", &from.to_rfc3339())
        .append_pair("timeMax", &to.to_rfc3339())
        .append_pair("maxResults", &MAX_RESULTS.to_string());

    let body = crate::google::get_json(app, account_id, url.as_str(), "Calendar")?;
    Ok(body
        .get("items")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_event).collect())
        .unwrap_or_default())
}

/// Everything starting between now and `minutes` from now.
///
/// The window starts slightly in the past so a meeting that began a moment ago
/// is still reported — otherwise a poll landing at 09:00:30 would skip the 09:00
/// meeting entirely rather than mentioning it late.
pub fn starting_soon(
    app: &tauri::AppHandle,
    account_id: &str,
    minutes: i64,
) -> Result<Vec<CalendarEvent>, TokenError> {
    let now = Utc::now();
    fetch(
        app,
        account_id,
        now - ChronoDuration::minutes(2),
        now + ChronoDuration::minutes(minutes),
    )
}

/// Everything on today's local calendar, in start order.
pub fn today(app: &tauri::AppHandle, account_id: &str) -> Result<Vec<CalendarEvent>, TokenError> {
    let start_of_day = Local::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|naive| Local.from_local_datetime(&naive).single())
        // A DST spring-forward can make local midnight not exist. Falling back to
        // "now" loses the earlier part of that one day rather than the whole day.
        .unwrap_or_else(Local::now);
    let end_of_day = start_of_day + ChronoDuration::days(1);
    fetch(
        app,
        account_id,
        start_of_day.with_timezone(&Utc),
        end_of_day.with_timezone(&Utc),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_timed_event_keeps_its_instant_across_zones() {
        let event = parse_event(&json!({
            "id": "abc_20260730T090000Z",
            "summary": "Standup",
            "location": "Meet",
            "htmlLink": "https://calendar.google.com/event?eid=abc",
            "start": { "dateTime": "2026-07-30T09:00:00+03:00" }
        }))
        .expect("parses");

        assert_eq!(event.title, "Standup");
        assert!(!event.all_day);
        // Whatever zone the test machine is in, this is the same moment.
        assert_eq!(event.start.with_timezone(&Utc).to_rfc3339(), "2026-07-30T06:00:00+00:00");
    }

    #[test]
    fn a_dateless_entry_is_marked_all_day() {
        let event = parse_event(&json!({
            "id": "holiday",
            "summary": "Bayram",
            "start": { "date": "2026-07-30" }
        }))
        .expect("parses");

        assert!(event.all_day);
        assert_eq!(event.clock(), "all day");
        assert_eq!(event.start.format("%H:%M").to_string(), "00:00");
    }

    // Both of these still arrive in the events list, and showing either would be
    // telling the user about a meeting that is not happening to them.
    #[test]
    fn cancelled_and_declined_entries_are_dropped() {
        assert!(parse_event(&json!({
            "id": "x",
            "status": "cancelled",
            "start": { "dateTime": "2026-07-30T09:00:00+03:00" }
        }))
        .is_none());

        assert!(parse_event(&json!({
            "id": "y",
            "summary": "Someone else's meeting",
            "start": { "dateTime": "2026-07-30T09:00:00+03:00" },
            "attendees": [
                { "self": true, "responseStatus": "declined" },
                { "responseStatus": "accepted" }
            ]
        }))
        .is_none());
    }

    // Somebody else declining is not a reason to hide the meeting from the user.
    #[test]
    fn another_attendee_declining_changes_nothing() {
        assert!(parse_event(&json!({
            "id": "z",
            "summary": "Review",
            "start": { "dateTime": "2026-07-30T09:00:00+03:00" },
            "attendees": [
                { "self": true, "responseStatus": "accepted" },
                { "responseStatus": "declined" }
            ]
        }))
        .is_some());
    }

    #[test]
    fn minutes_away_counts_down_and_then_goes_negative() {
        let now = Local::now();
        let mut event = parse_event(&json!({
            "id": "a",
            "summary": "Soon",
            "start": { "dateTime": "2026-07-30T09:00:00+03:00" }
        }))
        .expect("parses");

        event.start = now + ChronoDuration::minutes(10);
        assert_eq!(event.minutes_away(now), 10);

        event.start = now - ChronoDuration::minutes(5);
        assert_eq!(event.minutes_away(now), -5);
    }
}
