//! Commands behind the Drive chip on a meeting and the export options.

use super::state::{self, ExportSettings, Readiness};
use super::worker;
use crate::google::oauth;
use crate::integrations::config_store;
use crate::state::AppState;
use serde::Serialize;

/// What the chip on a meeting shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveExportStatus {
    /// `not-connected`, `none` (not in Drive and not due), `waiting`, `sent`,
    /// `attention` (the user has to act) or `failed`.
    pub state: String,
    /// One sentence: what is waited for, or what went wrong.
    pub message: Option<String>,
    pub summary_link: Option<String>,
    pub transcript_link: Option<String>,
    pub folder_link: Option<String>,
    /// Unix seconds of the last successful export.
    pub exported_at: Option<i64>,
}

pub(crate) async fn read_status(pool: &sqlx::SqlitePool, meeting_id: &str, now: i64) -> Result<DriveExportStatus, sqlx::Error> {
    let google = oauth::status(pool).await?;
    let row = state::get(pool, meeting_id).await?;
    let links = |row: &state::ExportRow| {
        (
            row.summary_link.clone(),
            row.transcript_link.clone(),
            row.folder_id.as_ref().map(|id| format!("https://drive.google.com/drive/folders/{id}")),
        )
    };
    let mut status = DriveExportStatus {
        state: "none".into(),
        message: None,
        summary_link: None,
        transcript_link: None,
        folder_link: None,
        exported_at: None,
    };
    if let Some(row) = &row {
        (status.summary_link, status.transcript_link, status.folder_link) = links(row);
        status.exported_at = row.exported_at;
    }
    if !google.connected {
        status.state = "not-connected".into();
        return Ok(status);
    }
    let Some(row) = row else { return Ok(status) };
    let up_to_date = row.exported_version == Some(row.content_version) && row.requested == 0;
    if up_to_date {
        status.state = if row.status == "sent" { "sent" } else { "none" }.into();
        return Ok(status);
    }
    if let Some(reason) = google.needs_attention.clone().or_else(|| (!google.drive_granted).then(|| "The Drive permission is missing. Sign in again and tick it.".to_string())) {
        status.state = "attention".into();
        status.message = Some(reason);
        return Ok(status);
    }
    match row.status.as_str() {
        "attention" | "failed" if row.next_attempt_at > now || row.status == "attention" => {
            status.state = row.status.clone();
            status.message = row.last_error.clone();
            return Ok(status);
        }
        _ => {}
    }
    // Due or waiting: say what for.
    let settings = state::settings(pool).await?;
    let candidate = state::candidates(pool, i64::MAX).await?.into_iter().find(|candidate| candidate.meeting_id == meeting_id);
    match candidate.map(|candidate| state::readiness(&candidate, &settings, now)) {
        Some(Readiness::Ready) => {
            status.state = "waiting".into();
            status.message = Some(match row.last_error {
                Some(error) if row.attempts > 0 => format!("Trying again shortly. Last attempt: {error}"),
                _ => "Sending to Drive shortly.".into(),
            });
        }
        Some(Readiness::Wait(reason)) => {
            status.state = "waiting".into();
            status.message = Some(reason.into());
        }
        // Not sent on its own (recorded before the export was switched on, or
        // the export is off). A file from an earlier export may still exist.
        Some(Readiness::NotEligible) | None => {
            status.state = if row.exported_version.is_some() { "sent" } else { "none" }.into();
            if row.exported_version.is_some() {
                status.message = Some("The meeting changed after it was sent. Automatic sending is off.".into());
            }
        }
    }
    Ok(status)
}

#[tauri::command]
pub async fn drive_export_status(state: tauri::State<'_, AppState>, meeting_id: String) -> Result<DriveExportStatus, String> {
    read_status(state.db_manager.pool(), &meeting_id, chrono::Utc::now().timestamp())
        .await
        .map_err(|error| format!("Could not read the Drive export state: {error}"))
}

/// Sends this meeting on the worker's next pass, whatever the automatic rules say.
#[tauri::command]
pub async fn drive_export_send_now(state: tauri::State<'_, AppState>, meeting_id: String) -> Result<DriveExportStatus, String> {
    let pool = state.db_manager.pool();
    let now = chrono::Utc::now().timestamp();
    state::request_now(pool, &meeting_id, now)
        .await
        .map_err(|error| format!("Could not queue the meeting for Drive: {error}"))?;
    worker::wake();
    read_status(pool, &meeting_id, now)
        .await
        .map_err(|error| format!("Could not read the Drive export state: {error}"))
}

#[tauri::command]
pub async fn drive_export_settings(state: tauri::State<'_, AppState>) -> Result<ExportSettings, String> {
    state::settings(state.db_manager.pool())
        .await
        .map_err(|error| format!("Could not read the Drive export options: {error}"))
}

#[tauri::command]
pub async fn drive_export_save_settings(
    state: tauri::State<'_, AppState>,
    auto_export: bool,
    folder_per_person: bool,
    root_folder_name: String,
) -> Result<ExportSettings, String> {
    let pool = state.db_manager.pool();
    state::save_settings(pool, auto_export, folder_per_person, &root_folder_name)
        .await
        .map_err(|error| format!("Could not save the Drive export options: {error}"))?;
    worker::wake();
    state::settings(pool)
        .await
        .map_err(|error| format!("Could not read the Drive export options: {error}"))
}

/// The user's display name lives in the webview's storage. The export writes
/// files without a webview, so the name is copied here whenever it is set.
#[tauri::command]
pub async fn set_user_display_name(state: tauri::State<'_, AppState>, name: String) -> Result<(), String> {
    let name: String = name.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect();
    config_store::set(state.db_manager.pool(), state::KEY_USER_NAME, &name)
        .await
        .map_err(|error| format!("Could not store the display name: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::worker::tests::Fixture;

    #[tokio::test]
    async fn the_chip_follows_a_meeting_from_waiting_to_sent_and_back_to_waiting() {
        let fixture = Fixture::new().await;
        let pool = fixture.manager.pool();
        fixture.add_meeting("m", "Budget").await;
        let now = chrono::Utc::now().timestamp();
        // No content yet: nothing to show.
        assert_eq!(read_status(pool, "m", now).await.unwrap().state, "none");

        fixture.add_line("m", "t1", "Alice", "Yes.").await;
        let job = |stage: &str| crate::post_call::PostCallJobUpdate { meeting_id: "m".into(), stage: stage.into(), ..Default::default() };
        crate::post_call::PostCallRepository::save(pool, &job("enhancing")).await.unwrap();
        let waiting = read_status(pool, "m", now + 3600).await.unwrap();
        assert_eq!((waiting.state.as_str(), waiting.message.as_deref()), ("waiting", Some("The transcript is still being processed.")));

        crate::post_call::PostCallRepository::save(pool, &job("done")).await.unwrap();
        assert_eq!(read_status(pool, "m", now + 7200).await.unwrap().message.as_deref(), Some("Sending to Drive shortly."));
        fixture.settle().await;
        let sent = read_status(pool, "m", now + 7200).await.unwrap();
        assert_eq!(sent.state, "sent");
        assert!(sent.transcript_link.unwrap().starts_with("https://drive.google.com/file/d/"));
        assert!(sent.folder_link.unwrap().starts_with("https://drive.google.com/drive/folders/"));
        assert_eq!(sent.summary_link, None);

        // An edit after the export: waiting again, with the links still usable.
        sqlx::query("UPDATE transcripts SET transcript = 'Edited.' WHERE id = 't1'").execute(pool).await.unwrap();
        let again = read_status(pool, "m", chrono::Utc::now().timestamp()).await.unwrap();
        assert_eq!(again.state, "waiting");
        assert!(again.transcript_link.is_some());
    }

    #[tokio::test]
    async fn problems_and_meetings_outside_the_automatic_rules_are_told_apart() {
        let fixture = Fixture::new().await;
        let pool = fixture.manager.pool();
        fixture.add_meeting("m", "Budget").await;
        fixture.add_line("m", "t1", "Alice", "Yes.").await;
        let now = chrono::Utc::now().timestamp() + 7200;

        fixture.drive.lock().unwrap().fail_next = vec![403];
        fixture.settle().await;
        let attention = read_status(pool, "m", now).await.unwrap();
        assert_eq!(attention.state, "attention");
        assert!(attention.message.unwrap().contains("refused"));

        // The connection itself needs attention: the chip says why for every pending meeting.
        oauth::mark_needs_attention(pool, "Google ended this sign-in. Sign in again.").await;
        assert_eq!(read_status(pool, "m", now).await.unwrap().message.as_deref(), Some("Google ended this sign-in. Sign in again."));

        // Not connected at all.
        oauth::disconnect(pool, &fixture.http, &fixture.google.endpoints).await.unwrap();
        assert_eq!(read_status(pool, "m", now).await.unwrap().state, "not-connected");
    }

    #[tokio::test]
    async fn an_older_meeting_is_offered_send_now_and_then_sent() {
        let fixture = Fixture::new().await;
        let pool = fixture.manager.pool();
        config_store::set(pool, "drive.enabled_since", &chrono::Utc::now().timestamp().to_string()).await.unwrap();
        fixture.add_meeting("old", "History").await;
        fixture.add_line("old", "t1", "Alice", "Long ago.").await;
        let now = chrono::Utc::now().timestamp() + 7200;
        assert_eq!(read_status(pool, "old", now).await.unwrap().state, "none");

        state::request_now(pool, "old", now).await.unwrap();
        assert_eq!(read_status(pool, "old", now).await.unwrap().state, "waiting");
        fixture.settle().await;
        assert_eq!(read_status(pool, "old", now).await.unwrap().state, "sent");
    }
}
