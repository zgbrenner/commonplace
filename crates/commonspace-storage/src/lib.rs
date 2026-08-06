//! SQLite persistence for Commonspace.
//!
//! Rules:
//! - The database is owned by the Rust backend. The frontend never sends SQL;
//!   it calls typed commands that call typed methods here.
//! - Secrets are never stored in this database. Provider credentials belong
//!   to the provider CLIs; API keys belong to the OS credential vault.
//! - Migrations are versioned and forward-only in release use; every schema
//!   change ships as a new migration, never an edit of an old one.

mod migrations;
mod store;

pub use store::{
    ConversationRecord, MessageRecord, SearchHit, SessionRecord, Storage, StorageError, TaskRecord,
};
