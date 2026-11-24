# Homebrew: Google Calendar (upstream PR #666, tested and rebuilt)

A recording made during a calendar event takes the event's title and the names
of the people invited. The calendar is only read. This note records what
upstream PR #666 does, why it was not ported, and how the module here works.

## PR #666 as written

`Zackriya-Solutions/meetily` PR #666 at commit `d371203` was open and
unreviewed, based on upstream 0.4.0. Its `calendar/client.rs` and
`calendar/matcher.rs` compile unmodified outside the app
(`tools/pr666-characterisation`), and its own two tests pass. Six added tests
state its behaviour. Result: sign-in and fetch are workable; matching and
parsing go wrong in common situations.

| # | Defect in the PR | Where | Shown by | Here |
| --- | --- | --- | --- | --- |
| 1 | The match window is +-2 h around one timestamp, and the overlap is scored against that window. A longer neighbouring event wins; an event that ended before the recording began can match. | `commands.rs:156-163`, `matcher.rs:14-19` | harness tests `defect_1_*` | `calendar/matcher.rs` scores the overlap with the time recorded, `[start, start + duration]`, and requires a minimum overlap |
| 2 | Only the primary calendar is read. | `client.rs:8` | harness test `defect_2_*` | Any calendars; chosen in Connections |
| 3 | Events without a video link are dropped; declined events are kept. | `client.rs:89-98` | reading | A link is optional; declined, cancelled, all-day and "out of office" entries are left out |
| 4 | One event the types cannot read fails the whole list; further pages are never requested. | `client.rs:36-53, 84` | harness tests `defect_4_*` | Each event is parsed on its own; `nextPageToken` is followed (bounded) |
| 5 | No `state`, no PKCE; a declined consent waits out a 3-minute timer; an idle socket can hang the listener. | `oauth.rs:30-97` | reading | The sign-in of `google/oauth.rs` (see HOMEBREW_CONNECTIONS.md) |
| 6 | No HTTP timeouts; the stop-and-save flow awaits Google. | `oauth.rs:107,147`, `client.rs:63` | reading | A trigger queues the lookup; a background worker performs it with bounded requests |
| 7 | A failed refresh is only logged; the status stays "connected". | `oauth.rs:155-160` | reading | "Needs attention" state, shown in Connections, on the home page and as a toast |
| 8 | The title is overwritten when it starts with `"Meeting "`. | `commands.rs:183` | reading | `update_generated_meeting_title`, whose SQL only touches titles the user has not set (`title_is_manual = 0`) |
| 9 | Endpoints are constants, so nothing can stand in for Google in a test. | `oauth.rs:17-18`, `client.rs:8` | harness test `defect_2_*` | `GoogleEndpoints` is a parameter; tests use a loopback server |

A straight port was also not possible: the PR adds fields to `MeetingModel`
and columns to `meetings`, which breaks four queries of this base that select
`*` into that model. The PR's Google Meet auto-detect half is macOS-only and
overlaps this base's own meeting detection; it is left out.

## Data flow here

1. Migration `20261124020000_add_calendar_links.sql` adds
   `meeting_calendar_links`. A trigger inserts a `pending` row for every new
   meeting. Saving a recording does nothing else and never waits for Google.
2. The background loop of `export/worker.rs` calls `calendar::worker::pass`
   before each export pass. A meeting is looked up once it has transcript
   lines.
3. `calendar/client.rs` lists the events of every chosen calendar for the
   recorded time plus 15 minutes on each side (GET requests only).
4. `calendar/matcher.rs` picks the event that best covers the time recorded,
   or none.
5. The result is stored. The event's title replaces an automatic meeting
   title. The invitees' names (not their addresses) are stored as JSON.

The names are used in three places: offered at "Who was on the call?"
(`calendar_match_meeting` looks the event up at that moment if needed), added
to the summary prompt as context with the rule that an invitee is not thereby
a speaker (`calendar::state::with_summary_context`), and used for the Drive
folder when no voice was named (`export/loader.rs`). A match that arrives
after the first export moves the files, because a trigger bumps the export
version.

## Interfaces

Commands (`calendar/commands.rs`): `calendar_options`, `calendar_save_options`,
`calendar_link`, `calendar_match_meeting`, `meeting_suggested_participants`.
Event `calendar-link-changed` (`{ meetingId }`). UI:
`components/connections/CalendarOptions.tsx` in the Google card.

## Tests

- `tools/pr666-characterisation`: the PR's code as published (8 tests).
- Native, `cargo test -p meetily --lib calendar::`: matcher cases (longer
  neighbour, nothing scheduled, back-to-back, early start, short recording,
  ties), client against a loopback server (paging, unreadable events, kept and
  dropped event kinds, invitee names, calendar list, error classes, a response
  that never ends, read-only guard), storage and queue, worker (title rule,
  several calendars, removed calendar, outage, ended sign-in, switched off,
  Drive folder after a late match), summary context.
- Frontend: calendar options in `tests/connections/google-card.test.tsx`,
  name suggestions in `tests/post-call/auto-chain.test.tsx`.

No test contacts Google. The first run against a real calendar is on the
user's PC, or here with a token from the user.

## Limits

- Invitees are the people invited who did not decline, not the people who
  attended.
- A name is the attendee's display name, or else the part of the address
  before the `@` with separators turned into spaces.
- Only meetings recorded after this version was installed are looked up on
  their own.
- The meeting page does not show the matched event beyond the title.
