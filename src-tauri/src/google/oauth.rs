// Google sign-in for the Gmail and Calendar features, using the OAuth 2.0
// "installed application" flow: a loopback redirect plus PKCE.
//
// Why the user brings their own client id and secret instead of one compiled
// into the widget: a desktop binary cannot keep a secret, which Google itself
// acknowledges by requiring PKCE for this client type. Shipping one would mean
// every copy of this app shared a single OAuth app's quota, consent screen and
// review status — and anyone could extract it anyway. Theirs stays theirs.
//
// The consent step is genuinely interactive (a browser, a human, a click), so
// everything here blocks and belongs off the main thread — see crate::offload.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tauri::Emitter;

use crate::config::{read_config, write_config, GoogleAccount};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

// Read-only throughout: this widget shows you your own mail and calendar. It
// never sends, deletes, accepts or declines anything, and asking for scopes
// that would let it is how a "show me my inbox" feature quietly becomes one
// that can empty it.
//
// gmail.readonly rather than the narrower gmail.metadata because summarizing a
// long message needs the message — metadata scope returns headers only, so with
// it the summary feature could not exist at all.
const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/calendar.readonly",
    "https://www.googleapis.com/auth/userinfo.email",
];

// How long the loopback listener waits for the browser to come back. Five
// minutes is enough to pick an account and read a consent screen; past that the
// user has almost certainly closed the tab, and the alternative is a thread and
// a bound port living until the app exits.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

// An access token is refreshed a little before Google's own expiry rather than
// exactly at it, so a request that starts while the token is technically still
// valid cannot arrive after it isn't.
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);

/// The single page served to the browser after Google redirects back. Plain and
/// self-contained: the loopback server is gone a moment later, so nothing it
/// references could load anyway.
fn done_page(message: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Widget</title>\
         <body style=\"font-family:-apple-system,'Segoe UI',sans-serif;background:#0d0d0d;\
         color:#eee;display:grid;place-items:center;height:100vh;margin:0\">\
         <div style=\"text-align:center\"><h2 style=\"font-weight:600\">{message}</h2>\
         <p style=\"color:#8b949e\">You can close this tab and go back to the widget.</p></div>"
    )
}

/// A 64-character random string, used both for the PKCE verifier and for the
/// `state` parameter.
///
/// Two v4 UUIDs rather than a new random-number dependency: uuid is already here
/// for request ids, v4 is 122 bits of OS randomness each, and hex digits are
/// inside PKCE's unreserved character set. 64 characters also sits comfortably
/// inside the 43..128 the spec allows.
fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn s256_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Why a token request failed, to the only resolution that differs.
pub enum TokenError {
    /// The grant itself is gone — revoked in the Google account, the password
    /// changed, or (the common one) the OAuth consent screen is still in
    /// "Testing", where Google expires refresh tokens after seven days. Nothing
    /// retries out of this; the user has to sign in again.
    NeedsConsent(String),
    /// Anything else: offline, DNS, a 500 at Google. Worth retrying on the next
    /// poll, and specifically NOT worth throwing the refresh token away over.
    Transient(String),
}

impl TokenError {
    pub fn message(&self) -> &str {
        match self {
            TokenError::NeedsConsent(m) | TokenError::Transient(m) => m,
        }
    }
}

fn post_token_request(form: &[(&str, &str)]) -> Result<TokenResponse, TokenError> {
    let response = reqwest::blocking::Client::new()
        .post(TOKEN_URL)
        .form(form)
        .timeout(HTTP_TIMEOUT)
        .send()
        .map_err(|e| TokenError::Transient(format!("Couldn't reach Google: {e}")))?;

    let status = response.status();
    let body: TokenResponse = response
        .json()
        .map_err(|e| TokenError::Transient(format!("Unexpected reply from Google: {e}")))?;

    if let Some(error) = &body.error {
        let detail = body.error_description.clone().unwrap_or_else(|| error.clone());
        // invalid_grant is the one error that means "this will never work
        // again", so it is the one that must not be retried forever in silence.
        return Err(if error == "invalid_grant" {
            TokenError::NeedsConsent(detail)
        } else {
            TokenError::Transient(format!("Google refused the request ({error}): {detail}"))
        });
    }
    if !status.is_success() {
        return Err(TokenError::Transient(format!("Google returned HTTP {status}")));
    }
    Ok(body)
}

// The live access token per account, and when each stops being usable.
// Deliberately memory-only and never written to config.json: one lasts an hour,
// so a persisted copy would be stale far more often than it was useful. Keyed by
// account id rather than a single slot, or two connected mailboxes would keep
// overwriting each other's token and every other request would 401.
static ACCESS_TOKENS: OnceLock<Mutex<HashMap<String, (String, Instant)>>> = OnceLock::new();

fn access_token_cache() -> &'static Mutex<HashMap<String, (String, Instant)>> {
    ACCESS_TOKENS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drops one account's cached access token. Called after a fresh sign-in and
/// after disconnecting, so nothing keeps working on a token just replaced.
pub fn forget_access_token(account_id: &str) {
    if let Ok(mut guard) = access_token_cache().lock() {
        guard.remove(account_id);
    }
}

/// Drops every cached token — for when the OAuth client itself changes, which
/// invalidates all of them at once.
fn forget_all_access_tokens() {
    if let Ok(mut guard) = access_token_cache().lock() {
        guard.clear();
    }
}

fn client_credentials(app: &tauri::AppHandle) -> Option<(String, String)> {
    let cfg = read_config(app);
    let id = cfg.google_client_id.filter(|v| !v.trim().is_empty())?;
    let secret = cfg.google_client_secret.filter(|v| !v.trim().is_empty())?;
    Some((id.trim().to_string(), secret.trim().to_string()))
}

/// Every connected account. This is what the watchers iterate — one mailbox is
/// simply a list of length one, so nothing downstream has a single-account path.
pub fn accounts(app: &tauri::AppHandle) -> Vec<GoogleAccount> {
    if client_credentials(app).is_none() {
        return Vec::new();
    }
    read_config(app)
        .google_accounts
        .into_iter()
        .filter(|account| !account.refresh_token.trim().is_empty())
        .collect()
}

// There is deliberately no `is_connected` helper: every caller needs the account
// list anyway (to poll each one), so a boolean would only ever be `accounts()`
// with the useful part thrown away — and the shape that invites a
// single-account assumption back in.

/// A usable access token for one account, refreshing first if the cached one has
/// run out.
///
/// Every caller in the Gmail and Calendar clients goes through here rather than
/// holding a token of its own, so there is exactly one place that knows how long
/// one lives.
pub fn access_token(app: &tauri::AppHandle, account_id: &str) -> Result<String, TokenError> {
    if let Ok(guard) = access_token_cache().lock() {
        if let Some((token, expires_at)) = guard.get(account_id) {
            if Instant::now() + EXPIRY_MARGIN < *expires_at {
                return Ok(token.clone());
            }
        }
    }

    let Some((client_id, client_secret)) = client_credentials(app) else {
        return Err(TokenError::NeedsConsent(
            "No Google OAuth client is configured yet.".to_string(),
        ));
    };
    let Some(account) = read_config(app)
        .google_accounts
        .into_iter()
        .find(|a| a.id == account_id)
        .filter(|a| !a.refresh_token.trim().is_empty())
    else {
        return Err(TokenError::NeedsConsent(
            "That Google account is no longer connected.".to_string(),
        ));
    };

    let body = post_token_request(&[
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("refresh_token", account.refresh_token.trim()),
        ("grant_type", "refresh_token"),
    ])
    .inspect_err(|error| {
        // The whole point of separating the two kinds: a flat network failure
        // must not look like "sign in again", or a laptop that woke up on a
        // dead wifi would nag the user to re-authorize.
        if let TokenError::NeedsConsent(reason) = error {
            report_needs_consent(app, &account, reason);
        }
    })?;

    let token = body
        .access_token
        .ok_or_else(|| TokenError::Transient("Google returned no access token.".to_string()))?;
    let lifetime = Duration::from_secs(body.expires_in.unwrap_or(3600));

    if let Ok(mut guard) = access_token_cache().lock() {
        guard.insert(account_id.to_string(), (token.clone(), Instant::now() + lifetime));
    }
    Ok(token)
}

/// Tells the widget one account's grant is dead, so the feature stops failing
/// invisibly.
///
/// Silence is the worst option here: mail notices would simply stop arriving,
/// which looks exactly like a quiet inbox. Names the account, because with
/// several connected "sign in again" would not say which one. Notably this does
/// NOT delete the stored account — leaving it lets Settings still show what
/// needs reconnecting, and re-consenting overwrites it in place anyway.
pub fn report_needs_consent(app: &tauri::AppHandle, account: &GoogleAccount, reason: &str) {
    forget_access_token(&account.id);
    let _ = app.emit(
        "google-auth-needed",
        serde_json::json!({ "reason": reason, "account": account.email }),
    );
}

struct Callback {
    code: String,
    state: String,
}

/// Serves exactly one request on the loopback port and pulls the authorization
/// code out of it.
///
/// The query is parsed with a real URL parser rather than by splitting on '&'
/// because Google's authorization codes routinely contain '/' and are therefore
/// percent-encoded — hand-splitting produces a code that looks fine and is
/// rejected at the token endpoint.
fn wait_for_callback(server: &tiny_http::Server) -> Result<Callback, String> {
    let deadline = Instant::now() + CONSENT_TIMEOUT;
    while Instant::now() < deadline {
        let request = match server.recv_timeout(Duration::from_secs(1)) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(e) => return Err(format!("Sign-in listener failed: {e}")),
        };

        // Browsers ask for /favicon.ico off their own bat; answering it as if it
        // were the redirect would end the flow before the real one arrived.
        if request.url().starts_with("/favicon") {
            let _ = request.respond(tiny_http::Response::empty(404));
            continue;
        }

        let parsed = reqwest::Url::parse(&format!("http://127.0.0.1{}", request.url()))
            .map_err(|e| format!("Unreadable redirect from Google: {e}"))?;
        let mut code = None;
        let mut state = None;
        let mut error = None;
        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state = Some(value.into_owned()),
                "error" => error = Some(value.into_owned()),
                _ => {}
            }
        }

        let outcome = match (&code, &error) {
            (Some(_), _) => done_page("Signed in."),
            (None, Some(error)) => done_page(&format!("Sign-in was cancelled ({error}).")),
            _ => done_page("That didn't look like a Google redirect."),
        };
        let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .expect("valid header");
        let _ = request.respond(tiny_http::Response::from_string(outcome).with_header(header));

        if let Some(error) = error {
            return Err(format!("Google reported: {error}"));
        }
        return Ok(Callback {
            code: code.ok_or("Google's redirect carried no authorization code.")?,
            state: state.unwrap_or_default(),
        });
    }
    Err("Timed out waiting for the browser. Nothing was changed.".to_string())
}

#[derive(serde::Serialize)]
pub struct AccountInfo {
    pub id: String,
    pub email: String,
}

#[derive(serde::Serialize)]
pub struct ConnectionStatus {
    /// Whether the OAuth client itself is filled in — the precondition for the
    /// Add-account button doing anything.
    pub client_configured: bool,
    pub accounts: Vec<AccountInfo>,
}

/// Runs the whole interactive sign-in for ONE account: bind a port, open the
/// browser, wait for the redirect, trade the code for a refresh token, remember
/// whose it is.
///
/// Called once per account. Re-running it for an address that is already
/// connected refreshes that entry in place rather than adding a duplicate, so
/// reconnecting an expired account is the same action as adding it was.
#[tauri::command]
pub async fn google_add_account(app: tauri::AppHandle) -> Result<AccountInfo, String> {
    crate::offload(move || {
        let Some((client_id, client_secret)) = client_credentials(&app) else {
            return Err("Enter your Google client ID and secret first.".to_string());
        };

        // Port 0 asks the OS for any free port. A fixed one would collide with
        // whatever else is on the machine, and Google allows any port on a
        // loopback redirect precisely so this can be chosen at runtime.
        let server = tiny_http::Server::http("127.0.0.1:0")
            .map_err(|e| format!("Couldn't open a local port for sign-in: {e}"))?;
        let port = server
            .server_addr()
            .to_ip()
            .ok_or("Sign-in listener bound to a non-IP address.")?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}");

        let verifier = random_token();
        let expected_state = random_token();

        let mut auth_url = reqwest::Url::parse(AUTH_URL).expect("static URL parses");
        auth_url
            .query_pairs_mut()
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &SCOPES.join(" "))
            .append_pair("code_challenge", &s256_challenge(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &expected_state)
            // Without access_type=offline Google issues no refresh token at all,
            // and the connection would silently last exactly one hour.
            .append_pair("access_type", "offline")
            // And without prompt=consent it withholds the refresh token on every
            // sign-in after the first, so re-connecting an already-authorized
            // account would appear to succeed and leave nothing to refresh with.
            .append_pair("prompt", "consent");

        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_url(auth_url.to_string(), None::<String>)
            .map_err(|e| format!("Couldn't open your browser: {e}"))?;

        let callback = wait_for_callback(&server)?;
        if callback.state != expected_state {
            return Err("Sign-in response didn't match the request; nothing was changed.".to_string());
        }

        let body = post_token_request(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", callback.code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .map_err(|e| e.message().to_string())?;

        let refresh_token = body.refresh_token.ok_or(
            "Google didn't return a refresh token. Remove this widget's access in your \
             Google account's security settings, then connect again.",
        )?;
        let access_token = body
            .access_token
            .ok_or("Google didn't return an access token.")?;
        let lifetime = Duration::from_secs(body.expires_in.unwrap_or(3600));

        // Asked for rather than assumed: which account the human actually picked
        // on the consent screen is only knowable after the fact, and with several
        // connected it is the only thing telling them apart.
        let email = fetch_account_email(&access_token).unwrap_or_default();

        let mut cfg = read_config(&app);
        // Same address again means "reconnect", not "add a second copy" — match on
        // it so an expired account is repaired in place and keeps its id, and with
        // it every cache keyed by that id (seen mail, announced meetings).
        let existing = cfg
            .google_accounts
            .iter()
            .position(|a| !a.email.is_empty() && a.email.eq_ignore_ascii_case(&email));
        let account = match existing {
            Some(index) => {
                cfg.google_accounts[index].refresh_token = refresh_token;
                cfg.google_accounts[index].clone()
            }
            None => {
                let account = GoogleAccount {
                    id: uuid::Uuid::new_v4().to_string(),
                    email,
                    refresh_token,
                };
                cfg.google_accounts.push(account.clone());
                account
            }
        };
        write_config(&app, &cfg);

        if let Ok(mut guard) = access_token_cache().lock() {
            guard.insert(account.id.clone(), (access_token, Instant::now() + lifetime));
        }

        Ok(AccountInfo { id: account.id, email: account.email })
    })
    .await
}

fn fetch_account_email(access_token: &str) -> Option<String> {
    let response = reqwest::blocking::Client::new()
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .bearer_auth(access_token)
        .timeout(HTTP_TIMEOUT)
        .send()
        .ok()?;
    let body: serde_json::Value = response.json().ok()?;
    body.get("email")?.as_str().map(str::to_string)
}

#[tauri::command]
pub fn google_status(app: tauri::AppHandle) -> ConnectionStatus {
    ConnectionStatus {
        client_configured: client_credentials(&app).is_some(),
        accounts: read_config(&app)
            .google_accounts
            .into_iter()
            .map(|a| AccountInfo { id: a.id, email: a.email })
            .collect(),
    }
}

/// Forgets one account's stored grant locally.
///
/// This cannot revoke anything at Google's end — that is done from the account's
/// own security page — so the UI deliberately says only what it did.
#[tauri::command]
pub fn google_remove_account(app: tauri::AppHandle, account_id: String) {
    let mut cfg = read_config(&app);
    cfg.google_accounts.retain(|a| a.id != account_id);
    write_config(&app, &cfg);
    forget_access_token(&account_id);
}

/// Saves the OAuth client the user created in their own Google Cloud console.
///
/// Changing either half invalidates EVERY connected account, not just one: a
/// refresh token belongs to the client that obtained it. So they are all cleared
/// rather than left to fail confusingly on the next poll — the user reconnects
/// each once, which is the honest cost of swapping the client out.
/// Saves the OAuth client.
///
/// An empty `client_secret` means "keep the one already stored", not "the secret
/// is now empty". Settings cannot show the secret back (see get_google_client —
/// it is write-only on purpose), so its box is blank on every load, and taking
/// that literally made pressing Save a second time wipe the secret and, because
/// the client had then "changed", disconnect every account with it. Re-saving
/// with the box untouched now changes nothing.
#[tauri::command]
pub fn save_google_client(app: tauri::AppHandle, client_id: String, client_secret: String) {
    let mut cfg = read_config(&app);
    let secret = match client_secret.trim() {
        "" => cfg.google_client_secret.clone().unwrap_or_default(),
        typed => typed.to_string(),
    };
    let changed = cfg.google_client_id.as_deref() != Some(client_id.trim())
        || cfg.google_client_secret.as_deref() != Some(secret.as_str());
    cfg.google_client_id = Some(client_id.trim().to_string());
    cfg.google_client_secret = Some(secret);
    if changed {
        cfg.google_accounts.clear();
    }
    write_config(&app, &cfg);
    if changed {
        forget_all_access_tokens();
    }
}

/// The stored OAuth client, for Settings to show.
///
/// The secret comes back with it. It used to be withheld on the reasoning that
/// it never needs to be displayed — but that was inconsistent with every other
/// credential in this window (the LLM, speech and GitHub keys all round-trip into
/// their boxes), and the inconsistency did real damage: the box was blank on
/// every load, blank read as "it did not save", and re-entering it counted as a
/// client change, which disconnects every account. A secret you cannot see is
/// one you retype.
///
/// It is masked on screen like the others, and it is already in config.json in
/// plain text beside them, so nothing is exposed here that was not already.
#[derive(serde::Serialize)]
pub struct ClientInfo {
    pub client_id: String,
    pub client_secret: String,
}

#[tauri::command]
pub fn get_google_client(app: tauri::AppHandle) -> ClientInfo {
    let cfg = read_config(&app);
    ClientInfo {
        client_id: cfg.google_client_id.unwrap_or_default(),
        client_secret: cfg.google_client_secret.unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 7636's own worked example (Appendix B). Getting S256 subtly wrong —
    // standard base64 instead of URL-safe, or keeping the '=' padding — produces
    // a challenge that looks entirely plausible and is rejected only later, by
    // Google, as an opaque invalid_grant.
    #[test]
    fn s256_challenge_matches_the_rfc_vector() {
        assert_eq!(
            s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_verifier_is_long_enough_and_uses_only_unreserved_characters() {
        let verifier = random_token();
        assert!(
            (43..=128).contains(&verifier.len()),
            "PKCE allows 43..128 characters, got {}",
            verifier.len()
        );
        assert!(verifier.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(random_token(), random_token());
    }

    #[test]
    fn every_scope_is_read_only() {
        for scope in SCOPES {
            assert!(
                scope.ends_with(".readonly") || scope.ends_with("userinfo.email"),
                "{scope} grants more than reading"
            );
        }
    }
}
