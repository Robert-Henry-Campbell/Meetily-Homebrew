//! Key-value storage for integration settings and secrets.
//!
//! The values include OAuth tokens and a client secret. They are stored as the
//! app stores its API keys: in the local database file. No Tauri command hands
//! a value from this table to the webview; callers return derived facts
//! (connected or not, the account's address) instead.

use sqlx::SqlitePool;

pub async fn get(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>("SELECT value FROM integration_config WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
}

/// Writes several keys in one transaction. `None` removes the key.
pub async fn set_many(pool: &SqlitePool, entries: &[(&str, Option<&str>)]) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().timestamp();
    let mut transaction = pool.begin().await?;
    for (key, value) in entries {
        match value {
            Some(value) => {
                sqlx::query(
                    "INSERT INTO integration_config (key, value, updated_at) VALUES (?, ?, ?) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                )
                .bind(key)
                .bind(value)
                .bind(now)
                .execute(&mut *transaction)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM integration_config WHERE key = ?")
                    .bind(key)
                    .execute(&mut *transaction)
                    .await?;
            }
        }
    }
    transaction.commit().await
}

pub async fn set(pool: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    set_many(pool, &[(key, Some(value))]).await
}

pub async fn remove(pool: &SqlitePool, key: &str) -> Result<(), sqlx::Error> {
    set_many(pool, &[(key, None)]).await
}

#[cfg(test)]
pub(crate) async fn test_pool() -> (tempfile::TempDir, crate::database::manager::DatabaseManager) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("meetings.sqlite");
    let legacy = directory.path().join("absent.db");
    let manager = crate::database::manager::DatabaseManager::new(path.to_str().unwrap(), legacy.to_str().unwrap())
        .await
        .unwrap();
    (directory, manager)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn values_are_written_replaced_and_removed_together() {
        let (_directory, manager) = test_pool().await;
        let pool = manager.pool();
        assert_eq!(get(pool, "google.client_id").await.unwrap(), None);

        set_many(pool, &[("google.client_id", Some("id-1")), ("google.refresh_token", Some("r-1"))]).await.unwrap();
        set(pool, "google.client_id", "id-2").await.unwrap();
        assert_eq!(get(pool, "google.client_id").await.unwrap().as_deref(), Some("id-2"));

        set_many(pool, &[("google.refresh_token", None), ("google.account_email", Some("a@example.com"))]).await.unwrap();
        assert_eq!(get(pool, "google.refresh_token").await.unwrap(), None);
        assert_eq!(get(pool, "google.account_email").await.unwrap().as_deref(), Some("a@example.com"));
        remove(pool, "google.account_email").await.unwrap();
        assert_eq!(get(pool, "google.account_email").await.unwrap(), None);
    }
}
