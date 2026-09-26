//! Файл библиотеки: SQLite в режиме WAL (Windows `LibraryDatabase.cs`, схема v6).
//!
//! Схема повторяет Windows, чтобы правила синка, истории и текстов переносились один в один: треки,
//! лайки, закладки, плейлисты с `sort_key` и плотной `position`, прослушивания с устройством,
//! счётчики, тексты, загрузки, скрытое, состояние синхронизации и её снимок. У Linux пользователей
//! прежних версий нет — новая база создаётся сразу в схеме 6; следующие версии — миграциями.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, Transaction};

pub const SCHEMA_VERSION: i32 = 6;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("библиотека создана более новой версией Melogold (схема {0})")]
    TooNew(i32),
}

pub struct Database {
    connection: Mutex<Connection>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Database, DbError> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let connection = Connection::open(path)?;
        Self::prepare(connection, true)
    }

    pub fn in_memory() -> Result<Database, DbError> {
        Self::prepare(Connection::open_in_memory()?, false)
    }

    fn prepare(connection: Connection, wal: bool) -> Result<Database, DbError> {
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        if wal {
            connection.pragma_update(None, "journal_mode", "WAL")?;
            connection.pragma_update(None, "synchronous", "NORMAL")?;
        }
        connection.pragma_update(None, "foreign_keys", true)?;
        let db = Database { connection: Mutex::new(connection) };
        db.migrate()?;
        Ok(db)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Запись одной транзакцией.
    pub fn write<T>(&self, work: impl FnOnce(&Transaction) -> rusqlite::Result<T>) -> Result<T, DbError> {
        let mut connection = self.lock();
        let transaction = connection.transaction()?;
        let result = work(&transaction)?;
        transaction.commit()?;
        Ok(result)
    }

    pub fn read<T>(&self, work: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, DbError> {
        let connection = self.lock();
        Ok(work(&connection)?)
    }

    fn migrate(&self) -> Result<(), DbError> {
        let mut connection = self.lock();
        let version: i32 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(DbError::TooNew(version));
        }
        if version < 1 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            transaction.commit()?;
        }
        Ok(())
    }

    /// Копия файла базы (автокопия перед заменой, «Сохранить копию»).
    pub fn backup_to(&self, destination: &Path) -> Result<(), DbError> {
        let connection = self.lock();
        let mut target = Connection::open(destination)?;
        let backup = rusqlite::backup::Backup::new(&connection, &mut target)?;
        backup.run_to_completion(256, std::time::Duration::ZERO, None)?;
        Ok(())
    }
}

/// Схема 6 одним файлом — то, к чему пришла база Windows за версии 1–6.
const SCHEMA: &str = "
CREATE TABLE tracks (
    video_id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    artists_text TEXT,
    artists_json TEXT,
    album_id TEXT,
    album_title TEXT,
    duration_ms INTEGER,
    duration_text TEXT,
    thumbnail_url TEXT,
    explicit INTEGER NOT NULL DEFAULT 0,
    video_type TEXT,
    metadata_stub INTEGER NOT NULL DEFAULT 0,
    liked_at INTEGER,
    total_play_ms INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);
CREATE INDEX tracks_liked ON tracks(liked_at) WHERE liked_at IS NOT NULL;

CREATE TABLE albums (
    browse_id TEXT PRIMARY KEY,
    title TEXT,
    artists_text TEXT,
    year TEXT,
    thumbnail_url TEXT,
    playlist_id TEXT,
    bookmarked_at INTEGER
);

CREATE TABLE artists (
    browse_id TEXT PRIMARY KEY,
    name TEXT,
    thumbnail_url TEXT,
    is_channel INTEGER NOT NULL DEFAULT 0,
    bookmarked_at INTEGER
);

CREATE TABLE playlists (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sync_id TEXT UNIQUE,
    name TEXT NOT NULL,
    browse_id TEXT,
    thumbnail_url TEXT,
    created_at INTEGER NOT NULL
);

CREATE TABLE playlist_items (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    video_id TEXT NOT NULL REFERENCES tracks(video_id),
    position INTEGER NOT NULL,
    sort_key TEXT,
    added_at INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, video_id)
);
CREATE INDEX playlist_items_order ON playlist_items(playlist_id, position);

CREATE TABLE play_events (
    event_id TEXT PRIMARY KEY,
    video_id TEXT NOT NULL,
    played_at INTEGER NOT NULL,
    play_time_ms INTEGER NOT NULL,
    synced INTEGER NOT NULL DEFAULT 0,
    device_id TEXT
);
CREATE INDEX play_events_time ON play_events(played_at);
CREATE INDEX play_events_video ON play_events(video_id, played_at);

CREATE TABLE history_ops (op_id TEXT PRIMARY KEY, kind TEXT NOT NULL, video_id TEXT, events_before INTEGER NOT NULL);

CREATE TABLE search_history (
    query TEXT PRIMARY KEY,
    searched_at INTEGER NOT NULL
);

CREATE TABLE lyrics (
    video_id TEXT PRIMARY KEY,
    synced TEXT,
    plain TEXT,
    source TEXT,
    plain_source TEXT,
    offset_ms INTEGER NOT NULL DEFAULT 0,
    language TEXT,
    chosen INTEGER NOT NULL DEFAULT 0,
    fetched_at INTEGER NOT NULL
);

CREATE TABLE content_blocks (
    type TEXT NOT NULL,
    key TEXT NOT NULL,
    level TEXT NOT NULL,
    title TEXT,
    subtitle TEXT,
    thumbnail_url TEXT,
    blocked_at INTEGER NOT NULL,
    PRIMARY KEY (type, key)
);

CREATE TABLE downloads (video_id TEXT PRIMARY KEY, added_at INTEGER NOT NULL);

CREATE TABLE app_state (key TEXT PRIMARY KEY, value TEXT);

CREATE TABLE sync_state (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE synced_likes (video_id TEXT PRIMARY KEY);
CREATE TABLE synced_bookmarks (type TEXT NOT NULL, browse_id TEXT NOT NULL, PRIMARY KEY (type, browse_id));
CREATE TABLE synced_playlists (
    sync_id TEXT PRIMARY KEY,
    name TEXT,
    thumbnail_url TEXT,
    video_ids TEXT NOT NULL DEFAULT ''
);
CREATE TABLE synced_lyrics (video_id TEXT PRIMARY KEY, rev INTEGER NOT NULL, hash TEXT NOT NULL);
";
