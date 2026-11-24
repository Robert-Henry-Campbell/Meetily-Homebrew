//! Google Calendar, read only: finds the event a recording belongs to, so the
//! meeting gets the event's title and the names of the people invited.
//!
//! Written for this build rather than ported from upstream PR #666. That PR's
//! matcher and client were first run unmodified; `docs/HOMEBREW_CALENDAR.md`
//! lists what they get wrong and how each point is handled here.

pub mod client;
pub mod commands;
pub mod matcher;
pub mod state;
pub mod worker;

use chrono::{DateTime, Utc};

/// Emitted with `{ meetingId }` when a meeting's calendar entry changed.
pub const CALENDAR_LINK_CHANGED: &str = "calendar-link-changed";

/// A timed event the user has not declined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub id: String,
    pub calendar_id: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// A video call address, when the event has one. Not required for a match.
    pub meet_link: Option<String>,
    /// The other people invited who have not declined. Names, not addresses.
    pub attendee_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarInfo {
    pub id: String,
    pub name: String,
    pub primary: bool,
}
