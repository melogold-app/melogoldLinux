//! Доступ синхронизации к библиотеке (вариант со снимком, REWRITE §4.12a Android, Windows
//! `SyncStore.cs`): состояние (`binding`, `cursor`, `needsMerge`, `lastSyncAt`), снимок сервера
//! (`synced_*`), `sync_id` плейлистов и `sort_key` их треков. Всё — внутри одной транзакции
//! [`Library::sync`]; что поменялось в библиотеке, копится в [`SyncTx::changes`] и рассылается после.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use melogold_core::music::Track;
use rusqlite::{params, OptionalExtension, Transaction};

use crate::database::DbError;
use crate::library::{read_track, upsert_track, video_ids, Change, Library, TRACK_COLUMNS};
use crate::overrides::TrackOverride;

/// Плейлист, каким его знал сервер после прошлой синхронизации: треки в порядке сервера.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncedPlaylist {
    pub sync_id: String,
    pub name: Option<String>,
    pub thumbnail_url: Option<String>,
    pub video_ids: Vec<String>,
}

/// Свой плейлист для синхронизации.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistRecord {
    pub id: i64,
    pub sync_id: Option<String>,
    pub name: String,
    pub browse_id: Option<String>,
    pub thumbnail_url: Option<String>,
}

/// Лайк этого устройства: трек с метаданными и время лайка.
#[derive(Clone, Debug, PartialEq)]
pub struct LikeRecord {
    pub track: Track,
    pub liked_at: i64,
}

/// Прослушивание этого устройства, ещё не отправленное на сервер.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayRecord {
    pub event_id: String,
    pub video_id: String,
    pub played_at: i64,
    pub play_time_ms: i64,
}

/// «Убрать из истории» (`history.forget`) или «Очистить историю» (`history.clear`) для сервера.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryOpRecord {
    pub op_id: String,
    pub kind: String,
    pub video_id: Option<String>,
    pub events_before: i64,
}

/// Закладка этого устройства: альбом (`album`) или исполнитель и канал (`artist`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookmarkRecord {
    pub kind: String,
    pub browse_id: String,
    pub bookmarked_at: i64,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub thumbnail_url: Option<String>,
    pub year: Option<String>,
}

impl Library {
    /// Работа синхронизации одной транзакцией; изменения библиотеки рассылаются после неё.
    pub fn sync<T>(&self, work: impl FnOnce(&SyncTx) -> rusqlite::Result<T>) -> Result<T, DbError> {
        let changes = Cell::new(0u32);
        let result = self.database().write(|t| work(&SyncTx { t, changes: &changes }))?;
        let changes = Change(changes.get());
        if changes.has(Change::OVERRIDES) {
            self.reload_overrides();
        }
        if changes.0 != 0 {
            self.notify(changes);
        }
        Ok(result)
    }

    /// Значение состояния синхронизации вне транзакции.
    pub fn sync_state(&self, key: &str) -> Option<String> {
        self.database().read(|c| c.query_row("SELECT value FROM sync_state WHERE key = ?1", [key], |r| r.get(0)).optional()).ok().flatten()
    }
}

/// Операции синхронизации в транзакции.
pub struct SyncTx<'a> {
    pub(crate) t: &'a Transaction<'a>,
    changes: &'a Cell<u32>,
}

impl SyncTx<'_> {
    pub(crate) fn changed(&self, change: Change) {
        self.changes.set(self.changes.get() | change.0);
    }

    // ── состояние ──

    pub fn state(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.t.query_row("SELECT value FROM sync_state WHERE key = ?1", [key], |r| r.get(0)).optional()
    }

    pub fn set_state(&self, key: &str, value: Option<&str>) -> rusqlite::Result<()> {
        match value {
            Some(value) => self.t.execute("INSERT OR REPLACE INTO sync_state (key, value) VALUES (?1, ?2)", params![key, value])?,
            None => self.t.execute("DELETE FROM sync_state WHERE key = ?1", [key])?,
        };
        Ok(())
    }

    /// Другой аккаунт или сервер: снимок, `sync_id`, `sort_key` и состояние забываются. Свои
    /// прослушивания примет новый аккаунт, чужие — от прошлого — уходят (задание Windows 0002 §3.6).
    pub fn forget_binding(&self) -> rusqlite::Result<()> {
        self.t.execute_batch(
            "DELETE FROM synced_likes;
             DELETE FROM synced_playlists;
             DELETE FROM synced_bookmarks;
             DELETE FROM synced_overrides;
             UPDATE playlists SET sync_id = NULL;
             UPDATE playlist_items SET sort_key = NULL;
             DELETE FROM synced_lyrics;
             DELETE FROM synced_lyrics_pins;
             UPDATE play_events SET synced = 0 WHERE device_id IS NULL;
             DELETE FROM play_events WHERE device_id IS NOT NULL;
             DELETE FROM history_ops;
             DELETE FROM sync_state;",
        )?;
        self.changed(Change::HISTORY);
        Ok(())
    }

    // ── история (задание Windows 0002) ──

    /// Свои прослушивания, которых сервер ещё не видел, по времени.
    pub fn unsent_plays(&self) -> rusqlite::Result<Vec<PlayRecord>> {
        let mut statement = self.t.prepare(
            "SELECT event_id, video_id, played_at, play_time_ms FROM play_events WHERE synced = 0 AND device_id IS NULL ORDER BY played_at",
        )?;
        let rows = statement.query_map([], |r| {
            Ok(PlayRecord { event_id: r.get(0)?, video_id: r.get(1)?, played_at: r.get(2)?, play_time_ms: r.get(3)? })
        })?;
        rows.collect()
    }

    pub fn mark_play_sent(&self, event_id: &str) -> rusqlite::Result<()> {
        self.t.execute("UPDATE play_events SET synced = 1 WHERE event_id = ?1", [event_id])?;
        Ok(())
    }

    pub fn history_ops(&self) -> rusqlite::Result<Vec<HistoryOpRecord>> {
        let mut statement = self.t.prepare("SELECT op_id, kind, video_id, events_before FROM history_ops ORDER BY events_before")?;
        let rows = statement
            .query_map([], |r| Ok(HistoryOpRecord { op_id: r.get(0)?, kind: r.get(1)?, video_id: r.get(2)?, events_before: r.get(3)? }))?;
        rows.collect()
    }

    pub fn delete_history_op(&self, op_id: &str) -> rusqlite::Result<()> {
        self.t.execute("DELETE FROM history_ops WHERE op_id = ?1", [op_id])?;
        Ok(())
    }

    /// Накопленное время треков (для `play.baseline atLeast` при первой синхронизации).
    pub fn play_totals(&self) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut statement = self.t.prepare("SELECT video_id, total_play_ms FROM tracks WHERE total_play_ms > 0 ORDER BY video_id")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    /// Общее время трека с сервера (`playStats`): уже по всем устройствам.
    pub fn set_play_total(&self, video_id: &str, total_ms: i64) -> rusqlite::Result<()> {
        self.t.execute("UPDATE tracks SET total_play_ms = ?2 WHERE video_id = ?1", params![video_id, total_ms])?;
        self.changed(Change::HISTORY);
        Ok(())
    }

    /// Прослушивание с сервера: новое вставляется, своё вернувшееся (тот же `eventId`) не задваивается.
    pub fn insert_play(
        &self,
        event_id: &str,
        video_id: &str,
        played_at: i64,
        play_time_ms: i64,
        device_id: Option<&str>,
    ) -> rusqlite::Result<()> {
        let inserted = self.t.execute(
            "INSERT OR IGNORE INTO play_events (event_id, video_id, played_at, play_time_ms, synced, device_id) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            params![event_id, video_id, played_at, play_time_ms, device_id],
        )?;
        if inserted > 0 {
            self.changed(Change::HISTORY);
        }
        Ok(())
    }

    /// `playForgets`: события трека (или все при `*`) по `events_before` включительно удалены.
    pub fn forget_plays(&self, video_id: &str, events_before: i64) -> rusqlite::Result<()> {
        let removed = if video_id == "*" {
            self.t.execute("DELETE FROM play_events WHERE played_at <= ?1", [events_before])?
        } else {
            self.t.execute("DELETE FROM play_events WHERE video_id = ?1 AND played_at <= ?2", params![video_id, events_before])?
        };
        if removed > 0 {
            self.changed(Change::HISTORY);
        }
        Ok(())
    }

    // ── библиотека этого устройства ──

    pub fn likes(&self) -> rusqlite::Result<Vec<LikeRecord>> {
        let mut statement = self.t.prepare(&format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE liked_at IS NOT NULL ORDER BY liked_at"))?;
        let rows = statement.query_map([], |r| Ok(LikeRecord { track: read_track(r, 0)?, liked_at: r.get(12)? }))?;
        rows.collect()
    }

    pub fn playlists(&self) -> rusqlite::Result<Vec<PlaylistRecord>> {
        let mut statement = self.t.prepare("SELECT id, sync_id, name, browse_id, thumbnail_url FROM playlists ORDER BY created_at, id")?;
        let rows = statement.query_map([], playlist_record)?;
        rows.collect()
    }

    pub fn playlist_by_sync_id(&self, sync_id: &str) -> rusqlite::Result<Option<PlaylistRecord>> {
        self.t
            .query_row("SELECT id, sync_id, name, browse_id, thumbnail_url FROM playlists WHERE sync_id = ?1", [sync_id], playlist_record)
            .optional()
    }

    pub fn playlist_video_ids(&self, playlist_id: i64) -> rusqlite::Result<Vec<String>> {
        video_ids(self.t, playlist_id)
    }

    pub fn track(&self, video_id: &str) -> rusqlite::Result<Option<Track>> {
        self.t.query_row(&format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE video_id = ?1"), [video_id], |r| read_track(r, 0)).optional()
    }

    pub fn bookmarks(&self) -> rusqlite::Result<Vec<BookmarkRecord>> {
        let mut list: Vec<BookmarkRecord> = {
            let mut statement = self.t.prepare(
                "SELECT browse_id, bookmarked_at, title, artists_text, thumbnail_url, year FROM albums WHERE bookmarked_at IS NOT NULL",
            )?;
            let rows = statement.query_map([], |r| {
                Ok(BookmarkRecord {
                    kind: "album".into(),
                    browse_id: r.get(0)?,
                    bookmarked_at: r.get(1)?,
                    title: r.get(2)?,
                    subtitle: r.get(3)?,
                    thumbnail_url: r.get(4)?,
                    year: r.get(5)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut statement =
            self.t.prepare("SELECT browse_id, bookmarked_at, name, thumbnail_url FROM artists WHERE bookmarked_at IS NOT NULL")?;
        let rows = statement.query_map([], |r| {
            Ok(BookmarkRecord {
                kind: "artist".into(),
                browse_id: r.get(0)?,
                bookmarked_at: r.get(1)?,
                title: r.get(2)?,
                subtitle: None,
                thumbnail_url: r.get(3)?,
                year: None,
            })
        })?;
        for row in rows {
            list.push(row?);
        }
        Ok(list)
    }

    pub fn set_playlist_sync_id(&self, playlist_id: i64, sync_id: Option<&str>) -> rusqlite::Result<()> {
        self.t.execute("UPDATE playlists SET sync_id = ?2 WHERE id = ?1", params![playlist_id, sync_id])?;
        Ok(())
    }

    /// Треки плейлиста без ключа сервера: уйдут на сервер как новые (копия восстановления).
    pub fn clear_sort_keys(&self, playlist_id: i64) -> rusqlite::Result<()> {
        self.t.execute("UPDATE playlist_items SET sort_key = NULL WHERE playlist_id = ?1", [playlist_id])?;
        Ok(())
    }

    // ── снимок ──

    pub fn synced_likes(&self) -> rusqlite::Result<HashSet<String>> {
        let mut statement = self.t.prepare("SELECT video_id FROM synced_likes")?;
        let rows = statement.query_map([], |r| r.get(0))?;
        rows.collect()
    }

    pub fn synced_playlists(&self) -> rusqlite::Result<HashMap<String, SyncedPlaylist>> {
        let mut statement = self.t.prepare("SELECT sync_id, name, thumbnail_url, video_ids FROM synced_playlists")?;
        let rows = statement.query_map([], |r| {
            let ids: String = r.get(3)?;
            Ok(SyncedPlaylist {
                sync_id: r.get(0)?,
                name: r.get(1)?,
                thumbnail_url: r.get(2)?,
                video_ids: if ids.is_empty() { Vec::new() } else { ids.split('\n').map(str::to_owned).collect() },
            })
        })?;
        rows.map(|r| r.map(|p| (p.sync_id.clone(), p))).collect()
    }

    pub fn synced_bookmarks(&self) -> rusqlite::Result<HashSet<(String, String)>> {
        let mut statement = self.t.prepare("SELECT type, browse_id FROM synced_bookmarks")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    pub fn upsert_synced_playlist(&self, playlist: &SyncedPlaylist) -> rusqlite::Result<()> {
        self.t.execute(
            "INSERT OR REPLACE INTO synced_playlists (sync_id, name, thumbnail_url, video_ids) VALUES (?1, ?2, ?3, ?4)",
            params![playlist.sync_id, playlist.name, playlist.thumbnail_url, playlist.video_ids.join("\n")],
        )?;
        Ok(())
    }

    pub fn delete_synced_playlist(&self, sync_id: &str) -> rusqlite::Result<()> {
        self.t.execute("DELETE FROM synced_playlists WHERE sync_id = ?1", [sync_id])?;
        Ok(())
    }

    // ── применение ответа сервера (API §4.8) ──

    /// Трек, о котором говорит сервер: новый — с его метаданными, иначе заглушка с названием
    /// `videoId`. Заглушка прошлой синхронизации узнаёт настоящее название.
    pub fn ensure_track(&self, video_id: &str, metadata: Option<&Track>) -> rusqlite::Result<()> {
        let existing: Option<String> =
            self.t.query_row("SELECT title FROM tracks WHERE video_id = ?1", [video_id], |r| r.get(0)).optional()?;
        match (existing, metadata) {
            (Some(title), Some(track)) if title == video_id && !track.title.is_empty() && track.title != video_id => {
                upsert_track(self.t, track, false)?;
                self.changed(Change::TRACKS);
            }
            (Some(_), _) => {}
            (None, Some(track)) => {
                upsert_track(self.t, track, false)?;
                self.changed(Change::TRACKS);
            }
            (None, None) => {
                upsert_track(self.t, &Track { video_id: video_id.to_owned(), title: video_id.to_owned(), ..Default::default() }, true)?;
                self.changed(Change::TRACKS);
            }
        }
        Ok(())
    }

    pub fn insert_playlist(
        &self,
        name: &str,
        browse_id: Option<&str>,
        thumbnail_url: Option<&str>,
        sync_id: &str,
        created_at: i64,
    ) -> rusqlite::Result<i64> {
        self.t.execute(
            "INSERT INTO playlists (sync_id, name, browse_id, thumbnail_url, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![sync_id, name, browse_id, thumbnail_url, created_at],
        )?;
        self.changed(Change::PLAYLISTS);
        Ok(self.t.last_insert_rowid())
    }

    pub fn update_playlist(&self, id: i64, name: &str, thumbnail_url: Option<&str>) -> rusqlite::Result<()> {
        self.t.execute("UPDATE playlists SET name = ?2, thumbnail_url = ?3 WHERE id = ?1", params![id, name, thumbnail_url])?;
        self.changed(Change::PLAYLISTS);
        Ok(())
    }

    pub fn delete_playlist(&self, id: i64) -> rusqlite::Result<()> {
        self.t.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
        self.t.execute("DELETE FROM playlists WHERE id = ?1", [id])?;
        self.changed(Change::PLAYLISTS);
        Ok(())
    }

    /// Трек плейлиста с ключом сервера; место выставит [`SyncTx::reorder`].
    pub fn upsert_item(&self, playlist_id: i64, video_id: &str, sort_key: &str, added_at: i64) -> rusqlite::Result<()> {
        self.t.execute(
            "INSERT INTO playlist_items (playlist_id, video_id, position, sort_key, added_at) VALUES (?1, ?2, 2147483647, ?3, ?4)
             ON CONFLICT(playlist_id, video_id) DO UPDATE SET sort_key = excluded.sort_key",
            params![playlist_id, video_id, sort_key, added_at],
        )?;
        self.changed(Change::PLAYLISTS);
        Ok(())
    }

    pub fn delete_item(&self, playlist_id: i64, video_id: &str) -> rusqlite::Result<()> {
        self.t.execute("DELETE FROM playlist_items WHERE playlist_id = ?1 AND video_id = ?2", params![playlist_id, video_id])?;
        self.changed(Change::PLAYLISTS);
        Ok(())
    }

    /// Треки с ключом сервера — в его порядке (ключ, затем `videoId`, побайтово), за ними треки без
    /// ключа, как стояли. В снимок попадают только треки с ключом: добавленное здесь и ещё не
    /// отправленное сервер не знает.
    pub fn reorder(&self, playlist_id: i64) -> rusqlite::Result<()> {
        let items: Vec<(String, Option<String>, i64)> = {
            let mut statement =
                self.t.prepare("SELECT video_id, sort_key, position FROM playlist_items WHERE playlist_id = ?1 ORDER BY position")?;
            let rows = statement.query_map([playlist_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut keyed: Vec<&(String, Option<String>, i64)> = items.iter().filter(|i| i.1.is_some()).collect();
        keyed.sort_by(|a, b| {
            a.1.as_deref()
                .unwrap_or("")
                .as_bytes()
                .cmp(b.1.as_deref().unwrap_or("").as_bytes())
                .then_with(|| a.0.as_bytes().cmp(b.0.as_bytes()))
        });
        let ordered: Vec<&(String, Option<String>, i64)> = keyed.iter().copied().chain(items.iter().filter(|i| i.1.is_none())).collect();
        for (index, item) in ordered.iter().enumerate() {
            if item.2 != index as i64 {
                self.t.execute(
                    "UPDATE playlist_items SET position = ?3 WHERE playlist_id = ?1 AND video_id = ?2",
                    params![playlist_id, item.0, index as i64],
                )?;
            }
        }
        let playlist: Option<(Option<String>, String, Option<String>)> = self
            .t
            .query_row("SELECT sync_id, name, thumbnail_url FROM playlists WHERE id = ?1", [playlist_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        if let Some((Some(sync_id), name, thumbnail_url)) = playlist {
            self.upsert_synced_playlist(&SyncedPlaylist {
                sync_id,
                name: Some(name),
                thumbnail_url,
                video_ids: keyed.iter().map(|i| i.0.clone()).collect(),
            })?;
        }
        Ok(())
    }

    /// Лайк с сервера: время лайка — серверное, снятый лайк — снятие.
    pub fn set_like(&self, video_id: &str, liked_at: Option<i64>) -> rusqlite::Result<()> {
        self.t.execute("UPDATE tracks SET liked_at = ?2 WHERE video_id = ?1", params![video_id, liked_at])?;
        if liked_at.is_some() {
            self.t.execute("INSERT OR IGNORE INTO synced_likes (video_id) VALUES (?1)", [video_id])?;
        } else {
            self.t.execute("DELETE FROM synced_likes WHERE video_id = ?1", [video_id])?;
        }
        self.changed(Change::LIKES);
        Ok(())
    }

    /// Закладка с сервера: у новой — снимок названия и обложки.
    #[allow(clippy::too_many_arguments)]
    pub fn set_bookmark(
        &self,
        kind: &str,
        browse_id: &str,
        bookmarked_at: Option<i64>,
        title: Option<&str>,
        subtitle: Option<&str>,
        thumbnail_url: Option<&str>,
        year: Option<&str>,
    ) -> rusqlite::Result<()> {
        if kind == "album" {
            if bookmarked_at.is_some() {
                self.t.execute(
                    "INSERT OR IGNORE INTO albums (browse_id, title, artists_text, year, thumbnail_url) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![browse_id, title, subtitle, year, thumbnail_url],
                )?;
            }
            self.t.execute("UPDATE albums SET bookmarked_at = ?2 WHERE browse_id = ?1", params![browse_id, bookmarked_at])?;
        } else {
            if bookmarked_at.is_some() {
                self.t.execute(
                    "INSERT OR IGNORE INTO artists (browse_id, name, thumbnail_url) VALUES (?1, ?2, ?3)",
                    params![browse_id, title, thumbnail_url],
                )?;
            }
            self.t.execute("UPDATE artists SET bookmarked_at = ?2 WHERE browse_id = ?1", params![browse_id, bookmarked_at])?;
        }
        if bookmarked_at.is_some() {
            self.t.execute("INSERT OR IGNORE INTO synced_bookmarks (type, browse_id) VALUES (?1, ?2)", params![kind, browse_id])?;
        } else {
            self.t.execute("DELETE FROM synced_bookmarks WHERE type = ?1 AND browse_id = ?2", params![kind, browse_id])?;
        }
        self.changed(Change::BOOKMARKS);
        Ok(())
    }

    // ── свои названия треков (задание 0005) ──

    pub fn overrides(&self) -> rusqlite::Result<HashMap<String, TrackOverride>> {
        self.override_rows("SELECT video_id, title, artists_text, album_title, updated_at FROM track_overrides")
    }

    /// Снимок сервера: какие правки он знает (поля без времени).
    pub fn synced_overrides(&self) -> rusqlite::Result<HashMap<String, TrackOverride>> {
        self.override_rows("SELECT video_id, title, artists_text, album_title, 0 FROM synced_overrides")
    }

    fn override_rows(&self, sql: &str) -> rusqlite::Result<HashMap<String, TrackOverride>> {
        let mut statement = self.t.prepare(sql)?;
        let rows = statement.query_map([], |r| {
            Ok(TrackOverride {
                video_id: r.get(0)?,
                title: r.get(1)?,
                artists_text: r.get(2)?,
                album_title: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })?;
        rows.map(|r| r.map(|o| (o.video_id.clone(), o))).collect()
    }

    /// Правка с сервера: и здесь, и в снимке — такая, как у сервера; снятая — снята и там и там.
    pub fn apply_override(&self, row: &TrackOverride, deleted: bool) -> rusqlite::Result<()> {
        if deleted || row.is_empty() {
            self.t.execute("DELETE FROM track_overrides WHERE video_id = ?1", [&row.video_id])?;
            self.t.execute("DELETE FROM synced_overrides WHERE video_id = ?1", [&row.video_id])?;
        } else {
            self.t.execute(
                "INSERT OR REPLACE INTO track_overrides (video_id, title, artists_text, album_title, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![row.video_id, row.title, row.artists_text, row.album_title, row.updated_at],
            )?;
            self.t.execute(
                "INSERT OR REPLACE INTO synced_overrides (video_id, title, artists_text, album_title) VALUES (?1, ?2, ?3, ?4)",
                params![row.video_id, row.title, row.artists_text, row.album_title],
            )?;
        }
        self.changed(Change::OVERRIDES);
        Ok(())
    }
}

fn playlist_record(r: &rusqlite::Row) -> rusqlite::Result<PlaylistRecord> {
    Ok(PlaylistRecord { id: r.get(0)?, sync_id: r.get(1)?, name: r.get(2)?, browse_id: r.get(3)?, thumbnail_url: r.get(4)? })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    fn track(id: &str) -> Track {
        Track { video_id: id.into(), title: format!("Трек {id}"), ..Default::default() }
    }

    #[test]
    fn server_rows_and_snapshot() {
        let lib = Library::open(Database::in_memory().unwrap());
        lib.set_liked(&[track("a")], true).unwrap();
        let id = lib.create_playlist("Дорога", &[track("x"), track("y")]).unwrap();
        lib.sync(|tx| {
            assert_eq!(tx.likes()?.len(), 1);
            assert!(tx.synced_likes()?.is_empty());
            tx.set_playlist_sync_id(id, Some("p1"))?;
            // Сервер вернул свой порядок: y раньше x; заглушка z — новый трек.
            tx.ensure_track("z", None)?;
            tx.upsert_item(id, "y", "a0", 1)?;
            tx.upsert_item(id, "x", "a1", 1)?;
            tx.upsert_item(id, "z", "a2", 1)?;
            tx.reorder(id)?;
            tx.set_like("a", Some(5))?;
            Ok(())
        })
        .unwrap();
        let order: Vec<String> = lib.playlist_tracks(id).unwrap().into_iter().map(|t| t.video_id).collect();
        assert_eq!(order, ["y", "x", "z"]);
        lib.sync(|tx| {
            assert_eq!(tx.synced_playlists()?["p1"].video_ids, ["y", "x", "z"]);
            assert!(tx.synced_likes()?.contains("a"));
            assert_eq!(tx.track("z")?.unwrap().title, "z");
            // Заглушка узнаёт название, известный трек не перезаписывается.
            tx.ensure_track("z", Some(&track("z")))?;
            assert_eq!(tx.track("z")?.unwrap().title, "Трек z");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn plays_are_not_duplicated_and_binding_forgets_foreign_ones() {
        let lib = Library::open(Database::in_memory().unwrap());
        lib.record_play(&track("a"), 60_000, 1000).unwrap();
        lib.sync(|tx| {
            let plays = tx.unsent_plays()?;
            assert_eq!(plays.len(), 1);
            let own = plays[0].event_id.clone();
            // Своё вернулось с сервера — не задваивается; чужое — вставляется.
            tx.insert_play(&own, "a", 1000, 60_000, Some("dev-me"))?;
            tx.insert_play("e-2", "a", 2000, 30_000, Some("dev-other"))?;
            tx.mark_play_sent(&own)?;
            assert!(tx.unsent_plays()?.is_empty());
            tx.forget_binding()?;
            // Свои снова уйдут новому аккаунту, чужие — ушли.
            assert_eq!(tx.unsent_plays()?.len(), 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(lib.play_count().unwrap(), 1);
    }

    #[test]
    fn server_override_rows_update_display() {
        let lib = Library::open(Database::in_memory().unwrap());
        let row = TrackOverride::new("v", Some("Песня"), None, Some("Альбом"), 7);
        lib.sync(|tx| tx.apply_override(&row, false)).unwrap();
        assert_eq!(lib.display(&track("v")).title, "Песня");
        lib.sync(|tx| {
            assert!(tx.synced_overrides()?["v"].same_fields(&row));
            tx.apply_override(&row, true)
        })
        .unwrap();
        assert_eq!(lib.display(&track("v")).title, "Трек v");
    }
}
