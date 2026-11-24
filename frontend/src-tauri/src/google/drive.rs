//! Google Drive v3, as far as the export needs it: find or create a folder,
//! create a file, update a file in place, move a file between the app's own
//! folders. There is no delete and no trash call in this module.
//!
//! The sign-in asks for `drive.file`, so the API only ever shows this client
//! the files and folders it created. Every object the app creates carries
//! private `appProperties`; those let it find its own objects again when a
//! stored ID is lost, instead of creating duplicates.

use super::oauth::GoogleEndpoints;
use serde::Deserialize;

pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
pub const MARKDOWN_MIME: &str = "text/markdown";
/// Google's limit for a single multipart upload.
const MAX_UPLOAD_BYTES: usize = 5 * 1024 * 1024;

/// Keys of the private properties on the app's Drive objects.
pub const PROP_KIND: &str = "meetilyKind";
pub const PROP_MEETING: &str = "meetilyMeeting";
pub const PROP_NAME: &str = "meetilyName";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DriveFile {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub parents: Vec<String>,
    #[serde(default)]
    pub trashed: bool,
    #[serde(default, rename = "webViewLink")]
    pub web_view_link: Option<String>,
}

const FILE_FIELDS: &str = "id,name,parents,trashed,webViewLink";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveError {
    /// 401: the access token was not accepted. The caller marks the connection.
    Unauthorized,
    /// 403 that names a missing permission or a disabled API; the user has to act.
    Forbidden(String),
    /// 404 on an object addressed by ID.
    NotFound,
    /// Rate limit, 5xx, network: another attempt later can work.
    Transient(String),
    /// Anything else, with Google's message.
    Failed(String),
}

impl std::fmt::Display for DriveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("Google did not accept the sign-in."),
            Self::NotFound => formatter.write_str("The file or folder no longer exists in Drive."),
            Self::Forbidden(message) | Self::Transient(message) | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

pub struct DriveClient<'a> {
    pub http: &'a reqwest::Client,
    pub endpoints: &'a GoogleEndpoints,
    pub access_token: &'a str,
}

async fn read<T: serde::de::DeserializeOwned>(result: Result<reqwest::Response, reqwest::Error>) -> Result<T, DriveError> {
    let response = result.map_err(|error| DriveError::Transient(format!("Google Drive could not be reached: {}.", error.without_url())))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.is_success() {
        return serde_json::from_str(&body).map_err(|_| DriveError::Transient("Google Drive's answer could not be read.".into()));
    }
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let said = parsed["error"]["message"].as_str().unwrap_or("").to_string();
    let reason = parsed["error"]["errors"][0]["reason"].as_str().unwrap_or("");
    Err(match status.as_u16() {
        401 => DriveError::Unauthorized,
        404 => DriveError::NotFound,
        // Drive reports quota problems as 403 with these reasons; they pass.
        403 if matches!(reason, "rateLimitExceeded" | "userRateLimitExceeded") => DriveError::Transient("Google Drive is rate limiting the app.".into()),
        403 if said.contains("has not been used") || said.contains("is disabled") => DriveError::Forbidden(
            "The Google Drive API is not enabled in your Google Cloud project (step 1 of the setup guide).".into(),
        ),
        403 if reason == "storageQuotaExceeded" => DriveError::Forbidden("Your Google Drive is full.".into()),
        403 => DriveError::Forbidden(if said.is_empty() { "Google Drive refused the request.".into() } else { format!("Google Drive refused the request: {said}") }),
        429 | 500..=599 => DriveError::Transient(if said.is_empty() { format!("Google Drive answered with HTTP {}.", status.as_u16()) } else { format!("Google Drive is temporarily unavailable: {said}") }),
        code => DriveError::Failed(if said.is_empty() { format!("Google Drive answered with HTTP {code}.") } else { format!("Google Drive said: {said}") }),
    })
}

/// A value inside a Drive search string: backslash and single quote are escaped.
fn query_literal(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn multipart_body(metadata: &serde_json::Value, mime: &str, content: &[u8]) -> (String, Vec<u8>) {
    // Random, so the content cannot contain the boundary by accident or design.
    let boundary = format!("meetily-{}", uuid::Uuid::new_v4().simple());
    let mut body = Vec::with_capacity(content.len() + 512);
    body.extend_from_slice(format!("--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n").as_bytes());
    body.extend_from_slice(format!("--{boundary}\r\nContent-Type: {mime}; charset=UTF-8\r\n\r\n").as_bytes());
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/related; boundary={boundary}"), body)
}

impl DriveClient<'_> {
    fn api(&self, path: &str) -> String {
        format!("{}{path}", self.endpoints.drive_api)
    }

    fn upload(&self, path: &str) -> String {
        format!("{}{path}", self.endpoints.drive_upload)
    }

    /// The object with this ID, or `None` when it is gone or in the trash.
    /// A trashed object is treated as gone: the app never takes it back out.
    pub async fn get(&self, id: &str) -> Result<Option<DriveFile>, DriveError> {
        let request = self.http.get(self.api(&format!("/files/{id}"))).query(&[("fields", FILE_FIELDS)]).bearer_auth(self.access_token);
        match read::<DriveFile>(request.send().await).await {
            Ok(file) if file.trashed => Ok(None),
            Ok(file) => Ok(Some(file)),
            Err(DriveError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The app's own objects that carry all of these private properties.
    pub async fn find(&self, properties: &[(&str, &str)], folders: bool) -> Result<Vec<DriveFile>, DriveError> {
        let mut terms: Vec<String> = properties
            .iter()
            .map(|(key, value)| format!("appProperties has {{ key='{}' and value='{}' }}", query_literal(key), query_literal(value)))
            .collect();
        terms.push("trashed = false".into());
        terms.push(format!("mimeType {} '{FOLDER_MIME}'", if folders { "=" } else { "!=" }));
        #[derive(Deserialize)]
        struct Listing {
            #[serde(default)]
            files: Vec<DriveFile>,
        }
        let request = self
            .http
            .get(self.api("/files"))
            .query(&[
                ("q", terms.join(" and ").as_str()),
                ("fields", &format!("files({FILE_FIELDS})")),
                ("spaces", "drive"),
                ("orderBy", "createdTime"),
                ("pageSize", "10"),
            ])
            .bearer_auth(self.access_token);
        Ok(read::<Listing>(request.send().await).await?.files)
    }

    pub async fn create_folder(&self, name: &str, parent: Option<&str>, properties: &[(&str, &str)]) -> Result<DriveFile, DriveError> {
        let mut metadata = serde_json::json!({
            "name": name,
            "mimeType": FOLDER_MIME,
            "appProperties": properties.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect::<std::collections::BTreeMap<_, _>>(),
        });
        if let Some(parent) = parent {
            metadata["parents"] = serde_json::json!([parent]);
        }
        let request = self.http.post(self.api("/files")).query(&[("fields", FILE_FIELDS)]).bearer_auth(self.access_token).json(&metadata);
        read(request.send().await).await
    }

    pub async fn create_file(&self, name: &str, parent: &str, properties: &[(&str, &str)], content: &str) -> Result<DriveFile, DriveError> {
        check_size(content)?;
        let metadata = serde_json::json!({
            "name": name,
            "mimeType": MARKDOWN_MIME,
            "parents": [parent],
            "appProperties": properties.iter().map(|(key, value)| (key.to_string(), value.to_string())).collect::<std::collections::BTreeMap<_, _>>(),
        });
        let (content_type, body) = multipart_body(&metadata, MARKDOWN_MIME, content.as_bytes());
        let request = self
            .http
            .post(self.upload("/files"))
            .query(&[("uploadType", "multipart"), ("fields", FILE_FIELDS)])
            .bearer_auth(self.access_token)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
        read(request.send().await).await
    }

    /// Replaces the content and name of an existing file, and moves it when
    /// `move_from` and `parent` differ. The file keeps its ID, its link and its
    /// revision history.
    pub async fn update_file(&self, id: &str, name: &str, parent: &str, move_from: Option<&str>, content: &str) -> Result<DriveFile, DriveError> {
        check_size(content)?;
        let metadata = serde_json::json!({ "name": name, "mimeType": MARKDOWN_MIME });
        let (content_type, body) = multipart_body(&metadata, MARKDOWN_MIME, content.as_bytes());
        let mut query = vec![("uploadType", "multipart"), ("fields", FILE_FIELDS)];
        if let Some(previous) = move_from.filter(|previous| *previous != parent) {
            query.push(("addParents", parent));
            query.push(("removeParents", previous));
        }
        let request = self
            .http
            .patch(self.upload(&format!("/files/{id}")))
            .query(&query)
            .bearer_auth(self.access_token)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
        read(request.send().await).await
    }
}

fn check_size(content: &str) -> Result<(), DriveError> {
    if content.len() > MAX_UPLOAD_BYTES {
        return Err(DriveError::Failed("The file is larger than 5 MB, which this export does not send.".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::oauth::tests::{test_http, FakeGoogle};
    use std::sync::Arc;

    fn client<'a>(http: &'a reqwest::Client, google: &'a FakeGoogle) -> DriveClient<'a> {
        DriveClient { http, endpoints: &google.endpoints, access_token: "token" }
    }

    fn decoded(target: &str) -> String {
        url::form_urlencoded::parse(target.split_once('?').map(|(_, query)| query).unwrap_or("").as_bytes())
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join("&")
    }

    #[tokio::test]
    async fn a_file_is_created_with_its_content_parent_and_private_tags() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, r#"{"id":"f1","name":"n","parents":["p1"],"webViewLink":"https://drive.google.com/file/d/f1/view"}"#.into()))).await;
        let http = test_http();
        let created = client(&http, &google)
            .create_file("2026-11-23 1000 Sync - summary.md", "p1", &[(PROP_KIND, "summary"), (PROP_MEETING, "meeting-1")], "# Sync\n\nCafé — 会議\n")
            .await
            .unwrap();
        assert_eq!(created.id, "f1");
        assert_eq!(created.web_view_link.as_deref(), Some("https://drive.google.com/file/d/f1/view"));

        let (method, target, body) = google.requests().remove(0);
        assert_eq!(method, "POST");
        assert!(target.starts_with("/upload/drive/v3/files?"));
        assert!(decoded(&target).contains("uploadType=multipart"));
        // Two parts: the metadata as JSON, then the Markdown, byte for byte.
        let metadata_start = body.find('{').unwrap();
        let metadata_end = body[metadata_start..].find("\r\n--meetily-").unwrap() + metadata_start;
        let metadata: serde_json::Value = serde_json::from_str(&body[metadata_start..metadata_end]).unwrap();
        assert_eq!(metadata["name"], "2026-11-23 1000 Sync - summary.md");
        assert_eq!(metadata["parents"], serde_json::json!(["p1"]));
        assert_eq!(metadata["mimeType"], "text/markdown");
        assert_eq!(metadata["appProperties"], serde_json::json!({ "meetilyKind": "summary", "meetilyMeeting": "meeting-1" }));
        assert!(body.contains("Content-Type: text/markdown; charset=UTF-8\r\n\r\n# Sync\n\nCafé — 会議\n\r\n--meetily-"));
        assert!(body.trim_end().ends_with("--"));
    }

    #[tokio::test]
    async fn an_update_keeps_the_file_and_moves_it_only_when_the_folder_changes() {
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, r#"{"id":"f1","name":"n","parents":["new"]}"#.into()))).await;
        let http = test_http();
        let drive = client(&http, &google);
        drive.update_file("f1", "name.md", "same", Some("same"), "v2").await.unwrap();
        drive.update_file("f1", "name.md", "new", Some("old"), "v3").await.unwrap();
        drive.update_file("f1", "name.md", "new", None, "v4").await.unwrap();
        let requests = google.requests();
        assert!(requests.iter().all(|(method, target, _)| method == "PATCH" && target.starts_with("/upload/drive/v3/files/f1?")));
        assert!(!requests[0].1.contains("addParents") && !requests[0].1.contains("removeParents"));
        assert!(decoded(&requests[1].1).contains("addParents=new&removeParents=old"));
        assert!(!requests[2].1.contains("addParents"));
        assert!(requests[1].2.contains("\r\n\r\nv3\r\n--meetily-"));
    }

    #[tokio::test]
    async fn folders_are_found_by_private_tag_and_names_with_quotes_are_escaped() {
        let google = FakeGoogle::start(Arc::new(|method, _, _| {
            if method == "GET" { (200, r#"{"files":[{"id":"d1","name":"O'Brien","parents":["root"]}]}"#.into()) }
            else { (200, r#"{"id":"d2","name":"O'Brien","parents":["root"]}"#.into()) }
        })).await;
        let http = test_http();
        let drive = client(&http, &google);
        let found = drive.find(&[(PROP_KIND, "person"), (PROP_NAME, "o'brien")], true).await.unwrap();
        assert_eq!(found[0].id, "d1");
        let query = decoded(&google.requests()[0].1);
        assert!(query.contains(r"appProperties has { key='meetilyKind' and value='person' } and appProperties has { key='meetilyName' and value='o\'brien' } and trashed = false and mimeType = 'application/vnd.google-apps.folder'"), "{query}");

        drive.create_folder("O'Brien", Some("root"), &[(PROP_KIND, "person")]).await.unwrap();
        let (method, target, body) = google.requests().remove(1);
        assert_eq!(method, "POST");
        assert!(target.starts_with("/drive/v3/files?"));
        let metadata: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(metadata["mimeType"], FOLDER_MIME);
        assert_eq!(metadata["parents"], serde_json::json!(["root"]));
    }

    #[tokio::test]
    async fn a_trashed_or_missing_object_reads_as_gone() {
        let google = FakeGoogle::start(Arc::new(|_, target, _| {
            if target.contains("/files/trashed") { (200, r#"{"id":"trashed","trashed":true}"#.into()) }
            else if target.contains("/files/missing") { (404, r#"{"error":{"code":404,"message":"File not found: missing."}}"#.into()) }
            else { (200, r#"{"id":"alive","name":"x"}"#.into()) }
        })).await;
        let http = test_http();
        let drive = client(&http, &google);
        assert_eq!(drive.get("trashed").await.unwrap(), None);
        assert_eq!(drive.get("missing").await.unwrap(), None);
        assert_eq!(drive.get("alive").await.unwrap().unwrap().id, "alive");
    }

    #[tokio::test]
    async fn errors_are_sorted_into_retry_later_and_the_user_must_act() {
        for (status, body, expected) in [
            (401, r#"{"error":{"message":"Invalid Credentials"}}"#, DriveError::Unauthorized),
            (403, r#"{"error":{"message":"Google Drive API has not been used in project 1 before or it is disabled."}}"#, DriveError::Forbidden("The Google Drive API is not enabled in your Google Cloud project (step 1 of the setup guide).".into())),
            (403, r#"{"error":{"message":"quota","errors":[{"reason":"storageQuotaExceeded"}]}}"#, DriveError::Forbidden("Your Google Drive is full.".into())),
            (403, r#"{"error":{"message":"slow","errors":[{"reason":"userRateLimitExceeded"}]}}"#, DriveError::Transient("Google Drive is rate limiting the app.".into())),
            (429, "{}", DriveError::Transient("Google Drive answered with HTTP 429.".into())),
            (503, "not json", DriveError::Transient("Google Drive answered with HTTP 503.".into())),
            (400, r#"{"error":{"message":"Invalid field"}}"#, DriveError::Failed("Google Drive said: Invalid field".into())),
        ] {
            let body = body.to_string();
            let google = FakeGoogle::start(Arc::new(move |_, _, _| (status, body.clone()))).await;
            let http = test_http();
            assert_eq!(client(&http, &google).create_folder("x", None, &[]).await.unwrap_err(), expected, "{status}");
        }
        let http = test_http();
        let google = FakeGoogle::start(Arc::new(|_, _, _| (200, "{}".into()))).await;
        let too_large = "x".repeat(MAX_UPLOAD_BYTES + 1);
        assert!(matches!(client(&http, &google).create_file("big.md", "p", &[], &too_large).await, Err(DriveError::Failed(_))));
        assert!(google.requests().is_empty());
    }

    #[test]
    fn this_module_has_no_way_to_delete_anything() {
        // A guard for the safety rule, not for behaviour: if a delete or trash
        // call is ever added here, this test has to be changed on purpose.
        let source = include_str!("drive.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        assert!(!code.contains(".delete("));
        assert!(!code.contains("\"trashed\": true"));
        assert!(!code.contains("emptyTrash"));
    }
}
