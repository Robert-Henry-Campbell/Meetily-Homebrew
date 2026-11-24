//! Commands for the calendar options in Connections and for a meeting's
//! calendar entry.

use super::client::CalendarClient;
use super::state::{self, CalendarLink};
use super::worker::{self, MatchOutcome};
use crate::export::worker::{local_time, ExportContext};
use crate::google::oauth::{self, GoogleEndpoints};
use crate::state::AppState;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarChoice {
    pub id: String,
    pub name: String,
    pub primary: bool,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarOptions {
    pub matching: bool,
    pub calendars: Vec<CalendarChoice>,
}

/// The account's calendars with the current choice. Reads from Google.
#[tauri::command]
pub async fn calendar_options(state: tauri::State<'_, AppState>) -> Result<CalendarOptions, String> {
    let pool = state.db_manager.pool();
    let http = oauth::http_client();
    let endpoints = GoogleEndpoints::default();
    let access_token = oauth::access_token(pool, &http, &endpoints).await.map_err(|error| error.to_string())?;
    let calendars = CalendarClient { http: &http, endpoints: &endpoints, access_token: &access_token }
        .calendars()
        .await
        .map_err(|error| error.to_string())?;
    let selected = state::selected_calendars(pool).await.map_err(|error| format!("Could not read the calendar choice: {error}"))?;
    let matching = state::matching_enabled(pool).await.map_err(|error| format!("Could not read the calendar options: {error}"))?;
    Ok(CalendarOptions {
        matching,
        calendars: calendars
            .into_iter()
            .map(|calendar| CalendarChoice {
                selected: selected.contains(&calendar.id) || (calendar.primary && selected.iter().any(|id| id == state::PRIMARY_CALENDAR)),
                id: calendar.id,
                name: calendar.name,
                primary: calendar.primary,
            })
            .collect(),
    })
}

#[tauri::command]
pub async fn calendar_save_options(state: tauri::State<'_, AppState>, matching: bool, calendar_ids: Vec<String>) -> Result<(), String> {
    let pool = state.db_manager.pool();
    state::set_matching_enabled(pool, matching).await.map_err(|error| format!("Could not save the calendar options: {error}"))?;
    state::save_selected_calendars(pool, &calendar_ids).await.map_err(|error| format!("Could not save the calendar choice: {error}"))?;
    crate::export::worker::wake();
    Ok(())
}

/// The stored calendar entry of a meeting, if it was looked up.
#[tauri::command]
pub async fn calendar_link(state: tauri::State<'_, AppState>, meeting_id: String) -> Result<Option<CalendarLink>, String> {
    state::get(state.db_manager.pool(), &meeting_id).await.map_err(|error| format!("Could not read the calendar entry: {error}"))
}

/// Looks the meeting up now instead of waiting for the background pass. The
/// speaker question uses this to offer the invitees' names; the call is
/// bounded by the HTTP timeouts and never blocks saving a recording.
#[tauri::command]
pub async fn calendar_match_meeting(state: tauri::State<'_, AppState>, meeting_id: String, again: Option<bool>) -> Result<Option<CalendarLink>, String> {
    let pool = state.db_manager.pool();
    let stored = state::get(pool, &meeting_id).await.map_err(|error| format!("Could not read the calendar entry: {error}"))?;
    let looked_up = stored.as_ref().is_some_and(|link| link.status == "matched" || link.status == "none");
    if looked_up && again != Some(true) {
        return Ok(stored);
    }
    state::request(pool, &meeting_id).await.map_err(|error| format!("Could not queue the calendar lookup: {error}"))?;
    let http = oauth::http_client();
    let endpoints = GoogleEndpoints::default();
    let context = ExportContext { pool, http: &http, endpoints: &endpoints, to_local: local_time };
    if let MatchOutcome::Skipped = worker::match_meeting(&context, &meeting_id, chrono::Utc::now().timestamp()).await {
        return Ok(None);
    }
    state::get(pool, &meeting_id).await.map_err(|error| format!("Could not read the calendar entry: {error}"))
}

/// Names to offer at "Who was on the call?": what the user typed before, or
/// else the invitees of the matched calendar event.
#[tauri::command]
pub async fn meeting_suggested_participants(state: tauri::State<'_, AppState>, meeting_id: String) -> Result<Vec<String>, String> {
    let pool = state.db_manager.pool();
    let typed = crate::post_call::PostCallRepository::participants(pool, &meeting_id)
        .await
        .map_err(|error| format!("Could not read the participants: {error}"))?;
    if !typed.is_empty() {
        return Ok(typed);
    }
    Ok(state::get(pool, &meeting_id)
        .await
        .map_err(|error| format!("Could not read the calendar entry: {error}"))?
        .filter(|link| link.status == "matched")
        .map(|link| link.attendee_names)
        .unwrap_or_default())
}
