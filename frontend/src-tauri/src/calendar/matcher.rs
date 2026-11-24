//! Chooses the calendar event a recording belongs to.
//!
//! The score is the overlap between the event and the time actually recorded,
//! `[start, start + duration]`. Upstream PR #666 scored against a four-hour
//! window around one timestamp instead; within that window the longest event
//! wins, whether or not it was the one recorded.

use super::CalendarEvent;
use chrono::{DateTime, Utc};

/// An overlap shorter than this is two things touching, not one meeting.
const MIN_OVERLAP_SECONDS: i64 = 60;
/// The overlap must also cover this share of the shorter of the two spans.
const MIN_OVERLAP_SHARE: f64 = 0.2;

fn overlap_seconds(event: &CalendarEvent, start: DateTime<Utc>, end: DateTime<Utc>) -> i64 {
    (event.end.min(end) - event.start.max(start)).num_seconds()
}

/// The event that best covers the recording, or `None` when nothing was
/// scheduled then. Ties go to the event that started closest to the recording,
/// then to one with a call link, then to the earlier one.
pub fn best_match(events: &[CalendarEvent], recording_start: DateTime<Utc>, recording_end: DateTime<Utc>) -> Option<&CalendarEvent> {
    let recorded = (recording_end - recording_start).num_seconds().max(1);
    events
        .iter()
        .filter_map(|event| {
            let overlap = overlap_seconds(event, recording_start, recording_end);
            let event_length = (event.end - event.start).num_seconds().max(1);
            let needed = MIN_OVERLAP_SECONDS.max((MIN_OVERLAP_SHARE * recorded.min(event_length) as f64).ceil() as i64);
            // A recording shorter than the minimum still matches the event it lies inside.
            let enough = overlap >= needed || (overlap > 0 && overlap >= recorded);
            enough.then_some((event, overlap))
        })
        .max_by(|(a, a_overlap), (b, b_overlap)| {
            let distance = |event: &CalendarEvent| (event.start - recording_start).num_seconds().abs();
            a_overlap
                .cmp(b_overlap)
                .then_with(|| distance(b).cmp(&distance(a)))
                .then_with(|| a.meet_link.is_some().cmp(&b.meet_link.is_some()))
                .then_with(|| b.start.cmp(&a.start))
                .then_with(|| b.id.cmp(&a.id))
        })
        .map(|(event, _)| event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, hour, minute, 0).unwrap()
    }

    fn event(id: &str, start: (u32, u32), end: (u32, u32)) -> CalendarEvent {
        CalendarEvent {
            id: id.into(),
            calendar_id: "primary".into(),
            title: id.into(),
            start: at(start.0, start.1),
            end: at(end.0, end.1),
            meet_link: None,
            attendee_names: vec![],
        }
    }

    fn matched<'a>(events: &'a [CalendarEvent], start: (u32, u32), end: (u32, u32)) -> Option<&'a str> {
        best_match(events, at(start.0, start.1), at(end.0, end.1)).map(|event| event.id.as_str())
    }

    #[test]
    fn the_recorded_event_wins_over_a_longer_neighbour() {
        // The case PR #666 gets wrong: recorded 10:00-10:30, a longer meeting follows.
        let events = [event("the recorded call", (10, 0), (10, 30)), event("later planning", (11, 0), (12, 30))];
        assert_eq!(matched(&events, (10, 0), (10, 30)), Some("the recorded call"));
        // Started two minutes late, ran three minutes over.
        assert_eq!(matched(&events, (10, 2), (10, 33)), Some("the recorded call"));
    }

    #[test]
    fn nothing_scheduled_during_the_recording_matches_nothing() {
        // An earlier meeting that ended before the recording began (PR #666 matches it).
        let events = [event("earlier meeting", (8, 30), (9, 30)), event("later", (11, 0), (12, 0))];
        assert_eq!(matched(&events, (10, 0), (10, 20)), None);
        assert_eq!(matched(&[], (10, 0), (10, 20)), None);
    }

    #[test]
    fn touching_or_barely_overlapping_events_are_not_the_meeting() {
        let events = [event("previous", (9, 0), (10, 0)), event("this one", (10, 0), (11, 0))];
        // Recording starts 30 seconds before the previous event ends.
        let start = at(9, 59) + chrono::Duration::seconds(30);
        assert_eq!(best_match(&events, start, at(10, 45)).unwrap().id, "this one");
        // Back to back, recorded across the boundary: the larger share wins.
        assert_eq!(matched(&events, (9, 55), (10, 40)), Some("this one"));
        assert_eq!(matched(&events, (9, 10), (10, 5)), Some("previous"));
        // Only a sliver of a long event, at the end of a long recording.
        let sliver = [event("all afternoon", (13, 0), (17, 0))];
        assert_eq!(matched(&sliver, (12, 0), (13, 4)), None);
    }

    #[test]
    fn early_starts_and_short_recordings_still_match() {
        let events = [event("weekly", (10, 0), (11, 0))];
        // Joined ten minutes early.
        assert_eq!(matched(&events, (9, 50), (10, 55)), Some("weekly"));
        // A recording cut short after two minutes, inside the event.
        assert_eq!(matched(&events, (10, 5), (10, 7)), Some("weekly"));
        // A thirty-second test recording inside the event.
        let start = at(10, 20);
        assert_eq!(best_match(&events, start, start + chrono::Duration::seconds(30)).unwrap().id, "weekly");
    }

    #[test]
    fn equal_overlap_goes_to_the_closer_start_then_to_the_call_link() {
        // Two events over the same hour (for example one in each of two calendars).
        let mut with_link = event("with a call link", (10, 0), (11, 0));
        with_link.meet_link = Some("https://meet.google.com/abc".into());
        let events = [event("no link", (10, 0), (11, 0)), with_link];
        assert_eq!(matched(&events, (10, 0), (11, 0)), Some("with a call link"));
        // Same overlap, different starts: the one that began with the recording.
        let events = [event("began earlier", (9, 0), (10, 30)), event("began with it", (10, 0), (11, 30))];
        assert_eq!(matched(&events, (10, 0), (10, 30)), Some("began with it"));
    }
}
