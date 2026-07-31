// The slice of the Gmail API this widget uses: which messages are unread, who
// they are from, what they say, and whether one of them is answering something
// the user sent.
//
// Everything here is read-only (see oauth.rs's SCOPES) and parses Google's JSON
// rather than raw RFC 822 — which is most of why this is the API and not IMAP.
// Subjects come back already decoded, so a Turkish subject line arrives as
// "Fatura hakkında" instead of "=?UTF-8?B?RmF0dXJhIGhha2vEsW5kYQ==?=".
use base64::Engine;
use serde_json::Value;

use crate::google::oauth::TokenError;

const API: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// A ceiling on how much of one message body is carried around.
///
/// Long threads quote their entire history back, so a "short" reply routinely
/// arrives as tens of kilobytes of repeated text. Everything downstream of this
/// (the summarizer's prompt, the notice, the daily card) only ever wants the
/// top of the message, and the model in particular should not be handed a
/// conversation's worth of quoted text to summarize one paragraph.
const MAX_BODY_CHARS: usize = 8000;

#[derive(Clone)]
pub struct MailMessage {
    pub thread_id: String,
    /// Display name when the sender supplied one, otherwise the address.
    pub from_name: String,
    pub from_address: String,
    pub subject: String,
    /// Gmail's own one-line preview. Always present, which makes it the safe
    /// thing to fall back on when a body cannot be decoded at all.
    pub snippet: String,
    pub body: String,
    /// True when this message's thread already contains something the user
    /// sent — that is, it is an answer to them rather than a new approach.
    pub is_reply_to_me: bool,
    /// When Gmail received it, in milliseconds since the epoch.
    ///
    /// Needed to order messages from DIFFERENT mailboxes against each other. The
    /// list endpoint returns each account's own mail newest-first, but those two
    /// orders say nothing about each other, and merging them without a shared
    /// clock left the morning card sorted by account: the first mailbox filled
    /// every row and the second was invisible unless it happened to contain a
    /// reply.
    pub received_at_ms: i64,
}

impl MailMessage {
    /// A link that opens this thread in the browser, in the RIGHT mailbox.
    ///
    /// `authuser=<address>` rather than the more familiar `/u/0/`: that path
    /// segment is a position in whatever order the browser happens to have the
    /// user's Google accounts signed in, so with two connected here, clicking a
    /// notice from the second one landed in the first one's inbox. Naming the
    /// address leaves nothing to be inferred.
    pub fn url(&self, account_email: &str) -> String {
        if account_email.is_empty() {
            return format!("https://mail.google.com/mail/u/0/#inbox/{}", self.thread_id);
        }
        format!(
            "https://mail.google.com/mail/u/?authuser={account_email}#inbox/{}",
            self.thread_id
        )
    }

    /// The best text available for this message, whatever was decodable.
    pub fn readable_body(&self) -> &str {
        if self.body.trim().is_empty() {
            &self.snippet
        } else {
            &self.body
        }
    }
}

fn get_json(app: &tauri::AppHandle, account_id: &str, url: &str) -> Result<Value, TokenError> {
    crate::google::get_json(app, account_id, url, "Gmail")
}

/// Gmail encodes every body part as base64url, and pads inconsistently.
fn decode_part(data: &str) -> Option<String> {
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let cleaned: String = data.chars().filter(|c| !c.is_whitespace() && *c != '=').collect();
    let bytes = engine.decode(cleaned).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Walks the MIME tree for the most readable body available.
///
/// Preference order is text/plain, then text/html rendered down to text. A
/// multipart/alternative message carries both, and the plain part is what the
/// sender's client wrote as the readable version — using the HTML one instead
/// means feeding the summarizer a wall of layout tables.
fn extract_body(payload: &Value) -> String {
    fn walk(node: &Value, plain: &mut String, html: &mut String) {
        let mime = node.get("mimeType").and_then(Value::as_str).unwrap_or("");
        // Attachments carry a filename; their bytes are not part of the message
        // text and can be megabytes.
        let is_attachment = node
            .get("filename")
            .and_then(Value::as_str)
            .map(|name| !name.is_empty())
            .unwrap_or(false);

        if !is_attachment {
            if let Some(data) = node.get("body").and_then(|b| b.get("data")).and_then(Value::as_str) {
                if let Some(text) = decode_part(data) {
                    if mime.starts_with("text/plain") && plain.is_empty() {
                        *plain = text;
                    } else if mime.starts_with("text/html") && html.is_empty() {
                        *html = text;
                    }
                }
            }
        }

        if let Some(parts) = node.get("parts").and_then(Value::as_array) {
            for part in parts {
                walk(part, plain, html);
            }
        }
    }

    let mut plain = String::new();
    let mut html = String::new();
    walk(payload, &mut plain, &mut html);

    let text = if !plain.trim().is_empty() {
        plain
    } else if !html.trim().is_empty() {
        // Reuses the same extraction the web tools use on arbitrary pages —
        // marketing mail is exactly as hostile as a web page, and this already
        // knows to drop navigation-shaped furniture.
        crate::web::extract::readable_text(&html).1
    } else {
        String::new()
    };

    let text = crate::web::extract::tidy(&text);
    text.chars().take(MAX_BODY_CHARS).collect()
}

fn header<'a>(payload: &'a Value, name: &str) -> Option<&'a str> {
    payload
        .get("headers")?
        .as_array()?
        .iter()
        .find(|h| {
            h.get("name")
                .and_then(Value::as_str)
                .map(|n| n.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        })?
        .get("value")?
        .as_str()
}

/// Splits a From header into a display name and a bare address.
///
/// Handles the three shapes that actually turn up: `Ali <ali@x.com>`,
/// `"Ali, Support" <ali@x.com>` and a bare `ali@x.com`. When there is no display
/// name the address stands in for it, so callers never have to decide what to
/// show.
pub fn split_from(raw: &str) -> (String, String) {
    let raw = raw.trim();
    if let (Some(open), Some(close)) = (raw.rfind('<'), raw.rfind('>')) {
        if open < close {
            let address = raw[open + 1..close].trim().to_string();
            let name = raw[..open].trim().trim_matches('"').trim().to_string();
            let name = if name.is_empty() { address.clone() } else { name };
            return (name, address);
        }
    }
    let address = raw.trim_matches('"').trim().to_string();
    (address.clone(), address)
}

/// Whether this thread already contains a message the user sent.
///
/// This is the whole of reply detection, and it is why the API is worth the
/// OAuth setup: over IMAP the same question means scanning the Sent folder,
/// keeping a rolling set of Message-IDs, and first finding that folder despite
/// its name being localised. Here it is one label on one cheap request —
/// `format=minimal` returns label lists without a single message body.
fn thread_has_own_message(app: &tauri::AppHandle, account_id: &str, thread_id: &str) -> bool {
    let url = format!("{API}/threads/{thread_id}?format=minimal");
    let Ok(thread) = get_json(app, account_id, &url) else {
        return false;
    };
    thread
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages.iter().any(|message| {
                message
                    .get("labelIds")
                    .and_then(Value::as_array)
                    .map(|labels| labels.iter().any(|l| l.as_str() == Some("SENT")))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Builds a MailMessage from a `messages.get?format=full` response.
///
/// Split out from the fetching so it can be tested against captured JSON without
/// a network or a token.
pub fn parse_message(app: Option<(&tauri::AppHandle, &str)>, message: &Value) -> Option<MailMessage> {
    let id = message.get("id")?.as_str()?.to_string();
    let thread_id = message
        .get("threadId")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_string();
    let payload = message.get("payload").cloned().unwrap_or(Value::Null);

    let (from_name, from_address) = split_from(header(&payload, "From").unwrap_or_default());
    let subject = header(&payload, "Subject").unwrap_or("(no subject)").trim().to_string();
    let subject = if subject.is_empty() { "(no subject)".to_string() } else { subject };

    let is_reply_to_me = match app {
        Some((app, account_id)) => thread_has_own_message(app, account_id, &thread_id),
        None => false,
    };

    // Gmail sends internalDate as a STRING of milliseconds, not a number — read
    // as a number it silently comes back None and every message sorts as if it
    // arrived in 1970, which is the same "one mailbox wins" bug in a new costume.
    let received_at_ms = message
        .get("internalDate")
        .and_then(Value::as_str)
        .and_then(|ms| ms.parse::<i64>().ok())
        .unwrap_or(0);

    Some(MailMessage {
        thread_id,
        received_at_ms,
        from_name,
        from_address,
        subject,
        snippet: message
            .get("snippet")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string(),
        body: extract_body(&payload),
        is_reply_to_me,
    })
}

/// A Gmail search for unread inbox mail, optionally limited to the last `days`.
///
/// Expressed as a query rather than label ids because a time window is only
/// available that way — `newer_than` is Gmail's own search operator, evaluated
/// server-side, so a mailbox with tens of thousands of ancient unread messages
/// costs the same as an empty one.
fn unread_query(days: Option<u32>) -> String {
    let mut q = "in:inbox is:unread".to_string();
    if let Some(days) = days {
        q.push_str(&format!(" newer_than:{days}d"));
    }
    // Spaces and ':' both have to survive the URL, and ':' is what makes an
    // operator an operator rather than a word to search for.
    q.replace(':', "%3A").replace(' ', "%20")
}

/// Ids of the unread messages in the inbox, newest first, within `days`.
///
/// Deliberately id-only: the list endpoint returns nothing else useful, and
/// fetching every message up front would mean paying for mail the caller is
/// about to filter out anyway.
pub fn unread_ids(
    app: &tauri::AppHandle,
    account_id: &str,
    max: u32,
    days: Option<u32>,
) -> Result<Vec<String>, TokenError> {
    let url = format!("{API}/messages?q={}&maxResults={max}", unread_query(days));
    let body = get_json(app, account_id, &url)?;
    Ok(body
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

pub fn fetch_message(app: &tauri::AppHandle, account_id: &str, id: &str) -> Result<MailMessage, TokenError> {
    let body = get_json(app, account_id, &format!("{API}/messages/{id}?format=full"))?;
    parse_message(Some((app, account_id)), &body)
        .ok_or_else(|| TokenError::Transient(format!("Gmail returned an unreadable message ({id}).")))
}

/// How many unread messages arrived in the inbox within `days`.
///
/// Counted through the same search the rows come from, rather than through the
/// INBOX label's `messagesUnread`, which is an all-time figure. That figure read
/// 18,600 on one mailbox here — nearly all of it years old — while the card
/// beneath it listed five messages from this morning. A headline and a list that
/// answer different questions is worse than either alone.
///
/// `resultSizeEstimate` is Google's own word for it: approximate, and it drifts
/// on very large result sets. Over a week of unread mail it is close, and the
/// alternative is paging through every id to count them, which is a lot of
/// requests to put a number on a card.
pub fn unread_count(
    app: &tauri::AppHandle,
    account_id: &str,
    days: Option<u32>,
) -> Result<u64, TokenError> {
    // maxResults=1 because only the estimate is wanted; the ids come from
    // unread_ids with its own, smaller, limit.
    let url = format!("{API}/messages?q={}&maxResults=1", unread_query(days));
    let body = get_json(app, account_id, &url)?;
    Ok(body
        .get("resultSizeEstimate")
        .and_then(Value::as_u64)
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes())
    }

    #[test]
    fn a_display_name_is_separated_from_the_address() {
        assert_eq!(
            split_from("Ali Veli <ali@example.com>"),
            ("Ali Veli".to_string(), "ali@example.com".to_string())
        );
        assert_eq!(
            split_from("\"Destek, Aras\" <destek@aras.com>"),
            ("Destek, Aras".to_string(), "destek@aras.com".to_string())
        );
    }

    // A bare address has to stand in as the display name, or every notice from a
    // sender who set no display name would be captioned with an empty string.
    #[test]
    fn a_bare_address_stands_in_for_its_own_name() {
        assert_eq!(
            split_from("noreply@example.com"),
            ("noreply@example.com".to_string(), "noreply@example.com".to_string())
        );
    }

    #[test]
    fn the_plain_part_wins_over_the_html_one() {
        let payload = json!({
            "mimeType": "multipart/alternative",
            "parts": [
                { "mimeType": "text/plain", "body": { "data": b64("the readable version") } },
                { "mimeType": "text/html", "body": { "data": b64("<table><tr><td>layout</td></tr></table>") } }
            ]
        });
        assert_eq!(extract_body(&payload), "the readable version");
    }

    #[test]
    fn an_html_only_message_is_rendered_down_to_text() {
        let payload = json!({
            "mimeType": "text/html",
            "body": { "data": b64("<html><body><p>Toplantı yarın</p></body></html>") }
        });
        assert!(extract_body(&payload).contains("Toplantı yarın"));
    }

    // Attachment bytes are not message text, and a base64 PDF decoded as UTF-8
    // is both enormous and meaningless — exactly what should not reach a model.
    #[test]
    fn attachments_are_not_treated_as_body_text() {
        let payload = json!({
            "mimeType": "multipart/mixed",
            "parts": [
                { "mimeType": "text/plain", "body": { "data": b64("see attached") } },
                {
                    "mimeType": "text/plain",
                    "filename": "notes.txt",
                    "body": { "data": b64("ATTACHMENT CONTENT") }
                }
            ]
        });
        let body = extract_body(&payload);
        assert!(body.contains("see attached"));
        assert!(!body.contains("ATTACHMENT CONTENT"));
    }

    #[test]
    fn a_whole_message_parses_without_a_token() {
        let message = json!({
            "id": "18f",
            "threadId": "18a",
            "snippet": "Merhaba",
            "payload": {
                "mimeType": "text/plain",
                "headers": [
                    { "name": "From", "value": "Ayşe <ayse@example.com>" },
                    { "name": "Subject", "value": "Fatura hakkında" }
                ],
                "body": { "data": b64("Merhaba, fatura ektedir.") }
            }
        });

        let parsed = parse_message(None, &message).expect("parses");
        assert_eq!(parsed.from_name, "Ayşe");
        assert_eq!(parsed.from_address, "ayse@example.com");
        assert_eq!(parsed.subject, "Fatura hakkında");
        assert!(parsed.body.contains("fatura ektedir"));
        // Names the mailbox, so a notice from the second connected account does
        // not open the first one's inbox.
        assert_eq!(
            parsed.url("me@work.com"),
            "https://mail.google.com/mail/u/?authuser=me@work.com#inbox/18a"
        );
        assert_eq!(parsed.url(""), "https://mail.google.com/mail/u/0/#inbox/18a");
    }

    // A message with no decodable body at all still has to say something, or the
    // notice for it would be blank.
    #[test]
    fn an_undecodable_body_falls_back_to_gmails_snippet() {
        let message = json!({
            "id": "1",
            "threadId": "1",
            "snippet": "Gmail's own preview",
            "payload": { "headers": [] }
        });
        let parsed = parse_message(None, &message).expect("parses");
        assert!(parsed.body.is_empty());
        assert_eq!(parsed.readable_body(), "Gmail's own preview");
        assert_eq!(parsed.subject, "(no subject)");
    }
}
