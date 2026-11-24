//! Commands behind the Claude card of the Connections screen.
//!
//! The stored key is never returned: the card gets a hint (the last four
//! characters), whether a key is saved, and the result of a real check.

use super::claude_api::{self, DEFAULT_CLAUDE_MODEL};
use crate::database::repositories::setting::SettingsRepository;
use crate::state::AppState;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeStatus {
    pub key_saved: bool,
    /// The end of the saved key, to recognise it by.
    pub key_hint: Option<String>,
    /// Claude is the provider that writes summaries.
    pub selected: bool,
    /// The Claude model in use, or the default when another provider is selected.
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCheck {
    pub ok: bool,
    pub message: String,
}

fn key_hint(key: &str) -> Option<String> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    // A very short value would be shown whole; show nothing of it instead.
    Some(if key.chars().count() > 12 { format!("…{tail}") } else { "…".to_string() })
}

pub(crate) async fn read_status(pool: &sqlx::SqlitePool) -> Result<ClaudeStatus, String> {
    let key = SettingsRepository::get_api_key(pool, "claude")
        .await
        .map_err(|error| format!("Could not read the Claude key: {error}"))?
        .unwrap_or_default();
    let config = SettingsRepository::get_model_config(pool)
        .await
        .map_err(|error| format!("Could not read the summary model: {error}"))?;
    let selected = config.as_ref().is_some_and(|config| config.provider == "claude");
    let model = config
        .filter(|config| config.provider == "claude" && !config.model.trim().is_empty())
        .map(|config| config.model)
        .unwrap_or_else(|| DEFAULT_CLAUDE_MODEL.to_string());
    Ok(ClaudeStatus { key_saved: !key.trim().is_empty(), key_hint: key_hint(&key), selected, model })
}

#[tauri::command]
pub async fn claude_status(state: tauri::State<'_, AppState>) -> Result<ClaudeStatus, String> {
    read_status(state.db_manager.pool()).await
}

/// The models the saved key can use, for the card's model list. The key stays
/// native; without one (or offline) the built-in list is returned.
#[tauri::command]
pub async fn claude_models(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<crate::anthropic::anthropic::AnthropicModel>, String> {
    let key = SettingsRepository::get_api_key(state.db_manager.pool(), "claude")
        .await
        .map_err(|error| format!("Could not read the Claude key: {error}"))?;
    crate::anthropic::anthropic::get_anthropic_models(key).await
}

/// Tries a key with a real request. Without `api_key` the saved key is tried.
#[tauri::command]
pub async fn claude_check_key(
    state: tauri::State<'_, AppState>,
    api_key: Option<String>,
    model: Option<String>,
) -> Result<ClaudeCheck, String> {
    let pool = state.db_manager.pool();
    let key = match api_key.as_deref().map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => key.to_string(),
        None => SettingsRepository::get_api_key(pool, "claude")
            .await
            .map_err(|error| format!("Could not read the Claude key: {error}"))?
            .unwrap_or_default(),
    };
    let model = model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(DEFAULT_CLAUDE_MODEL);
    let check = claude_api::check_key(&reqwest::Client::new(), claude_api::CLAUDE_API_BASE, &key, model).await;
    Ok(ClaudeCheck { ok: check.ok, message: check.message })
}

/// Saves the key (when one is given) and makes Claude, with this model, the
/// provider that writes summaries. Other summary settings keep their values.
#[tauri::command]
pub async fn claude_connect<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    api_key: Option<String>,
    model: String,
) -> Result<ClaudeStatus, String> {
    let pool = state.db_manager.pool();
    let model = model.trim();
    if model.is_empty() {
        return Err("Choose a Claude model.".into());
    }
    if let Some(key) = api_key.as_deref().map(str::trim).filter(|key| !key.is_empty()) {
        SettingsRepository::save_api_key(pool, "claude", key)
            .await
            .map_err(|error| format!("Could not save the Claude key: {error}"))?;
    }
    select_provider(&app, pool, "claude", model).await?;
    read_status(pool).await
}

/// Removes the key. If Claude was writing summaries, the best downloaded local
/// model takes that role, so the next summary does not start with a failure.
#[tauri::command]
pub async fn claude_remove_key<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<ClaudeStatus, String> {
    let pool = state.db_manager.pool();
    SettingsRepository::delete_api_key(pool, "claude")
        .await
        .map_err(|error| format!("Could not remove the Claude key: {error}"))?;
    let was_selected = read_status(pool).await?.selected;
    if was_selected {
        let data_dir = crate::paths::install_data_root();
        if let Some(local) = crate::summary::summary_engine::models::first_downloaded_model(&data_dir) {
            select_provider(&app, pool, "builtin-ai", &local).await?;
        }
    }
    read_status(pool).await
}

async fn select_provider<R: Runtime>(
    app: &AppHandle<R>,
    pool: &sqlx::SqlitePool,
    provider: &str,
    model: &str,
) -> Result<(), String> {
    let current = SettingsRepository::get_model_config(pool)
        .await
        .map_err(|error| format!("Could not read the summary model: {error}"))?;
    let whisper_model = current.as_ref().map(|config| config.whisper_model.clone()).unwrap_or_else(|| "large-v3".to_string());
    let ollama_endpoint = current.as_ref().and_then(|config| config.ollama_endpoint.clone());
    // A saved output limit belongs to the Claude model it was set for; it is
    // kept only while it is still valid for the model chosen now.
    let summary_max_tokens = current
        .as_ref()
        .and_then(|config| config.summary_max_tokens)
        .filter(|tokens| provider == "claude" && super::llm_client::claude_max_tokens(model, Some(*tokens)).is_ok());
    SettingsRepository::save_model_config(pool, provider, model, &whisper_model, ollama_endpoint.as_deref(), summary_max_tokens)
        .await
        .map_err(|error| format!("Could not save the summary model: {error}"))?;
    // As when the model is changed in Settings: a running local model is stopped.
    if let Err(error) = crate::summary::summary_engine::client::shutdown_sidecar_gracefully().await {
        log::warn!("Could not stop the local summary engine after a provider change: {error}");
    }
    // The webview keeps a copy of the model setting; this is the event it listens to.
    let _ = app.emit("model-config-updated", serde_json::json!({
        "provider": provider,
        "model": model,
        "whisperModel": whisper_model,
        "ollamaEndpoint": ollama_endpoint,
        "summaryMaxTokens": summary_max_tokens,
    }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::config_store::test_pool;

    #[test]
    fn the_hint_shows_the_end_of_a_real_key_and_nothing_of_a_short_value() {
        assert_eq!(key_hint("sk-ant-api03-abcdefghijklmnop-WXYZ").as_deref(), Some("…WXYZ"));
        assert_eq!(key_hint("  short  ").as_deref(), Some("…"));
        assert_eq!(key_hint("   "), None);
    }

    #[tokio::test]
    async fn status_reports_the_key_without_returning_it() {
        let (_directory, manager) = test_pool().await;
        let pool = manager.pool();
        assert_eq!(read_status(pool).await.unwrap(), ClaudeStatus {
            key_saved: false, key_hint: None, selected: false, model: DEFAULT_CLAUDE_MODEL.into(),
        });

        SettingsRepository::save_api_key(pool, "claude", "sk-ant-api03-abcdefghijklmnop-WXYZ").await.unwrap();
        SettingsRepository::save_model_config(pool, "claude", "claude-sonnet-5-5", "large-v3", None, None).await.unwrap();
        let status = read_status(pool).await.unwrap();
        assert_eq!(status, ClaudeStatus {
            key_saved: true, key_hint: Some("…WXYZ".into()), selected: true, model: "claude-sonnet-5-5".into(),
        });
        assert!(!serde_json::to_string(&status).unwrap().contains("abcdefgh"));

        // Another provider selected: the card offers the default Claude model.
        SettingsRepository::save_model_config(pool, "builtin-ai", "qwen3.5:4b", "large-v3", None, None).await.unwrap();
        let status = read_status(pool).await.unwrap();
        assert!(status.key_saved && !status.selected);
        assert_eq!(status.model, DEFAULT_CLAUDE_MODEL);
    }
}
