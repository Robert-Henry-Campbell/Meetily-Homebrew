//! Looks up the calendar event of a recording, in the background.
//!
//! Stop-and-save only inserts the meeting; a trigger queues the lookup and this
//! code performs it later with bounded HTTP calls. Upstream PR #666 awaited
//! Google inside the stop flow, without timeouts.

use super::client::{CalendarClient, CalendarError};
use super::matcher::best_match;
use super::state;
use super::CalendarEvent;
use crate::database::repositories::meeting::MeetingsRepository;
use crate::export::worker::ExportContext;
use crate::google::oauth::{self, AccessError};
use chrono::Duration;

/// Events are fetched a little beyond the recording; the matcher itself only
/// counts the time actually recorded.
const FETCH_MARGIN_MINUTES: i64 = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchOutcome {
    Matched(CalendarEvent),
    /// Nothing was scheduled during the recording.
    NoEvent,
    /// Not attempted: Google is not connected, Calendar was not granted, or
    /// matching is switched off. The meeting stays queued.
    Skipped,
    /// The user has to act.
    Attention(String),
    /// Trouble that passes; tried again later.
    Retry(String),
}

enum Step {
    Calendar(CalendarError),
    Store(sqlx::Error),
}

impl From<CalendarError> for Step {
    fn from(error: CalendarError) -> Self {
        Self::Calendar(error)
    }
}

impl From<sqlx::Error> for Step {
    fn from(error: sqlx::Error) -> Self {
        Self::Store(error)
    }
}

async fn find(context: &ExportContext<'_>, meeting_id: &str, access_token: &str) -> Result<Option<CalendarEvent>, Step> {
    let Some((start, end)) = state::recording_interval(context.pool, meeting_id).await? else { return Ok(None) };
    let client = CalendarClient { http: context.http, endpoints: context.endpoints, access_token };
    let margin = Duration::minutes(FETCH_MARGIN_MINUTES);
    let mut events = Vec::new();
    for calendar_id in state::selected_calendars(context.pool).await? {
        match client.events(&calendar_id, start - margin, end + margin).await {
            Ok(found) => events.extend(found),
            // A calendar that was removed or unshared since it was ticked: the others still count.
            Err(CalendarError::NotFound) => log::info!("A selected calendar no longer exists and was skipped"),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(best_match(&events, start, end).cloned())
}

/// Looks up one meeting now and stores the result.
pub async fn match_meeting(context: &ExportContext<'_>, meeting_id: &str, now: i64) -> MatchOutcome {
    let usable = match (oauth::status(context.pool).await, state::matching_enabled(context.pool).await) {
        (Ok(google), Ok(enabled)) => enabled && google.connected && google.calendar_granted && google.needs_attention.is_none(),
        _ => false,
    };
    if !usable {
        return MatchOutcome::Skipped;
    }
    let mut refreshed = false;
    let outcome = loop {
        let access_token = match oauth::access_token(context.pool, context.http, context.endpoints).await {
            Ok(token) => token,
            Err(AccessError::NotConnected) => return MatchOutcome::Skipped,
            Err(AccessError::NeedsAttention(reason)) => break MatchOutcome::Attention(reason),
            Err(AccessError::Transient(reason)) => break MatchOutcome::Retry(reason),
        };
        match find(context, meeting_id, &access_token).await {
            Ok(Some(event)) => break MatchOutcome::Matched(event),
            Ok(None) => break MatchOutcome::NoEvent,
            Err(Step::Calendar(CalendarError::Unauthorized)) if !refreshed => {
                refreshed = true;
                oauth::expire_access_token(context.pool).await;
            }
            Err(Step::Calendar(CalendarError::Unauthorized)) => {
                let reason = "Google did not accept the sign-in. Sign in again.";
                oauth::mark_needs_attention(context.pool, reason).await;
                break MatchOutcome::Attention(reason.into());
            }
            Err(Step::Calendar(CalendarError::Forbidden(reason))) => break MatchOutcome::Attention(reason),
            Err(Step::Calendar(error)) => break MatchOutcome::Retry(error.to_string()),
            Err(Step::Store(error)) => break MatchOutcome::Retry(format!("The app's database could not be read: {error}")),
        }
    };
    let stored = match &outcome {
        MatchOutcome::Matched(event) => {
            let stored = state::record_result(context.pool, meeting_id, Some(event), now).await;
            // The event's title names the meeting, unless the user already named it:
            // the SQL predicate (title_is_manual = 0) decides, not a guess from the text.
            if stored.is_ok() && !event.title.trim().is_empty() {
                if let Err(error) = MeetingsRepository::update_generated_meeting_title(context.pool, meeting_id, &event.title).await {
                    log::warn!("The calendar title could not be applied to {meeting_id}: {error}");
                }
            }
            stored
        }
        MatchOutcome::NoEvent => state::record_result(context.pool, meeting_id, None, now).await,
        MatchOutcome::Attention(reason) => state::record_failure(context.pool, meeting_id, true, reason, now).await,
        MatchOutcome::Retry(reason) => state::record_failure(context.pool, meeting_id, false, reason, now).await,
        MatchOutcome::Skipped => Ok(()),
    };
    if let Err(error) = stored {
        log::warn!("Could not store the calendar lookup for {meeting_id}: {error}");
    }
    outcome
}

/// One look at the queue. Returns the meetings whose calendar entry changed.
pub async fn pass(context: &ExportContext<'_>, now: i64) -> Vec<String> {
    let mut changed = Vec::new();
    let Ok(due) = state::due(context.pool, now).await else { return changed };
    for meeting_id in due {
        match match_meeting(context, &meeting_id, now).await {
            // Nothing can be looked up right now; the rest of the queue would fare the same.
            MatchOutcome::Skipped => break,
            _ => changed.push(meeting_id),
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::worker::tests::Fixture;
    use std::sync::{Arc, Mutex};

    /// Events the fake Google serves per calendar, and what it was asked.
    #[derive(Default)]
    struct FakeCalendar {
        events: std::collections::HashMap<String, Vec<serde_json::Value>>,
        fail_next: Vec<u16>,
        asked: Vec<String>,
    }

    fn timed(id: &str, title: &str, start: &str, end: &str, attendees: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "id": id, "status": "confirmed", "summary": title,
            "start": { "dateTime": start }, "end": { "dateTime": end },
            "attendees": attendees.iter().map(|name| serde_json::json!({ "displayName": name, "email": format!("{}@example.com", name.to_lowercase().replace(' ', ".")) })).collect::<Vec<_>>(),
        })
    }

    /// A signed-in app whose fake Google serves `calendar` on the calendar API
    /// and the in-memory Drive on everything else.
    async fn fixture(calendar: Arc<Mutex<FakeCalendar>>) -> (Fixture, crate::google::oauth::tests::FakeGoogle) {
        let fixture = Fixture::new().await;
        let served = calendar.clone();
        let google = crate::google::oauth::tests::FakeGoogle::start(Arc::new(move |_, target, _| {
            let mut calendar = served.lock().unwrap();
            if target.starts_with("/token") {
                return (200, r#"{"access_token":"fresh","expires_in":3600}"#.into());
            }
            if let Some(status) = (!calendar.fail_next.is_empty()).then(|| calendar.fail_next.remove(0)) {
                return (status, r#"{"error":{"message":"scripted failure"}}"#.into());
            }
            let path = target.split('?').next().unwrap();
            let id: String = url::form_urlencoded::parse(path.trim_start_matches("/calendar/v3/calendars/").trim_end_matches("/events").as_bytes())
                .map(|(key, _)| key.into_owned())
                .collect();
            calendar.asked.push(id.clone());
            match calendar.events.get(&id) {
                Some(events) => (200, serde_json::json!({ "items": events }).to_string()),
                None => (404, r#"{"error":{"message":"Not Found"}}"#.into()),
            }
        })).await;
        (fixture, google)
    }

    async fn recorded(fixture: &Fixture, id: &str, seconds: f64) {
        // Recorded from 14:30 UTC on 2026-11-23 (see Fixture::add_meeting).
        // As the app saves a recording that still has its automatic title.
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at, title_is_manual) VALUES (?, 'Meeting · Mon, Nov 23 · 2:30 PM', '2026-11-23T14:30:00+00:00', 'x', 0)")
            .bind(id).execute(fixture.manager.pool()).await.unwrap();
        sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, speaker) VALUES (?, ?, 'hello', 'x', 1.0, ?, 'You')")
            .bind(format!("{id}-t")).bind(id).bind(seconds).execute(fixture.manager.pool()).await.unwrap();
    }

    fn context<'a>(fixture: &'a Fixture, google: &'a crate::google::oauth::tests::FakeGoogle) -> ExportContext<'a> {
        ExportContext { pool: fixture.manager.pool(), http: &fixture.http, endpoints: &google.endpoints, to_local: |time| time.naive_utc() }
    }

    #[tokio::test]
    async fn the_recorded_event_names_the_meeting_and_supplies_the_invitees() {
        let calendar = Arc::new(Mutex::new(FakeCalendar::default()));
        calendar.lock().unwrap().events.insert("primary".into(), vec![
            timed("own", "Budget review", "2026-11-23T14:30:00Z", "2026-11-23T15:00:00Z", &["Alice Smith"]),
            timed("later", "Quarterly planning", "2026-11-23T15:00:00Z", "2026-11-23T17:00:00Z", &["Bob", "Carol"]),
        ]);
        let (fixture, google) = fixture(calendar.clone()).await;
        recorded(&fixture, "m", 1750.0).await;

        assert_eq!(pass(&context(&fixture, &google), 100).await, ["m"]);
        let link = state::get(fixture.manager.pool(), "m").await.unwrap().unwrap();
        assert_eq!((link.status.as_str(), link.event_title.as_deref()), ("matched", Some("Budget review")));
        assert_eq!(link.attendee_names, ["Alice Smith"]);
        // The automatic title is replaced by the event's.
        let title: String = sqlx::query_scalar("SELECT title FROM meetings WHERE id = 'm'").fetch_one(fixture.manager.pool()).await.unwrap();
        assert_eq!(title, "Budget review");
        // Looked up once; the queue is empty afterwards.
        assert!(pass(&context(&fixture, &google), 200).await.is_empty());
        assert_eq!(calendar.lock().unwrap().asked, ["primary"]);
    }

    #[tokio::test]
    async fn a_title_the_user_chose_is_kept_and_no_event_means_no_change() {
        let calendar = Arc::new(Mutex::new(FakeCalendar::default()));
        calendar.lock().unwrap().events.insert("primary".into(), vec![
            timed("own", "Budget review", "2026-11-23T14:30:00Z", "2026-11-23T15:00:00Z", &[]),
        ]);
        let (fixture, google) = fixture(calendar.clone()).await;
        recorded(&fixture, "named", 1700.0).await;
        sqlx::query("UPDATE meetings SET title = 'My own title', title_is_manual = 1 WHERE id = 'named'").execute(fixture.manager.pool()).await.unwrap();
        pass(&context(&fixture, &google), 100).await;
        let title: String = sqlx::query_scalar("SELECT title FROM meetings WHERE id = 'named'").fetch_one(fixture.manager.pool()).await.unwrap();
        assert_eq!(title, "My own title");
        assert_eq!(state::get(fixture.manager.pool(), "named").await.unwrap().unwrap().status, "matched");

        // Nothing scheduled then.
        calendar.lock().unwrap().events.insert("primary".into(), vec![]);
        recorded(&fixture, "free", 600.0).await;
        pass(&context(&fixture, &google), 200).await;
        let link = state::get(fixture.manager.pool(), "free").await.unwrap().unwrap();
        assert_eq!((link.status.as_str(), link.event_title), ("none", None));
        let title: String = sqlx::query_scalar("SELECT title FROM meetings WHERE id = 'free'").fetch_one(fixture.manager.pool()).await.unwrap();
        assert!(title.starts_with("Meeting · "));
    }

    #[tokio::test]
    async fn every_ticked_calendar_is_searched_and_a_removed_one_is_skipped() {
        let calendar = Arc::new(Mutex::new(FakeCalendar::default()));
        calendar.lock().unwrap().events.insert("primary".into(), vec![]);
        calendar.lock().unwrap().events.insert("team@group.calendar.google.com".into(), vec![
            timed("team", "Team sync", "2026-11-23T14:30:00Z", "2026-11-23T15:00:00Z", &["Dana"]),
        ]);
        let (fixture, google) = fixture(calendar.clone()).await;
        state::save_selected_calendars(fixture.manager.pool(), &["primary".into(), "gone@group.calendar.google.com".into(), "team@group.calendar.google.com".into()]).await.unwrap();
        recorded(&fixture, "m", 1700.0).await;
        pass(&context(&fixture, &google), 100).await;
        assert_eq!(state::get(fixture.manager.pool(), "m").await.unwrap().unwrap().event_title.as_deref(), Some("Team sync"));
        assert_eq!(calendar.lock().unwrap().asked, ["primary", "gone@group.calendar.google.com", "team@group.calendar.google.com"]);
    }

    #[tokio::test]
    async fn outages_are_retried_and_an_ended_sign_in_stops_the_lookups() {
        let calendar = Arc::new(Mutex::new(FakeCalendar::default()));
        calendar.lock().unwrap().events.insert("primary".into(), vec![
            timed("own", "Budget review", "2026-11-23T14:30:00Z", "2026-11-23T15:00:00Z", &[]),
        ]);
        let (fixture, google) = fixture(calendar.clone()).await;
        recorded(&fixture, "m", 1700.0).await;
        calendar.lock().unwrap().fail_next = vec![503];
        assert_eq!(match_meeting(&context(&fixture, &google), "m", 1_000).await, MatchOutcome::Retry("Google Calendar is temporarily unavailable (HTTP 503).".into()));
        assert!(pass(&context(&fixture, &google), 1_029).await.is_empty());
        assert_eq!(pass(&context(&fixture, &google), 1_030).await, ["m"]);
        assert_eq!(state::get(fixture.manager.pool(), "m").await.unwrap().unwrap().status, "matched");

        // 401 twice: one refresh, then the connection itself needs attention.
        recorded(&fixture, "n", 1700.0).await;
        calendar.lock().unwrap().fail_next = vec![401, 401];
        assert!(matches!(match_meeting(&context(&fixture, &google), "n", 2_000).await, MatchOutcome::Attention(_)));
        assert!(oauth::status(fixture.manager.pool()).await.unwrap().needs_attention.is_some());
        // While it does, nothing is asked of Google and the meeting stays queued.
        let asked = calendar.lock().unwrap().asked.len();
        assert!(pass(&context(&fixture, &google), i64::MAX - 10_000).await.is_empty());
        assert_eq!(calendar.lock().unwrap().asked.len(), asked);
    }

    #[tokio::test]
    async fn without_the_calendar_permission_or_with_matching_off_nothing_is_asked() {
        let calendar = Arc::new(Mutex::new(FakeCalendar::default()));
        let (fixture, google) = fixture(calendar.clone()).await;
        recorded(&fixture, "m", 600.0).await;
        state::set_matching_enabled(fixture.manager.pool(), false).await.unwrap();
        assert_eq!(match_meeting(&context(&fixture, &google), "m", 1).await, MatchOutcome::Skipped);
        state::set_matching_enabled(fixture.manager.pool(), true).await.unwrap();
        crate::integrations::config_store::set(fixture.manager.pool(), "google.scopes", "https://www.googleapis.com/auth/drive.file").await.unwrap();
        assert_eq!(match_meeting(&context(&fixture, &google), "m", 2).await, MatchOutcome::Skipped);
        assert!(google.requests().is_empty());
        assert_eq!(state::get(fixture.manager.pool(), "m").await.unwrap().unwrap().status, "pending");
    }

    #[tokio::test]
    async fn the_invitee_decides_the_drive_folder_when_no_voice_is_named() {
        let calendar = Arc::new(Mutex::new(FakeCalendar::default()));
        calendar.lock().unwrap().events.insert("primary".into(), vec![
            timed("own", "Budget review", "2026-11-23T14:30:00Z", "2026-11-23T15:00:00Z", &["Alice Smith"]),
        ]);
        let (fixture, google) = fixture(calendar).await;
        recorded(&fixture, "m", 1700.0).await;
        // First export: nobody is known yet.
        fixture.settle().await;
        assert_eq!(fixture.paths(), ["Meetily meetings/2026-11-23 1430 Meeting - transcript.md"]);
        // The calendar match arrives; the same file moves to the invitee's folder under the event's title.
        pass(&context(&fixture, &google), 100).await;
        fixture.settle().await;
        assert_eq!(fixture.paths(), ["Meetily meetings/Alice Smith/2026-11-23 1430 Budget review - transcript.md"]);
    }
}
