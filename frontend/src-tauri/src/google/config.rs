//! The OAuth client the user registered in Google Cloud.

use serde::Deserialize;

/// The app can see and change only the files it created itself.
pub const SCOPE_DRIVE_FILE: &str = "https://www.googleapis.com/auth/drive.file";
/// Read-only: events are never created, changed or answered.
pub const SCOPE_CALENDAR_READONLY: &str = "https://www.googleapis.com/auth/calendar.readonly";
pub const REQUESTED_SCOPES: [&str; 2] = [SCOPE_DRIVE_FILE, SCOPE_CALENDAR_READONLY];

#[derive(Clone, PartialEq, Eq)]
pub struct GoogleClient {
    pub client_id: String,
    /// Google issues one for "Desktop app" clients and treats it as not confidential.
    pub client_secret: Option<String>,
}

impl std::fmt::Debug for GoogleClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GoogleClient")
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Deserialize)]
struct DownloadedClient {
    installed: Option<ClientSection>,
    web: Option<ClientSection>,
}

#[derive(Deserialize)]
struct ClientSection {
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// Reads the JSON file Google offers for download after creating a client.
pub fn parse_client_json(text: &str) -> Result<GoogleClient, String> {
    let downloaded: DownloadedClient = serde_json::from_str(text.trim())
        .map_err(|_| "This is not the JSON file Google offers for download. Choose the file that starts with \"client_secret_\".".to_string())?;
    if downloaded.installed.is_none() && downloaded.web.is_some() {
        return Err("This client has the type \"Web application\". Create a client of the type \"Desktop app\" instead; only that type can sign in from a desktop program.".to_string());
    }
    let section = downloaded
        .installed
        .ok_or_else(|| "The file does not describe a \"Desktop app\" client.".to_string())?;
    client_from_values(section.client_id.as_deref().unwrap_or(""), section.client_secret.as_deref())
}

/// Checks the two values when they are pasted instead of imported.
pub fn client_from_values(client_id: &str, client_secret: Option<&str>) -> Result<GoogleClient, String> {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return Err("The client ID is missing.".to_string());
    }
    if !client_id.ends_with(".apps.googleusercontent.com") || client_id.contains(char::is_whitespace) {
        return Err("A Google client ID ends with \".apps.googleusercontent.com\". Copy it from the client's page in Google Cloud.".to_string());
    }
    let client_secret = client_secret.map(str::trim).filter(|secret| !secret.is_empty());
    if client_secret.is_some_and(|secret| secret.contains(char::is_whitespace)) {
        return Err("The client secret must not contain spaces. Copy it again from Google Cloud.".to_string());
    }
    Ok(GoogleClient { client_id: client_id.to_string(), client_secret: client_secret.map(str::to_string) })
}

/// Enough of the client ID to recognise it, without showing all of it.
pub fn client_id_hint(client_id: &str) -> String {
    let name = client_id.trim_end_matches(".apps.googleusercontent.com");
    let head: String = name.chars().take(8).collect();
    format!("{head}….apps.googleusercontent.com")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_downloaded_desktop_client_file_is_accepted() {
        let client = parse_client_json(r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"p",
            "auth_uri":"https://accounts.google.com/o/oauth2/auth","token_uri":"https://oauth2.googleapis.com/token",
            "client_secret":"GOCSPX-secret","redirect_uris":["http://localhost"]}}"#).unwrap();
        assert_eq!(client.client_id, "123-abc.apps.googleusercontent.com");
        assert_eq!(client.client_secret.as_deref(), Some("GOCSPX-secret"));
        assert!(!format!("{client:?}").contains("GOCSPX-secret"));
    }

    #[test]
    fn wrong_files_and_wrong_client_types_are_explained() {
        let web = parse_client_json(r#"{"web":{"client_id":"1.apps.googleusercontent.com","client_secret":"s"}}"#).unwrap_err();
        assert!(web.contains("Desktop app"));
        assert!(parse_client_json("not json").unwrap_err().contains("client_secret_"));
        assert!(parse_client_json(r#"{"type":"service_account"}"#).unwrap_err().contains("Desktop app"));
        assert!(parse_client_json(r#"{"installed":{}}"#).unwrap_err().contains("missing"));
    }

    #[test]
    fn pasted_values_are_checked() {
        assert!(client_from_values(" 123-abc.apps.googleusercontent.com ", Some(" GOCSPX-x ")).is_ok());
        assert_eq!(client_from_values("123-abc.apps.googleusercontent.com", Some("  ")).unwrap().client_secret, None);
        assert!(client_from_values("my-project", None).is_err());
        assert!(client_from_values("123 abc.apps.googleusercontent.com", None).is_err());
        assert!(client_from_values("123.apps.googleusercontent.com", Some("two words")).is_err());
        assert_eq!(client_id_hint("1234567890-abcdefg.apps.googleusercontent.com"), "12345678….apps.googleusercontent.com");
    }
}
