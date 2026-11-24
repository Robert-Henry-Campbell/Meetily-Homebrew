//! What the Drive export knows about each meeting, and its settings.
//!
//! `drive_export_state` is written by SQLite triggers when a meeting's content
//! changes and by the worker when files are sent. The decision whether a
//! meeting is ready (`readiness`) is a pure function, so the rules can be read
//! and tested without a database or a clock.

use crate::integrations::config_store;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;

const KEY_AUTO_EXPORT: &str = "drive.auto_export";
const KEY_FOLDER_PER_PERSON: &str = "drive.folder_per_person";
const KEY_ROOT_FOLDER_NAME: &str = "drive.root_folder_name";
const KEY_ENABLED_SINCE: &str = "drive.enabled_since";
pub const KEY_USER_NAME: &str = "user.display_name";

pub const DEFAULT_ROOT_FOLDER_NAME: &str = "Meetily meetings";
/// A meeting is sent once nothing in it has changed for this long.
pub const QUIET_SECONDS: i64 = 30;
/// How long a finished transcript waits for its summary to start.
pub const SUMMARY_START_GRACE_SECONDS: i64 = 15 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSettings {
    /// Send meetings on their own once they are stable.
    pub auto_export: bool,
    /// One folder per person (and per group) under the root folder.
    pub folder_per_person: bool,
    /// The name given to the root folder when the app creates it.
    pub root_folder_name: String,
    /// Unix seconds. Only meetings recorded after this are sent on their own,
    /// so switching the export on never uploads the existing history.
    pub enabled_since: Option<i64>,
}

pub async fn settings(pool: &SqlitePool) -> Result<ExportSettings, sqlx::Error> {
    let flag = |value: Option<String>| value.map(|value| value != "0").unwrap_or(true);
    Ok(ExportSettings {
        auto_export: flag(config_store::get(pool, KEY_AUTO_EXPORT).await?),
        folder_per_person: flag(config_store::get(pool, KEY_FOLDER_PER_PERSON).await?),
        root_folder_name: config_store::get(pool, KEY_ROOT_FOLDER_NAME)
            .await?
            .map(|name| super::naming::safe_name(&name, 80))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| DEFAULT_ROOT_FOLDER_NAME.to_string()),
        enabled_since: config_store::get(pool, KEY_ENABLED_SINCE).await?.and_then(|value| value.parse().ok()),
    })
}

pub async fn save_settings(pool: &SqlitePool, auto_export: bool, folder_per_person: bool, root_folder_name: &str) -> Result<(), sqlx::Error> {
    let name = super::naming::safe_name(root_folder_name, 80);
    let name = if name.is_empty() { DEFAULT_ROOT_FOLDER_NAME.to_string() } else { name };
    config_store::set_many(pool, &[
        (KEY_AUTO_EXPORT, Some(if auto_export { "1" } else { "0" })),
        (KEY_FOLDER_PER_PERSON, Some(if folder_per_person { "1" } else { "0" })),
        (KEY_ROOT_FOLDER_NAME, Some(name.as_str())),
    ])
    .await
}

/// Records the moment from which meetings are sent on their own, once.
pub async fn mark_enabled_if_new(pool: &SqlitePool, now: i64) -> Result<i64, sqlx::Error> {
    if let Some(since) = config_store::get(pool, KEY_ENABLED_SINCE).await?.and_then(|value| value.parse().ok()) {
        return Ok(since);
    }
    config_store::set(pool, KEY_ENABLED_SINCE, &now.to_string()).await?;
    Ok(now)
}

/// A meeting whose files in Drive are missing or out of date.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub meeting_id: String,
    pub content_version: i64,
    pub exported_version: Option<i64>,
    pub requested: bool,
    pub changed_at: i64,
    pub recorded_at: DateTime<Utc>,
    /// Stage of the chain after Stop, if the meeting went through it.
    pub job_stage: Option<String>,
    /// `summary_processes.status`, lower case.
    pub summary_status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    /// Not yet; the text says what is being waited for.
    Wait(&'static str),
    /// The automatic export does not apply to this meeting.
    NotEligible,
}

/// Whether a meeting is sent now.
pub fn readiness(candidate: &Candidate, settings: &ExportSettings, now: i64) -> Readiness {
    // "Send now" is the user's decision; only the content itself can delay it.
    if candidate.requested {
        return Readiness::Ready;
    }
    if !settings.auto_export {
        return Readiness::NotEligible;
    }
    // History is not uploaded on its own. A meeting that was sent before stays in step.
    let recorded_before_export = settings.enabled_since.map_or(true, |since| candidate.recorded_at.timestamp() < since);
    if candidate.exported_version.is_none() && recorded_before_export {
        return Readiness::NotEligible;
    }
    match candidate.job_stage.as_deref() {
        Some("prompt") => return Readiness::Wait("Waiting for the speaker question to be answered."),
        Some("enhancing" | "diarizing") => return Readiness::Wait("The transcript is still being processed."),
        _ => {}
    }
    if candidate.summary_status.as_deref() == Some("pending") {
        return Readiness::Wait("The summary is being written.");
    }
    let quiet_for = now - candidate.changed_at;
    if candidate.job_stage.as_deref() == Some("summary") && quiet_for < SUMMARY_START_GRACE_SECONDS {
        return Readiness::Wait("Waiting for the summary to start.");
    }
    if quiet_for < QUIET_SECONDS {
        return Readiness::Wait("Waiting until the meeting has stopped changing.");
    }
    Readiness::Ready
}

type CandidateRow = (String, i64, Option<i64>, i64, i64, crate::database::models::DateTimeUtc, Option<String>, Option<String>);

/// Meetings whose Drive files are missing or stale and whose next attempt is due.
pub async fn candidates(pool: &SqlitePool, now: i64) -> Result<Vec<Candidate>, sqlx::Error> {
    let rows = sqlx::query_as::<_, CandidateRow>(
        "SELECT s.meeting_id, s.content_version, s.exported_version, s.requested, s.changed_at, m.created_at, \
                j.stage, lower(p.status) \
         FROM drive_export_state s \
         JOIN meetings m ON m.id = s.meeting_id \
         LEFT JOIN post_call_jobs j ON j.meeting_id = s.meeting_id \
         LEFT JOIN summary_processes p ON p.meeting_id = s.meeting_id \
         WHERE (s.exported_version IS NULL OR s.exported_version != s.content_version OR s.requested = 1) \
           AND s.next_attempt_at <= ? \
         ORDER BY s.requested DESC, s.changed_at ASC",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(meeting_id, content_version, exported_version, requested, changed_at, recorded_at, job_stage, summary_status)| Candidate {
            meeting_id,
            content_version,
            exported_version,
            requested: requested != 0,
            changed_at,
            recorded_at: recorded_at.0,
            job_stage,
            summary_status,
        })
        .collect())
}

/// Everything stored about one meeting's export.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct ExportRow {
    pub meeting_id: String,
    pub content_version: i64,
    pub exported_version: Option<i64>,
    pub changed_at: i64,
    pub requested: i64,
    pub status: String,
    pub last_error: Option<String>,
    pub attempts: i64,
    pub next_attempt_at: i64,
    pub folder_id: Option<String>,
    pub summary_file_id: Option<String>,
    pub transcript_file_id: Option<String>,
    pub summary_link: Option<String>,
    pub transcript_link: Option<String>,
    pub summary_hash: Option<String>,
    pub transcript_hash: Option<String>,
    pub exported_at: Option<i64>,
}

pub async fn get(pool: &SqlitePool, meeting_id: &str) -> Result<Option<ExportRow>, sqlx::Error> {
    sqlx::query_as::<_, ExportRow>("SELECT * FROM drive_export_state WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
}

/// "Send now": the meeting is exported on the worker's next pass whatever the
/// automatic rules say, and earlier failures no longer delay it.
pub async fn request_now(pool: &SqlitePool, meeting_id: &str, now: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO drive_export_state (meeting_id, changed_at, requested) \
         SELECT id, ?, 1 FROM meetings WHERE id = ? \
         ON CONFLICT(meeting_id) DO UPDATE SET requested = 1, attempts = 0, next_attempt_at = 0, \
             last_error = NULL, status = CASE WHEN status = 'sent' THEN 'sent' ELSE 'waiting' END",
    )
    .bind(now)
    .bind(meeting_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// One file's place in Drive after an export.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SentFile {
    pub id: Option<String>,
    pub link: Option<String>,
    pub hash: Option<String>,
}

/// The files in Drive now reflect `version`. If the content changed while the
/// upload ran, `content_version` is already higher and the meeting stays due.
pub async fn record_sent(
    pool: &SqlitePool,
    meeting_id: &str,
    version: i64,
    folder_id: Option<&str>,
    summary: &SentFile,
    transcript: &SentFile,
    now: i64,
) -> Result<(), sqlx::Error> {
    let anything = summary.id.is_some() || transcript.id.is_some();
    sqlx::query(
        "UPDATE drive_export_state SET exported_version = ?, requested = 0, status = ?, last_error = NULL, \
             attempts = 0, next_attempt_at = 0, folder_id = ?, \
             summary_file_id = ?, summary_link = ?, summary_hash = ?, \
             transcript_file_id = ?, transcript_link = ?, transcript_hash = ?, exported_at = ? \
         WHERE meeting_id = ?",
    )
    .bind(version)
    .bind(if anything { "sent" } else { "empty" })
    .bind(folder_id)
    .bind(&summary.id)
    .bind(&summary.link)
    .bind(&summary.hash)
    .bind(&transcript.id)
    .bind(&transcript.link)
    .bind(&transcript.hash)
    .bind(now)
    .bind(meeting_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Seconds until the next attempt after `attempts` failures in a row.
pub fn backoff_seconds(attempts: i64) -> i64 {
    match attempts {
        ..=1 => 30,
        2 => 2 * 60,
        3 => 10 * 60,
        4 => 30 * 60,
        _ => 60 * 60,
    }
}

/// The export did not complete. `status` is `waiting` for trouble that passes,
/// `attention` when the user has to act, `failed` otherwise. The request flag
/// is kept, so a "Send now" survives a failed attempt.
pub async fn record_failure(pool: &SqlitePool, meeting_id: &str, status: &str, message: &str, now: i64) -> Result<(), sqlx::Error> {
    // Read, then write: the worker is the only writer of these two columns.
    let attempts = sqlx::query_scalar::<_, i64>("SELECT attempts FROM drive_export_state WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await?
        .unwrap_or(0)
        + 1;
    sqlx::query("UPDATE drive_export_state SET status = ?, last_error = ?, attempts = ?, next_attempt_at = ? WHERE meeting_id = ?")
        .bind(status)
        .bind(message)
        .bind(attempts)
        .bind(now + backoff_seconds(attempts))
        .bind(meeting_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn stored_folder(pool: &SqlitePool, kind: &str, name_key: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT folder_id FROM drive_folders WHERE kind = ? AND name_key = ?")
        .bind(kind)
        .bind(name_key)
        .fetch_optional(pool)
        .await
}

pub async fn store_folder(pool: &SqlitePool, kind: &str, name_key: &str, folder_id: &str, now: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO drive_folders (kind, name_key, folder_id, updated_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(kind, name_key) DO UPDATE SET folder_id = excluded.folder_id, updated_at = excluded.updated_at",
    )
    .bind(kind)
    .bind(name_key)
    .bind(folder_id)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// True for a folder the app created. A file is moved only out of such a
/// folder: one the user filed elsewhere by hand stays where they put it.
pub async fn is_app_folder(pool: &SqlitePool, folder_id: &str) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM drive_folders WHERE folder_id = ?")
        .bind(folder_id)
        .fetch_one(pool)
        .await?
        > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::config_store::test_pool;

    fn candidate() -> Candidate {
        Candidate {
            meeting_id: "m".into(),
            content_version: 3,
            exported_version: None,
            requested: false,
            changed_at: 1_000,
            recorded_at: DateTime::from_timestamp(900, 0).unwrap(),
            job_stage: Some("done".into()),
            summary_status: Some("completed".into()),
        }
    }

    fn on() -> ExportSettings {
        ExportSettings { auto_export: true, folder_per_person: true, root_folder_name: DEFAULT_ROOT_FOLDER_NAME.into(), enabled_since: Some(500) }
    }

    #[test]
    fn a_finished_quiet_meeting_recorded_after_the_export_was_switched_on_is_sent() {
        assert_eq!(readiness(&candidate(), &on(), 1_000 + QUIET_SECONDS), Readiness::Ready);
        assert!(matches!(readiness(&candidate(), &on(), 1_000 + QUIET_SECONDS - 1), Readiness::Wait(_)));
    }

    #[test]
    fn the_chain_and_the_summary_are_waited_for() {
        let late = 1_000 + 10 * QUIET_SECONDS;
        for stage in ["prompt", "enhancing", "diarizing"] {
            let waiting = Candidate { job_stage: Some(stage.into()), ..candidate() };
            assert!(matches!(readiness(&waiting, &on(), late), Readiness::Wait(_)), "{stage}");
        }
        let writing = Candidate { summary_status: Some("pending".into()), ..candidate() };
        assert_eq!(readiness(&writing, &on(), late), Readiness::Wait("The summary is being written."));

        // A summary that has not started yet is waited for, but not for ever.
        let not_started = Candidate { job_stage: Some("summary".into()), summary_status: None, ..candidate() };
        assert!(matches!(readiness(&not_started, &on(), late), Readiness::Wait(_)));
        assert_eq!(readiness(&not_started, &on(), 1_000 + SUMMARY_START_GRACE_SECONDS), Readiness::Ready);

        // No chain at all (an imported recording) and a failed summary are both final states.
        let imported = Candidate { job_stage: None, summary_status: None, ..candidate() };
        assert_eq!(readiness(&imported, &on(), late), Readiness::Ready);
        let failed = Candidate { summary_status: Some("failed".into()), ..candidate() };
        assert_eq!(readiness(&failed, &on(), late), Readiness::Ready);
    }

    #[test]
    fn the_existing_history_is_not_uploaded_unless_asked() {
        let late = 5_000;
        let old = Candidate { recorded_at: DateTime::from_timestamp(100, 0).unwrap(), ..candidate() };
        assert_eq!(readiness(&old, &on(), late), Readiness::NotEligible);
        // Never switched on: nothing is automatic.
        assert_eq!(readiness(&candidate(), &ExportSettings { enabled_since: None, ..on() }, late), Readiness::NotEligible);
        assert_eq!(readiness(&candidate(), &ExportSettings { auto_export: false, ..on() }, late), Readiness::NotEligible);
        // An old meeting that was sent once stays in step with its changes.
        assert_eq!(readiness(&Candidate { exported_version: Some(1), ..old.clone() }, &on(), late), Readiness::Ready);
        // "Send now" overrides the rules, the waiting and the switch.
        let asked = Candidate { requested: true, job_stage: Some("prompt".into()), ..old };
        assert_eq!(readiness(&asked, &ExportSettings { auto_export: false, ..on() }, 1_000), Readiness::Ready);
    }

    async fn add_meeting(pool: &SqlitePool, id: &str) {
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES (?, 'Call', ?, ?)")
            .bind(id)
            .bind("2026-11-23T10:00:00+00:00")
            .bind("2026-11-23T10:30:00+00:00")
            .execute(pool)
            .await
            .unwrap();
    }

    async fn add_line(pool: &SqlitePool, id: &str, meeting: &str) {
        sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, speaker) VALUES (?, ?, 'hello', '10:00', 1.0, 'You')")
            .bind(id)
            .bind(meeting)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn version(pool: &SqlitePool, meeting: &str) -> Option<i64> {
        get(pool, meeting).await.unwrap().map(|row| row.content_version)
    }

    #[tokio::test]
    async fn every_change_to_exported_content_bumps_the_version() {
        let (_directory, manager) = test_pool().await;
        let pool = manager.pool();
        add_meeting(pool, "m").await;
        // A meeting without content has no row: there is nothing to send.
        assert_eq!(version(pool, "m").await, None);

        add_line(pool, "t1", "m").await;
        assert_eq!(version(pool, "m").await, Some(1));
        let mut last = 1;
        let mut expect_bump = |now: Option<i64>, what: &str| {
            let now = now.unwrap();
            assert!(now > last, "{what} did not bump the version");
            last = now;
        };

        sqlx::query("UPDATE transcripts SET speaker = 'Alice' WHERE id = 't1'").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "a speaker rename");
        sqlx::query("UPDATE meetings SET title = 'Budget' WHERE id = 'm'").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "a title change");
        sqlx::query("INSERT INTO summary_processes (meeting_id, status, created_at, updated_at) VALUES ('m', 'PENDING', 'x', 'x')").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "a summary start");
        sqlx::query("UPDATE summary_processes SET status = 'completed', result = '{\"markdown\":\"x\"}' WHERE meeting_id = 'm'").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "a finished summary");
        sqlx::query("INSERT INTO meeting_notes (meeting_id, notes_markdown, created_at, updated_at) VALUES ('m', 'note', 'x', 'x')").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "new notes");
        sqlx::query("UPDATE meeting_notes SET notes_markdown = 'changed' WHERE meeting_id = 'm'").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "edited notes");
        sqlx::query("INSERT INTO action_items (id, meeting_id, text, created_at, updated_at) VALUES ('a1', 'm', 'Do it', 'x', 'x')").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "a new action item");
        sqlx::query("UPDATE action_items SET done = 1 WHERE id = 'a1'").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "a ticked action item");
        sqlx::query("DELETE FROM action_items WHERE id = 'a1'").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "a removed action item");
        crate::post_call::PostCallRepository::set_participants(pool, "m", &["Alice".into()]).await.unwrap();
        expect_bump(version(pool, "m").await, "a typed name");
        let job = |stage: &str| crate::post_call::PostCallJobUpdate { meeting_id: "m".into(), stage: stage.into(), ..Default::default() };
        crate::post_call::PostCallRepository::save(pool, &job("enhancing")).await.unwrap();
        crate::post_call::PostCallRepository::save(pool, &job("done")).await.unwrap();
        expect_bump(version(pool, "m").await, "a chain stage change");
        sqlx::query("DELETE FROM transcripts WHERE id = 't1'").execute(pool).await.unwrap();
        expect_bump(version(pool, "m").await, "a removed line");

        // Bookkeeping that is not content leaves the version alone.
        let before = version(pool, "m").await;
        sqlx::query("UPDATE meetings SET updated_at = 'later' WHERE id = 'm'").execute(pool).await.unwrap();
        assert_eq!(version(pool, "m").await, before);

        // Deleting the meeting removes the bookkeeping.
        sqlx::query("DELETE FROM meetings WHERE id = 'm'").execute(pool).await.unwrap();
        assert_eq!(get(pool, "m").await.unwrap(), None);
    }

    #[tokio::test]
    async fn sent_failed_and_requested_states_are_stored() {
        let (_directory, manager) = test_pool().await;
        let pool = manager.pool();
        add_meeting(pool, "m").await;
        add_line(pool, "t1", "m").await;
        let due = candidates(pool, i64::MAX).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].recorded_at.to_rfc3339(), "2026-11-23T10:00:00+00:00");

        record_failure(pool, "m", "waiting", "Google Drive could not be reached.", 1_000).await.unwrap();
        let row = get(pool, "m").await.unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.attempts, row.next_attempt_at), ("waiting", 1, 1_030));
        assert!(candidates(pool, 1_029).await.unwrap().is_empty());
        assert_eq!(candidates(pool, 1_030).await.unwrap().len(), 1);
        record_failure(pool, "m", "failed", "x", 2_000).await.unwrap();
        assert_eq!(get(pool, "m").await.unwrap().unwrap().next_attempt_at, 2_120);
        for (attempts, seconds) in [(0, 30), (1, 30), (2, 120), (3, 600), (4, 1800), (5, 3600), (50, 3600)] {
            assert_eq!(backoff_seconds(attempts), seconds);
        }

        // "Send now" clears the delay.
        request_now(pool, "m", 2_001).await.unwrap();
        let asked = candidates(pool, 2_001).await.unwrap();
        assert!(asked[0].requested);

        let file = |id: &str| SentFile { id: Some(id.into()), link: Some(format!("https://drive/{id}")), hash: Some("h".into()) };
        record_sent(pool, "m", 1, Some("folder"), &file("s"), &file("t"), 3_000).await.unwrap();
        let row = get(pool, "m").await.unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.exported_version, row.requested, row.attempts), ("sent", Some(1), 0, 0));
        assert_eq!(row.summary_link.as_deref(), Some("https://drive/s"));
        assert!(candidates(pool, i64::MAX).await.unwrap().is_empty());

        // A change after the export makes the meeting due again.
        sqlx::query("UPDATE transcripts SET transcript = 'edited' WHERE id = 't1'").execute(pool).await.unwrap();
        assert_eq!(candidates(pool, i64::MAX).await.unwrap()[0].exported_version, Some(1));

        // Nothing to send is its own state, not "sent".
        record_sent(pool, "m", 2, None, &SentFile::default(), &SentFile::default(), 4_000).await.unwrap();
        assert_eq!(get(pool, "m").await.unwrap().unwrap().status, "empty");

        // "Send now" for a meeting that never changed creates its row; an unknown meeting does not.
        add_meeting(pool, "old").await;
        request_now(pool, "old", 5_000).await.unwrap();
        assert!(get(pool, "old").await.unwrap().unwrap().requested == 1);
        request_now(pool, "missing", 5_000).await.unwrap();
        assert_eq!(get(pool, "missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn settings_default_to_automatic_with_folders_and_folder_ids_are_remembered() {
        let (_directory, manager) = test_pool().await;
        let pool = manager.pool();
        assert_eq!(settings(pool).await.unwrap(), ExportSettings {
            auto_export: true, folder_per_person: true, root_folder_name: "Meetily meetings".into(), enabled_since: None,
        });
        save_settings(pool, false, false, "  My: meetings/  ").await.unwrap();
        assert_eq!(mark_enabled_if_new(pool, 700).await.unwrap(), 700);
        // The first moment stays: switching off and on again does not reach back.
        assert_eq!(mark_enabled_if_new(pool, 900).await.unwrap(), 700);
        assert_eq!(settings(pool).await.unwrap(), ExportSettings {
            auto_export: false, folder_per_person: false, root_folder_name: "My meetings".into(), enabled_since: Some(700),
        });
        save_settings(pool, true, true, "///").await.unwrap();
        assert_eq!(settings(pool).await.unwrap().root_folder_name, "Meetily meetings");

        assert_eq!(stored_folder(pool, "person", "k").await.unwrap(), None);
        store_folder(pool, "person", "k", "f1", 1).await.unwrap();
        store_folder(pool, "person", "k", "f2", 2).await.unwrap();
        assert_eq!(stored_folder(pool, "person", "k").await.unwrap().as_deref(), Some("f2"));
        assert!(is_app_folder(pool, "f2").await.unwrap());
        assert!(!is_app_folder(pool, "f1").await.unwrap());
    }
}
