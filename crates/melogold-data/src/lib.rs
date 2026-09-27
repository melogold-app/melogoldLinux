//! Данные (docs/PROMPT.md §3 «Устройство кода»): база SQLite, библиотека, отложенные удаления.

pub mod database;
pub mod library;
pub mod overrides;
pub mod sync_store;

pub use database::{Database, DbError};
pub use library::{Change, Library};
pub use overrides::TrackOverride;
