//! OAuth 2.0 for an installed app: the system browser asks for consent, Google
//! redirects to a listener on the loopback address, and the code is exchanged
//! for tokens with PKCE. A `state` value ties the redirect to this attempt.
//!
//! Every network step has a timeout and the sign-in can be cancelled, so the
//! app never waits on Google without bound. A refresh that Google rejects
//! (`invalid_grant`) is stored as "needs attention" instead of being retried
//! silently: the user has to sign in again and should be told.

use super::config::{client_id_hint, GoogleClient, REQUESTED_SCOPES, SCOPE_CALENDAR_READONLY, SCOPE_DRIVE_FILE};
use crate::integrations::config_store;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const KEY_CLIENT_ID: &str = "google.client_id";
const KEY_CLIENT_SECRET: &str = "google.client_secret";
const KEY_REFRESH_TOKEN: &str = "google.refresh_token";
const KEY_ACCESS_TOKEN: &str = "google.access_token";
const KEY_ACCESS_EXPIRES_AT: &str = "google.access_expires_at";
const KEY_SCOPES: &str = "google.scopes";
const KEY_ACCOUNT_EMAIL: &str = "google.account_email";
const KEY_NEEDS_ATTENTION: &str = "google.needs_attention";

/// How long the browser step may take before the attempt is abandoned.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// A connection to the redirect listener that sends nothing is dropped after this.
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// An access token this close to expiry is refreshed before use.
const EXPIRY_MARGIN_SECONDS: i64 = 90;

/// Where Google is. Tests point these at a loopback server.
#[derive(Debug, Clone)]
pub struct GoogleEndpoints {
    pub authorization: String,
    pub token: String,
    pub revoke: String,
    pub drive_api: String,
    pub drive_upload: String,
    pub calendar_api: String,
}

impl Default for GoogleEndpoints {
    fn default() -> Self {
        Self {
            authorization: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            revoke: "https://oauth2.googleapis.com/revoke".into(),
            drive_api: "https://www.googleapis.com/drive/v3".into(),
            drive_upload: "https://www.googleapis.com/upload/drive/v3".into(),
            calendar_api: "https://www.googleapis.com/calendar/v3".into(),
        }
    }
}

/// The client used for every Google request: bounded connect and total time.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// What the webview may know about the connection. No token, no secret.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleStatus {
    /// An OAuth client was imported (steps 1 to 3 of the guide are done).
    pub client_configured: bool,
    pub client_id_hint: Option<String>,
    /// A refresh token is stored.
    pub connected: bool,
    pub account_email: Option<String>,
    /// The user can leave single permissions unticked on Google's consent page.
    pub drive_granted: bool,
    pub calendar_granted: bool,
    /// Set when Google rejected the stored sign-in; the user must sign in again.
    pub needs_attention: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessError {
    /// No client or no sign-in yet.
    NotConnected,
    /// Google rejected the stored sign-in. Carries the text shown to the user.
    NeedsAttention(String),
    /// Network or server trouble; trying later can work.
    Transient(String),
}

impl std::fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => formatter.write_str("Google is not connected."),
            Self::NeedsAttention(reason) | Self::Transient(reason) => formatter.write_str(reason),
        }
    }
}

pub async fn load_client(pool: &SqlitePool) -> Result<Option<GoogleClient>, sqlx::Error> {
    let Some(client_id) = config_store::get(pool, KEY_CLIENT_ID).await? else { return Ok(None) };
    let client_secret = config_store::get(pool, KEY_CLIENT_SECRET).await?;
    Ok(Some(GoogleClient { client_id, client_secret }))
}

/// Stores the client. A different client invalidates the stored sign-in,
/// because tokens belong to the client that obtained them.
pub async fn save_client(pool: &SqlitePool, client: &GoogleClient) -> Result<(), sqlx::Error> {
    let changed = load_client(pool).await?.is_some_and(|stored| stored.client_id != client.client_id);
    config_store::set_many(pool, &[
        (KEY_CLIENT_ID, Some(client.client_id.as_str())),
        (KEY_CLIENT_SECRET, client.client_secret.as_deref()),
    ])
    .await?;
    if changed {
        forget_sign_in(pool).await?;
    }
    Ok(())
}

pub async fn remove_client(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    forget_sign_in(pool).await?;
    config_store::set_many(pool, &[(KEY_CLIENT_ID, None), (KEY_CLIENT_SECRET, None)]).await
}

async fn forget_sign_in(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    config_store::set_many(pool, &[
        (KEY_REFRESH_TOKEN, None),
        (KEY_ACCESS_TOKEN, None),
        (KEY_ACCESS_EXPIRES_AT, None),
        (KEY_SCOPES, None),
        (KEY_ACCOUNT_EMAIL, None),
        (KEY_NEEDS_ATTENTION, None),
    ])
    .await
}

pub async fn status(pool: &SqlitePool) -> Result<GoogleStatus, sqlx::Error> {
    let client = load_client(pool).await?;
    let connected = config_store::get(pool, KEY_REFRESH_TOKEN).await?.is_some();
    let scopes = config_store::get(pool, KEY_SCOPES).await?.unwrap_or_default();
    let granted = |scope: &str| connected && scopes.split_whitespace().any(|granted| granted == scope);
    Ok(GoogleStatus {
        client_configured: client.is_some(),
        client_id_hint: client.as_ref().map(|client| client_id_hint(&client.client_id)),
        connected,
        account_email: config_store::get(pool, KEY_ACCOUNT_EMAIL).await?,
        drive_granted: granted(SCOPE_DRIVE_FILE),
        calendar_granted: granted(SCOPE_CALENDAR_READONLY),
        needs_attention: config_store::get(pool, KEY_NEEDS_ATTENTION).await?,
    })
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let value = u32::from(group[0]) << 16 | u32::from(group[1]) << 8 | u32::from(group[2]);
        // No padding: one, two or three input bytes give two, three or four characters.
        for position in 0..=chunk.len() {
            text.push(ALPHABET[(value >> (18 - 6 * position) & 63) as usize] as char);
        }
    }
    text
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64url(&bytes)
}

fn code_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// One sign-in attempt: the address to open, and the secrets that prove the
/// redirect and the code exchange belong to it.
pub struct Authorization {
    pub url: String,
    pub redirect_uri: String,
    state: String,
    code_verifier: String,
}

pub fn build_authorization(endpoints: &GoogleEndpoints, client: &GoogleClient, port: u16) -> Result<Authorization, String> {
    let state = random_token();
    let code_verifier = random_token();
    // No path: Google documents the loopback redirect as scheme, address and port.
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let mut url = url::Url::parse(&endpoints.authorization).map_err(|error| error.to_string())?;
    url.query_pairs_mut()
        .append_pair("client_id", &client.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &REQUESTED_SCOPES.join(" "))
        .append_pair("code_challenge", &code_challenge(&code_verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);
    Ok(Authorization { url: url.into(), redirect_uri, state, code_verifier })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInError {
    Cancelled,
    TimedOut,
    /// The user declined on Google's page, or Google reported an error there.
    Declined(String),
    Failed(String),
}

impl std::fmt::Display for SignInError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("Sign-in was cancelled."),
            Self::TimedOut => formatter.write_str("Sign-in was not finished within five minutes. Start it again when you are ready."),
            Self::Declined(reason) if reason == "access_denied" => formatter.write_str("Access was not granted on Google's page."),
            Self::Declined(reason) => write!(formatter, "Google did not complete the sign-in ({reason})."),
            Self::Failed(reason) => formatter.write_str(reason),
        }
    }
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem;color:#1f2328\">\
         <h1 style=\"font-size:1.25rem\">{title}</h1><p>{body}</p></body></html>"
    )
}

async fn respond(socket: &mut tokio::net::TcpStream, status: &str, html: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

/// Reads the request line of one connection and returns its query parameters.
async fn read_callback_query(socket: &mut tokio::net::TcpStream) -> Option<Vec<(String, String)>> {
    let mut received = Vec::new();
    let mut chunk = [0u8; 2048];
    // Only the first line is needed; stop there, at the end of the head, or at 16 KB.
    while !received.contains(&b'\n') && received.len() < 16 * 1024 {
        let read = socket.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        received.extend_from_slice(&chunk[..read]);
    }
    let head = String::from_utf8_lossy(&received);
    let mut parts = head.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    if url.path() != "/" {
        // Browsers also ask for /favicon.ico; that is not the redirect.
        return None;
    }
    Some(url.query_pairs().map(|(name, value)| (name.into_owned(), value.into_owned())).collect())
}

/// Waits for Google's redirect and returns the authorization code.
///
/// Requests without this attempt's `state` are answered and ignored: another
/// local program must not be able to finish or abort the sign-in.
pub async fn wait_for_code(
    listener: TcpListener,
    authorization: &Authorization,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> Result<String, SignInError> {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        let accepted = tokio::select! {
            _ = cancellation.cancelled() => return Err(SignInError::Cancelled),
            _ = &mut deadline => return Err(SignInError::TimedOut),
            accepted = listener.accept() => accepted,
        };
        let Ok((mut socket, _)) = accepted else { continue };
        let query = match tokio::time::timeout(CALLBACK_READ_TIMEOUT, read_callback_query(&mut socket)).await {
            Ok(Some(query)) => query,
            // A silent or unrelated connection must not hold up the real redirect.
            Ok(None) | Err(_) => {
                respond(&mut socket, "404 Not Found", &page("Not found", "This address only completes a Meetily sign-in.")).await;
                continue;
            }
        };
        let value = |name: &str| query.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
        if value("state") != Some(authorization.state.as_str()) {
            respond(&mut socket, "400 Bad Request", &page("This link is not valid", "It does not belong to the sign-in that is waiting in Meetily. Start the sign-in again from the app.")).await;
            continue;
        }
        if let Some(error) = value("error") {
            respond(&mut socket, "200 OK", &page("Sign-in was not completed", "No access was granted. You can close this window and return to Meetily.")).await;
            return Err(SignInError::Declined(error.to_string()));
        }
        match value("code") {
            Some(code) if !code.is_empty() => {
                respond(&mut socket, "200 OK", &page("Meetily is connected to Google", "You can close this window and return to Meetily.")).await;
                return Ok(code.to_string());
            }
            _ => {
                respond(&mut socket, "400 Bad Request", &page("This link is not valid", "Google's answer did not contain a code. Start the sign-in again from the app.")).await;
            }
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize, Default)]
struct TokenErrorResponse {
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

struct TokenFailure {
    /// `invalid_grant`, `invalid_client`, ... or empty when Google gave no JSON.
    code: String,
    message: String,
    transient: bool,
}

async fn token_request(http: &reqwest::Client, endpoints: &GoogleEndpoints, form: &[(&str, &str)]) -> Result<TokenResponse, TokenFailure> {
    let response = http.post(&endpoints.token).form(form).send().await.map_err(|error| TokenFailure {
        code: String::new(),
        message: format!("Google could not be reached: {}.", error.without_url()),
        transient: true,
    })?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.is_success() {
        return serde_json::from_str::<TokenResponse>(&body).map_err(|_| TokenFailure {
            code: String::new(),
            message: "Google's answer could not be read.".into(),
            transient: true,
        });
    }
    let error = serde_json::from_str::<TokenErrorResponse>(&body).unwrap_or_default();
    let detail = if error.error_description.is_empty() { error.error.clone() } else { error.error_description.clone() };
    Err(TokenFailure {
        message: if detail.is_empty() { format!("Google answered with HTTP {}.", status.as_u16()) } else { format!("Google said: {detail}") },
        transient: status.is_server_error() || status.as_u16() == 429,
        code: error.error,
    })
}

fn explain_exchange_failure(failure: &TokenFailure) -> String {
    match failure.code.as_str() {
        "invalid_client" | "unauthorized_client" => "Google does not accept this client. Check that the client ID and secret are those of a \"Desktop app\" client, then import them again.".to_string(),
        "invalid_grant" => "Google did not accept the sign-in code. Start the sign-in again.".to_string(),
        _ => failure.message.clone(),
    }
}

/// Runs the whole sign-in: listener, browser, redirect, code exchange, storage.
///
/// `open_browser` receives the address to open. It is a parameter so that tests
/// can play the browser's part.
pub async fn sign_in<F, Fut>(
    pool: &SqlitePool,
    http: &reqwest::Client,
    endpoints: &GoogleEndpoints,
    cancellation: &CancellationToken,
    open_browser: F,
) -> Result<GoogleStatus, SignInError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let failed = |error: sqlx::Error| SignInError::Failed(format!("Could not read the stored connection: {error}"));
    let client = load_client(pool).await.map_err(failed)?
        .ok_or_else(|| SignInError::Failed("Import your Google client first (step 3).".into()))?;

    // Port 0: the system picks a free port, so nothing has to be configured.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await
        .map_err(|error| SignInError::Failed(format!("Could not open a local address for the sign-in: {error}")))?;
    let port = listener.local_addr().map_err(|error| SignInError::Failed(error.to_string()))?.port();
    let authorization = build_authorization(endpoints, &client, port).map_err(SignInError::Failed)?;

    open_browser(authorization.url.clone()).await
        .map_err(|error| SignInError::Failed(format!("The browser could not be opened: {error}")))?;
    let code = wait_for_code(listener, &authorization, SIGN_IN_TIMEOUT, cancellation).await?;

    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("client_id", client.client_id.as_str()),
        ("redirect_uri", authorization.redirect_uri.as_str()),
        ("code_verifier", authorization.code_verifier.as_str()),
    ];
    if let Some(secret) = client.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    let tokens = tokio::select! {
        _ = cancellation.cancelled() => return Err(SignInError::Cancelled),
        tokens = token_request(http, endpoints, &form) => tokens.map_err(|failure| SignInError::Failed(explain_exchange_failure(&failure)))?,
    };
    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
        return Err(SignInError::Failed("Google did not return a long-lived sign-in. Start the sign-in again.".into()));
    };
    let scopes = tokens.scope.clone().unwrap_or_default();
    let expires_at = (chrono::Utc::now().timestamp() + tokens.expires_in.unwrap_or(3600)).to_string();
    config_store::set_many(pool, &[
        (KEY_REFRESH_TOKEN, Some(refresh_token)),
        (KEY_ACCESS_TOKEN, Some(tokens.access_token.as_str())),
        (KEY_ACCESS_EXPIRES_AT, Some(expires_at.as_str())),
        (KEY_SCOPES, Some(scopes.as_str())),
        (KEY_ACCOUNT_EMAIL, None),
        (KEY_NEEDS_ATTENTION, None),
    ])
    .await
    .map_err(failed)?;

    // Which account this is. The address is for display only, so a failure
    // here does not undo the sign-in.
    if let Some(email) = account_email(http, endpoints, &tokens.access_token, &scopes).await {
        let _ = config_store::set(pool, KEY_ACCOUNT_EMAIL, &email).await;
    }
    status(pool).await.map_err(failed)
}

/// The signed-in account's address, from whichever granted API can tell.
async fn account_email(http: &reqwest::Client, endpoints: &GoogleEndpoints, access_token: &str, scopes: &str) -> Option<String> {
    let granted = |scope: &str| scopes.split_whitespace().any(|granted| granted == scope);
    if granted(SCOPE_DRIVE_FILE) {
        let url = format!("{}/about?fields=user(emailAddress)", endpoints.drive_api);
        if let Ok(response) = http.get(url).bearer_auth(access_token).send().await {
            if let Ok(body) = response.json::<serde_json::Value>().await {
                if let Some(email) = body["user"]["emailAddress"].as_str() {
                    return Some(email.to_string());
                }
            }
        }
    }
    if granted(SCOPE_CALENDAR_READONLY) {
        // The primary calendar's ID is the account's address.
        let url = format!("{}/users/me/calendarList/primary?fields=id", endpoints.calendar_api);
        if let Ok(response) = http.get(url).bearer_auth(access_token).send().await {
            if let Ok(body) = response.json::<serde_json::Value>().await {
                return body["id"].as_str().filter(|id| id.contains('@')).map(str::to_string);
            }
        }
    }
    None
}

/// One refresh at a time: two callers must not both spend the refresh token
/// and race to store the result.
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A valid access token, refreshed when it is about to expire.
pub async fn access_token(pool: &SqlitePool, http: &reqwest::Client, endpoints: &GoogleEndpoints) -> Result<String, AccessError> {
    let stored = |error: sqlx::Error| AccessError::Transient(format!("Could not read the stored connection: {error}"));
    let _guard = REFRESH_LOCK.lock().await;
    let Some(client) = load_client(pool).await.map_err(stored)? else { return Err(AccessError::NotConnected) };
    let Some(refresh_token) = config_store::get(pool, KEY_REFRESH_TOKEN).await.map_err(stored)? else {
        return Err(AccessError::NotConnected);
    };
    if let Some(reason) = config_store::get(pool, KEY_NEEDS_ATTENTION).await.map_err(stored)? {
        return Err(AccessError::NeedsAttention(reason));
    }
    let expires_at = config_store::get(pool, KEY_ACCESS_EXPIRES_AT).await.map_err(stored)?
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    if expires_at - chrono::Utc::now().timestamp() > EXPIRY_MARGIN_SECONDS {
        if let Some(token) = config_store::get(pool, KEY_ACCESS_TOKEN).await.map_err(stored)? {
            return Ok(token);
        }
    }

    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", client.client_id.as_str()),
    ];
    if let Some(secret) = client.client_secret.as_deref() {
        form.push(("client_secret", secret));
    }
    match token_request(http, endpoints, &form).await {
        Ok(tokens) => {
            let expires_at = (chrono::Utc::now().timestamp() + tokens.expires_in.unwrap_or(3600)).to_string();
            let mut entries = vec![
                (KEY_ACCESS_TOKEN, Some(tokens.access_token.as_str())),
                (KEY_ACCESS_EXPIRES_AT, Some(expires_at.as_str())),
            ];
            // Google may rotate the refresh token; keep the newest.
            if let Some(rotated) = tokens.refresh_token.as_deref() {
                entries.push((KEY_REFRESH_TOKEN, Some(rotated)));
            }
            config_store::set_many(pool, &entries).await.map_err(stored)?;
            Ok(tokens.access_token)
        }
        Err(failure) if failure.transient => Err(AccessError::Transient(failure.message)),
        Err(failure) => {
            // `invalid_grant`: the sign-in expired or was revoked. `invalid_client`:
            // the client was deleted or changed. Neither heals by waiting.
            let reason = match failure.code.as_str() {
                "invalid_grant" => "Google ended this sign-in (it expired or access was removed). Sign in again.".to_string(),
                "invalid_client" | "unauthorized_client" | "deleted_client" => "Google no longer accepts the imported client. Check it in Google Cloud and import it again.".to_string(),
                _ => format!("Google rejected the stored sign-in. {} Sign in again.", failure.message),
            };
            mark_needs_attention(pool, &reason).await;
            Err(AccessError::NeedsAttention(reason))
        }
    }
}

/// Records that the user has to act. Used by the refresh above and by API
/// clients that receive 401 with a token that was just refreshed.
pub async fn mark_needs_attention(pool: &SqlitePool, reason: &str) {
    if let Err(error) = config_store::set(pool, KEY_NEEDS_ATTENTION, reason).await {
        log::warn!("Could not store the Google connection state: {error}");
    }
}

/// Forgets the sign-in locally and asks Google to revoke it. The local part
/// always happens; revocation is best effort (the user may be offline).
pub async fn disconnect(pool: &SqlitePool, http: &reqwest::Client, endpoints: &GoogleEndpoints) -> Result<(), sqlx::Error> {
    let refresh_token = config_store::get(pool, KEY_REFRESH_TOKEN).await?;
    forget_sign_in(pool).await?;
    if let Some(token) = refresh_token {
        let revoked = http
            .post(&endpoints.revoke)
            .form(&[("token", token.as_str())])
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        if let Err(error) = revoked {
            log::info!("Google sign-in removed locally; revocation at Google did not complete: {}", error.without_url());
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::integrations::config_store::test_pool;
    use std::sync::{Arc, Mutex};

    /// A loopback stand-in for Google's token, revoke and API endpoints.
    pub(crate) struct FakeGoogle {
        pub endpoints: GoogleEndpoints,
        pub requests: Arc<Mutex<Vec<(String, String, String)>>>,
    }

    pub(crate) type Handler = dyn Fn(&str, &str, &str) -> (u16, String) + Send + Sync;

    impl FakeGoogle {
        /// `handler(method, target, body)` answers every request with (status, JSON).
        pub(crate) async fn start(handler: Arc<Handler>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else { return };
                    let handler = handler.clone();
                    let seen = seen.clone();
                    tokio::spawn(async move {
                        let mut received = Vec::new();
                        let mut chunk = [0u8; 8192];
                        let (method, target, body) = loop {
                            let read = socket.read(&mut chunk).await.unwrap_or(0);
                            if read == 0 { return; }
                            received.extend_from_slice(&chunk[..read]);
                            let Some(split) = received.windows(4).position(|window| window == b"\r\n\r\n") else { continue };
                            let head = String::from_utf8_lossy(&received[..split]).to_string();
                            let length = head.lines().find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok())?
                            }).unwrap_or(0);
                            if received.len() < split + 4 + length { continue; }
                            let mut first = head.lines().next().unwrap_or("").split_whitespace();
                            let method = first.next().unwrap_or("").to_string();
                            let target = first.next().unwrap_or("").to_string();
                            break (method, target, String::from_utf8_lossy(&received[split + 4..split + 4 + length]).to_string());
                        };
                        seen.lock().unwrap().push((method.clone(), target.clone(), body.clone()));
                        let (status, json) = handler(&method, &target, &body);
                        let response = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}", json.len());
                        let _ = socket.write_all(response.as_bytes()).await;
                        let _ = socket.shutdown().await;
                    });
                }
            });
            Self {
                endpoints: GoogleEndpoints {
                    authorization: format!("{base}/auth"),
                    token: format!("{base}/token"),
                    revoke: format!("{base}/revoke"),
                    drive_api: format!("{base}/drive/v3"),
                    drive_upload: format!("{base}/upload/drive/v3"),
                    calendar_api: format!("{base}/calendar/v3"),
                },
                requests,
            }
        }

        pub(crate) fn requests(&self) -> Vec<(String, String, String)> {
            self.requests.lock().unwrap().clone()
        }
    }

    pub(crate) fn test_http() -> reqwest::Client {
        // The container routes outbound HTTP through a proxy; loopback must bypass it.
        reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().unwrap()
    }

    pub(crate) fn test_client() -> GoogleClient {
        GoogleClient { client_id: "123-abc.apps.googleusercontent.com".into(), client_secret: Some("GOCSPX-secret".into()) }
    }

    fn form(body: &str) -> std::collections::HashMap<String, String> {
        url::form_urlencoded::parse(body.as_bytes()).into_owned().collect()
    }

    fn token_json(access: &str, refresh: Option<&str>, scope: &str) -> String {
        let mut value = serde_json::json!({ "access_token": access, "expires_in": 3599, "scope": scope, "token_type": "Bearer" });
        if let Some(refresh) = refresh { value["refresh_token"] = refresh.into(); }
        value.to_string()
    }

    const BOTH_SCOPES: &str = "https://www.googleapis.com/auth/calendar.readonly https://www.googleapis.com/auth/drive.file";

    /// Plays the browser: follows the authorization address and "redirects" to
    /// the app's listener the way Google would.
    async fn browser(url: String, answer: impl Fn(&str) -> String) -> Result<(), String> {
        let parsed = url::Url::parse(&url).unwrap();
        let query: std::collections::HashMap<String, String> = parsed.query_pairs().into_owned().collect();
        let callback = format!("{}/?{}", query["redirect_uri"], answer(&query["state"]));
        tokio::spawn(async move {
            let _ = test_http().get(callback).send().await;
        });
        Ok(())
    }

    #[test]
    fn pkce_values_follow_the_specification() {
        // RFC 4648 test vectors, URL-safe alphabet, no padding.
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(&[0xfb, 0xff, 0xfe]), "-__-");
        // RFC 7636, appendix B.
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let verifier = random_token();
        assert!((43..=128).contains(&verifier.len()));
        assert!(verifier.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(random_token(), random_token());
    }

    #[test]
    fn the_authorization_address_asks_only_for_the_two_permissions() {
        let authorization = build_authorization(&GoogleEndpoints::default(), &test_client(), 51234).unwrap();
        let url = url::Url::parse(&authorization.url).unwrap();
        let query: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(url.origin().ascii_serialization(), "https://accounts.google.com");
        assert_eq!(query["client_id"], "123-abc.apps.googleusercontent.com");
        assert_eq!(query["redirect_uri"], "http://127.0.0.1:51234");
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["scope"], "https://www.googleapis.com/auth/drive.file https://www.googleapis.com/auth/calendar.readonly");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["code_challenge"], code_challenge(&authorization.code_verifier));
        assert_eq!(query["state"], authorization.state);
        // The secret is never part of the address the browser opens.
        assert!(!authorization.url.contains("GOCSPX"));
    }

    #[tokio::test]
    async fn sign_in_exchanges_the_code_with_pkce_and_stores_the_connection() {
        let google = FakeGoogle::start(Arc::new(|_, target, _| {
            if target.starts_with("/token") { (200, token_json("access-1", Some("refresh-1"), BOTH_SCOPES)) }
            else if target.starts_with("/drive/v3/about") { (200, r#"{"user":{"emailAddress":"person@example.com"}}"#.into()) }
            else { (404, "{}".into()) }
        })).await;
        let (_directory, manager) = test_pool().await;
        let pool = manager.pool();
        save_client(pool, &test_client()).await.unwrap();
        assert!(!status(pool).await.unwrap().connected);

        let opened = Arc::new(Mutex::new(String::new()));
        let seen = opened.clone();
        let result = sign_in(pool, &test_http(), &google.endpoints, &CancellationToken::new(), |url| {
            *seen.lock().unwrap() = url.clone();
            browser(url, |state| format!("state={state}&code=4%2Fthe-code&scope=ignored"))
        }).await.unwrap();

        assert_eq!(result, GoogleStatus {
            client_configured: true,
            client_id_hint: Some("123-abc….apps.googleusercontent.com".into()),
            connected: true,
            account_email: Some("person@example.com".into()),
            drive_granted: true,
            calendar_granted: true,
            needs_attention: None,
        });
        let exchange = google.requests().into_iter().find(|(_, target, _)| target == "/token").unwrap();
        let sent = form(&exchange.2);
        let opened_query: std::collections::HashMap<String, String> = url::Url::parse(&opened.lock().unwrap()).unwrap().query_pairs().into_owned().collect();
        assert_eq!(sent["grant_type"], "authorization_code");
        assert_eq!(sent["code"], "4/the-code");
        assert_eq!(sent["client_id"], "123-abc.apps.googleusercontent.com");
        assert_eq!(sent["client_secret"], "GOCSPX-secret");
        assert_eq!(sent["redirect_uri"], opened_query["redirect_uri"]);
        // The verifier sent to the token endpoint matches the challenge shown to the browser.
        assert_eq!(code_challenge(&sent["code_verifier"]), opened_query["code_challenge"]);
        // The stored token is used as it is while it is fresh: no refresh request.
        assert_eq!(access_token(pool, &test_http(), &google.endpoints).await.unwrap(), "access-1");
        assert_eq!(google.requests().iter().filter(|(_, target, _)| target == "/token").count(), 1);
    }

    #[tokio::test]
    async fn a_permission_left_unticked_is_reported() {
        let google = FakeGoogle::start(Arc::new(|_, target, _| {
            if target.starts_with("/token") { (200, token_json("a", Some("r"), "https://www.googleapis.com/auth/calendar.readonly")) }
            else if target.starts_with("/calendar/v3/users/me/calendarList/primary") { (200, r#"{"id":"person@example.com"}"#.into()) }
            else { (404, "{}".into()) }
        })).await;
        let (_directory, manager) = test_pool().await;
        save_client(manager.pool(), &test_client()).await.unwrap();
        let result = sign_in(manager.pool(), &test_http(), &google.endpoints, &CancellationToken::new(), |url| browser(url, |state| format!("state={state}&code=c"))).await.unwrap();
        assert!(result.connected && result.calendar_granted && !result.drive_granted);
        assert_eq!(result.account_email.as_deref(), Some("person@example.com"));
    }

    #[tokio::test]
    async fn a_redirect_without_this_attempts_state_neither_finishes_nor_aborts_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let authorization = build_authorization(&GoogleEndpoints::default(), &test_client(), port).unwrap();
        let state = authorization.state.clone();
        tokio::spawn(async move {
            let http = test_http();
            let base = format!("http://127.0.0.1:{port}");
            // Another local program guesses, a browser asks for its icon, a socket stays silent.
            assert_eq!(http.get(format!("{base}/?state=guess&code=stolen")).send().await.unwrap().status(), 400);
            assert_eq!(http.get(format!("{base}/?state=guess&error=access_denied")).send().await.unwrap().status(), 400);
            assert_eq!(http.get(format!("{base}/favicon.ico")).send().await.unwrap().status(), 404);
            let _silent = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            assert_eq!(http.get(format!("{base}/?state={state}")).send().await.unwrap().status(), 400);
            let done = http.get(format!("{base}/?state={state}&code=real")).send().await.unwrap();
            assert!(done.text().await.unwrap().contains("connected to Google"));
        });
        let code = wait_for_code(listener, &authorization, Duration::from_secs(10), &CancellationToken::new()).await.unwrap();
        assert_eq!(code, "real");
    }

    #[tokio::test]
    async fn declining_cancelling_and_waiting_too_long_end_the_attempt_promptly() {
        let (_directory, manager) = test_pool().await;
        let pool = manager.pool();
        save_client(pool, &test_client()).await.unwrap();
        let endpoints = GoogleEndpoints::default();

        let declined = sign_in(pool, &test_http(), &endpoints, &CancellationToken::new(), |url| browser(url, |state| format!("state={state}&error=access_denied"))).await.unwrap_err();
        assert_eq!(declined, SignInError::Declined("access_denied".into()));
        assert_eq!(declined.to_string(), "Access was not granted on Google's page.");

        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(50)).await; cancel.cancel(); });
        let started = std::time::Instant::now();
        let cancelled = sign_in(pool, &test_http(), &endpoints, &token, |_| async { Ok(()) }).await.unwrap_err();
        assert_eq!(cancelled, SignInError::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(3));

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authorization = build_authorization(&endpoints, &test_client(), 1).unwrap();
        assert_eq!(wait_for_code(listener, &authorization, Duration::from_millis(40), &CancellationToken::new()).await.unwrap_err(), SignInError::TimedOut);
        // Nothing was stored by any of the failed attempts.
        assert!(!status(pool).await.unwrap().connected);
    }

    #[tokio::test]
    async fn a_rejected_client_or_code_is_explained_and_nothing_is_stored() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (401, r#"{"error":"invalid_client","error_description":"The OAuth client was not found."}"#.into()))).await;
        let (_directory, manager) = test_pool().await;
        save_client(manager.pool(), &test_client()).await.unwrap();
        let error = sign_in(manager.pool(), &test_http(), &google.endpoints, &CancellationToken::new(), |url| browser(url, |state| format!("state={state}&code=c"))).await.unwrap_err();
        assert!(error.to_string().contains("Desktop app"));
        assert!(!status(manager.pool()).await.unwrap().connected);

        // Sign-in without an imported client says which step is missing.
        let (_other, empty) = test_pool().await;
        let missing = sign_in(empty.pool(), &test_http(), &google.endpoints, &CancellationToken::new(), |_| async { Ok(()) }).await.unwrap_err();
        assert!(missing.to_string().contains("step 3"));
    }

    async fn connected_pool(google: &FakeGoogle) -> (tempfile::TempDir, crate::database::manager::DatabaseManager) {
        let (directory, manager) = test_pool().await;
        save_client(manager.pool(), &test_client()).await.unwrap();
        config_store::set_many(manager.pool(), &[
            (KEY_REFRESH_TOKEN, Some("refresh-1")),
            (KEY_ACCESS_TOKEN, Some("stale")),
            (KEY_ACCESS_EXPIRES_AT, Some("0")),
            (KEY_SCOPES, Some(BOTH_SCOPES)),
        ]).await.unwrap();
        let _ = google;
        (directory, manager)
    }

    #[tokio::test]
    async fn an_expired_access_token_is_refreshed_once_and_reused() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, token_json("fresh", None, BOTH_SCOPES)))).await;
        let (_directory, manager) = connected_pool(&google).await;
        let pool = manager.pool();
        let http = test_http();
        // Two callers at once share one refresh.
        let (first, second) = tokio::join!(access_token(pool, &http, &google.endpoints), access_token(pool, &http, &google.endpoints));
        assert_eq!(first.unwrap(), "fresh");
        assert_eq!(second.unwrap(), "fresh");
        let requests = google.requests();
        assert_eq!(requests.len(), 1);
        let sent = form(&requests[0].2);
        assert_eq!(sent["grant_type"], "refresh_token");
        assert_eq!(sent["refresh_token"], "refresh-1");
        assert_eq!(sent["client_secret"], "GOCSPX-secret");
        // No refresh token in the answer: the stored one is kept.
        assert_eq!(config_store::get(pool, KEY_REFRESH_TOKEN).await.unwrap().as_deref(), Some("refresh-1"));
    }

    #[tokio::test]
    async fn a_revoked_sign_in_becomes_needs_attention_and_stops_asking_google() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (400, r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#.into()))).await;
        let (_directory, manager) = connected_pool(&google).await;
        let pool = manager.pool();
        let first = access_token(pool, &test_http(), &google.endpoints).await.unwrap_err();
        assert!(matches!(&first, AccessError::NeedsAttention(reason) if reason.contains("Sign in again")));
        let state = status(pool).await.unwrap();
        assert!(state.connected);
        assert!(state.needs_attention.unwrap().contains("Sign in again"));
        // Later calls report the stored state without another request.
        assert!(matches!(access_token(pool, &test_http(), &google.endpoints).await, Err(AccessError::NeedsAttention(_))));
        assert_eq!(google.requests().len(), 1);
    }

    #[tokio::test]
    async fn an_outage_during_refresh_is_transient_and_keeps_the_connection() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (503, "{}".into()))).await;
        let (_directory, manager) = connected_pool(&google).await;
        assert!(matches!(access_token(manager.pool(), &test_http(), &google.endpoints).await, Err(AccessError::Transient(_))));
        assert_eq!(status(manager.pool()).await.unwrap().needs_attention, None);

        // Unreachable: bind, then drop, so nothing listens.
        let port = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        let offline = GoogleEndpoints { token: format!("http://127.0.0.1:{port}/token"), ..google.endpoints.clone() };
        assert!(matches!(access_token(manager.pool(), &test_http(), &offline).await, Err(AccessError::Transient(_))));
        assert_eq!(status(manager.pool()).await.unwrap().needs_attention, None);
    }

    #[tokio::test]
    async fn disconnect_forgets_the_sign_in_keeps_the_client_and_revokes_at_google() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, "{}".into()))).await;
        let (_directory, manager) = connected_pool(&google).await;
        let pool = manager.pool();
        disconnect(pool, &test_http(), &google.endpoints).await.unwrap();
        let state = status(pool).await.unwrap();
        assert!(state.client_configured && !state.connected && !state.drive_granted);
        let revoke = google.requests().into_iter().find(|(_, target, _)| target == "/revoke").unwrap();
        assert_eq!(form(&revoke.2)["token"], "refresh-1");
        assert_eq!(access_token(pool, &test_http(), &google.endpoints).await, Err(AccessError::NotConnected));

        // Offline: the local sign-in is still removed.
        let (_second, other) = connected_pool(&google).await;
        let port = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port();
        let offline = GoogleEndpoints { revoke: format!("http://127.0.0.1:{port}/revoke"), ..google.endpoints.clone() };
        disconnect(other.pool(), &test_http(), &offline).await.unwrap();
        assert!(!status(other.pool()).await.unwrap().connected);
    }

    #[tokio::test]
    async fn importing_a_different_client_ends_the_sign_in_made_with_the_old_one() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, "{}".into()))).await;
        let (_directory, manager) = connected_pool(&google).await;
        let pool = manager.pool();
        // The same client again (for example a re-import) keeps the sign-in.
        save_client(pool, &test_client()).await.unwrap();
        assert!(status(pool).await.unwrap().connected);
        save_client(pool, &GoogleClient { client_id: "999-new.apps.googleusercontent.com".into(), client_secret: None }).await.unwrap();
        let state = status(pool).await.unwrap();
        assert!(state.client_configured && !state.connected);
        assert_eq!(config_store::get(pool, KEY_CLIENT_SECRET).await.unwrap(), None);
        remove_client(pool).await.unwrap();
        assert_eq!(status(pool).await.unwrap(), GoogleStatus::default());
    }
}
