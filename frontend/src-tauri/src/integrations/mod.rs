//! Connections to outside services. Their settings and secrets live in the
//! `integration_config` table, which only this crate reads.

pub mod config_store;
