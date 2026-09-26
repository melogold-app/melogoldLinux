//! Библиотека (Windows `Library.cs`): треки, Избранное, закладки альбомов и исполнителей, свои
//! плейлисты, история, поиск, скрытое, загрузки, «Все треки». Каждое изменение — событие [`Change`]:
//! экраны и синхронизация подписаны на него.

use std::collections::HashSet;
use std::sync::Arc;

use melogold_core::music::{AlbumItem, ArtistItem, ArtistRef, Track};
use melogold_core::text::{format_duration, now_ms, parse_duration};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

use crate::database::{Database, DbError};

/// Что изменилось — битами, как у Windows `LibraryChange`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Change(pub u32);

impl Change {
    pub const LIKES: Change = Change(1);
    pub const PLAYLISTS: Change = Change(2);
    pub const BOOKMARKS: Change = Change(4);
    pub const HISTORY: Change = Change(8);
    pub const TRACKS: Change = Change(16);
    pub const SEARCHES: Change = Change(32);
    pub const BLOCKS: Change = Change(64);
    pub const LYRICS: Change = Change(128);
    pub const DOWNLOADS: Change = Change(256);

    pub fn has(self, other: Change) -> bool {
        self.0 & other.0 != 0
    }
}

/// Свой плейлист: `sync_id` — UUID сервера, если плейлист синхронизирован.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalPlaylist {
    pub id: i64,
    pub sync_id: Option<String>,
    pub name: String,
    pub browse_id: Option<String>,
    pub thumbnail_url: Option<String>,
    pub created_at: i64,
    pub track_count: i64,
    /// До четырёх обложек для плитки.
    pub mosaic: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    pub track: Track,
    pub played_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TopEntry {
    pub track: Track,
    pub play_time_ms: i64,
}

/// Трек «Всех треков»: когда слушали последний раз и сколько всего.
#[derive(Clone, Debug, PartialEq)]
pub struct AllTracksEntry {
    pub track: Track,
    pub last_played_at: Option<i64>,
    pub play_time_ms: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub likes: i64,
    pub playlists: i64,
    pub albums: i64,
    pub artists: i64,
}

type Listener = Box<dyn Fn(Change) + Send + Sync>;

pub struct Library {
    db: Database,
    listeners: std::sync::Mutex<Vec<Listener>>,
}

const TRACK_COLUMNS: &str =
    "video_id, title, artists_text, artists_json, album_id, album_title, duration_ms, duration_text, thumbnail_url, explicit, video_type, metadata_stub, liked_at, total_play_ms";

fn prefixed(prefix: &str) -> String {
    TRACK_COLUMNS.split(", ").map(|c| format!("{prefix}.{c}")).collect::<Vec<_>>().join(", ")
}

fn read_track(row: &Row, o: usize) -> rusqlite::Result<Track> {
    let artists_json: Option<String> = row.get(o + 3)?;
    let artists: Vec<ArtistRef> = artists_json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
    Ok(Track {
        video_id: row.get(o)?,
        title: row.get(o + 1)?,
        artists_text: row.get(o + 2)?,
        artists,
        album_id: row.get(o + 4)?,
        album_title: row.get(o + 5)?,
        duration_ms: row.get(o + 6)?,
        duration_text: row.get(o + 7)?,
        thumbnail_url: row.get(o + 8)?,
        explicit: row.get::<_, i64>(o + 9)? != 0,
        video_type: row.get(o + 10)?,
        ..Default::default()
    })
}

fn tracks(c: &Connection, sql: &str, params: impl rusqlite::Params) -> rusqlite::Result<Vec<Track>> {
    let mut statement = c.prepare(sql)?;
    let rows = statement.query_map(params, |r| read_track(r, 0))?;
    rows.collect()
}

/// Вставить трек или обновить его сведения: пустые поля новой версии не затирают известные,
/// заглушка (название = videoId) получает настоящее название.
pub(crate) fn upsert_track(t: &Transaction, track: &Track, stub: bool) -> rusqlite::Result<()> {
    let artists_json = (!track.artists.is_empty()).then(|| serde_json::to_string(&track.artists).unwrap_or_default());
    let duration_ms = track.duration_ms.or_else(|| parse_duration(track.duration_text.as_deref()));
    let duration_text = track.duration_text.clone().or_else(|| track.duration_ms.map(format_duration));
    t.execute(
        "INSERT INTO tracks (video_id, title, artists_text, artists_json, album_id, album_title, duration_ms, duration_text,
                             thumbnail_url, explicit, video_type, metadata_stub, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(video_id) DO UPDATE SET
             title = CASE WHEN ?12 = 0 THEN excluded.title ELSE tracks.title END,
             artists_text = COALESCE(excluded.artists_text, tracks.artists_text),
             artists_json = COALESCE(excluded.artists_json, tracks.artists_json),
             album_id = COALESCE(excluded.album_id, tracks.album_id),
             album_title = COALESCE(excluded.album_title, tracks.album_title),
             duration_ms = COALESCE(excluded.duration_ms, tracks.duration_ms),
             duration_text = COALESCE(excluded.duration_text, tracks.duration_text),
             thumbnail_url = COALESCE(excluded.thumbnail_url, tracks.thumbnail_url),
             explicit = MAX(excluded.explicit, tracks.explicit),
             video_type = COALESCE(tracks.video_type, excluded.video_type),
             metadata_stub = CASE WHEN ?12 = 0 THEN 0 ELSE tracks.metadata_stub END",
        params![
            track.video_id,
            track.title,
            track.artists_text,
            artists_json,
            track.album_id,
            track.album_title,
            duration_ms,
            duration_text,
            track.thumbnail_url,
            track.explicit as i64,
            track.video_type,
            stub as i64,
            now_ms()
        ],
    )?;
    Ok(())
}

fn truncate_utf16(value: &str, max: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= max
        })
        .collect()
}

fn new_id() -> String {
    // UUID v4 без внешних зависимостей: случайные байты ядра.
    let mut bytes = [0u8; 16];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = file.read_exact(&mut bytes);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

impl Library {
    pub fn open(db: Database) -> Arc<Library> {
        Arc::new(Library { db, listeners: std::sync::Mutex::default() })
    }

    pub fn database(&self) -> &Database {
        &self.db
    }

    pub fn subscribe(&self, listener: impl Fn(Change) + Send + Sync + 'static) {
        if let Ok(mut listeners) = self.listeners.lock() {
            listeners.push(Box::new(listener));
        }
    }

    pub fn notify(&self, change: Change) {
        if let Ok(listeners) = self.listeners.lock() {
            for listener in listeners.iter() {
                listener(change);
            }
        }
    }

    // ── треки ──

    pub fn save_tracks(&self, list: &[Track]) -> Result<(), DbError> {
        self.db.write(|t| list.iter().try_for_each(|track| upsert_track(t, track, false)))
    }

    pub fn track(&self, video_id: &str) -> Result<Option<Track>, DbError> {
        self.db.read(|c| {
            c.query_row(&format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE video_id = ?1"), [video_id], |r| read_track(r, 0)).optional()
        })
    }

    // ── Избранное ──

    pub fn is_liked(&self, video_id: &str) -> Result<bool, DbError> {
        self.db.read(|c| {
            Ok(c.query_row("SELECT liked_at IS NOT NULL FROM tracks WHERE video_id = ?1", [video_id], |r| r.get::<_, bool>(0))
                .optional()?
                .unwrap_or(false))
        })
    }

    pub fn liked_ids(&self) -> Result<HashSet<String>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare("SELECT video_id FROM tracks WHERE liked_at IS NOT NULL")?;
            let ids = statement.query_map([], |r| r.get(0))?.collect();
            ids
        })
    }

    pub fn favorites(&self) -> Result<Vec<Track>, DbError> {
        self.db.read(|c| tracks(c, &format!("SELECT {TRACK_COLUMNS} FROM tracks WHERE liked_at IS NOT NULL ORDER BY liked_at DESC"), []))
    }

    /// ♡ у одного или нескольких треков (действия с выделенным): одна запись, одно уведомление.
    pub fn set_liked(&self, list: &[Track], liked: bool) -> Result<(), DbError> {
        if list.is_empty() {
            return Ok(());
        }
        let now = now_ms();
        self.db.write(|t| {
            for track in list {
                upsert_track(t, track, false)?;
                if liked {
                    t.execute("UPDATE tracks SET liked_at = COALESCE(liked_at, ?2) WHERE video_id = ?1", params![track.video_id, now])?;
                } else {
                    t.execute("UPDATE tracks SET liked_at = NULL WHERE video_id = ?1", [&track.video_id])?;
                }
            }
            Ok(())
        })?;
        self.notify(Change::LIKES);
        Ok(())
    }

    // ── альбомы и исполнители ──

    pub fn is_album_saved(&self, browse_id: &str) -> Result<bool, DbError> {
        self.db.read(|c| {
            Ok(c.query_row("SELECT bookmarked_at IS NOT NULL FROM albums WHERE browse_id = ?1", [browse_id], |r| r.get::<_, bool>(0))
                .optional()?
                .unwrap_or(false))
        })
    }

    pub fn set_album_saved(&self, album: &AlbumItem, saved: bool) -> Result<(), DbError> {
        let at = saved.then(now_ms);
        self.db.write(|t| {
            t.execute(
                "INSERT INTO albums (browse_id, title, artists_text, year, thumbnail_url, playlist_id, bookmarked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(browse_id) DO UPDATE SET
                     title = COALESCE(excluded.title, albums.title),
                     artists_text = COALESCE(excluded.artists_text, albums.artists_text),
                     year = COALESCE(excluded.year, albums.year),
                     thumbnail_url = COALESCE(excluded.thumbnail_url, albums.thumbnail_url),
                     playlist_id = COALESCE(excluded.playlist_id, albums.playlist_id),
                     bookmarked_at = CASE WHEN ?7 IS NULL THEN NULL ELSE COALESCE(albums.bookmarked_at, ?7) END",
                params![album.browse_id, album.title, album.artists_text, album.year, album.thumbnail_url, album.playlist_id, at],
            )?;
            Ok(())
        })?;
        self.notify(Change::BOOKMARKS);
        Ok(())
    }

    pub fn saved_albums(&self) -> Result<Vec<AlbumItem>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare(
                "SELECT browse_id, title, artists_text, year, thumbnail_url, playlist_id FROM albums WHERE bookmarked_at IS NOT NULL ORDER BY bookmarked_at DESC",
            )?;
            let rows = statement.query_map([], |r| {
                let browse_id: String = r.get(0)?;
                Ok(AlbumItem {
                    title: r.get::<_, Option<String>>(1)?.unwrap_or_else(|| browse_id.clone()),
                    browse_id,
                    artists_text: r.get(2)?,
                    year: r.get(3)?,
                    thumbnail_url: r.get(4)?,
                    playlist_id: r.get(5)?,
                    ..Default::default()
                })
            })?;
            rows.collect()
        })
    }

    pub fn is_artist_saved(&self, browse_id: &str) -> Result<bool, DbError> {
        self.db.read(|c| {
            Ok(c.query_row("SELECT bookmarked_at IS NOT NULL FROM artists WHERE browse_id = ?1", [browse_id], |r| r.get::<_, bool>(0))
                .optional()?
                .unwrap_or(false))
        })
    }

    pub fn set_artist_saved(&self, artist: &ArtistItem, saved: bool) -> Result<(), DbError> {
        let at = saved.then(now_ms);
        self.db.write(|t| {
            t.execute(
                "INSERT INTO artists (browse_id, name, thumbnail_url, is_channel, bookmarked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(browse_id) DO UPDATE SET
                     name = COALESCE(excluded.name, artists.name),
                     thumbnail_url = COALESCE(excluded.thumbnail_url, artists.thumbnail_url),
                     is_channel = excluded.is_channel,
                     bookmarked_at = CASE WHEN ?5 IS NULL THEN NULL ELSE COALESCE(artists.bookmarked_at, ?5) END",
                params![artist.browse_id, artist.name, artist.thumbnail_url, artist.is_channel as i64, at],
            )?;
            Ok(())
        })?;
        self.notify(Change::BOOKMARKS);
        Ok(())
    }

    pub fn saved_artists(&self) -> Result<Vec<ArtistItem>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare(
                "SELECT browse_id, name, thumbnail_url, is_channel FROM artists WHERE bookmarked_at IS NOT NULL ORDER BY bookmarked_at DESC",
            )?;
            let rows = statement.query_map([], |r| {
                let browse_id: String = r.get(0)?;
                Ok(ArtistItem {
                    name: r.get::<_, Option<String>>(1)?.unwrap_or_else(|| browse_id.clone()),
                    browse_id,
                    thumbnail_url: r.get(2)?,
                    is_channel: r.get::<_, i64>(3)? != 0,
                    ..Default::default()
                })
            })?;
            rows.collect()
        })
    }

    // ── плейлисты ──

    pub fn playlists(&self) -> Result<Vec<LocalPlaylist>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare(
                "SELECT p.id, p.sync_id, p.name, p.browse_id, p.thumbnail_url, p.created_at,
                        (SELECT COUNT(*) FROM playlist_items i WHERE i.playlist_id = p.id)
                 FROM playlists p ORDER BY p.created_at DESC, p.id DESC",
            )?;
            let rows: Vec<LocalPlaylist> = statement
                .query_map([], |r| {
                    Ok(LocalPlaylist {
                        id: r.get(0)?,
                        sync_id: r.get(1)?,
                        name: r.get(2)?,
                        browse_id: r.get(3)?,
                        thumbnail_url: r.get(4)?,
                        created_at: r.get(5)?,
                        track_count: r.get(6)?,
                        mosaic: Vec::new(),
                    })
                })?
                .collect::<rusqlite::Result<_>>()?;
            let mut mosaic = c.prepare(
                "SELECT t.thumbnail_url FROM playlist_items i JOIN tracks t ON t.video_id = i.video_id
                 WHERE i.playlist_id = ?1 AND t.thumbnail_url IS NOT NULL ORDER BY i.position LIMIT 4",
            )?;
            rows.into_iter()
                .map(|mut p| {
                    p.mosaic = mosaic.query_map([p.id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                    Ok(p)
                })
                .collect()
        })
    }

    pub fn playlist(&self, id: i64) -> Result<Option<LocalPlaylist>, DbError> {
        Ok(self.playlists()?.into_iter().find(|p| p.id == id))
    }

    pub fn playlist_tracks(&self, playlist_id: i64) -> Result<Vec<Track>, DbError> {
        self.db.read(|c| {
            tracks(
                c,
                &format!(
                    "SELECT {} FROM playlist_items i JOIN tracks t ON t.video_id = i.video_id WHERE i.playlist_id = ?1 ORDER BY i.position",
                    prefixed("t")
                ),
                [playlist_id],
            )
        })
    }

    pub fn create_playlist(&self, name: &str, list: &[Track]) -> Result<i64, DbError> {
        let name = name.trim();
        let name = truncate_utf16(if name.is_empty() { "Без названия" } else { name }, 200);
        let id = self.db.write(|t| {
            t.execute("INSERT INTO playlists (name, created_at) VALUES (?1, ?2)", params![name, now_ms()])?;
            let id = t.last_insert_rowid();
            append_items(t, id, list)?;
            Ok(id)
        })?;
        self.notify(Change::PLAYLISTS);
        Ok(id)
    }

    pub fn rename_playlist(&self, id: i64, name: &str) -> Result<(), DbError> {
        self.db.write(|t| t.execute("UPDATE playlists SET name = ?2 WHERE id = ?1", params![id, truncate_utf16(name.trim(), 200)]))?;
        self.notify(Change::PLAYLISTS);
        Ok(())
    }

    pub fn delete_playlist(&self, id: i64) -> Result<(), DbError> {
        self.db.write(|t| {
            t.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", [id])?;
            t.execute("DELETE FROM playlists WHERE id = ?1", [id])
        })?;
        self.notify(Change::PLAYLISTS);
        Ok(())
    }

    /// Добавить в конец; трек встречается в плейлисте не больше одного раза. Возвращает число добавленных.
    pub fn add_to_playlist(&self, playlist_id: i64, list: &[Track]) -> Result<usize, DbError> {
        let added = self.db.write(|t| append_items(t, playlist_id, list))?;
        if added > 0 {
            self.notify(Change::PLAYLISTS);
        }
        Ok(added)
    }

    pub fn remove_from_playlist(&self, playlist_id: i64, video_id: &str) -> Result<(), DbError> {
        self.db.write(|t| {
            t.execute("DELETE FROM playlist_items WHERE playlist_id = ?1 AND video_id = ?2", params![playlist_id, video_id])?;
            renumber(t, playlist_id)
        })?;
        self.notify(Change::PLAYLISTS);
        Ok(())
    }

    /// Перенести трек на место `index` (0..n-1).
    pub fn move_in_playlist(&self, playlist_id: i64, video_id: &str, index: usize) -> Result<(), DbError> {
        self.db.write(|t| {
            let mut order = video_ids(t, playlist_id)?;
            let Some(from) = order.iter().position(|v| v == video_id) else { return Ok(()) };
            let item = order.remove(from);
            order.insert(index.min(order.len()), item);
            for (position, id) in order.iter().enumerate() {
                t.execute(
                    "UPDATE playlist_items SET position = ?3 WHERE playlist_id = ?1 AND video_id = ?2",
                    params![playlist_id, id, position as i64],
                )?;
            }
            Ok(())
        })?;
        self.notify(Change::PLAYLISTS);
        Ok(())
    }

    /// Плейлисты, в которых есть трек (галочки в «Добавить в плейлист…»).
    pub fn playlists_containing(&self, video_id: &str) -> Result<HashSet<i64>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare("SELECT playlist_id FROM playlist_items WHERE video_id = ?1")?;
            let ids = statement.query_map([video_id], |r| r.get(0))?.collect();
            ids
        })
    }

    // ── история (DESIGN §3.11) ──

    /// Прослушивание (сеанс ≥ 5 с): одной транзакцией трек, событие с UUID и счётчик времени.
    pub fn record_play(&self, track: &Track, play_time_ms: i64, ended_at_ms: i64) -> Result<(), DbError> {
        if play_time_ms < 5000 {
            return Ok(());
        }
        self.db.write(|t| {
            upsert_track(t, track, false)?;
            t.execute(
                "INSERT INTO play_events (event_id, video_id, played_at, play_time_ms) VALUES (?1, ?2, ?3, ?4)",
                params![new_id(), track.video_id, ended_at_ms, play_time_ms],
            )?;
            t.execute("UPDATE tracks SET total_play_ms = total_play_ms + ?2 WHERE video_id = ?1", params![track.video_id, play_time_ms])?;
            Ok(())
        })?;
        self.notify(Change::HISTORY);
        Ok(())
    }

    /// «Недавние»: последние 100 разных треков по последнему прослушиванию.
    pub fn recent_history(&self, limit: usize) -> Result<Vec<HistoryEntry>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare(&format!(
                "SELECT {}, h.last
                 FROM (SELECT video_id, MAX(played_at) AS last FROM play_events GROUP BY video_id ORDER BY last DESC LIMIT ?1) h
                 JOIN tracks t ON t.video_id = h.video_id ORDER BY h.last DESC",
                prefixed("t")
            ))?;
            let rows = statement.query_map([limit as i64], |r| Ok(HistoryEntry { track: read_track(r, 0)?, played_at: r.get(14)? }))?;
            rows.collect()
        })
    }

    /// «Чаще всего» за период; за всё время — общее время трека (`total_play_ms`).
    pub fn most_played(&self, since_ms: Option<i64>, limit: usize) -> Result<Vec<TopEntry>, DbError> {
        self.db.read(|c| {
            let sql = match since_ms {
                None => format!(
                    "SELECT {}, t.total_play_ms FROM tracks t WHERE t.total_play_ms > 0 ORDER BY t.total_play_ms DESC LIMIT ?1",
                    prefixed("t")
                ),
                Some(_) => format!(
                    "SELECT {}, s.total FROM (SELECT video_id, SUM(play_time_ms) AS total FROM play_events WHERE played_at >= ?2
                     GROUP BY video_id ORDER BY total DESC LIMIT ?1) s JOIN tracks t ON t.video_id = s.video_id ORDER BY s.total DESC",
                    prefixed("t")
                ),
            };
            let mut statement = c.prepare(&sql)?;
            let map = |r: &Row| Ok(TopEntry { track: read_track(r, 0)?, play_time_ms: r.get(14)? });
            match since_ms {
                None => statement.query_map([limit as i64], map)?.collect(),
                Some(since) => statement.query_map(params![limit as i64, since], map)?.collect(),
            }
        })
    }

    /// «Очистить историю»: события удаляются, счётчики остаются, как в ViTune; синк отправит `history.clear`.
    pub fn clear_history(&self) -> Result<(), DbError> {
        let now = now_ms();
        self.db.write(|t| {
            t.execute("DELETE FROM play_events WHERE played_at <= ?1", [now])?;
            t.execute(
                "INSERT INTO history_ops (op_id, kind, video_id, events_before) VALUES (?1, 'history.clear', NULL, ?2)",
                params![new_id(), now],
            )
        })?;
        self.notify(Change::HISTORY);
        Ok(())
    }

    /// «Убрать из истории» (задание Windows 0008): события трека удаляются и общее время обнуляется.
    /// `before` — когда нажали: у действия есть «Отменить», и прослушивания за эти секунды не стираются.
    pub fn remove_from_history(&self, video_id: &str, before: Option<i64>) -> Result<(), DbError> {
        let now = before.unwrap_or_else(now_ms);
        self.db.write(|t| {
            t.execute("DELETE FROM play_events WHERE video_id = ?1 AND played_at <= ?2", params![video_id, now])?;
            t.execute("UPDATE tracks SET total_play_ms = 0 WHERE video_id = ?1", [video_id])?;
            t.execute(
                "INSERT INTO history_ops (op_id, kind, video_id, events_before) VALUES (?1, 'history.forget', ?2, ?3)",
                params![new_id(), video_id, now],
            )
        })?;
        self.notify(Change::HISTORY);
        Ok(())
    }

    pub fn play_count(&self) -> Result<i64, DbError> {
        self.db.read(|c| c.query_row("SELECT COUNT(*) FROM play_events", [], |r| r.get(0)))
    }

    /// Затравки «Для вас» (REWRITE §4.10.5): последний лайк, самый частый за 30 дней, последний прослушанный.
    pub fn for_you_seeds(&self) -> Result<Vec<Track>, DbError> {
        let mut seeds = Vec::new();
        if let Some(liked) = self.favorites()?.into_iter().next() {
            seeds.push(liked);
        }
        if let Some(top) = self.most_played(Some(now_ms() - 30 * 24 * 3_600_000), 1)?.into_iter().next() {
            seeds.push(top.track);
        }
        if let Some(recent) = self.recent_history(1)?.into_iter().next() {
            seeds.push(recent.track);
        }
        let mut seen = HashSet::new();
        seeds.retain(|t| seen.insert(t.video_id.clone()));
        Ok(seeds)
    }

    // ── поиск ──

    pub fn add_search(&self, query: &str) -> Result<(), DbError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(());
        }
        self.db.write(|t| {
            t.execute(
                "INSERT INTO search_history (query, searched_at) VALUES (?1, ?2) ON CONFLICT(query) DO UPDATE SET searched_at = excluded.searched_at",
                params![truncate_utf16(query, 200), now_ms()],
            )?;
            t.execute("DELETE FROM search_history WHERE query NOT IN (SELECT query FROM search_history ORDER BY searched_at DESC LIMIT 50)", [])
        })?;
        self.notify(Change::SEARCHES);
        Ok(())
    }

    pub fn recent_searches(&self, limit: usize) -> Result<Vec<String>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare("SELECT query FROM search_history ORDER BY searched_at DESC LIMIT ?1")?;
            let list = statement.query_map([limit as i64], |r| r.get(0))?.collect();
            list
        })
    }

    pub fn remove_search(&self, query: &str) -> Result<(), DbError> {
        self.db.write(|t| t.execute("DELETE FROM search_history WHERE query = ?1", [query]))?;
        self.notify(Change::SEARCHES);
        Ok(())
    }

    pub fn clear_searches(&self) -> Result<(), DbError> {
        self.db.write(|t| t.execute("DELETE FROM search_history", []))?;
        self.notify(Change::SEARCHES);
        Ok(())
    }

    /// Поиск по своей библиотеке («В библиотеке» при вводе).
    pub fn search_library(&self, query: &str, limit: usize) -> Result<Vec<Track>, DbError> {
        let pattern = format!("%{}%", query.trim().replace(['%', '_'], ""));
        self.db.read(|c| {
            tracks(
                c,
                &format!(
                    "SELECT {TRACK_COLUMNS} FROM tracks
                     WHERE (liked_at IS NOT NULL OR total_play_ms > 0 OR video_id IN (SELECT video_id FROM playlist_items))
                       AND (title LIKE ?1 OR artists_text LIKE ?1)
                     ORDER BY liked_at IS NULL, total_play_ms DESC LIMIT ?2"
                ),
                params![pattern, limit as i64],
            )
        })
    }

    // ── «Не показывать» ──

    pub fn hidden_tracks(&self) -> Result<HashSet<String>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare("SELECT key FROM content_blocks WHERE type = 'track'")?;
            let ids = statement.query_map([], |r| r.get(0))?.collect();
            ids
        })
    }

    pub fn set_track_hidden(&self, track: &Track, hidden: bool) -> Result<(), DbError> {
        self.db.write(|t| {
            if hidden {
                t.execute(
                    "INSERT OR REPLACE INTO content_blocks (type, key, level, title, subtitle, thumbnail_url, blocked_at)
                     VALUES ('track', ?1, 'hide', ?2, ?3, ?4, ?5)",
                    params![track.video_id, track.title, track.artists_text, track.thumbnail_url, now_ms()],
                )
            } else {
                t.execute("DELETE FROM content_blocks WHERE type = 'track' AND key = ?1", [&track.video_id])
            }
        })?;
        self.notify(Change::BLOCKS);
        Ok(())
    }

    pub fn clear_hidden_tracks(&self) -> Result<(), DbError> {
        self.db.write(|t| t.execute("DELETE FROM content_blocks WHERE type = 'track'", []))?;
        self.notify(Change::BLOCKS);
        Ok(())
    }

    // ── состояние приложения ──

    pub fn state(&self, key: &str) -> Result<Option<String>, DbError> {
        self.db.read(|c| c.query_row("SELECT value FROM app_state WHERE key = ?1", [key], |r| r.get(0)).optional())
    }

    pub fn set_state(&self, key: &str, value: Option<&str>) -> Result<(), DbError> {
        self.db.write(|t| match value {
            Some(value) => t.execute("INSERT OR REPLACE INTO app_state (key, value) VALUES (?1, ?2)", params![key, value]),
            None => t.execute("DELETE FROM app_state WHERE key = ?1", [key]),
        })?;
        Ok(())
    }

    // ── загрузки ──

    /// «Скачать»: трек — в библиотеку (название, обложка для «Скачанного») и в список загрузок.
    pub fn add_downloads(&self, list: &[Track]) -> Result<(), DbError> {
        let now = now_ms();
        self.db.write(|t| {
            for track in list {
                upsert_track(t, track, false)?;
                t.execute("INSERT OR IGNORE INTO downloads (video_id, added_at) VALUES (?1, ?2)", params![track.video_id, now])?;
            }
            Ok(())
        })?;
        self.notify(Change::DOWNLOADS);
        Ok(())
    }

    /// «Удалить загрузку»: трек остаётся в библиотеке, но без сети играть не будет.
    pub fn remove_download(&self, video_id: &str) -> Result<(), DbError> {
        self.db.write(|t| t.execute("DELETE FROM downloads WHERE video_id = ?1", [video_id]))?;
        self.notify(Change::DOWNLOADS);
        Ok(())
    }

    pub fn remove_all_downloads(&self) -> Result<(), DbError> {
        self.db.write(|t| t.execute("DELETE FROM downloads", []))?;
        self.notify(Change::DOWNLOADS);
        Ok(())
    }

    /// Скачанные треки, сначала недавние.
    pub fn downloads(&self) -> Result<Vec<Track>, DbError> {
        self.db.read(|c| {
            tracks(
                c,
                &format!("SELECT {} FROM downloads d JOIN tracks t ON t.video_id = d.video_id ORDER BY d.added_at DESC", prefixed("t")),
                [],
            )
        })
    }

    pub fn download_ids(&self) -> Result<HashSet<String>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare("SELECT video_id FROM downloads")?;
            let ids = statement.query_map([], |r| r.get(0))?.collect();
            ids
        })
    }

    // ── «Все треки» (задание Windows 0005) ──

    /// Прослушанное, лайкнутое, лежащее в своих плейлистах и скачанное; не скрытое. По умолчанию —
    /// «Недавно слушали»: последнее прослушивание, у непрослушанных — лайк, остальные в конце.
    pub fn all_tracks(&self) -> Result<Vec<AllTracksEntry>, DbError> {
        self.db.read(|c| {
            let mut statement = c.prepare(&format!(
                "SELECT {}, p.last FROM tracks t
                 LEFT JOIN (SELECT video_id, MAX(played_at) AS last FROM play_events GROUP BY video_id) p ON p.video_id = t.video_id
                 WHERE {ALL_TRACKS_WHERE}
                 ORDER BY COALESCE(p.last, t.liked_at, 0) DESC",
                prefixed("t")
            ))?;
            let rows = statement
                .query_map([], |r| Ok(AllTracksEntry { track: read_track(r, 0)?, play_time_ms: r.get(13)?, last_played_at: r.get(14)? }))?;
            rows.collect()
        })
    }

    pub fn all_tracks_count(&self) -> Result<i64, DbError> {
        self.db.read(|c| {
            c.query_row(
                &format!("SELECT COUNT(*) FROM tracks t LEFT JOIN (SELECT DISTINCT video_id FROM play_events) p ON p.video_id = t.video_id WHERE {ALL_TRACKS_WHERE}"),
                [],
                |r| r.get(0),
            )
        })
    }

    pub fn counts(&self) -> Result<Counts, DbError> {
        self.db.read(|c| {
            let count = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0));
            Ok(Counts {
                likes: count("SELECT COUNT(*) FROM tracks WHERE liked_at IS NOT NULL")?,
                playlists: count("SELECT COUNT(*) FROM playlists")?,
                albums: count("SELECT COUNT(*) FROM albums WHERE bookmarked_at IS NOT NULL")?,
                artists: count("SELECT COUNT(*) FROM artists WHERE bookmarked_at IS NOT NULL")?,
            })
        })
    }
}

const ALL_TRACKS_WHERE: &str = "(p.video_id IS NOT NULL OR t.total_play_ms > 0 OR t.liked_at IS NOT NULL
     OR EXISTS (SELECT 1 FROM playlist_items i WHERE i.video_id = t.video_id)
     OR EXISTS (SELECT 1 FROM downloads d WHERE d.video_id = t.video_id))
    AND NOT EXISTS (SELECT 1 FROM content_blocks b WHERE b.type = 'track' AND b.key = t.video_id)";

fn append_items(t: &Transaction, playlist_id: i64, list: &[Track]) -> rusqlite::Result<usize> {
    let next: i64 =
        t.query_row("SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_items WHERE playlist_id = ?1", [playlist_id], |r| r.get(0))?;
    let now = now_ms();
    let mut added = 0;
    for track in list {
        upsert_track(t, track, false)?;
        added += t.execute(
            "INSERT OR IGNORE INTO playlist_items (playlist_id, video_id, position, added_at) VALUES (?1, ?2, ?3, ?4)",
            params![playlist_id, track.video_id, next + added as i64, now],
        )?;
    }
    Ok(added)
}

fn video_ids(t: &Transaction, playlist_id: i64) -> rusqlite::Result<Vec<String>> {
    let mut statement = t.prepare("SELECT video_id FROM playlist_items WHERE playlist_id = ?1 ORDER BY position")?;
    let ids = statement.query_map([playlist_id], |r| r.get(0))?.collect();
    ids
}

fn renumber(t: &Transaction, playlist_id: i64) -> rusqlite::Result<()> {
    for (position, id) in video_ids(t, playlist_id)?.iter().enumerate() {
        t.execute(
            "UPDATE playlist_items SET position = ?3 WHERE playlist_id = ?1 AND video_id = ?2",
            params![playlist_id, id, position as i64],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> Arc<Library> {
        Library::open(Database::in_memory().unwrap())
    }

    fn track(id: &str) -> Track {
        Track { video_id: id.into(), title: format!("Трек {id}"), artists_text: Some("Кино".into()), ..Default::default() }
    }

    #[test]
    fn likes_and_all_tracks() {
        let lib = library();
        lib.set_liked(&[track("a"), track("b")], true).unwrap();
        assert!(lib.is_liked("a").unwrap());
        assert_eq!(lib.favorites().unwrap().len(), 2);
        lib.set_liked(&[track("a")], false).unwrap();
        assert_eq!(lib.liked_ids().unwrap(), HashSet::from(["b".to_owned()]));
        lib.record_play(&track("c"), 60_000, now_ms()).unwrap();
        lib.record_play(&track("d"), 3_000, now_ms()).unwrap();
        let all: Vec<String> = lib.all_tracks().unwrap().into_iter().map(|e| e.track.video_id).collect();
        assert_eq!(all, ["c", "b"], "прослушанное и лайкнутое; меньше 5 с — не прослушивание");
        assert_eq!(lib.all_tracks_count().unwrap(), 2);
        lib.set_track_hidden(&track("c"), true).unwrap();
        assert_eq!(lib.all_tracks_count().unwrap(), 1, "скрытое во «Все треки» не входит");
    }

    #[test]
    fn stub_does_not_overwrite_known_metadata() {
        let lib = library();
        lib.save_tracks(&[Track { album_title: Some("Группа крови".into()), ..track("a") }]).unwrap();
        lib.db.write(|t| upsert_track(t, &Track { video_id: "a".into(), title: "a".into(), ..Default::default() }, true)).unwrap();
        let saved = lib.track("a").unwrap().unwrap();
        assert_eq!(saved.title, "Трек a");
        assert_eq!(saved.album_title.as_deref(), Some("Группа крови"));
    }

    #[test]
    fn playlists_keep_order_and_uniqueness() {
        let lib = library();
        let id = lib.create_playlist("  Мой  ", &[track("a"), track("b")]).unwrap();
        assert_eq!(lib.add_to_playlist(id, &[track("b"), track("c")]).unwrap(), 1, "трек в плейлисте один раз");
        lib.move_in_playlist(id, "c", 0).unwrap();
        let order: Vec<String> = lib.playlist_tracks(id).unwrap().into_iter().map(|t| t.video_id).collect();
        assert_eq!(order, ["c", "a", "b"]);
        lib.remove_from_playlist(id, "a").unwrap();
        let playlist = lib.playlist(id).unwrap().unwrap();
        assert_eq!((playlist.name.as_str(), playlist.track_count), ("Мой", 2));
        assert_eq!(lib.playlists_containing("b").unwrap(), HashSet::from([id]));
        lib.delete_playlist(id).unwrap();
        assert!(lib.playlists().unwrap().is_empty());
    }

    #[test]
    fn history_recent_top_and_forget() {
        let lib = library();
        let now = now_ms();
        lib.record_play(&track("a"), 10_000, now - 3000).unwrap();
        lib.record_play(&track("b"), 90_000, now - 2000).unwrap();
        lib.record_play(&track("a"), 10_000, now - 1000).unwrap();
        let recent: Vec<String> = lib.recent_history(100).unwrap().into_iter().map(|e| e.track.video_id).collect();
        assert_eq!(recent, ["a", "b"]);
        let top = lib.most_played(None, 10).unwrap();
        assert_eq!((top[0].track.video_id.as_str(), top[0].play_time_ms), ("b", 90_000));
        lib.remove_from_history("b", None).unwrap();
        assert!(lib.most_played(None, 10).unwrap().iter().all(|e| e.track.video_id != "b"), "«Убрать из истории» обнуляет общее время");
        lib.clear_history().unwrap();
        assert!(lib.recent_history(10).unwrap().is_empty());
        assert_eq!(lib.most_played(None, 10).unwrap().len(), 1, "счётчики остаются после «Очистить историю»");
    }

    #[test]
    fn searches_and_bookmarks_and_notifications() {
        let lib = library();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        lib.subscribe(move |change| sink.lock().unwrap().push(change));
        lib.add_search("кино").unwrap();
        lib.add_search("  daft punk ").unwrap();
        lib.add_search("кино").unwrap();
        assert_eq!(lib.recent_searches(10).unwrap(), ["кино", "daft punk"]);
        let album = AlbumItem { browse_id: "MPREb_1".into(), title: "Группа крови".into(), ..Default::default() };
        lib.set_album_saved(&album, true).unwrap();
        assert!(lib.is_album_saved("MPREb_1").unwrap());
        lib.set_album_saved(&album, false).unwrap();
        assert!(lib.saved_albums().unwrap().is_empty());
        let artist = ArtistItem { browse_id: "UC1".into(), name: "Кино".into(), ..Default::default() };
        lib.set_artist_saved(&artist, true).unwrap();
        assert_eq!(lib.saved_artists().unwrap()[0].name, "Кино");
        let changes = seen.lock().unwrap();
        assert!(changes.contains(&Change::SEARCHES) && changes.contains(&Change::BOOKMARKS));
    }

    #[test]
    fn downloads_list() {
        let lib = library();
        lib.add_downloads(&[track("a"), track("b")]).unwrap();
        assert_eq!(lib.download_ids().unwrap().len(), 2);
        lib.remove_download("a").unwrap();
        assert_eq!(lib.downloads().unwrap()[0].video_id, "b");
        assert_eq!(lib.all_tracks_count().unwrap(), 1, "скачанное входит во «Все треки», снятое с загрузки без лайка — нет");
    }
}
