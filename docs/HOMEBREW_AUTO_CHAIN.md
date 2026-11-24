# Homebrew: the automatic chain after Stop

This fork runs everything after Stop without further clicks: the speaker
question, the accurate re-transcription, speaker labelling, and the summary.
This note records the data flow, the interfaces, the tests, and the limits.

## Data flow

1. Stop saves the meeting and opens its page (unchanged).
2. `PostCallJobsContext.tsx` owns one job per meeting above navigation and
   mounts `PostCallProcessingWorker.tsx` for the first queued job.
3. The worker asks "How many people spoke?" and, optionally, "Who was on the
   call?". This is the only step that waits for the user.
4. The worker runs the chain: `start_retranscription_command` (events
   `retranscription-progress`, `-complete`, `-error`), then `diarize_meeting`.
   A failed step is retried once. If it fails again the chain continues: a
   failed re-transcription keeps the live transcript, failed labelling keeps
   the source labels. The reason is stored and shown on the meeting
   (`PostCallNotice.tsx`, with "Run again"). Only Cancel returns to a manual
   choice.
5. When the worker completes, the job owner (not the meeting page) calls
   `startPostCallSummary` in `lib/post-call-summary.ts`, which sends
   `api_process_transcript`. `useSummaryGeneration` on an open meeting page
   reattaches through the window event `meetily:summary-started`.

## Stored progress

Migration `20261123000000_add_post_call_jobs.sql` adds two tables and one
trigger. No existing table changes shape.

- `post_call_jobs`: one row per meeting with `stage` (`prompt`, `enhancing`,
  `diarizing`, `summary`, `done`), the answer to the speaker question, whether
  the live transcript was kept, and a note.
- `meeting_participants`: the names typed at the question, in order.
- Trigger `post_call_rows_follow_meeting` removes both when a meeting is deleted.

Native commands in `src-tauri/src/post_call.rs`: `post_call_job_save`,
`post_call_job_get`, `post_call_jobs_unfinished`, `meeting_participants_set`,
`meeting_participants_get`, `meeting_speaker_labels`. The webview wrappers are
in `lib/post-call-jobs.ts`.

On launch the job owner reads `post_call_jobs_unfinished` after a short delay.
A job at `prompt` asks the question again and names the meeting. A job at
`enhancing` or `diarizing` continues with the stored answer. A job at
`summary` only starts the summary. A native re-transcription that is still
running after a webview reload is adopted, not started twice.

## Model for the accurate pass

- `optional_models::WHISPER_MODEL` and `OPTIONAL_WHISPER_MODEL` are `large-v3`.
  Setup offers it ticked by default; native activation makes it the post-call
  model when the download completes.
- A configured post-call model is used exactly. While it downloads, the chain
  waits. If it is absent, the step fails with the reason and the live
  transcript is kept. The earlier behaviour (first downloaded model of the same
  provider) is removed for an explicit post-call setting.
- With the post-call setting on "live", a recording that ends while `large-v3`
  is still downloading waits for that download.
- The fixed 30-minute limit is replaced by a no-progress limit
  (`postCallTimings.stallMs`, 20 minutes). Native progress arrives before each
  segment of at most 25 seconds of audio.

## Names

One typed name and exactly one unnamed remote voice ("Speaker N" or "Guest")
after labelling: that voice is renamed through the existing
`rename_meeting_speaker`. Every other case is left to the user; a model's
speaker number is not an identity.

## Tests

- Native: `cargo test -p meetily --lib post_call::` (stored stages, names,
  delete trigger, labels) and `database::manager` (migration LF guard).
- Frontend: `tests/post-call/auto-chain.test.tsx`,
  `tests/post-call/summary-start.test.ts`, `tests/lib/post-call-jobs.test.ts`,
  `tests/hooks/summary-external-start.test.tsx`, and the updated
  `tests/post-call-jobs.test.tsx`.

All of these use mocked IPC or a temporary database. None runs a model or
reads audio.

## Limits

- The chain is driven by the webview. It resumes after a quit only when the
  app is started again.
- The unattended summary uses the `standard_meeting` template; the template
  picker on the meeting page is not stored.
- `large-v3` on integrated graphics can take as long as the recording.
