# Homebrew: Google Drive export

Each meeting's summary and transcript are sent to the user's Google Drive as
two Markdown files, without a click, once the meeting is final. This note
records the data flow, the rules, the interfaces, the tests, and the limits.

## Data flow

1. SQLite triggers (migration `20261124010000_add_drive_export.sql`) bump
   `drive_export_state.content_version` whenever something the files contain
   changes: transcript lines, title, group, summary, notes, action items,
   typed names, the chain's stage and note.
2. `export/worker.rs` runs for the life of the app (started in `lib.rs` after
   the database). Every 15 seconds, or when woken, it lists meetings whose
   exported version differs from the content version (`state::candidates`).
3. `state::readiness` decides: the chain after Stop must be past labelling,
   no summary may be running, and nothing may have changed for 30 seconds. A
   summary that has not started is waited for up to 15 minutes.
4. `export/loader.rs` reads the meeting from the database;
   `export/markdown.rs` renders the files; `export/naming.rs` chooses folder
   and file names; `google/drive.rs` creates or updates the files.
5. The result is stored in `drive_export_state` and announced with the event
   `drive-export-changed` (`{ meetingId }`).

The webview takes no part. The export also runs with the meeting page closed
and continues after a restart, because the queue is the database.

## Files

`YYYY-MM-DD HHmm <Title> - summary.md` and `... - transcript.md`, local time.
An automatic title ("Meeting · Mon, Nov 23 · ...") becomes "Meeting". Front
matter: title, meeting ID, date, time, attendees, `summary_model` (when
recorded), tag `meetily`. The transcript lists `[mm:ss] **Speaker:** text`.
The summary file also holds action items and the user's notes. A summary the
local model wrote in Claude's place says so; a transcript whose accurate pass
failed says so.

## Folders

Under one root folder ("Meetily meetings", name configurable):

- exactly one other person known: `<root>/<person>/`;
- several people and the meeting is in a group: `<root>/<group>/`;
- several people, no group: `<root>/Group meetings/`, names in the file title;
- nobody known: `<root>/`, or the group folder if the meeting has a group.

People are taken, in this order, from the names typed at the speaker
question, named voices in the transcript, and calendar invitees. "You" and
unnamed voices ("Speaker 2", "Guest") are nobody. With "A folder per person"
off, everything goes into the root folder with names in the file title.

## What the export does and does not do in Drive

- The sign-in asks for `drive.file`: the app sees only objects it created.
- Each object carries private `appProperties` (`meetilyKind`,
  `meetilyMeeting`, `meetilyName`). Before anything is created, the stored ID
  and then these tags are looked up, so a retry after a lost response adopts
  the existing object.
- A later change (title, speaker name, edited summary) updates the same file
  in place; the file keeps its ID and link. If the person changes, the file is
  moved between the app's own folders.
- A file the user moved elsewhere stays there and is updated in place. An
  object in the trash is treated as gone and is not taken back out.
- There is no delete and no trash call (`drive.rs` has a guard test).
  Deleting a meeting in the app leaves its files in Drive.

## Which meetings are sent

- Meetings recorded after the export first became usable
  (`drive.enabled_since`) are sent on their own. The existing history is not.
- "Send to Drive" on a meeting sends it regardless; it then stays in step.
- With "Send meetings to Drive automatically" off, only "Send now" sends.

## Failures

`DriveError` separates trouble that passes (network, 5xx, rate limits: retried
after 30 s, 2 min, 10 min, 30 min, then hourly) from trouble that needs the
user (API not enabled, Drive full, sign-in ended: state `attention`). A 401
forces one token refresh; a second 401 marks the connection as needing
attention. While it does, nothing is sent and Google is not contacted.

## Interfaces

Commands (`export/commands.rs`): `drive_export_status`,
`drive_export_send_now`, `drive_export_settings`,
`drive_export_save_settings`, `set_user_display_name`. Webview wrappers:
`lib/drive-export.ts`. UI: `components/meeting/DriveChip.tsx` in the meeting
header, `components/connections/DriveExportOptions.tsx` in the Google card.
The user's display name lived only in webview storage; it is now copied to
`integration_config` so files written in the background can use it.

## Tests

- Native, `cargo test -p meetily --lib export::`: golden files for both
  Markdown files, naming and folder rules, trigger coverage for every content
  change, readiness rules, and the worker against an in-memory Drive behind a
  loopback server (first export, no-op second pass, in-place update, move and
  rename, lost response, restart without stored IDs, user-moved and trashed
  files, outage, missing permission, rejected token, history, no deletes).
- Native: `google::drive` (request shapes, error classes),
  `summary::stored_markdown` (the three stored summary formats).
- Frontend: `tests/connections/drive-chip.test.tsx`.

No test contacts Google. A run against the real Drive API needs a token from
the user and is recorded separately when it has happened.

## Limits

- Files over 5 MB are not sent (one multipart upload).
- Drive's search can lag behind a create by a moment; the retry delay of at
  least 30 seconds covers that, but a duplicate after a lost response is
  possible in principle.
- A file the user deleted from Drive is written again when the meeting next
  changes.
- Changing the display name does not rewrite files already sent.
