//! Characterisation of upstream PR #666 (Zackriya-Solutions/meetily @ d371203).
//! `client.rs` and `matcher.rs` are compiled as fetched, byte for byte. The
//! tests below state what that code does, including where it goes wrong.

pub mod calendar {
    include!(concat!(env!("OUT_DIR"), "/types.rs"));

    pub mod matcher {
        include!("../pr666/matcher.rs");

        #[cfg(test)]
        mod characterisation {
            use super::*;
            use crate::calendar::CalendarEvent;
            use chrono::{Duration, TimeZone};

            fn event(id: &str, start: (u32, u32), end: (u32, u32)) -> CalendarEvent {
                CalendarEvent {
                    id: id.into(),
                    title: id.into(),
                    start_time: Utc.with_ymd_and_hms(2026, 1, 1, start.0, start.1, 0).unwrap(),
                    end_time: Utc.with_ymd_and_hms(2026, 1, 1, end.0, end.1, 0).unwrap(),
                    meet_link: None,
                    attendees: vec![],
                }
            }

            /// commands.rs:156-163 passes the same +-2 h window to the fetch and
            /// to the matcher, centred on `created_at`.
            fn match_as_the_pr_does(events: &[CalendarEvent], created_at: DateTime<Utc>) -> Option<&CalendarEvent> {
                let window_start = created_at - Duration::hours(2);
                let window_end = created_at + Duration::hours(2);
                find_matching_event(events, window_start, window_end)
            }

            #[test]
            fn defect_1_a_longer_neighbouring_event_wins_over_the_recorded_one() {
                // Recorded 10:00-10:30. A longer meeting follows at 11:00.
                let events = vec![event("the recorded call", (10, 0), (10, 30)), event("later planning", (11, 0), (12, 30))];
                // Upstream stores the save time (10:30) in created_at ...
                let upstream = match_as_the_pr_does(&events, Utc.with_ymd_and_hms(2026, 1, 1, 10, 30, 0).unwrap()).unwrap();
                assert_eq!(upstream.id, "later planning");
                // ... the base stores the recording start (10:00). Same wrong answer.
                let base = match_as_the_pr_does(&events, Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap()).unwrap();
                assert_eq!(base.id, "later planning");
            }

            #[test]
            fn defect_1_an_event_that_ended_before_the_recording_began_can_still_match() {
                // Recorded 10:00-10:20 with nothing scheduled. An earlier meeting ended at 09:30.
                let events = vec![event("earlier meeting", (8, 30), (9, 30))];
                let matched = match_as_the_pr_does(&events, Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap());
                assert_eq!(matched.unwrap().id, "earlier meeting");
            }
        }
    }

    pub mod client {
        include!("../pr666/client.rs");

        #[cfg(test)]
        mod characterisation {
            use super::*;

            #[test]
            fn defect_2_only_the_primary_calendar_is_read_and_the_address_is_fixed() {
                assert_eq!(EVENTS_ENDPOINT, "https://www.googleapis.com/calendar/v3/calendars/primary/events");
            }

            #[test]
            fn defect_4_one_unexpected_event_fails_the_whole_list() {
                let ordinary = serde_json::json!({
                    "id": "a", "summary": "Sync",
                    "start": { "dateTime": "2026-01-01T10:00:00Z" }, "end": { "dateTime": "2026-01-01T10:30:00Z" },
                    "attendees": [{ "email": "alice@example.com", "displayName": "Alice" }]
                });
                assert!(serde_json::from_value::<EventsListResponse>(serde_json::json!({ "items": [ordinary.clone()] })).is_ok());
                // An attendee entry without an address (the field is optional in Google's schema).
                let odd = serde_json::json!({
                    "id": "b", "start": { "dateTime": "2026-01-01T11:00:00Z" }, "end": { "dateTime": "2026-01-01T12:00:00Z" },
                    "attendees": [{ "displayName": "Room 4", "resource": true }]
                });
                let parsed = serde_json::from_value::<EventsListResponse>(serde_json::json!({ "items": [ordinary, odd] }));
                assert!(parsed.is_err(), "the ordinary event is lost with the odd one");
            }

            #[test]
            fn defect_4_further_pages_are_never_requested() {
                let parsed: EventsListResponse = serde_json::from_value(serde_json::json!({ "items": [], "nextPageToken": "page-2" })).unwrap();
                // The response type has nowhere to keep the token.
                assert!(!format!("{parsed:?}").contains("page-2"));
            }

            #[test]
            fn all_day_events_parse_and_have_no_time() {
                let parsed: EventsListResponse = serde_json::from_value(serde_json::json!({ "items": [{
                    "id": "c", "start": { "date": "2026-01-01" }, "end": { "date": "2026-01-02" }
                }] })).unwrap();
                assert!(parsed.items[0].start.date_time.is_none());
            }
        }
    }
}
