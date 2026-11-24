//! Google Calendar v3, read only: the list of calendars and the events of a
//! time span. Nothing in this module creates, changes, answers or deletes an
//! event; it sends GET requests only.
//!
//! Differences from upstream PR #666's client, each of which was a defect
//! there: any calendar can be read (not only the primary one), further pages
//! are followed, one event this code cannot read does not discard the others,
//! a call link is optional, declined events are left out, and the address of
//! the API is a parameter so tests can stand in for Google.

use super::{CalendarEvent, CalendarInfo};
use crate::google::oauth::GoogleEndpoints;
use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Pages followed per request. With 250 entries a page this is far more than a
/// few hours of calendar hold; the bound only stops a response that never ends.
const MAX_PAGES: usize = 8;
const MAX_ATTENDEE_NAMES: usize = 20;
const MAX_NAME_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarError {
    /// 401: the access token was not accepted.
    Unauthorized,
    /// The user has to act: the API is disabled, or permission is missing.
    Forbidden(String),
    /// The calendar no longer exists or is not shared with the account.
    NotFound,
    /// Rate limit, 5xx, network: another attempt later can work.
    Transient(String),
    Failed(String),
}

impl std::fmt::Display for CalendarError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("Google did not accept the sign-in."),
            Self::NotFound => formatter.write_str("The calendar was not found."),
            Self::Forbidden(message) | Self::Transient(message) | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

pub struct CalendarClient<'a> {
    pub http: &'a reqwest::Client,
    pub endpoints: &'a GoogleEndpoints,
    pub access_token: &'a str,
}

async fn read(result: Result<reqwest::Response, reqwest::Error>) -> Result<serde_json::Value, CalendarError> {
    let response = result.map_err(|error| CalendarError::Transient(format!("Google Calendar could not be reached: {}.", error.without_url())))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    if status.is_success() {
        return if parsed.is_object() { Ok(parsed) } else { Err(CalendarError::Transient("Google Calendar's answer could not be read.".into())) };
    }
    let said = parsed["error"]["message"].as_str().unwrap_or("").to_string();
    let reason = parsed["error"]["errors"][0]["reason"].as_str().unwrap_or("");
    Err(match status.as_u16() {
        401 => CalendarError::Unauthorized,
        404 => CalendarError::NotFound,
        403 if matches!(reason, "rateLimitExceeded" | "userRateLimitExceeded" | "quotaExceeded") => CalendarError::Transient("Google Calendar is rate limiting the app.".into()),
        403 if said.contains("has not been used") || said.contains("is disabled") => CalendarError::Forbidden(
            "The Google Calendar API is not enabled in your Google Cloud project (step 1 of the setup guide).".into(),
        ),
        403 => CalendarError::Forbidden(if said.is_empty() { "Google Calendar refused the request.".into() } else { format!("Google Calendar refused the request: {said}") }),
        429 | 500..=599 => CalendarError::Transient(format!("Google Calendar is temporarily unavailable (HTTP {}).", status.as_u16())),
        code => CalendarError::Failed(if said.is_empty() { format!("Google Calendar answered with HTTP {code}.") } else { format!("Google Calendar said: {said}") }),
    })
}

#[derive(Deserialize)]
struct RawCalendar {
    id: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default, rename = "summaryOverride")]
    summary_override: Option<String>,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    deleted: bool,
}

#[derive(Deserialize)]
struct RawTime {
    #[serde(default, rename = "dateTime")]
    date_time: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct RawAttendee {
    #[serde(default)]
    email: Option<String>,
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
    #[serde(default, rename = "self")]
    is_self: bool,
    #[serde(default)]
    resource: bool,
    #[serde(default, rename = "responseStatus")]
    response_status: Option<String>,
}

#[derive(Deserialize)]
struct RawEntryPoint {
    #[serde(default, rename = "entryPointType")]
    entry_point_type: Option<String>,
    #[serde(default)]
    uri: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawConference {
    #[serde(default, rename = "entryPoints")]
    entry_points: Vec<RawEntryPoint>,
}

#[derive(Deserialize)]
struct RawEvent {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    start: RawTime,
    end: RawTime,
    #[serde(default)]
    attendees: Vec<RawAttendee>,
    #[serde(default, rename = "hangoutLink")]
    hangout_link: Option<String>,
    #[serde(default, rename = "conferenceData")]
    conference_data: Option<RawConference>,
    #[serde(default, rename = "eventType")]
    event_type: Option<String>,
}

/// A readable name for an attendee: the display name when Google has one,
/// otherwise the part of the address before the `@`, with separators as spaces.
fn attendee_name(attendee: &RawAttendee) -> Option<String> {
    let clean = |text: &str| -> String {
        // Control characters (a tab between first and last name) separate words.
        let text: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_NAME_CHARS).collect()
    };
    if let Some(name) = attendee.display_name.as_deref().map(clean).filter(|name| !name.is_empty()) {
        return Some(name);
    }
    let local = attendee.email.as_deref()?.split('@').next()?;
    let words: Vec<String> = local
        .split(['.', '_', '-', '+'])
        .filter(|word| !word.is_empty() && !word.chars().all(|c| c.is_ascii_digit()))
        .map(|word| {
            let mut letters = word.chars();
            letters.next().map(|first| first.to_uppercase().collect::<String>() + letters.as_str()).unwrap_or_default()
        })
        .collect();
    Some(clean(&words.join(" "))).filter(|name| !name.is_empty())
}

/// Turns one event from Google into a `CalendarEvent`, or `None` when it is
/// not a meeting the user could have been in: cancelled, all day, declined,
/// or a block such as "out of office" or a working location.
fn to_event(raw: RawEvent, calendar_id: &str) -> Option<CalendarEvent> {
    if raw.status.as_deref() == Some("cancelled") {
        return None;
    }
    if matches!(raw.event_type.as_deref(), Some("outOfOffice" | "focusTime" | "workingLocation" | "birthday")) {
        return None;
    }
    let (start, end) = (raw.start.date_time?, raw.end.date_time?);
    if end <= start {
        return None;
    }
    let declined = |attendee: &RawAttendee| attendee.response_status.as_deref() == Some("declined");
    if raw.attendees.iter().any(|attendee| attendee.is_self && declined(attendee)) {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    let attendee_names = raw
        .attendees
        .iter()
        .filter(|attendee| !attendee.is_self && !attendee.resource && !declined(attendee))
        .filter_map(attendee_name)
        .filter(|name| seen.insert(name.to_lowercase()))
        .take(MAX_ATTENDEE_NAMES)
        .collect();
    let meet_link = raw.hangout_link.filter(|link| !link.is_empty()).or_else(|| {
        raw.conference_data
            .unwrap_or_default()
            .entry_points
            .into_iter()
            .find(|entry| entry.entry_point_type.as_deref() == Some("video"))
            .and_then(|entry| entry.uri)
    });
    Some(CalendarEvent {
        id: raw.id,
        calendar_id: calendar_id.to_string(),
        title: raw.summary.map(|title| title.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default(),
        start,
        end,
        meet_link,
        attendee_names,
    })
}

impl CalendarClient<'_> {
    /// Follows `nextPageToken` and returns every item, each still as JSON so
    /// that one unreadable item cannot spoil the rest.
    async fn list(&self, url: &str, query: &[(&str, String)]) -> Result<Vec<serde_json::Value>, CalendarError> {
        let mut items = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut request = self.http.get(url).bearer_auth(self.access_token).query(query);
            if let Some(token) = &page_token {
                request = request.query(&[("pageToken", token)]);
            }
            let mut page = read(request.send().await).await?;
            if let Some(serde_json::Value::Array(page_items)) = page.get_mut("items").map(serde_json::Value::take) {
                items.extend(page_items);
            }
            page_token = page["nextPageToken"].as_str().map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        Ok(items)
    }

    /// The calendars of the account, primary first.
    pub async fn calendars(&self) -> Result<Vec<CalendarInfo>, CalendarError> {
        let url = format!("{}/users/me/calendarList", self.endpoints.calendar_api);
        let query = [
            ("maxResults", "250".to_string()),
            ("fields", "items(id,summary,summaryOverride,primary,deleted),nextPageToken".to_string()),
        ];
        let mut calendars: Vec<CalendarInfo> = self
            .list(&url, &query)
            .await?
            .into_iter()
            .filter_map(|item| serde_json::from_value::<RawCalendar>(item).ok())
            .filter(|calendar| !calendar.deleted)
            .map(|calendar| CalendarInfo {
                name: calendar.summary_override.or(calendar.summary).filter(|name| !name.trim().is_empty()).unwrap_or_else(|| calendar.id.clone()),
                primary: calendar.primary,
                id: calendar.id,
            })
            .collect();
        calendars.sort_by(|a, b| b.primary.cmp(&a.primary).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(calendars)
    }

    /// Timed events of one calendar that overlap `[from, to]` and that the
    /// user has not declined. Recurring events arrive as single occurrences.
    pub async fn events(&self, calendar_id: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<CalendarEvent>, CalendarError> {
        // The ID is part of the path and often an address ("name@group.calendar.google.com").
        let encoded: String = url::form_urlencoded::byte_serialize(calendar_id.as_bytes()).collect::<String>().replace('+', "%20");
        let url = format!("{}/calendars/{encoded}/events", self.endpoints.calendar_api);
        let query = [
            // timeMin bounds the end of an event and timeMax its start, so the
            // pair selects everything that overlaps the span.
            ("timeMin", from.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            ("timeMax", to.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
            ("singleEvents", "true".to_string()),
            ("orderBy", "startTime".to_string()),
            ("maxResults", "250".to_string()),
            ("fields", "items(id,status,summary,start,end,eventType,hangoutLink,conferenceData(entryPoints(entryPointType,uri)),attendees(email,displayName,self,resource,responseStatus)),nextPageToken".to_string()),
        ];
        Ok(self
            .list(&url, &query)
            .await?
            .into_iter()
            .filter_map(|item| serde_json::from_value::<RawEvent>(item).ok())
            .filter_map(|raw| to_event(raw, calendar_id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::oauth::tests::{test_http, FakeGoogle};
    use chrono::TimeZone;
    use std::sync::Arc;

    fn client<'a>(http: &'a reqwest::Client, google: &'a FakeGoogle) -> CalendarClient<'a> {
        CalendarClient { http, endpoints: &google.endpoints, access_token: "token" }
    }

    fn query(target: &str) -> std::collections::HashMap<String, String> {
        url::form_urlencoded::parse(target.split_once('?').map(|(_, query)| query).unwrap_or("").as_bytes()).into_owned().collect()
    }

    fn timed(id: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut event = serde_json::json!({
            "id": id, "status": "confirmed", "summary": format!("Event {id}"),
            "start": { "dateTime": "2026-01-01T10:00:00+01:00" }, "end": { "dateTime": "2026-01-01T11:00:00+01:00" },
        });
        for (key, value) in extra.as_object().unwrap() {
            event[key] = value.clone();
        }
        event
    }

    #[tokio::test]
    async fn events_are_requested_read_only_for_the_span_and_every_page_is_followed() {
        let google = FakeGoogle::start(Arc::new(|_, target, _| {
            if target.contains("pageToken=second") {
                (200, serde_json::json!({ "items": [timed("b", serde_json::json!({}))] }).to_string())
            } else {
                (200, serde_json::json!({ "items": [timed("a", serde_json::json!({}))], "nextPageToken": "second" }).to_string())
            }
        })).await;
        let http = test_http();
        let from = Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let events = client(&http, &google).events("team@group.calendar.google.com", from, to).await.unwrap();
        assert_eq!(events.iter().map(|event| event.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        // Times are normalised to UTC whatever offset Google sends.
        assert_eq!(events[0].start, Utc.with_ymd_and_hms(2026, 1, 1, 9, 0, 0).unwrap());
        assert_eq!(events[0].calendar_id, "team@group.calendar.google.com");

        let requests = google.requests();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|(method, _, body)| method == "GET" && body.is_empty()));
        assert!(requests[0].1.starts_with("/calendar/v3/calendars/team%40group.calendar.google.com/events?"));
        let sent = query(&requests[0].1);
        assert_eq!(sent["timeMin"], "2026-01-01T08:00:00Z");
        assert_eq!(sent["timeMax"], "2026-01-01T12:00:00Z");
        assert_eq!(sent["singleEvents"], "true");
        // Addresses are requested only to derive a name; nothing else personal is asked for.
        assert!(!sent["fields"].contains("description") && !sent["fields"].contains("location"));
    }

    #[tokio::test]
    async fn one_unreadable_event_does_not_discard_the_others() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, serde_json::json!({ "items": [
            timed("good", serde_json::json!({})),
            // No ID, a number where text belongs, an attendee without an address.
            { "status": "confirmed", "start": { "dateTime": "2026-01-01T10:00:00Z" }, "end": { "dateTime": "2026-01-01T11:00:00Z" } },
            { "id": "odd", "summary": 42, "start": { "dateTime": "2026-01-01T10:00:00Z" }, "end": { "dateTime": "2026-01-01T11:00:00Z" } },
            timed("room", serde_json::json!({ "attendees": [{ "displayName": "Room 4", "resource": true }, { "displayName": "Alice" }] })),
            "not even an object",
        ] }).to_string()))).await;
        let http = test_http();
        let events = client(&http, &google).events("primary", Utc::now(), Utc::now()).await.unwrap();
        assert_eq!(events.iter().map(|event| event.id.as_str()).collect::<Vec<_>>(), ["good", "room"]);
        assert_eq!(events[1].attendee_names, ["Alice"]);
    }

    #[tokio::test]
    async fn only_meetings_the_user_could_have_been_in_are_kept() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, serde_json::json!({ "items": [
            timed("no link, in person", serde_json::json!({})),
            timed("meet", serde_json::json!({ "hangoutLink": "https://meet.google.com/abc-defg-hij" })),
            timed("zoom", serde_json::json!({ "conferenceData": { "entryPoints": [
                { "entryPointType": "phone", "uri": "tel:+1" }, { "entryPointType": "video", "uri": "https://zoom.us/j/1" }] } })),
            timed("cancelled", serde_json::json!({ "status": "cancelled" })),
            timed("declined by me", serde_json::json!({ "attendees": [{ "email": "me@example.com", "self": true, "responseStatus": "declined" }] })),
            timed("accepted by me", serde_json::json!({ "attendees": [{ "email": "me@example.com", "self": true, "responseStatus": "accepted" }] })),
            timed("out of office", serde_json::json!({ "eventType": "outOfOffice" })),
            timed("working location", serde_json::json!({ "eventType": "workingLocation" })),
            { "id": "all day", "start": { "date": "2026-01-01" }, "end": { "date": "2026-01-02" } },
            { "id": "backwards", "start": { "dateTime": "2026-01-01T11:00:00Z" }, "end": { "dateTime": "2026-01-01T10:00:00Z" } },
        ] }).to_string()))).await;
        let http = test_http();
        let events = client(&http, &google).events("primary", Utc::now(), Utc::now()).await.unwrap();
        assert_eq!(events.iter().map(|event| event.id.as_str()).collect::<Vec<_>>(), ["no link, in person", "meet", "zoom", "accepted by me"]);
        assert_eq!(events[0].meet_link, None);
        assert_eq!(events[1].meet_link.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(events[2].meet_link.as_deref(), Some("https://zoom.us/j/1"));
    }

    #[tokio::test]
    async fn invitee_names_leave_out_the_user_rooms_and_people_who_declined() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, serde_json::json!({ "items": [timed("e", serde_json::json!({ "attendees": [
            { "email": "me@example.com", "self": true, "responseStatus": "accepted" },
            { "email": "alice.smith@example.com", "displayName": "  Alice\tSmith ", "responseStatus": "accepted" },
            { "email": "bob_jones-2@example.com", "responseStatus": "needsAction" },
            { "email": "carol@example.com", "displayName": "Carol", "responseStatus": "declined" },
            { "email": "room-4@resource.calendar.google.com", "displayName": "Room 4", "resource": true },
            { "email": "ALICE.SMITH@example.com", "displayName": "alice smith" },
            { "displayName": "" },
            { "email": "12345@example.com" },
        ] }))] }).to_string()))).await;
        let http = test_http();
        let events = client(&http, &google).events("primary", Utc::now(), Utc::now()).await.unwrap();
        assert_eq!(events[0].attendee_names, ["Alice Smith", "Bob Jones"]);
    }

    #[tokio::test]
    async fn calendars_are_listed_primary_first_with_their_shown_names() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, serde_json::json!({ "items": [
            { "id": "team@group.calendar.google.com", "summary": "Team", "summaryOverride": "My team" },
            { "id": "me@example.com", "summary": "me@example.com", "primary": true },
            { "id": "old@group.calendar.google.com", "summary": "Old", "deleted": true },
            { "id": "holidays", "summary": "  " },
            { "summary": "no id" },
        ] }).to_string()))).await;
        let http = test_http();
        let calendars = client(&http, &google).calendars().await.unwrap();
        assert_eq!(calendars, [
            CalendarInfo { id: "me@example.com".into(), name: "me@example.com".into(), primary: true },
            CalendarInfo { id: "holidays".into(), name: "holidays".into(), primary: false },
            CalendarInfo { id: "team@group.calendar.google.com".into(), name: "My team".into(), primary: false },
        ]);
        assert!(google.requests()[0].1.starts_with("/calendar/v3/users/me/calendarList?"));
    }

    #[tokio::test]
    async fn errors_are_sorted_into_retry_later_and_the_user_must_act() {
        for (status, body, expected) in [
            (401, r#"{"error":{"message":"Invalid Credentials"}}"#, CalendarError::Unauthorized),
            (404, r#"{"error":{"message":"Not Found"}}"#, CalendarError::NotFound),
            (403, r#"{"error":{"message":"Google Calendar API has not been used in project 1 before or it is disabled."}}"#, CalendarError::Forbidden("The Google Calendar API is not enabled in your Google Cloud project (step 1 of the setup guide).".into())),
            (403, r#"{"error":{"message":"x","errors":[{"reason":"rateLimitExceeded"}]}}"#, CalendarError::Transient("Google Calendar is rate limiting the app.".into())),
            (503, "busy", CalendarError::Transient("Google Calendar is temporarily unavailable (HTTP 503).".into())),
            (200, "[]", CalendarError::Transient("Google Calendar's answer could not be read.".into())),
        ] {
            let body = body.to_string();
            let google = FakeGoogle::start(Arc::new(move |_, _, _| (status, body.clone()))).await;
            let http = test_http();
            assert_eq!(client(&http, &google).calendars().await.unwrap_err(), expected, "{status}");
        }
    }

    #[tokio::test]
    async fn a_response_that_never_ends_is_cut_off() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, r#"{"items":[],"nextPageToken":"again"}"#.into()))).await;
        let http = test_http();
        assert!(client(&http, &google).events("primary", Utc::now(), Utc::now()).await.unwrap().is_empty());
        assert_eq!(google.requests().len(), MAX_PAGES);
    }

    #[test]
    fn this_module_only_reads() {
        let source = include_str!("client.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in [".post(", ".put(", ".patch(", ".delete("] {
            assert!(!code.contains(forbidden), "{forbidden} found in the calendar client");
        }
    }
}
