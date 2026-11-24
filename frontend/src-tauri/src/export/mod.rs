//! Sends each meeting's summary and transcript to Google Drive as Markdown.
//! See `docs/HOMEBREW_DRIVE_EXPORT.md`.

pub mod commands;
pub mod loader;
pub mod markdown;
pub mod naming;
pub mod state;
pub mod worker;
