//! Durable record of the chain that runs after Stop.
//!
//! The steps themselves (re-transcription, speaker labelling, summary) are
//! started from the webview, which can be reloaded or closed while native work
//! is still running. A JS promise is therefore not durable ownership of "this
//! meeting still needs processing". Each stage change is stored here, and the
//! webview asks for unfinished jobs on launch and resumes them.
//!
//! Stages: `prompt` (speaker question not answered) -> `enhancing` ->
//! `diarizing` -> `summary` (transcript and speakers are final, summary not
//! started) -> `done`.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

const STAGES: [&str; 5] = ["prompt", "enhancing", "diarizing", "summary", "done"];
const MAX_PARTICIPANTS: usize = 20;
const MAX_NAME_CHARS: usize = 120;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostCallJob {
    pub meeting_id: String,
    pub folder_path: Option<String>,
    pub stage: String,
    pub speaker_choice_made: bool,
    /// Total speakers including the user. `None` means auto-detect.
    pub speaker_count: Option<i64>,
    pub used_live_transcript: bool,
    pub note: Option<String>,
    /// For the question shown again after a restart, which is not on the meeting's page.
    pub meeting_title: Option<String>,
}

/// Fields left out keep their stored value. `speaker_count` is written only
/// together with `speaker_choice_made = true`, so that an absent count cannot
/// be confused with the deliberate "auto-detect" choice (`null`). An empty
/// `note` clears the stored note (a run that succeeds after an earlier failure).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostCallJobUpdate {
    pub meeting_id: String,
    pub stage: String,
    #[serde(default)]
    pub folder_path: Option<String>,
    #[serde(default)]
    pub speaker_choice_made: Option<bool>,
    #[serde(default)]
    pub speaker_count: Option<i64>,
    #[serde(default)]
    pub used_live_transcript: Option<bool>,
    #[serde(default)]
    pub note: Option<String>,
}

type JobRow = (String, Option<String>, String, i64, Option<i64>, i64, Option<String>, Option<String>);

fn job_from_row(row: JobRow) -> PostCallJob {
    let (meeting_id, folder_path, stage, speaker_choice_made, speaker_count, used_live_transcript, note, meeting_title) = row;
    PostCallJob {
        meeting_id,
        folder_path,
        stage,
        speaker_choice_made: speaker_choice_made != 0,
        speaker_count,
        used_live_transcript: used_live_transcript != 0,
        note,
        meeting_title,
    }
}

const JOB_COLUMNS: &str =
    "meeting_id, folder_path, stage, speaker_choice_made, speaker_count, used_live_transcript, note, \
     (SELECT title FROM meetings WHERE meetings.id = post_call_jobs.meeting_id) AS meeting_title";

pub struct PostCallRepository;

impl PostCallRepository {
    pub async fn get(pool: &SqlitePool, meeting_id: &str) -> Result<Option<PostCallJob>, sqlx::Error> {
        let row = sqlx::query_as::<_, JobRow>(&format!(
            "SELECT {JOB_COLUMNS} FROM post_call_jobs WHERE meeting_id = ?"
        ))
        .bind(meeting_id)
        .fetch_optional(pool)
        .await?;
        Ok(row.map(job_from_row))
    }

    /// Jobs to resume, oldest first. Rows whose meeting no longer exists are
    /// ignored (the delete trigger normally removes them).
    pub async fn unfinished(pool: &SqlitePool) -> Result<Vec<PostCallJob>, sqlx::Error> {
        let rows = sqlx::query_as::<_, JobRow>(&format!(
            "SELECT {JOB_COLUMNS} FROM post_call_jobs \
             WHERE stage != 'done' AND meeting_id IN (SELECT id FROM meetings) \
             ORDER BY created_at ASC, meeting_id ASC"
        ))
        .fetch_all(pool)
        .await?;
        Ok(rows.into_iter().map(job_from_row).collect())
    }

    pub async fn save(pool: &SqlitePool, update: &PostCallJobUpdate) -> Result<PostCallJob, String> {
        if !STAGES.contains(&update.stage.as_str()) {
            return Err(format!("Unknown post-call stage: {}", update.stage));
        }
        let meeting_exists: Option<String> = sqlx::query_scalar("SELECT id FROM meetings WHERE id = ?")
            .bind(&update.meeting_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| error.to_string())?;
        if meeting_exists.is_none() {
            return Err("Meeting not found".to_string());
        }

        let now = chrono::Utc::now().timestamp();
        let choice_made = update.speaker_choice_made;
        // One statement, so a concurrent save for the same meeting cannot
        // interleave a read with this write.
        sqlx::query(
            "INSERT INTO post_call_jobs \
                 (meeting_id, folder_path, stage, speaker_choice_made, speaker_count, \
                  used_live_transcript, note, created_at, updated_at) \
             VALUES (?1, ?2, ?3, COALESCE(?4, 0), CASE WHEN ?4 = 1 THEN ?5 END, COALESCE(?6, 0), ?7, ?8, ?8) \
             ON CONFLICT(meeting_id) DO UPDATE SET \
                 folder_path = COALESCE(excluded.folder_path, post_call_jobs.folder_path), \
                 stage = excluded.stage, \
                 speaker_choice_made = COALESCE(?4, post_call_jobs.speaker_choice_made), \
                 speaker_count = CASE WHEN ?4 = 1 THEN ?5 ELSE post_call_jobs.speaker_count END, \
                 used_live_transcript = COALESCE(?6, post_call_jobs.used_live_transcript), \
                 note = CASE WHEN ?9 = 1 THEN ?7 ELSE post_call_jobs.note END, \
                 updated_at = ?8",
        )
        .bind(&update.meeting_id)
        .bind(update.folder_path.as_deref().filter(|path| !path.trim().is_empty()))
        .bind(&update.stage)
        .bind(choice_made.map(i64::from))
        .bind(update.speaker_count)
        .bind(update.used_live_transcript.map(i64::from))
        .bind(update.note.as_deref().map(str::trim).filter(|note| !note.is_empty()))
        .bind(now)
        .bind(i64::from(update.note.is_some()))
        .execute(pool)
        .await
        .map_err(|error| error.to_string())?;

        Self::get(pool, &update.meeting_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Post-call job was not stored".to_string())
    }

    pub async fn participants(pool: &SqlitePool, meeting_id: &str) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar::<_, String>(
            "SELECT name FROM meeting_participants WHERE meeting_id = ? ORDER BY position ASC",
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await
    }

    /// Replaces the list. Names are trimmed, de-duplicated without regard to
    /// case, and kept in the order given.
    pub async fn set_participants(
        pool: &SqlitePool,
        meeting_id: &str,
        names: &[String],
    ) -> Result<Vec<String>, String> {
        let cleaned = clean_participant_names(names);
        let mut transaction = pool.begin().await.map_err(|error| error.to_string())?;
        let meeting_exists: Option<String> = sqlx::query_scalar("SELECT id FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
        if meeting_exists.is_none() {
            return Err("Meeting not found".to_string());
        }
        sqlx::query("DELETE FROM meeting_participants WHERE meeting_id = ?")
            .bind(meeting_id)
            .execute(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
        let now = chrono::Utc::now().timestamp();
        for (position, name) in cleaned.iter().enumerate() {
            sqlx::query(
                "INSERT INTO meeting_participants (meeting_id, position, name, created_at) VALUES (?, ?, ?, ?)",
            )
            .bind(meeting_id)
            .bind(position as i64)
            .bind(name)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
        }
        transaction.commit().await.map_err(|error| error.to_string())?;
        Ok(cleaned)
    }

    /// Distinct labels as stored, including combined ones such as "You + Alice".
    pub async fn speaker_labels(pool: &SqlitePool, meeting_id: &str) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar::<_, String>(
            "SELECT speaker FROM transcripts \
             WHERE meeting_id = ? AND speaker IS NOT NULL AND trim(speaker) != '' \
             GROUP BY speaker ORDER BY MIN(COALESCE(audio_start_time, 1e30)), speaker",
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await
    }
}

fn clean_participant_names(names: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut cleaned = Vec::new();
    for raw in names {
        let name: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() {
            continue;
        }
        let name: String = name.chars().take(MAX_NAME_CHARS).collect();
        if seen.insert(name.to_lowercase()) {
            cleaned.push(name);
        }
        if cleaned.len() == MAX_PARTICIPANTS {
            break;
        }
    }
    cleaned
}

#[tauri::command]
pub async fn post_call_job_save(
    state: tauri::State<'_, crate::state::AppState>,
    update: PostCallJobUpdate,
) -> Result<PostCallJob, String> {
    PostCallRepository::save(state.db_manager.pool(), &update).await
}

#[tauri::command]
pub async fn post_call_job_get(
    state: tauri::State<'_, crate::state::AppState>,
    meeting_id: String,
) -> Result<Option<PostCallJob>, String> {
    PostCallRepository::get(state.db_manager.pool(), &meeting_id)
        .await
        .map_err(|error| format!("Failed to load post-call progress: {error}"))
}

#[tauri::command]
pub async fn post_call_jobs_unfinished(
    state: tauri::State<'_, crate::state::AppState>,
) -> Result<Vec<PostCallJob>, String> {
    PostCallRepository::unfinished(state.db_manager.pool())
        .await
        .map_err(|error| format!("Failed to load unfinished post-call work: {error}"))
}

#[tauri::command]
pub async fn meeting_participants_set(
    state: tauri::State<'_, crate::state::AppState>,
    meeting_id: String,
    names: Vec<String>,
) -> Result<Vec<String>, String> {
    PostCallRepository::set_participants(state.db_manager.pool(), &meeting_id, &names).await
}

#[tauri::command]
pub async fn meeting_participants_get(
    state: tauri::State<'_, crate::state::AppState>,
    meeting_id: String,
) -> Result<Vec<String>, String> {
    PostCallRepository::participants(state.db_manager.pool(), &meeting_id)
        .await
        .map_err(|error| format!("Failed to load participants: {error}"))
}

#[tauri::command]
pub async fn meeting_speaker_labels(
    state: tauri::State<'_, crate::state::AppState>,
    meeting_id: String,
) -> Result<Vec<String>, String> {
    PostCallRepository::speaker_labels(state.db_manager.pool(), &meeting_id)
        .await
        .map_err(|error| format!("Failed to load speaker labels: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::manager::DatabaseManager;

    /// The real migrator on a file database, so the tables, the delete trigger
    /// and every earlier migration are exercised together.
    async fn migrated() -> (tempfile::TempDir, DatabaseManager) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("meetings.sqlite");
        let legacy = directory.path().join("absent.db");
        let manager = DatabaseManager::new(path.to_str().unwrap(), legacy.to_str().unwrap())
            .await
            .unwrap();
        (directory, manager)
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

    fn update(meeting_id: &str, stage: &str) -> PostCallJobUpdate {
        PostCallJobUpdate { meeting_id: meeting_id.into(), stage: stage.into(), ..Default::default() }
    }

    #[tokio::test]
    async fn stage_changes_keep_the_answer_to_the_speaker_question() {
        let (_directory, manager) = migrated().await;
        let pool = manager.pool();
        add_meeting(pool, "m1").await;

        let first = PostCallRepository::save(pool, &PostCallJobUpdate {
            folder_path: Some("/recordings/m1".into()),
            ..update("m1", "prompt")
        }).await.unwrap();
        assert_eq!(first.stage, "prompt");
        assert!(!first.speaker_choice_made);
        assert_eq!(first.meeting_title.as_deref(), Some("Call"));

        // The count arrives with the choice; later stage writes must not erase it.
        PostCallRepository::save(pool, &PostCallJobUpdate {
            speaker_choice_made: Some(true),
            speaker_count: Some(3),
            ..update("m1", "enhancing")
        }).await.unwrap();
        let later = PostCallRepository::save(pool, &update("m1", "diarizing")).await.unwrap();
        assert_eq!(later.speaker_count, Some(3));
        assert!(later.speaker_choice_made);
        assert_eq!(later.folder_path.as_deref(), Some("/recordings/m1"));
    }

    #[tokio::test]
    async fn auto_detect_is_stored_as_a_made_choice_without_a_count() {
        let (_directory, manager) = migrated().await;
        let pool = manager.pool();
        add_meeting(pool, "m1").await;
        PostCallRepository::save(pool, &PostCallJobUpdate {
            speaker_choice_made: Some(true),
            speaker_count: Some(4),
            ..update("m1", "enhancing")
        }).await.unwrap();
        let auto = PostCallRepository::save(pool, &PostCallJobUpdate {
            speaker_choice_made: Some(true),
            speaker_count: None,
            ..update("m1", "enhancing")
        }).await.unwrap();
        assert!(auto.speaker_choice_made);
        assert_eq!(auto.speaker_count, None);
    }

    #[tokio::test]
    async fn downgrade_note_and_flag_survive_until_done() {
        let (_directory, manager) = migrated().await;
        let pool = manager.pool();
        add_meeting(pool, "m1").await;
        PostCallRepository::save(pool, &PostCallJobUpdate {
            used_live_transcript: Some(true),
            note: Some("  Kept the live transcript: model missing  ".into()),
            ..update("m1", "diarizing")
        }).await.unwrap();
        let done = PostCallRepository::save(pool, &update("m1", "done")).await.unwrap();
        assert!(done.used_live_transcript);
        assert_eq!(done.note.as_deref(), Some("Kept the live transcript: model missing"));

        // A later run that succeeds clears both with explicit values.
        let rerun = PostCallRepository::save(pool, &PostCallJobUpdate {
            used_live_transcript: Some(false),
            note: Some(String::new()),
            ..update("m1", "summary")
        }).await.unwrap();
        assert!(!rerun.used_live_transcript);
        assert_eq!(rerun.note, None);
    }

    #[tokio::test]
    async fn only_unfinished_jobs_of_existing_meetings_are_resumed() {
        let (_directory, manager) = migrated().await;
        let pool = manager.pool();
        for id in ["a", "b", "c"] {
            add_meeting(pool, id).await;
        }
        PostCallRepository::save(pool, &update("a", "enhancing")).await.unwrap();
        PostCallRepository::save(pool, &update("b", "done")).await.unwrap();
        PostCallRepository::save(pool, &update("c", "summary")).await.unwrap();

        let ids = |jobs: Vec<PostCallJob>| jobs.into_iter().map(|job| job.meeting_id).collect::<Vec<_>>();
        let mut unfinished = ids(PostCallRepository::unfinished(pool).await.unwrap());
        unfinished.sort();
        assert_eq!(unfinished, ["a", "c"]);

        // Deleting the meeting removes its job and participants through the trigger.
        PostCallRepository::set_participants(pool, "a", &["Alice".into()]).await.unwrap();
        sqlx::query("DELETE FROM meetings WHERE id = 'a'").execute(pool).await.unwrap();
        assert_eq!(ids(PostCallRepository::unfinished(pool).await.unwrap()), ["c"]);
        assert!(PostCallRepository::get(pool, "a").await.unwrap().is_none());
        assert!(PostCallRepository::participants(pool, "a").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_stage_and_unknown_meeting_are_rejected() {
        let (_directory, manager) = migrated().await;
        let pool = manager.pool();
        add_meeting(pool, "m1").await;
        assert!(PostCallRepository::save(pool, &update("m1", "uploading")).await.is_err());
        assert!(PostCallRepository::save(pool, &update("missing", "prompt")).await.is_err());
        assert!(PostCallRepository::set_participants(pool, "missing", &["Alice".into()]).await.is_err());
    }

    #[tokio::test]
    async fn participants_are_cleaned_ordered_and_replaced() {
        let (_directory, manager) = migrated().await;
        let pool = manager.pool();
        add_meeting(pool, "m1").await;
        let saved = PostCallRepository::set_participants(
            pool,
            "m1",
            &["  Alice   Smith ".into(), "".into(), "bob".into(), "ALICE SMITH".into()],
        ).await.unwrap();
        assert_eq!(saved, ["Alice Smith", "bob"]);
        assert_eq!(PostCallRepository::participants(pool, "m1").await.unwrap(), ["Alice Smith", "bob"]);

        PostCallRepository::set_participants(pool, "m1", &["Carol".into()]).await.unwrap();
        assert_eq!(PostCallRepository::participants(pool, "m1").await.unwrap(), ["Carol"]);
        PostCallRepository::set_participants(pool, "m1", &[]).await.unwrap();
        assert!(PostCallRepository::participants(pool, "m1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn speaker_labels_are_distinct_and_in_first_spoken_order() {
        let (_directory, manager) = migrated().await;
        let pool = manager.pool();
        add_meeting(pool, "m1").await;
        for (id, speaker, start) in [
            ("t1", Some("Speaker 1"), 4.0),
            ("t2", Some("You"), 1.0),
            ("t3", Some("Speaker 1"), 9.0),
            ("t4", None, 2.0),
            ("t5", Some("  "), 3.0),
        ] {
            sqlx::query(
                "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, speaker) \
                 VALUES (?, 'm1', 'text', '2026-11-23T10:00:00Z', ?, ?)",
            )
            .bind(id)
            .bind(start)
            .bind(speaker)
            .execute(pool)
            .await
            .unwrap();
        }
        assert_eq!(PostCallRepository::speaker_labels(pool, "m1").await.unwrap(), ["You", "Speaker 1"]);
    }
}
