-- Google Calendar: which calendar event a recording belongs to.
-- New table and triggers only; `meetings` keeps its shape (upstream PR #666
-- added columns to it, which breaks the queries here that list its columns).

CREATE TABLE IF NOT EXISTS meeting_calendar_links (
    meeting_id      TEXT PRIMARY KEY,
    -- pending: not looked up yet. matched / none: looked up. attention: the
    -- lookup needs the user (sign-in ended, API disabled).
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'matched', 'none', 'attention')),
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL DEFAULT 0,
    last_error      TEXT,
    event_id        TEXT,
    calendar_id     TEXT,
    event_title     TEXT,
    event_start     TEXT,
    event_end       TEXT,
    meet_link       TEXT,
    -- JSON array of the other invitees' names. Names only: no addresses.
    attendee_names  TEXT,
    matched_at      INTEGER
);

-- Every new meeting is queued for a lookup. The stop-and-save path writes
-- nothing else and never waits for Google; a background worker does the lookup.
CREATE TRIGGER IF NOT EXISTS calendar_link_meeting_added AFTER INSERT ON meetings
BEGIN
    INSERT OR IGNORE INTO meeting_calendar_links (meeting_id) VALUES (NEW.id);
END;

CREATE TRIGGER IF NOT EXISTS calendar_link_meeting_deleted AFTER DELETE ON meetings
BEGIN
    DELETE FROM meeting_calendar_links WHERE meeting_id = OLD.id;
END;

-- The invitees decide the Drive folder and appear in the files.
CREATE TRIGGER IF NOT EXISTS drive_export_calendar_changed AFTER UPDATE OF attendee_names, event_title ON meeting_calendar_links
BEGIN
    UPDATE drive_export_state
    SET content_version = content_version + 1, changed_at = CAST(strftime('%s', 'now') AS INTEGER)
    WHERE meeting_id = NEW.meeting_id;
END;
