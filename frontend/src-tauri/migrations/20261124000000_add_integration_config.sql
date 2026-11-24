-- Settings and secrets of outside services (Google sign-in). Read and written
-- from Rust only: no command returns a stored secret to the webview.
-- New table only; `settings` keeps its shape.
CREATE TABLE IF NOT EXISTS integration_config (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  INTEGER NOT NULL
);
