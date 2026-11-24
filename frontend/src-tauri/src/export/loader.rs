//! Reads one meeting from the database into the shape the Markdown writer and
//! the folder rules need. Everything comes from tables; the webview is not
//! involved, so the export also runs when no window is open.

use super::markdown::{ActionItem, MeetingExport, TranscriptLine};
use super::naming::known_people;
use super::state::KEY_USER_NAME;
use crate::integrations::config_store;
use crate::summary::stored_markdown::{stored_summary_author, stored_summary_markdown};
use chrono::{DateTime, NaiveDateTime, Utc};
use sqlx::SqlitePool;

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedMeeting {
    pub export: MeetingExport,
    /// The name of the meeting's group, if it belongs to one.
    pub group: Option<String>,
}

/// Loads a meeting. `to_local` turns the recording start into the user's local
/// time (a parameter, so tests do not depend on the machine's time zone).
pub async fn load_meeting(
    pool: &SqlitePool,
    meeting_id: &str,
    to_local: impl Fn(DateTime<Utc>) -> NaiveDateTime,
) -> Result<Option<LoadedMeeting>, sqlx::Error> {
    let Some((title, created_at, group_id)) = sqlx::query_as::<_, (String, crate::database::models::DateTimeUtc, Option<String>)>(
        "SELECT title, created_at, group_id FROM meetings WHERE id = ?",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };

    let group = match group_id {
        Some(group_id) => sqlx::query_scalar::<_, String>("SELECT name FROM groups WHERE id = ?")
            .bind(group_id)
            .fetch_optional(pool)
            .await?,
        None => None,
    };

    // The order the app itself shows and exports; rowid breaks ties so the
    // file is the same on every run.
    let lines: Vec<TranscriptLine> = sqlx::query_as::<_, (String, Option<f64>, Option<String>)>(
        "SELECT transcript, audio_start_time, speaker FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time ASC, rowid ASC",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(text, start_seconds, speaker)| TranscriptLine { start_seconds, speaker, text })
    .collect();

    let stored_summary = sqlx::query_scalar::<_, Option<String>>("SELECT result FROM summary_processes WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await?
        .flatten();
    let summary_markdown = stored_summary.as_deref().map(stored_summary_markdown).unwrap_or_default();
    let author = stored_summary.as_deref().and_then(stored_summary_author);

    let action_items = sqlx::query_as::<_, (String, Option<String>, Option<String>, i64)>(
        "SELECT text, owner_label, due_text, done FROM action_items WHERE meeting_id = ? ORDER BY position, created_at, id",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(text, owner, due, done)| ActionItem { text, owner, due, done: done != 0 })
    .collect();

    let notes_markdown = sqlx::query_scalar::<_, Option<String>>("SELECT notes_markdown FROM meeting_notes WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await?
        .flatten()
        .unwrap_or_default();

    let typed = crate::post_call::PostCallRepository::participants(pool, meeting_id).await?;
    let speaker_labels = crate::post_call::PostCallRepository::speaker_labels(pool, meeting_id).await?;
    let invitees = calendar_invitees(pool, meeting_id).await;
    let user_name = config_store::get(pool, KEY_USER_NAME).await?.unwrap_or_default();
    let attendees = known_people(&typed, &speaker_labels, &invitees, &user_name);

    let processing_note = sqlx::query_scalar::<_, Option<String>>("SELECT note FROM post_call_jobs WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await?
        .flatten();

    Ok(Some(LoadedMeeting {
        export: MeetingExport {
            meeting_id: meeting_id.to_string(),
            title,
            started_local: to_local(created_at.0),
            user_name,
            attendees,
            lines,
            summary_markdown,
            summary_model: author.as_ref().map(|author| format!("{}/{}", author.provider, author.model)),
            summary_fallback_reason: author.and_then(|author| author.fallback_reason),
            action_items,
            notes_markdown,
            processing_note,
        },
        group,
    }))
}

/// Names of the people invited to the calendar event matched to this meeting.
/// The table arrives with the calendar feature; without it there are none.
async fn calendar_invitees(pool: &SqlitePool, meeting_id: &str) -> Vec<String> {
    let stored = sqlx::query_scalar::<_, Option<String>>("SELECT attendee_names FROM meeting_calendar_links WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await;
    match stored {
        Ok(Some(Some(json))) => serde_json::from_str::<Vec<String>>(&json).unwrap_or_default(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::config_store::test_pool;

    fn utc_plus_one(time: DateTime<Utc>) -> NaiveDateTime {
        (time + chrono::Duration::hours(1)).naive_utc()
    }

    #[tokio::test]
    async fn a_meeting_is_read_with_everything_the_files_contain() {
        let (_directory, manager) = test_pool().await;
        let pool = manager.pool();
        sqlx::query("INSERT INTO groups (id, name, normalized_name, created_at, updated_at) VALUES ('g1', 'Weekly sync', 'weekly sync', 'x', 'x')").execute(pool).await.unwrap();
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at, group_id) VALUES ('m', 'Budget', '2026-11-23T23:30:00+00:00', '2026-11-23T23:59:00+00:00', 'g1')").execute(pool).await.unwrap();
        for (id, start, speaker, text) in [("t2", Some(65.0), Some("Alice Smith"), "Yes."), ("t1", Some(1.0), Some("You"), "Shall we?"), ("t3", Some(70.0), Some("Speaker 2"), "Hm.")] {
            sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, speaker) VALUES (?, 'm', ?, 'x', ?, ?)")
                .bind(id).bind(text).bind(start).bind(speaker).execute(pool).await.unwrap();
        }
        let stored = serde_json::json!({ "markdown": "## Decisions\n- Ship", "generated_by": { "provider": "claude", "model": "claude-opus-5-5" } }).to_string();
        sqlx::query("INSERT INTO summary_processes (meeting_id, status, created_at, updated_at, result) VALUES ('m', 'completed', 'x', 'x', ?)").bind(stored).execute(pool).await.unwrap();
        sqlx::query("INSERT INTO action_items (id, meeting_id, text, owner_label, due_text, done, position, created_at, updated_at) VALUES ('a2', 'm', 'Second', NULL, NULL, 1, 1, 'x', 'x'), ('a1', 'm', 'First', 'Alice', 'Friday', 0, 0, 'x', 'x')").execute(pool).await.unwrap();
        sqlx::query("INSERT INTO meeting_notes (meeting_id, notes_markdown, created_at, updated_at) VALUES ('m', 'Ask about Q4.', 'x', 'x')").execute(pool).await.unwrap();
        crate::post_call::PostCallRepository::set_participants(pool, "m", &["Bob".into()]).await.unwrap();
        config_store::set(pool, KEY_USER_NAME, "Rob").await.unwrap();

        let loaded = load_meeting(pool, "m", utc_plus_one).await.unwrap().unwrap();
        assert_eq!(loaded.group.as_deref(), Some("Weekly sync"));
        let export = loaded.export;
        // Local time decides the date in the file name: 23:30 UTC is the next day at UTC+1.
        assert_eq!(export.started_local.to_string(), "2026-11-24 00:30:00");
        assert_eq!(export.title, "Budget");
        assert_eq!(export.user_name, "Rob");
        // Typed names first, then named voices; "You" and "Speaker 2" are nobody.
        assert_eq!(export.attendees, ["Bob", "Alice Smith"]);
        assert_eq!(export.lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(), ["Shall we?", "Yes.", "Hm."]);
        assert_eq!(export.summary_markdown, "## Decisions\n- Ship");
        assert_eq!(export.summary_model.as_deref(), Some("claude/claude-opus-5-5"));
        assert_eq!(export.summary_fallback_reason, None);
        assert_eq!(export.action_items.iter().map(|item| (item.text.as_str(), item.done)).collect::<Vec<_>>(), [("First", false), ("Second", true)]);
        assert_eq!(export.notes_markdown, "Ask about Q4.");
        assert_eq!(export.processing_note, None);
    }

    #[tokio::test]
    async fn a_bare_meeting_loads_with_empty_parts_and_a_missing_one_is_none() {
        let (_directory, manager) = test_pool().await;
        let pool = manager.pool();
        sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES ('m', 'Call', '2026-11-23T10:00:00+00:00', 'x')").execute(pool).await.unwrap();
        let loaded = load_meeting(pool, "m", |time| time.naive_utc()).await.unwrap().unwrap();
        assert_eq!(loaded.group, None);
        assert!(loaded.export.lines.is_empty() && loaded.export.attendees.is_empty() && loaded.export.action_items.is_empty());
        assert_eq!((loaded.export.summary_markdown.as_str(), loaded.export.notes_markdown.as_str(), loaded.export.user_name.as_str()), ("", "", ""));
        assert!(load_meeting(pool, "missing", |time| time.naive_utc()).await.unwrap().is_none());
    }
}
