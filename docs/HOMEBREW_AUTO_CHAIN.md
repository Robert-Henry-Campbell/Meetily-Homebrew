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

## Claude summaries and the local takeover

`src-tauri/src/summary/claude_api.rs` holds the Claude request (raw HTTP; there
is no official Rust SDK). `llm_client::generate_summary` calls it for the
`claude` provider.

- API version `2026-10-21`, streaming, no sampling parameters, no `thinking`
  field. Text is read from the stream; thinking is never part of a summary.
- The default model is `claude-opus-5-5`. The default output budget in
  `lib/claude-output-limits.json` is 32,000 tokens, because thinking counts
  toward the limit on current models.
- Refusal fallback is enabled: the request carries `fallbacks: "default"` with
  the beta header `server-side-fallback-2026-07-01`, so a request that a safety
  classifier declines is re-run by the API on the model Anthropic recommends.
  A model or account that rejects the beta gets the same request without it.
- `stop_reason` decides whether a response is complete. `max_tokens`,
  `refusal` and unknown reasons never become a stored summary.
- Rate limits, 5xx and network failures repeat up to three attempts and honour
  `retry-after` up to 60 seconds. A request has no fixed 60 or 300 second
  limit; a stream that is silent for 180 seconds counts as cut off.
- Failures are typed (`ClaudeFailureKind`). The kind reaches
  `summary/service.rs` through the task-local `LAST_CLAUDE_FAILURE`, because
  the pipeline passes errors as text.

When Claude fails for a lasting reason (no key or rejected key, billing,
permission, unknown model, spend limit, rate limit or outage after the
retries, unreachable, declined), `service.rs` writes the summary with the best
completely downloaded local model (`models::first_downloaded_model`). The
stored result carries `generated_by` (`provider`, `model`, `fallback_reason`);
`MeetingDocument.tsx` shows the reason above the summary. A wrong request, a
truncated answer and a cancellation are reported and not taken over.

## Tests

- Native: `cargo test -p meetily --lib post_call::` (stored stages, names,
  delete trigger, labels) and `database::manager` (migration LF guard).
- Native: `summary::claude_api` (request shape, stream parsing with split
  multi-byte text, refusal continuation, error classes, retries, silence,
  cancellation) against a loopback fake server; `summary::service` (author
  field, takeover classes); `summary_engine::models` (takeover model choice).
- Frontend: `tests/post-call/auto-chain.test.tsx`,
  `tests/post-call/summary-start.test.ts`, `tests/lib/post-call-jobs.test.ts`,
  `tests/hooks/summary-external-start.test.tsx`, and the updated
  `tests/post-call-jobs.test.tsx`.

All of these use mocked IPC, a temporary database or a loopback fake server.
None runs a model, reads audio, or contacts Anthropic. The takeover path in
`service.rs` as a whole (Claude failure followed by a real local generation)
is not covered by an automated test.

## Limits

- The chain is driven by the webview. It resumes after a quit only when the
  app is started again.
- The unattended summary uses the `standard_meeting` template; the template
  picker on the meeting page is not stored.
- `large-v3` on integrated graphics can take as long as the recording.
