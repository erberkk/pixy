// Google integration: the OAuth grant (oauth), and the two read-only API
// clients built on it (gmail, calendar).
//
// Nothing here decides anything or shows anything — that belongs to the mail/
// and calendar/ watchers. This layer's whole job is "given a signed-in account,
// hand back typed data", so those watchers never touch a token or an HTTP status.
pub mod calendar;
pub mod gmail;
pub mod oauth;

use std::time::Duration;

use serde_json::Value;

use oauth::TokenError;

const TIMEOUT: Duration = Duration::from_secs(20);

/// One authenticated GET against a Google API, with the failure classification
/// both clients need.
///
/// Shared rather than written twice because the interesting part is not the
/// request, it is deciding what each failure means: only the errors that a
/// second attempt can never fix should reach the user as "sign in again".
pub(crate) fn get_json(
    app: &tauri::AppHandle,
    account_id: &str,
    url: &str,
    service: &str,
) -> Result<Value, TokenError> {
    let token = oauth::access_token(app, account_id)?;
    let response = reqwest::blocking::Client::new()
        .get(url)
        .bearer_auth(token)
        .timeout(TIMEOUT)
        .send()
        .map_err(|e| TokenError::Transient(format!("Couldn't reach {service}: {e}")))?;

    let status = response.status();

    // A 401 means the access token went stale between the expiry check and this
    // request landing. Transient on purpose: the next poll refreshes and
    // succeeds, whereas calling this a dead grant would ask the user to sign in
    // again over a one-second race.
    if status == reqwest::StatusCode::UNAUTHORIZED {
        oauth::forget_access_token(account_id);
        return Err(TokenError::Transient(format!("{service} rejected the access token.")));
    }
    // A 403 is the opposite: the token is fine and the account is not allowed to
    // do this — a scope that was never granted, the API not switched on in the
    // Cloud project, or a Workspace policy. Retrying will produce the same answer
    // forever.
    //
    // Google's own message is passed through rather than replaced with a guess.
    // The guess used to be "the account may not have granted this permission",
    // which names the wrong thing for the most common cause by far: an API that
    // was never enabled in the project answers 403 too, and its real message
    // includes the console URL that switches it on. Discarding that turned a
    // one-click fix into a hunt.
    if status == reqwest::StatusCode::FORBIDDEN {
        let detail = google_error_message(response);
        return Err(TokenError::NeedsConsent(match detail {
            Some(message) => format!("{service}: {message}"),
            None => format!(
                "{service} access was refused — the API may not be enabled for your \
                 Google Cloud project, or the account did not grant this permission."
            ),
        }));
    }
    if !status.is_success() {
        return Err(TokenError::Transient(format!("{service} returned HTTP {status}")));
    }

    response
        .json()
        .map_err(|e| TokenError::Transient(format!("Unreadable reply from {service}: {e}")))
}

/// Google's own explanation out of an error response, if it sent one.
///
/// Every Google API returns the same envelope — `{"error": {"message": …}}` — and
/// that message is usually the whole answer: which API is off, which project it
/// belongs to, and the URL that turns it on.
fn google_error_message(response: reqwest::blocking::Response) -> Option<String> {
    let body: Value = response.json().ok()?;
    let message = body["error"]["message"].as_str()?.trim();
    (!message.is_empty()).then(|| message.to_string())
}
