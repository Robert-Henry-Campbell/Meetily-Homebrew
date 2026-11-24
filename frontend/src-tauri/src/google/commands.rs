//! Tauri commands for the Connections screen. They return `GoogleStatus` and
//! plain messages; no token and no client secret crosses into the webview.

use super::config::{client_from_values, parse_client_json};
use super::oauth::{self, GoogleEndpoints, GoogleStatus};
use crate::state::AppState;
use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Runtime};
use tokio_util::sync::CancellationToken;

/// Emitted with a `GoogleStatus` whenever the connection changes.
pub const GOOGLE_CONNECTION_CHANGED: &str = "google-connection-changed";

/// The sign-in that is waiting for the browser, if any, with its attempt
/// number. A second sign-in cancels the first, so only one redirect listener
/// exists.
static ACTIVE_SIGN_IN: Mutex<Option<(u64, CancellationToken)>> = Mutex::new(None);
static SIGN_IN_ATTEMPTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn current_status(state: &tauri::State<'_, AppState>) -> Result<GoogleStatus, String> {
    oauth::status(state.db_manager.pool())
        .await
        .map_err(|error| format!("Could not read the Google connection: {error}"))
}

fn announce<R: Runtime>(app: &AppHandle<R>, status: &GoogleStatus) {
    let _ = app.emit(GOOGLE_CONNECTION_CHANGED, status);
}

#[tauri::command]
pub async fn google_connection_status(state: tauri::State<'_, AppState>) -> Result<GoogleStatus, String> {
    current_status(&state).await
}

/// Step 3 of the guide. Either the downloaded JSON file's text, or the two
/// pasted values. The secret is stored and never returned.
#[tauri::command]
pub async fn google_save_client<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    json: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
) -> Result<GoogleStatus, String> {
    let client = match json.as_deref().map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => parse_client_json(text)?,
        None => client_from_values(client_id.as_deref().unwrap_or(""), client_secret.as_deref())?,
    };
    oauth::save_client(state.db_manager.pool(), &client)
        .await
        .map_err(|error| format!("Could not store the Google client: {error}"))?;
    let status = current_status(&state).await?;
    announce(&app, &status);
    Ok(status)
}

#[tauri::command]
pub async fn google_remove_client<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<GoogleStatus, String> {
    let pool = state.db_manager.pool();
    // Revoke first, while the token is still known.
    oauth::disconnect(pool, &oauth::http_client(), &GoogleEndpoints::default())
        .await
        .map_err(|error| format!("Could not remove the Google sign-in: {error}"))?;
    oauth::remove_client(pool)
        .await
        .map_err(|error| format!("Could not remove the Google client: {error}"))?;
    let status = current_status(&state).await?;
    announce(&app, &status);
    Ok(status)
}

/// Step 4. Opens the system browser and waits for Google's redirect. Returns
/// when the sign-in finished, failed, was cancelled, or timed out.
#[tauri::command]
pub async fn google_sign_in<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<GoogleStatus, String> {
    let cancellation = CancellationToken::new();
    let attempt = SIGN_IN_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    if let Some((_, previous)) = ACTIVE_SIGN_IN.lock().unwrap().replace((attempt, cancellation.clone())) {
        previous.cancel();
    }
    let result = oauth::sign_in(
        state.db_manager.pool(),
        &oauth::http_client(),
        &GoogleEndpoints::default(),
        &cancellation,
        |url| async move { crate::api::open_external_url(url).await },
    )
    .await;
    {
        // Clear the slot only if it still belongs to this attempt; a newer
        // attempt that replaced it keeps its own entry.
        let mut active = ACTIVE_SIGN_IN.lock().unwrap();
        if active.as_ref().is_some_and(|(number, _)| *number == attempt) {
            *active = None;
        }
    }
    match result {
        Ok(status) => {
            announce(&app, &status);
            // Meetings that waited for the connection are sent now.
            crate::export::worker::wake();
            Ok(status)
        }
        Err(error) => Err(error.to_string()),
    }
}

#[tauri::command]
pub async fn google_cancel_sign_in() -> Result<(), String> {
    if let Some((_, active)) = ACTIVE_SIGN_IN.lock().unwrap().take() {
        active.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn google_disconnect<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<GoogleStatus, String> {
    oauth::disconnect(state.db_manager.pool(), &oauth::http_client(), &GoogleEndpoints::default())
        .await
        .map_err(|error| format!("Could not remove the Google sign-in: {error}"))?;
    let status = current_status(&state).await?;
    announce(&app, &status);
    Ok(status)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleCheck {
    pub drive: CheckResult,
    pub calendar: CheckResult,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub ok: bool,
    pub message: String,
}

/// A read-only call against each API that was granted, so the user sees that
/// the connection works before the first meeting depends on it.
#[tauri::command]
pub async fn google_test_connection<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<GoogleCheck, String> {
    let check = run_checks(state.db_manager.pool(), &oauth::http_client(), &GoogleEndpoints::default()).await;
    // A check can discover that Google ended the sign-in.
    if let Ok(status) = current_status(&state).await {
        announce(&app, &status);
    }
    Ok(check)
}

pub(crate) async fn run_checks(
    pool: &sqlx::SqlitePool,
    http: &reqwest::Client,
    endpoints: &GoogleEndpoints,
) -> GoogleCheck {
    let failed = |message: String| CheckResult { ok: false, message };
    let status = match oauth::status(pool).await {
        Ok(status) => status,
        Err(error) => {
            let message = format!("Could not read the Google connection: {error}");
            return GoogleCheck { drive: failed(message.clone()), calendar: failed(message) };
        }
    };
    let token = match oauth::access_token(pool, http, endpoints).await {
        Ok(token) => token,
        Err(error) => {
            return GoogleCheck { drive: failed(error.to_string()), calendar: failed(error.to_string()) };
        }
    };
    let call = |url: String, granted: bool, missing: &'static str, works: &'static str| {
        let token = token.clone();
        async move {
            if !granted {
                return failed(missing.to_string());
            }
            match http.get(url).bearer_auth(token).send().await {
                Ok(response) if response.status().is_success() => CheckResult { ok: true, message: works.to_string() },
                Ok(response) => {
                    let status = response.status().as_u16();
                    let body = response.json::<serde_json::Value>().await.unwrap_or_default();
                    let said = body["error"]["message"].as_str().unwrap_or("");
                    failed(match status {
                        // "has not been used in project ... or it is disabled": step 1 was skipped for this API.
                        403 if said.contains("has not been used") || said.contains("is disabled") =>
                            "This API is not enabled in your Google Cloud project (step 1). Enable it, wait a minute, and test again.".to_string(),
                        401 => "Google did not accept the sign-in. Sign in again.".to_string(),
                        _ if said.is_empty() => format!("Google answered with HTTP {status}."),
                        _ => format!("Google said: {said}"),
                    })
                }
                Err(error) => failed(format!("Google could not be reached: {}.", error.without_url())),
            }
        }
    };
    let (drive, calendar) = tokio::join!(
        call(
            format!("{}/about?fields=user(emailAddress)", endpoints.drive_api),
            status.drive_granted,
            "The Drive permission was not ticked on Google's page. Sign in again and tick it.",
            "Drive works.",
        ),
        call(
            format!("{}/users/me/calendarList?maxResults=1&fields=items(id)", endpoints.calendar_api),
            status.calendar_granted,
            "The Calendar permission was not ticked on Google's page. Sign in again and tick it.",
            "Calendar works.",
        ),
    );
    GoogleCheck { drive, calendar }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::oauth::tests::{test_client, test_http, FakeGoogle};
    use crate::integrations::config_store::{self, test_pool};
    use std::sync::Arc;

    async fn signed_in(scopes: &str) -> (tempfile::TempDir, crate::database::manager::DatabaseManager) {
        let (directory, manager) = test_pool().await;
        oauth::save_client(manager.pool(), &test_client()).await.unwrap();
        let far_future = (chrono::Utc::now().timestamp() + 3000).to_string();
        config_store::set_many(manager.pool(), &[
            ("google.refresh_token", Some("refresh")),
            ("google.access_token", Some("access")),
            ("google.access_expires_at", Some(far_future.as_str())),
            ("google.scopes", Some(scopes)),
        ]).await.unwrap();
        (directory, manager)
    }

    const BOTH: &str = "https://www.googleapis.com/auth/drive.file https://www.googleapis.com/auth/calendar.readonly";

    #[tokio::test]
    async fn the_test_reads_from_both_apis_and_changes_nothing() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, "{}".into()))).await;
        let (_directory, manager) = signed_in(BOTH).await;
        let check = run_checks(manager.pool(), &test_http(), &google.endpoints).await;
        assert!(check.drive.ok && check.calendar.ok);
        let requests = google.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|(method, _, body)| method == "GET" && body.is_empty()));
    }

    #[tokio::test]
    async fn a_disabled_api_and_an_unticked_permission_each_name_the_step_to_repeat() {
        let google = FakeGoogle::start(Arc::new(|_, target, _| {
            if target.starts_with("/drive") {
                (403, r#"{"error":{"code":403,"message":"Google Drive API has not been used in project 1 before or it is disabled."}}"#.into())
            } else {
                (200, "{}".into())
            }
        })).await;
        let (_directory, manager) = signed_in(BOTH).await;
        let check = run_checks(manager.pool(), &test_http(), &google.endpoints).await;
        assert!(!check.drive.ok && check.drive.message.contains("step 1"));
        assert!(check.calendar.ok);

        let (_other, drive_only) = signed_in("https://www.googleapis.com/auth/drive.file").await;
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, "{}".into()))).await;
        let check = run_checks(drive_only.pool(), &test_http(), &google.endpoints).await;
        assert!(check.drive.ok);
        assert!(!check.calendar.ok && check.calendar.message.contains("not ticked"));
        // The API whose permission is missing is not called at all.
        assert_eq!(google.requests().len(), 1);
    }

    #[tokio::test]
    async fn without_a_sign_in_the_test_says_so() {
        let (_directory, manager) = test_pool().await;
        let check = run_checks(manager.pool(), &test_http(), &GoogleEndpoints::default()).await;
        assert!(!check.drive.ok && !check.calendar.ok);
        assert_eq!(check.drive.message, "Google is not connected.");
    }
}
