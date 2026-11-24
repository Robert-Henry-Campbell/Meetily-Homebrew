//! Google sign-in for the desktop app, and the API clients built on it.
//!
//! The user registers their own OAuth client in Google Cloud ("Desktop app")
//! and gives it to this app; the app then signs in through the system browser
//! with a loopback redirect, PKCE and a `state` value. See
//! `docs/HOMEBREW_CONNECTIONS.md`.

pub mod commands;
pub mod config;
pub mod drive;
pub mod oauth;
