//! Sends due meetings to Google Drive in the background.
//!
//! The worker owns the whole operation natively: it is started with the app,
//! reads everything from the database, and keeps its progress there. A closed
//! window, a quit in the middle of an upload, or a lost response therefore
//! cannot leave a meeting half sent without the next pass noticing. Before any
//! object is created, the stored ID and then the object's private tag are
//! looked up, so a retry adopts what an earlier attempt created instead of
//! making a duplicate. Nothing is ever deleted from Drive.

use super::loader::load_meeting;
use super::markdown::{render_summary_file, render_transcript_file};
use super::naming::{choose_folder, file_stem, folder_key, Folder, GROUP_MEETINGS_FOLDER};
use super::state::{self, Readiness, SentFile};
use crate::google::drive::{DriveClient, DriveError, PROP_KIND, PROP_MEETING, PROP_NAME};
use crate::google::oauth::{self, AccessError, GoogleEndpoints};
use chrono::{DateTime, NaiveDateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use std::time::Duration;

/// How often the queue is looked at when nothing wakes the worker earlier.
const POLL_INTERVAL: Duration = Duration::from_secs(15);
/// Meetings sent per pass; the rest follow on the next one.
const MAX_PER_PASS: usize = 5;

/// Emitted with `{ meetingId }` when a meeting's export state changed.
pub const DRIVE_EXPORT_CHANGED: &str = "drive-export-changed";

static WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Makes the worker look at the queue now ("Send now", a new sign-in).
pub fn wake() {
    WAKE.notify_one();
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportOutcome {
    /// Files are in Drive and reflect the meeting.
    Sent,
    /// The meeting has nothing to send yet.
    Empty,
    /// The user has to act (sign in again, enable the API, free space).
    Attention(String),
    /// Trouble that passes; tried again later.
    Retry(String),
    Failed(String),
}

pub struct ExportContext<'a> {
    pub pool: &'a SqlitePool,
    pub http: &'a reqwest::Client,
    pub endpoints: &'a GoogleEndpoints,
    /// The recording start in the user's local time, for file names and dates.
    pub to_local: fn(DateTime<Utc>) -> NaiveDateTime,
}

pub fn local_time(time: DateTime<Utc>) -> NaiveDateTime {
    time.with_timezone(&chrono::Local).naive_local()
}

enum Step {
    Drive(DriveError),
    Store(sqlx::Error),
}

impl From<DriveError> for Step {
    fn from(error: DriveError) -> Self {
        Self::Drive(error)
    }
}

impl From<sqlx::Error> for Step {
    fn from(error: sqlx::Error) -> Self {
        Self::Store(error)
    }
}

/// The folder's ID in Drive: the stored one if it still exists, else one found
/// by its private tag, else a new one.
async fn ensure_folder(
    context: &ExportContext<'_>,
    drive: &DriveClient<'_>,
    kind: &str,
    name_key: &str,
    name: &str,
    parent: Option<&str>,
    now: i64,
) -> Result<String, Step> {
    if let Some(stored) = state::stored_folder(context.pool, kind, name_key).await? {
        if drive.get(&stored).await?.is_some() {
            return Ok(stored);
        }
    }
    let tags = [(PROP_KIND, kind), (PROP_NAME, name_key)];
    let folder = match drive.find(&tags, true).await?.into_iter().next() {
        Some(found) => found,
        None => drive.create_folder(name, parent, &tags).await?,
    };
    state::store_folder(context.pool, kind, name_key, &folder.id, now).await?;
    Ok(folder.id)
}

async fn target_folder(
    context: &ExportContext<'_>,
    drive: &DriveClient<'_>,
    folder: &Folder,
    root_name: &str,
    now: i64,
) -> Result<String, Step> {
    let root = ensure_folder(context, drive, "root", "", root_name, None, now).await?;
    match folder {
        Folder::Root => Ok(root),
        Folder::Person(name) => ensure_folder(context, drive, "person", &folder_key(name), name, Some(&root), now).await,
        Folder::Group(name) => ensure_folder(context, drive, "group", &folder_key(name), name, Some(&root), now).await,
        Folder::GroupMeetings => ensure_folder(context, drive, "group-meetings", "", GROUP_MEETINGS_FOLDER, Some(&root), now).await,
    }
}

fn content_hash(name: &str, folder_id: &str, content: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [name, "\n", folder_id, "\n", content] {
        hasher.update(part.as_bytes());
    }
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Creates or updates one of the meeting's two files.
#[allow(clippy::too_many_arguments)]
async fn send_file(
    context: &ExportContext<'_>,
    drive: &DriveClient<'_>,
    meeting_id: &str,
    kind: &str,
    name: &str,
    folder_id: &str,
    content: Option<&str>,
    previous: SentFile,
) -> Result<SentFile, Step> {
    // Nothing to write for this file (no summary yet, or it was removed): what
    // is in Drive stays as it is.
    let Some(content) = content else { return Ok(previous) };
    let hash = content_hash(name, folder_id, content);
    if previous.id.is_some() && previous.hash.as_deref() == Some(hash.as_str()) {
        return Ok(previous);
    }

    let mut existing = match previous.id.as_deref() {
        Some(id) => drive.get(id).await?,
        None => None,
    };
    if existing.is_none() {
        // No usable ID: an earlier attempt may have created the file without
        // the app learning its ID. The private tag finds it.
        existing = drive.find(&[(PROP_KIND, kind), (PROP_MEETING, meeting_id)], false).await?.into_iter().next();
    }
    let file = match existing {
        Some(file) => {
            let current = file.parents.first().map(String::as_str);
            // A file is moved only between the app's own folders. One the user
            // filed elsewhere stays there and is updated in place.
            let move_from = match current {
                Some(current) if current != folder_id && state::is_app_folder(context.pool, current).await? => Some(current),
                _ => None,
            };
            drive.update_file(&file.id, name, folder_id, move_from, content).await?
        }
        None => drive.create_file(name, folder_id, &[(PROP_KIND, kind), (PROP_MEETING, meeting_id)], content).await?,
    };
    Ok(SentFile { link: file.web_view_link.or(previous.link), id: Some(file.id), hash: Some(hash) })
}

async fn send(context: &ExportContext<'_>, meeting_id: &str, access_token: &str, now: i64) -> Result<ExportOutcome, Step> {
    let Some(row) = state::get(context.pool, meeting_id).await? else { return Ok(ExportOutcome::Empty) };
    // The version read before the content: a change during the upload leaves
    // the meeting due, because its version is then already higher.
    let version = row.content_version;
    let Some(loaded) = load_meeting(context.pool, meeting_id, context.to_local).await? else { return Ok(ExportOutcome::Empty) };
    let settings = state::settings(context.pool).await?;
    let meeting = &loaded.export;
    let folder = choose_folder(&meeting.attendees, loaded.group.as_deref(), settings.folder_per_person);
    let stem = file_stem(&meeting.title, meeting.started_local, &meeting.attendees, &folder);
    let summary = render_summary_file(meeting);
    let transcript = render_transcript_file(meeting);

    let previous_summary = SentFile { id: row.summary_file_id, link: row.summary_link, hash: row.summary_hash };
    let previous_transcript = SentFile { id: row.transcript_file_id, link: row.transcript_link, hash: row.transcript_hash };
    if summary.is_none() && transcript.is_none() {
        state::record_sent(context.pool, meeting_id, version, row.folder_id.as_deref(), &previous_summary, &previous_transcript, now).await?;
        return Ok(if previous_summary.id.is_some() || previous_transcript.id.is_some() { ExportOutcome::Sent } else { ExportOutcome::Empty });
    }

    let drive = DriveClient { http: context.http, endpoints: context.endpoints, access_token };
    let folder_id = target_folder(context, &drive, &folder, &settings.root_folder_name, now).await?;
    let sent_transcript = send_file(context, &drive, meeting_id, "transcript", &format!("{stem} - transcript.md"), &folder_id, transcript.as_deref(), previous_transcript).await?;
    let sent_summary = send_file(context, &drive, meeting_id, "summary", &format!("{stem} - summary.md"), &folder_id, summary.as_deref(), previous_summary).await?;
    state::record_sent(context.pool, meeting_id, version, Some(&folder_id), &sent_summary, &sent_transcript, now).await?;
    Ok(ExportOutcome::Sent)
}

/// Sends one meeting and records the result.
pub async fn export_meeting(context: &ExportContext<'_>, meeting_id: &str, now: i64) -> ExportOutcome {
    let mut refreshed = false;
    let outcome = loop {
        let access_token = match oauth::access_token(context.pool, context.http, context.endpoints).await {
            Ok(token) => token,
            Err(AccessError::NotConnected) => break ExportOutcome::Attention("Google is not connected. Connect it in Settings, Connections.".into()),
            Err(AccessError::NeedsAttention(reason)) => break ExportOutcome::Attention(reason),
            Err(AccessError::Transient(reason)) => break ExportOutcome::Retry(reason),
        };
        match send(context, meeting_id, &access_token, now).await {
            Ok(outcome) => break outcome,
            // The token can be revoked while it still looks fresh. One forced
            // refresh tells an expired token from an ended sign-in.
            Err(Step::Drive(DriveError::Unauthorized)) if !refreshed => {
                refreshed = true;
                oauth::expire_access_token(context.pool).await;
            }
            Err(Step::Drive(DriveError::Unauthorized)) => {
                let reason = "Google did not accept the sign-in. Sign in again.";
                oauth::mark_needs_attention(context.pool, reason).await;
                break ExportOutcome::Attention(reason.into());
            }
            Err(Step::Drive(DriveError::Forbidden(reason))) => break ExportOutcome::Attention(reason),
            Err(Step::Drive(DriveError::Transient(reason))) => break ExportOutcome::Retry(reason),
            // A folder vanished between the check and its use; the next pass finds or makes it again.
            Err(Step::Drive(DriveError::NotFound)) => break ExportOutcome::Retry("A Drive folder was removed during the upload.".into()),
            Err(Step::Drive(DriveError::Failed(reason))) => break ExportOutcome::Failed(reason),
            Err(Step::Store(error)) => break ExportOutcome::Retry(format!("The app's database could not be read or written: {error}")),
        }
    };
    let recorded = match &outcome {
        ExportOutcome::Sent | ExportOutcome::Empty => Ok(()),
        ExportOutcome::Attention(reason) => state::record_failure(context.pool, meeting_id, "attention", reason, now).await,
        ExportOutcome::Retry(reason) => state::record_failure(context.pool, meeting_id, "waiting", reason, now).await,
        ExportOutcome::Failed(reason) => state::record_failure(context.pool, meeting_id, "failed", reason, now).await,
    };
    if let Err(error) = recorded {
        log::warn!("Could not record the Drive export result for {meeting_id}: {error}");
    }
    outcome
}

/// One look at the queue. Returns the meetings whose state changed.
pub async fn pass(context: &ExportContext<'_>, now: i64) -> Vec<String> {
    let mut changed = Vec::new();
    let (Ok(settings), Ok(google), Ok(due)) = (
        state::settings(context.pool).await,
        oauth::status(context.pool).await,
        state::candidates(context.pool, now).await,
    ) else {
        return changed;
    };
    let usable = google.connected && google.drive_granted && google.needs_attention.is_none();
    let settings = if usable && settings.auto_export && settings.enabled_since.is_none() {
        // From this moment on, new meetings are sent on their own.
        match state::mark_enabled_if_new(context.pool, now).await {
            Ok(since) => state::ExportSettings { enabled_since: Some(since), ..settings },
            Err(_) => settings,
        }
    } else {
        settings
    };

    for candidate in due {
        if changed.len() >= MAX_PER_PASS {
            break;
        }
        if state::readiness(&candidate, &settings, now) != Readiness::Ready {
            continue;
        }
        if !usable {
            // Only a meeting the user asked for is marked; the automatic ones
            // simply wait until Google is connected again.
            if candidate.requested {
                let reason = google.needs_attention.clone().unwrap_or_else(|| {
                    if google.connected { "The Drive permission is missing. Sign in again and tick it.".into() } else { "Google is not connected. Connect it in Settings, Connections.".into() }
                });
                let _ = state::record_failure(context.pool, &candidate.meeting_id, "attention", &reason, now).await;
                changed.push(candidate.meeting_id);
            }
            continue;
        }
        export_meeting(context, &candidate.meeting_id, now).await;
        changed.push(candidate.meeting_id);
    }
    changed
}

/// Starts the worker. Called once, after the database is ready.
pub fn spawn<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    use tauri::{Emitter, Manager};
    tauri::async_runtime::spawn(async move {
        let http = oauth::http_client();
        let endpoints = GoogleEndpoints::default();
        loop {
            tokio::select! {
                _ = WAKE.notified() => {}
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
            }
            let Some(app_state) = app.try_state::<crate::state::AppState>() else { continue };
            let context = ExportContext { pool: app_state.db_manager.pool(), http: &http, endpoints: &endpoints, to_local: local_time };
            // Calendar first: a match found now already decides this pass's folder.
            for meeting_id in crate::calendar::worker::pass(&context, Utc::now().timestamp()).await {
                let _ = app.emit(crate::calendar::CALENDAR_LINK_CHANGED, serde_json::json!({ "meetingId": meeting_id }));
            }
            for meeting_id in pass(&context, Utc::now().timestamp()).await {
                let _ = app.emit(DRIVE_EXPORT_CHANGED, serde_json::json!({ "meetingId": meeting_id }));
            }
        }
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::google::oauth::tests::{test_client, test_http, FakeGoogle};
    use crate::integrations::config_store::{self, test_pool};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Clone, Default)]
    pub(crate) struct FakeObject {
        pub name: String,
        pub parent: Option<String>,
        pub folder: bool,
        pub trashed: bool,
        pub tags: BTreeMap<String, String>,
        pub content: String,
        pub writes: usize,
    }

    /// An in-memory Drive behind the fake HTTP server: enough of files.create,
    /// files.update, files.get and files.list to observe what the worker does.
    #[derive(Default)]
    pub(crate) struct FakeDrive {
        pub objects: BTreeMap<String, FakeObject>,
        next: usize,
        /// Statuses returned for the next requests instead of serving them.
        pub fail_next: Vec<u16>,
        /// The next create succeeds in Drive but the app never sees the answer.
        pub lose_next_create_response: bool,
        pub token_refreshes: usize,
        pub refresh_status: u16,
    }

    impl FakeDrive {
        pub(crate) fn files(&self) -> Vec<(String, &FakeObject)> {
            self.objects.iter().filter(|(_, object)| !object.folder && !object.trashed).map(|(id, object)| (id.clone(), object)).collect()
        }

        pub(crate) fn path(&self, id: &str) -> String {
            let object = &self.objects[id];
            match &object.parent {
                Some(parent) if self.objects.contains_key(parent) => format!("{}/{}", self.path(parent), object.name),
                _ => object.name.clone(),
            }
        }

        fn json(&self, id: &str) -> String {
            let object = &self.objects[id];
            serde_json::json!({
                "id": id, "name": object.name, "trashed": object.trashed,
                "parents": object.parent.iter().collect::<Vec<_>>(),
                "webViewLink": format!("https://drive.google.com/file/d/{id}/view"),
            }).to_string()
        }

        fn serve(&mut self, method: &str, target: &str, body: &str) -> (u16, String) {
            if target.starts_with("/token") {
                self.token_refreshes += 1;
                return match self.refresh_status {
                    0 | 200 => (200, r#"{"access_token":"fresh","expires_in":3600}"#.into()),
                    status => (status, r#"{"error":"invalid_grant"}"#.into()),
                };
            }
            if let Some(status) = (!self.fail_next.is_empty()).then(|| self.fail_next.remove(0)) {
                return (status, r#"{"error":{"message":"scripted failure"}}"#.into());
            }
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            let query: BTreeMap<String, String> = url::form_urlencoded::parse(query.as_bytes()).into_owned().collect();
            let multipart = |body: &str| -> (serde_json::Value, String) {
                let start = body.find('{').unwrap();
                let end = body[start..].find("\r\n--meetily-").unwrap() + start;
                let rest = &body[end + 2..];
                let content_start = rest.find("\r\n\r\n").unwrap() + 4;
                let content_end = rest.rfind("\r\n--meetily-").unwrap();
                (serde_json::from_str(&body[start..end]).unwrap(), rest[content_start..content_end].to_string())
            };
            let tags = |metadata: &serde_json::Value| -> BTreeMap<String, String> {
                metadata["appProperties"].as_object().map(|map| map.iter().map(|(key, value)| (key.clone(), value.as_str().unwrap_or("").to_string())).collect()).unwrap_or_default()
            };
            match (method, path) {
                ("POST", "/drive/v3/files") => {
                    let metadata: serde_json::Value = serde_json::from_str(body).unwrap();
                    self.next += 1;
                    let id = format!("d{}", self.next);
                    self.objects.insert(id.clone(), FakeObject {
                        name: metadata["name"].as_str().unwrap().into(),
                        parent: metadata["parents"][0].as_str().map(str::to_string),
                        folder: true,
                        tags: tags(&metadata),
                        ..Default::default()
                    });
                    (200, self.json(&id))
                }
                ("POST", "/upload/drive/v3/files") => {
                    let (metadata, content) = multipart(body);
                    self.next += 1;
                    let id = format!("f{}", self.next);
                    self.objects.insert(id.clone(), FakeObject {
                        name: metadata["name"].as_str().unwrap().into(),
                        parent: metadata["parents"][0].as_str().map(str::to_string),
                        tags: tags(&metadata),
                        content,
                        writes: 1,
                        ..Default::default()
                    });
                    if std::mem::take(&mut self.lose_next_create_response) {
                        return (503, "{}".into());
                    }
                    (200, self.json(&id))
                }
                ("PATCH", path) if path.starts_with("/upload/drive/v3/files/") => {
                    let id = path.trim_start_matches("/upload/drive/v3/files/").to_string();
                    let (metadata, content) = multipart(body);
                    let Some(object) = self.objects.get_mut(&id) else { return (404, "{}".into()) };
                    object.name = metadata["name"].as_str().unwrap().into();
                    object.content = content;
                    object.writes += 1;
                    if let Some(parent) = query.get("addParents") {
                        assert_eq!(query.get("removeParents"), object.parent.as_ref(), "a move must name the folder the file is in");
                        object.parent = Some(parent.clone());
                    }
                    (200, self.json(&id))
                }
                ("GET", "/drive/v3/files") => {
                    let q = &query["q"];
                    let want_folder = q.contains("mimeType = 'application/vnd.google-apps.folder'");
                    let found: Vec<serde_json::Value> = self
                        .objects
                        .iter()
                        .filter(|(_, object)| !object.trashed && object.folder == want_folder)
                        .filter(|(_, object)| object.tags.iter().all(|(key, value)| !q.contains(&format!("key='{key}'")) || q.contains(&format!("key='{key}' and value='{value}'"))))
                        .filter(|(_, object)| q.matches("appProperties has").count() <= object.tags.len())
                        .map(|(id, _)| serde_json::from_str(&self.json(id)).unwrap())
                        .collect();
                    (200, serde_json::json!({ "files": found }).to_string())
                }
                ("GET", path) if path.starts_with("/drive/v3/files/") => {
                    let id = path.trim_start_matches("/drive/v3/files/");
                    if self.objects.contains_key(id) { (200, self.json(id)) } else { (404, r#"{"error":{"message":"File not found"}}"#.into()) }
                }
                other => panic!("the export made a request the fake Drive does not know: {other:?}"),
            }
        }
    }

    pub(crate) struct Fixture {
        pub drive: Arc<Mutex<FakeDrive>>,
        pub google: FakeGoogle,
        pub manager: crate::database::manager::DatabaseManager,
        pub http: reqwest::Client,
        _directory: tempfile::TempDir,
    }

    impl Fixture {
        /// A signed-in app with automatic export on, and an empty Drive.
        pub(crate) async fn new() -> Self {
            let drive = Arc::new(Mutex::new(FakeDrive::default()));
            let served = drive.clone();
            let google = FakeGoogle::start(Arc::new(move |method, target, body| served.lock().unwrap().serve(method, target, body))).await;
            let (_directory, manager) = test_pool().await;
            let pool = manager.pool();
            oauth::save_client(pool, &test_client()).await.unwrap();
            let fresh = (Utc::now().timestamp() + 3000).to_string();
            config_store::set_many(pool, &[
                ("google.refresh_token", Some("refresh")),
                ("google.access_token", Some("access")),
                ("google.access_expires_at", Some(fresh.as_str())),
                ("google.scopes", Some("https://www.googleapis.com/auth/drive.file https://www.googleapis.com/auth/calendar.readonly")),
                (state::KEY_USER_NAME, Some("Rob")),
            ]).await.unwrap();
            state::mark_enabled_if_new(pool, 0).await.unwrap();
            Self { drive, google, manager, http: test_http(), _directory }
        }

        pub(crate) fn context(&self) -> ExportContext<'_> {
            ExportContext { pool: self.manager.pool(), http: &self.http, endpoints: &self.google.endpoints, to_local: |time| time.naive_utc() }
        }

        pub(crate) async fn add_meeting(&self, id: &str, title: &str) {
            sqlx::query("INSERT INTO meetings (id, title, created_at, updated_at) VALUES (?, ?, '2026-11-23T14:30:00+00:00', 'x')")
                .bind(id).bind(title).execute(self.manager.pool()).await.unwrap();
        }

        pub(crate) async fn add_line(&self, meeting: &str, id: &str, speaker: &str, text: &str) {
            sqlx::query("INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, speaker) VALUES (?, ?, ?, 'x', 1.0, ?)")
                .bind(id).bind(meeting).bind(text).bind(speaker).execute(self.manager.pool()).await.unwrap();
        }

        pub(crate) async fn set_summary(&self, meeting: &str, markdown: &str) {
            let stored = serde_json::json!({ "markdown": markdown }).to_string();
            sqlx::query("INSERT INTO summary_processes (meeting_id, status, created_at, updated_at, result) VALUES (?, 'completed', 'x', 'x', ?) \
                         ON CONFLICT(meeting_id) DO UPDATE SET result = excluded.result, status = 'completed'")
                .bind(meeting).bind(stored).execute(self.manager.pool()).await.unwrap();
        }

        /// A pass far enough in the future that every quiet period has ended.
        pub(crate) async fn settle(&self) -> Vec<String> {
            pass(&self.context(), Utc::now().timestamp() + 7200).await
        }

        pub(crate) fn paths(&self) -> Vec<String> {
            let drive = self.drive.lock().unwrap();
            let mut paths: Vec<String> = drive.files().iter().map(|(id, _)| drive.path(id)).collect();
            paths.sort();
            paths
        }

        pub(crate) fn requests(&self) -> usize {
            self.google.requests().len()
        }
    }

    #[tokio::test]
    async fn a_finished_meeting_with_one_other_person_is_filed_under_that_person() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget review").await;
        fixture.add_line("m", "t1", "You", "Shall we start?").await;
        fixture.add_line("m", "t2", "Alice Smith", "Yes.").await;
        fixture.set_summary("m", "## Decisions\n- Ship on Friday").await;

        assert_eq!(fixture.settle().await, ["m"]);
        assert_eq!(fixture.paths(), [
            "Meetily meetings/Alice Smith/2026-11-23 1430 Budget review - summary.md",
            "Meetily meetings/Alice Smith/2026-11-23 1430 Budget review - transcript.md",
        ]);
        let drive = fixture.drive.lock().unwrap();
        let transcript = drive.files().into_iter().find(|(_, file)| file.name.ends_with("transcript.md")).unwrap().1.clone();
        assert!(transcript.content.contains("[00:01] **Rob (You):** Shall we start?"));
        assert!(transcript.content.starts_with("---\ntitle: \"Budget review\"\nmeeting_id: \"m\"\ndate: 2026-11-23\ntime: \"14:30\"\nattendees:\n  - \"Alice Smith\"\n"));
        let summary = drive.files().into_iter().find(|(_, file)| file.name.ends_with("summary.md")).unwrap().1.clone();
        assert!(summary.content.contains("## Summary\n\n### Decisions\n- Ship on Friday"));
        drop(drive);

        let row = state::get(fixture.manager.pool(), "m").await.unwrap().unwrap();
        assert_eq!(row.status, "sent");
        assert_eq!(row.exported_version, Some(row.content_version));
        assert!(row.summary_link.unwrap().starts_with("https://drive.google.com/file/d/"));
    }

    #[tokio::test]
    async fn an_unchanged_meeting_costs_no_requests_and_a_changed_summary_rewrites_only_that_file() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget review").await;
        fixture.add_line("m", "t1", "Alice Smith", "Yes.").await;
        fixture.set_summary("m", "First version").await;
        fixture.settle().await;
        let after_first = fixture.requests();
        assert!(fixture.settle().await.is_empty());
        assert_eq!(fixture.requests(), after_first);

        fixture.set_summary("m", "Second version").await;
        assert_eq!(fixture.settle().await, ["m"]);
        let drive = fixture.drive.lock().unwrap();
        let files = drive.files();
        assert_eq!(files.len(), 2, "the file was updated in place, not uploaded again");
        let summary = files.iter().find(|(_, file)| file.name.ends_with("summary.md")).unwrap().1;
        let transcript = files.iter().find(|(_, file)| file.name.ends_with("transcript.md")).unwrap().1;
        assert!(summary.content.contains("Second version"));
        assert_eq!((summary.writes, transcript.writes), (2, 1));
    }

    #[tokio::test]
    async fn naming_the_speaker_later_moves_and_renames_the_same_files() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget review").await;
        fixture.add_line("m", "t1", "You", "Hello.").await;
        fixture.add_line("m", "t2", "Speaker 1", "Hi.").await;
        fixture.settle().await;
        // Nobody is known yet: the root folder.
        assert_eq!(fixture.paths(), ["Meetily meetings/2026-11-23 1430 Budget review - transcript.md"]);
        let id = fixture.drive.lock().unwrap().files()[0].0.clone();

        sqlx::query("UPDATE transcripts SET speaker = 'Alice Smith' WHERE id = 't2'").execute(fixture.manager.pool()).await.unwrap();
        sqlx::query("UPDATE meetings SET title = 'Q4 budget' WHERE id = 'm'").execute(fixture.manager.pool()).await.unwrap();
        fixture.settle().await;
        assert_eq!(fixture.paths(), ["Meetily meetings/Alice Smith/2026-11-23 1430 Q4 budget - transcript.md"]);
        // Same file ID: links shared earlier still work, and nothing was left behind.
        assert_eq!(fixture.drive.lock().unwrap().files()[0].0, id);
    }

    #[tokio::test]
    async fn several_people_share_a_folder_and_a_group_gets_its_own() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Planning").await;
        fixture.add_line("m", "t1", "Alice", "One.").await;
        fixture.add_line("m", "t2", "Bob", "Two.").await;
        fixture.settle().await;
        assert_eq!(fixture.paths(), ["Meetily meetings/Group meetings/2026-11-23 1430 Planning (Alice, Bob) - transcript.md"]);

        sqlx::query("INSERT INTO groups (id, name, normalized_name, created_at, updated_at) VALUES ('g', 'Weekly sync', 'weekly sync', 'x', 'x')").execute(fixture.manager.pool()).await.unwrap();
        sqlx::query("UPDATE meetings SET group_id = 'g' WHERE id = 'm'").execute(fixture.manager.pool()).await.unwrap();
        fixture.settle().await;
        assert_eq!(fixture.paths(), ["Meetily meetings/Weekly sync/2026-11-23 1430 Planning (Alice, Bob) - transcript.md"]);

        // One folder for everything when folders are switched off.
        state::save_settings(fixture.manager.pool(), true, false, "Meetily meetings").await.unwrap();
        sqlx::query("UPDATE meetings SET title = 'Planning 2' WHERE id = 'm'").execute(fixture.manager.pool()).await.unwrap();
        fixture.settle().await;
        assert_eq!(fixture.paths(), ["Meetily meetings/2026-11-23 1430 Planning 2 (Alice, Bob) - transcript.md"]);
    }

    #[tokio::test]
    async fn a_lost_response_does_not_leave_a_duplicate() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget").await;
        fixture.add_line("m", "t1", "Alice", "Yes.").await;
        fixture.drive.lock().unwrap().lose_next_create_response = true;
        fixture.settle().await;
        let row = state::get(fixture.manager.pool(), "m").await.unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.attempts, row.transcript_file_id), ("waiting", 1, None));
        assert_eq!(fixture.drive.lock().unwrap().files().len(), 1, "Drive has the file although the app never saw the answer");

        // The retry (after the delay) finds the file by its private tag and adopts it.
        pass(&fixture.context(), Utc::now().timestamp() + 7200 + 60).await;
        assert_eq!(fixture.drive.lock().unwrap().files().len(), 1);
        let row = state::get(fixture.manager.pool(), "m").await.unwrap().unwrap();
        assert_eq!(row.status, "sent");
        assert!(row.transcript_file_id.is_some());
    }

    #[tokio::test]
    async fn the_queue_survives_a_restart_and_reuses_existing_folders_without_stored_ids() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget").await;
        fixture.add_line("m", "t1", "Alice", "Yes.").await;
        fixture.settle().await;
        // As after a restart with a lost database row for the folders and files.
        sqlx::query("DELETE FROM drive_folders").execute(fixture.manager.pool()).await.unwrap();
        sqlx::query("UPDATE drive_export_state SET transcript_file_id = NULL, transcript_hash = NULL, exported_version = NULL").execute(fixture.manager.pool()).await.unwrap();
        fixture.settle().await;
        let drive = fixture.drive.lock().unwrap();
        assert_eq!(drive.objects.values().filter(|object| object.folder).count(), 2, "root and person folder were found again, not created again");
        assert_eq!(drive.files().len(), 1);
    }

    #[tokio::test]
    async fn a_file_the_user_filed_elsewhere_stays_there_and_a_trashed_one_is_not_taken_back() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget").await;
        fixture.add_line("m", "t1", "Alice", "Yes.").await;
        fixture.settle().await;
        let id = fixture.drive.lock().unwrap().files()[0].0.clone();
        {
            let mut drive = fixture.drive.lock().unwrap();
            drive.objects.insert("user-folder".into(), FakeObject { name: "My archive".into(), folder: true, ..Default::default() });
            drive.objects.get_mut(&id).unwrap().parent = Some("user-folder".into());
        }
        sqlx::query("UPDATE transcripts SET transcript = 'Yes, agreed.' WHERE id = 't1'").execute(fixture.manager.pool()).await.unwrap();
        fixture.settle().await;
        assert_eq!(fixture.paths(), ["My archive/2026-11-23 1430 Budget - transcript.md"]);
        assert!(fixture.drive.lock().unwrap().objects[&id].content.contains("Yes, agreed."));

        // In the trash: the app writes a new file and leaves the trashed one alone.
        fixture.drive.lock().unwrap().objects.get_mut(&id).unwrap().trashed = true;
        sqlx::query("UPDATE transcripts SET transcript = 'Final.' WHERE id = 't1'").execute(fixture.manager.pool()).await.unwrap();
        fixture.settle().await;
        let drive = fixture.drive.lock().unwrap();
        assert!(drive.objects[&id].trashed);
        assert!(!drive.objects[&id].content.contains("Final."));
        assert_eq!(drive.files().len(), 1);
        assert_eq!(drive.path(&drive.files()[0].0), "Meetily meetings/Alice/2026-11-23 1430 Budget - transcript.md");
    }

    #[tokio::test]
    async fn outages_are_retried_later_and_a_missing_permission_asks_for_the_user() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget").await;
        fixture.add_line("m", "t1", "Alice", "Yes.").await;
        fixture.drive.lock().unwrap().fail_next = vec![503];
        let now = Utc::now().timestamp() + 7200;
        pass(&fixture.context(), now).await;
        let row = state::get(fixture.manager.pool(), "m").await.unwrap().unwrap();
        assert_eq!((row.status.as_str(), row.next_attempt_at), ("waiting", now + 30));
        // Not before the delay has passed.
        assert!(pass(&fixture.context(), now + 29).await.is_empty());
        assert_eq!(pass(&fixture.context(), now + 30).await, ["m"]);
        assert_eq!(state::get(fixture.manager.pool(), "m").await.unwrap().unwrap().status, "sent");

        fixture.add_meeting("n", "Other").await;
        fixture.add_line("n", "t2", "Alice", "Hi.").await;
        fixture.drive.lock().unwrap().fail_next = vec![403];
        fixture.settle().await;
        let row = state::get(fixture.manager.pool(), "n").await.unwrap().unwrap();
        assert_eq!(row.status, "attention");
        assert!(row.last_error.unwrap().contains("refused"));
    }

    #[tokio::test]
    async fn a_rejected_token_is_refreshed_once_and_an_ended_sign_in_is_marked() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget").await;
        fixture.add_line("m", "t1", "Alice", "Yes.").await;
        // The token looks fresh but Google rejects it once: refresh, then succeed.
        fixture.drive.lock().unwrap().fail_next = vec![401];
        fixture.settle().await;
        assert_eq!(fixture.drive.lock().unwrap().token_refreshes, 1);
        assert_eq!(state::get(fixture.manager.pool(), "m").await.unwrap().unwrap().status, "sent");

        // Rejected again after a successful refresh: the sign-in itself is the problem.
        fixture.add_meeting("n", "Other").await;
        fixture.add_line("n", "t2", "Alice", "Hi.").await;
        fixture.drive.lock().unwrap().fail_next = vec![401, 401];
        fixture.settle().await;
        assert_eq!(state::get(fixture.manager.pool(), "n").await.unwrap().unwrap().status, "attention");
        assert!(oauth::status(fixture.manager.pool()).await.unwrap().needs_attention.is_some());
        // While the connection needs attention nothing is sent and Google is not asked.
        let before = fixture.requests();
        sqlx::query("UPDATE transcripts SET transcript = 'Changed.' WHERE id = 't1'").execute(fixture.manager.pool()).await.unwrap();
        assert!(fixture.settle().await.is_empty());
        assert_eq!(fixture.requests(), before);
    }

    #[tokio::test]
    async fn meetings_recorded_before_the_export_was_switched_on_wait_for_send_now() {
        let fixture = Fixture::new().await;
        // The export was switched on after this meeting was recorded.
        config_store::set(fixture.manager.pool(), "drive.enabled_since", &(Utc::now().timestamp()).to_string()).await.unwrap();
        fixture.add_meeting("old", "History").await;
        fixture.add_line("old", "t1", "Alice", "Long ago.").await;
        assert!(fixture.settle().await.is_empty());
        assert!(fixture.paths().is_empty());

        state::request_now(fixture.manager.pool(), "old", 1).await.unwrap();
        assert_eq!(fixture.settle().await, ["old"]);
        assert_eq!(fixture.paths().len(), 1);
        // Once sent, it stays in step with later edits.
        sqlx::query("UPDATE transcripts SET transcript = 'Edited.' WHERE id = 't1'").execute(fixture.manager.pool()).await.unwrap();
        assert_eq!(fixture.settle().await, ["old"]);

        // Switched off: nothing automatic, also for new meetings.
        state::save_settings(fixture.manager.pool(), false, true, "Meetily meetings").await.unwrap();
        sqlx::query("UPDATE transcripts SET transcript = 'Again.' WHERE id = 't1'").execute(fixture.manager.pool()).await.unwrap();
        assert!(fixture.settle().await.is_empty());
    }

    #[tokio::test]
    async fn a_meeting_still_in_the_chain_or_without_content_is_not_sent() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget").await;
        fixture.add_line("m", "t1", "Alice", "Live text.").await;
        let job = |stage: &str| crate::post_call::PostCallJobUpdate { meeting_id: "m".into(), stage: stage.into(), ..Default::default() };
        crate::post_call::PostCallRepository::save(fixture.manager.pool(), &job("enhancing")).await.unwrap();
        assert!(fixture.settle().await.is_empty());
        assert_eq!(fixture.requests(), 0);
        crate::post_call::PostCallRepository::save(fixture.manager.pool(), &job("done")).await.unwrap();
        assert_eq!(fixture.settle().await, ["m"]);

        // Only blank lines: nothing to send, and that is recorded as such.
        fixture.add_meeting("blank", "Silence").await;
        fixture.add_line("blank", "t9", "You", "   ").await;
        fixture.settle().await;
        assert_eq!(state::get(fixture.manager.pool(), "blank").await.unwrap().unwrap().status, "empty");
        assert_eq!(fixture.paths().len(), 1);
    }

    #[tokio::test]
    async fn the_worker_never_asks_drive_to_delete_or_trash() {
        let fixture = Fixture::new().await;
        fixture.add_meeting("m", "Budget").await;
        fixture.add_line("m", "t1", "Alice", "Yes.").await;
        fixture.set_summary("m", "Summary").await;
        fixture.settle().await;
        sqlx::query("DELETE FROM summary_processes WHERE meeting_id = 'm'").execute(fixture.manager.pool()).await.unwrap();
        sqlx::query("UPDATE transcripts SET speaker = 'Bob' WHERE id = 't1'").execute(fixture.manager.pool()).await.unwrap();
        fixture.settle().await;
        sqlx::query("DELETE FROM meetings WHERE id = 'm'").execute(fixture.manager.pool()).await.unwrap();
        fixture.settle().await;
        for (method, target, body) in fixture.google.requests() {
            assert_ne!(method, "DELETE", "{target}");
            assert!(!body.contains("\"trashed\""), "{target}");
        }
        // The summary file of the removed summary and the files of the deleted
        // meeting are still in Drive.
        assert_eq!(fixture.drive.lock().unwrap().files().len(), 2);
    }
}
