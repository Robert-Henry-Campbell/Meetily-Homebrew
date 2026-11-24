-- Progress of the chain that runs after Stop (speaker question, re-transcription,
-- speaker labelling, summary). The webview drives the steps; this row lets a
-- reload or a quit resume them instead of leaving a meeting half processed.
-- New tables only: existing queries select explicit meeting columns and must not
-- see a changed shape.
CREATE TABLE IF NOT EXISTS post_call_jobs (
    meeting_id            TEXT PRIMARY KEY,
    folder_path           TEXT,
    stage                 TEXT NOT NULL DEFAULT 'prompt'
                          CHECK (stage IN ('prompt', 'enhancing', 'diarizing', 'summary', 'done')),
    speaker_choice_made   INTEGER NOT NULL DEFAULT 0 CHECK (speaker_choice_made IN (0, 1)),
    -- Total speakers including the user; NULL means auto-detect.
    speaker_count         INTEGER,
    used_live_transcript  INTEGER NOT NULL DEFAULT 0 CHECK (used_live_transcript IN (0, 1)),
    note                  TEXT,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);

-- People the user named at the speaker question ("Who was on the call?").
-- Meeting-level only: a name here is not tied to a speaker label.
CREATE TABLE IF NOT EXISTS meeting_participants (
    meeting_id  TEXT NOT NULL,
    position    INTEGER NOT NULL,
    name        TEXT NOT NULL CHECK (length(trim(name)) > 0),
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (meeting_id, position)
);

-- Meeting deletion removes child rows explicitly in Rust for the older tables.
-- A trigger keeps these two in step without touching that code path.
CREATE TRIGGER IF NOT EXISTS post_call_rows_follow_meeting
AFTER DELETE ON meetings
BEGIN
    DELETE FROM post_call_jobs WHERE meeting_id = OLD.id;
    DELETE FROM meeting_participants WHERE meeting_id = OLD.id;
END;
