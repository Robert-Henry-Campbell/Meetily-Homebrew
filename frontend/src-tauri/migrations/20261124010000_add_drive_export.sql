-- Google Drive export: what was sent, what changed since, and where it went.
-- New tables and triggers only; existing tables keep their shape.

-- One row per meeting whose exported content exists or has changed.
-- content_version counts changes; exported_version is the count the files in
-- Drive reflect. The export is due while the two differ.
CREATE TABLE IF NOT EXISTS drive_export_state (
    meeting_id          TEXT PRIMARY KEY,
    content_version     INTEGER NOT NULL DEFAULT 1,
    exported_version    INTEGER,
    -- Unix seconds of the last content change; the worker waits for quiet.
    changed_at          INTEGER NOT NULL,
    -- Set by "Send now": export regardless of the automatic rules.
    requested           INTEGER NOT NULL DEFAULT 0 CHECK (requested IN (0, 1)),
    status              TEXT NOT NULL DEFAULT 'waiting'
                        CHECK (status IN ('waiting', 'sent', 'empty', 'attention', 'failed')),
    last_error          TEXT,
    attempts            INTEGER NOT NULL DEFAULT 0,
    next_attempt_at     INTEGER NOT NULL DEFAULT 0,
    folder_id           TEXT,
    summary_file_id     TEXT,
    transcript_file_id  TEXT,
    summary_link        TEXT,
    transcript_link     TEXT,
    -- Hash of name, folder and content last sent, to skip unchanged files.
    summary_hash        TEXT,
    transcript_hash     TEXT,
    exported_at         INTEGER
);

-- Drive folders the app created: the root, one per person, one per group,
-- and the shared folder for meetings with several people.
CREATE TABLE IF NOT EXISTS drive_folders (
    kind        TEXT NOT NULL CHECK (kind IN ('root', 'person', 'group', 'group-meetings')),
    name_key    TEXT NOT NULL,
    folder_id   TEXT NOT NULL,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (kind, name_key)
);

-- Every change to something the files contain bumps the meeting's version.
-- Triggers, not calls in Rust: the content is written from many places, and a
-- missed call would leave a stale file in Drive without any sign of it.

CREATE TRIGGER IF NOT EXISTS drive_export_transcript_added AFTER INSERT ON transcripts
BEGIN
    INSERT INTO drive_export_state (meeting_id, changed_at)
    VALUES (NEW.meeting_id, CAST(strftime('%s', 'now') AS INTEGER))
    ON CONFLICT(meeting_id) DO UPDATE SET content_version = content_version + 1, changed_at = excluded.changed_at;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_transcript_changed AFTER UPDATE ON transcripts
BEGIN
    INSERT INTO drive_export_state (meeting_id, changed_at)
    VALUES (NEW.meeting_id, CAST(strftime('%s', 'now') AS INTEGER))
    ON CONFLICT(meeting_id) DO UPDATE SET content_version = content_version + 1, changed_at = excluded.changed_at;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_transcript_removed AFTER DELETE ON transcripts
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = OLD.meeting_id;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_meeting_changed AFTER UPDATE OF title, group_id, created_at ON meetings
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_summary_added AFTER INSERT ON summary_processes
BEGIN
    INSERT INTO drive_export_state (meeting_id, changed_at)
    VALUES (NEW.meeting_id, CAST(strftime('%s', 'now') AS INTEGER))
    ON CONFLICT(meeting_id) DO UPDATE SET content_version = content_version + 1, changed_at = excluded.changed_at;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_summary_changed AFTER UPDATE OF result, status ON summary_processes
BEGIN
    INSERT INTO drive_export_state (meeting_id, changed_at)
    VALUES (NEW.meeting_id, CAST(strftime('%s', 'now') AS INTEGER))
    ON CONFLICT(meeting_id) DO UPDATE SET content_version = content_version + 1, changed_at = excluded.changed_at;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_notes_added AFTER INSERT ON meeting_notes
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = NEW.meeting_id;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_notes_changed AFTER UPDATE OF notes_markdown ON meeting_notes
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = NEW.meeting_id;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_action_added AFTER INSERT ON action_items
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = NEW.meeting_id;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_action_changed AFTER UPDATE OF text, owner_label, due_text, done, position ON action_items
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = NEW.meeting_id;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_action_removed AFTER DELETE ON action_items
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = OLD.meeting_id;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_participant_added AFTER INSERT ON meeting_participants
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = NEW.meeting_id;
END;

CREATE TRIGGER IF NOT EXISTS drive_export_participant_removed AFTER DELETE ON meeting_participants
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = OLD.meeting_id;
END;

-- The chain's stage decides when a meeting is stable; its note is written
-- into the transcript file.
CREATE TRIGGER IF NOT EXISTS drive_export_job_changed AFTER UPDATE OF stage, note, used_live_transcript ON post_call_jobs
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = NEW.meeting_id;
END;

-- Deleting a meeting in the app removes the bookkeeping. The files in Drive
-- are left as they are: the app never deletes from Drive.
CREATE TRIGGER IF NOT EXISTS drive_export_meeting_deleted AFTER DELETE ON meetings
BEGIN
    DELETE FROM drive_export_state WHERE meeting_id = OLD.id;
END;
