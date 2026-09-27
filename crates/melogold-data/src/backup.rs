//! Копия библиотеки (`melogoldAndroid/docs/spec/backup-format.md`, задание Windows 0004; Windows
//! `LibraryBackup.cs`, `LibraryImport.cs`):
//!
//! - «Сохранить копию» переводит свою схему в формат копии Melogold — те же таблицы, что у ViTune и
//!   ViMusic, плюс колонки Melogold и метка `MelogoldBackup`. Такую копию открывают Melogold на
//!   Android, Windows и Apple. В копию не входят настройки, вход, состояние синка и кэш; из текстов —
//!   свои (`user`, `file`) и выбранные;
//! - «Импорт копии» **добавляет** копию ViTune, ViMusic, их форков или Melogold любой платформы к
//!   библиотеке — одной транзакцией, ничего не заменяя и не удаляя. Прослушивание того же трека в тот
//!   же момент — один раз; id — из копии или [`melogold_core::import_ids`], поэтому одна копия на двух
//!   устройствах не удваивает историю на сервере.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use melogold_core::import_ids;
use melogold_core::iso;
use melogold_core::lyrics::sync_rules::sources;
use melogold_core::text::{now_ms, parse_duration, truncate_utf16};
use regex::Regex;
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::database::DbError;
use crate::library::{Change, Library};

/// `user_version` копии Windows, Apple и Linux (Android пишет версию своей базы Room).
pub const FORMAT_USER_VERSION: i32 = 31;
const MIN_VERSION: i64 = 12;
const MAX_PLAY_TIME_MS: i64 = 86_400_000;
const SEARCH_QUERIES: usize = 200;
const NAME_MAX: usize = 200;
const OVERRIDE_MAX: usize = 500;
/// Даты вне [2000-01-01, 2100-01-01) отбрасываются: сервер их не примет.
const SANE: std::ops::Range<i64> = 946_684_800_000..4_102_444_800_000;

static VIDEO_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new("^[A-Za-z0-9_-]{11}$").expect("videoId"));

/// Таблицы копии (§2): имена и колонки с учётом регистра.
const SCHEMA: &str = "
CREATE TABLE Song (id TEXT PRIMARY KEY, title TEXT NOT NULL, artistsText TEXT, durationText TEXT, thumbnailUrl TEXT,
    likedAt INTEGER, totalPlayTimeMs INTEGER NOT NULL DEFAULT 0, blacklisted INTEGER NOT NULL DEFAULT 0, explicit INTEGER NOT NULL DEFAULT 0);
CREATE TABLE Event (id INTEGER PRIMARY KEY, songId TEXT NOT NULL, timestamp INTEGER NOT NULL, playTime INTEGER NOT NULL,
    syncId TEXT, deviceId TEXT);
CREATE TABLE Lyrics (songId TEXT PRIMARY KEY, fixed TEXT, synced TEXT, startTime INTEGER, fixedSource TEXT, syncedSource TEXT);
CREATE TABLE Album (id TEXT PRIMARY KEY, title TEXT, thumbnailUrl TEXT, year TEXT, authorsText TEXT, shareUrl TEXT,
    timestamp INTEGER, bookmarkedAt INTEGER);
CREATE TABLE Artist (id TEXT PRIMARY KEY, name TEXT, thumbnailUrl TEXT, timestamp INTEGER, bookmarkedAt INTEGER);
CREATE TABLE SongAlbumMap (songId TEXT NOT NULL, albumId TEXT NOT NULL, position INTEGER, PRIMARY KEY (songId, albumId));
CREATE TABLE SongArtistMap (songId TEXT NOT NULL, artistId TEXT NOT NULL, PRIMARY KEY (songId, artistId));
CREATE TABLE Playlist (id INTEGER PRIMARY KEY, name TEXT NOT NULL, browseId TEXT, thumbnail TEXT, syncId TEXT);
CREATE TABLE SongPlaylistMap (songId TEXT NOT NULL, playlistId INTEGER NOT NULL, position INTEGER NOT NULL, PRIMARY KEY (songId, playlistId));
CREATE TABLE SearchQuery (id INTEGER PRIMARY KEY, query TEXT NOT NULL);
CREATE TABLE TrackOverride (videoId TEXT PRIMARY KEY, title TEXT, artistsText TEXT, albumTitle TEXT, updatedAt INTEGER);
CREATE TABLE MelogoldBackup (key TEXT PRIMARY KEY, value TEXT);
";

/// Источник текста словом API §4.10 → словом копии (как у ViTune и Android).
fn source_to_backup(column: &str) -> String {
    format!(
        "CASE {column} WHEN 'user' THEN 'User' WHEN 'file' THEN 'File' WHEN 'youtube_music' THEN 'YouTubeMusic' \
         WHEN 'lrclib' THEN 'LrcLib' WHEN 'kugou' THEN 'KuGou' ELSE NULL END"
    )
}

/// Источник, как его пишут копии (имена ViTune и Android или слова API) → слово API.
fn source_from_backup(source: Option<&str>) -> Option<String> {
    let word = match source? {
        "User" | "user" => sources::USER,
        "File" | "file" => sources::FILE,
        "YouTubeMusic" | "youtube_music" => sources::YOUTUBE_MUSIC,
        "LrcLib" | "lrclib" => sources::LRCLIB,
        "KuGou" | "kugou" => sources::KUGOU,
        _ => return None,
    };
    Some(word.to_owned())
}

/// Имя файла копии, как у Android: `Melogold_backup_ггггММддЧЧммсс.db`; `stamp` — местное время.
pub fn suggested_name(stamp: &str) -> String {
    format!("Melogold_backup_{stamp}.db")
}

// ── экспорт ──

/// «Сохранить копию»: цельный снимок библиотеки (SQLite backup API) переводится в формат копии.
/// Файл сначала пишется рядом и потом переименовывается.
pub fn export(library: &Library, target: &Path, platform: &str, app_version: &str) -> Result<(), DbError> {
    let snapshot = sibling(target, "snapshot");
    let _ = std::fs::remove_file(&snapshot);
    let result = library.database().backup_to(&snapshot).and_then(|()| Ok(convert(&snapshot, target, platform, app_version)?));
    let _ = std::fs::remove_file(&snapshot);
    result
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(format!(".{suffix}"));
    path.with_file_name(name)
}

/// База своей схемы (живая библиотека или старая копия Windows) → формат копии.
fn convert(source: &Path, target: &Path, platform: &str, app_version: &str) -> rusqlite::Result<()> {
    let temp = sibling(target, "tmp");
    let _ = std::fs::remove_file(&temp);
    let written = (|| {
        let mut copy = Connection::open(&temp)?;
        copy.execute_batch(SCHEMA)?;
        copy.execute("ATTACH DATABASE ?1 AS src", [source.to_string_lossy()])?;
        {
            let tx = copy.transaction()?;
            let has_column = |table: &str, column: &str| -> rusqlite::Result<bool> {
                tx.query_row("SELECT COUNT(*) FROM pragma_table_info(?1, 'src') WHERE name = ?2", params![table, column], |r| {
                    r.get::<_, i64>(0)
                })
                .map(|n| n > 0)
            };
            let has_table = |table: &str| -> rusqlite::Result<bool> {
                tx.query_row("SELECT COUNT(*) FROM src.sqlite_master WHERE type = 'table' AND name = ?1", [table], |r| r.get::<_, i64>(0))
                    .map(|n| n > 0)
            };
            let device = if has_column("play_events", "device_id")? { "device_id" } else { "NULL" };
            let offset = if has_column("lyrics", "offset_ms")? { "offset_ms" } else { "NULL" };
            let plain_source = if has_column("lyrics", "plain_source")? { "plain_source" } else { "NULL" };
            let chosen = if has_column("lyrics", "chosen")? { "chosen" } else { "0" };
            tx.execute_batch(&format!(
                "INSERT INTO Song (id, title, artistsText, durationText, thumbnailUrl, likedAt, totalPlayTimeMs, blacklisted, explicit)
                 SELECT t.video_id, t.title, t.artists_text,
                        COALESCE(t.duration_text, CASE WHEN t.duration_ms IS NOT NULL THEN (t.duration_ms / 60000) || ':' || printf('%02d', (t.duration_ms / 1000) % 60) END),
                        t.thumbnail_url, t.liked_at, t.total_play_ms,
                        EXISTS (SELECT 1 FROM src.content_blocks b WHERE b.type = 'track' AND b.key = t.video_id),
                        t.explicit
                 FROM src.tracks t;
                 INSERT INTO Event (songId, timestamp, playTime, syncId, deviceId)
                 SELECT video_id, played_at, play_time_ms, event_id, {device} FROM src.play_events ORDER BY played_at;
                 INSERT INTO Lyrics (songId, fixed, synced, startTime, fixedSource, syncedSource)
                 SELECT video_id, NULLIF(plain, ''), NULLIF(synced, ''),
                        CASE WHEN COALESCE({offset}, 0) = 0 THEN NULL ELSE -{offset} END,
                        CASE WHEN NULLIF(plain, '') IS NULL THEN NULL ELSE {plain_word} END,
                        CASE WHEN NULLIF(synced, '') IS NULL THEN NULL ELSE {synced_word} END
                 FROM src.lyrics
                 WHERE COALESCE(source, '') IN ('user', 'file') OR COALESCE({plain_source}, '') IN ('user', 'file') OR {chosen} = 1;
                 INSERT INTO Album (id, title, thumbnailUrl, year, authorsText, timestamp, bookmarkedAt)
                 SELECT browse_id, title, thumbnail_url, year, artists_text, bookmarked_at, bookmarked_at FROM src.albums;
                 INSERT INTO Artist (id, name, thumbnailUrl, timestamp, bookmarkedAt)
                 SELECT browse_id, name, thumbnail_url, bookmarked_at, bookmarked_at FROM src.artists;
                 INSERT OR IGNORE INTO SongAlbumMap (songId, albumId, position)
                 SELECT video_id, album_id, NULL FROM src.tracks WHERE album_id IN (SELECT id FROM Album);
                 INSERT OR IGNORE INTO SongArtistMap (songId, artistId)
                 SELECT t.video_id, json_extract(j.value, '$.id') FROM src.tracks t, json_each(t.artists_json) j
                 WHERE t.artists_json IS NOT NULL AND json_valid(t.artists_json) AND json_extract(j.value, '$.id') IN (SELECT id FROM Artist);
                 INSERT INTO Playlist (id, name, browseId, thumbnail, syncId)
                 SELECT id, name, browse_id, thumbnail_url, sync_id FROM src.playlists;
                 INSERT INTO SongPlaylistMap (songId, playlistId, position)
                 SELECT video_id, playlist_id, position FROM src.playlist_items;
                 INSERT INTO SearchQuery (query) SELECT query FROM src.search_history ORDER BY searched_at;",
                plain_word = source_to_backup(plain_source),
                synced_word = source_to_backup("source"),
            ))?;
            if has_table("track_overrides")? {
                tx.execute_batch(
                    "INSERT INTO TrackOverride (videoId, title, artistsText, albumTitle, updatedAt)
                     SELECT video_id, title, artists_text, album_title, updated_at FROM src.track_overrides",
                )?;
            }
            for (key, value) in
                [("format", "1"), ("platform", platform), ("appVersion", app_version), ("createdAt", iso::format(now_ms()).as_str())]
            {
                tx.execute("INSERT INTO MelogoldBackup (key, value) VALUES (?1, ?2)", [key, value])?;
            }
            tx.commit()?;
        }
        copy.execute("DETACH DATABASE src", [])?;
        copy.execute_batch(&format!("PRAGMA user_version = {FORMAT_USER_VERSION}; PRAGMA journal_mode = DELETE;"))?;
        Ok(())
    })();
    match written {
        Ok(()) => std::fs::rename(&temp, target).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e))),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

// ── импорт ──

/// Что принёс импорт (§3.4) — для итога.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub version: i64,
    pub tracks: usize,
    pub plays: usize,
    pub plays_known: usize,
    pub favorites: usize,
    pub lyrics: usize,
    pub playlists: usize,
    pub saved: usize,
    pub local_skipped: usize,
    pub dates_skipped: usize,
}

/// Почему копию не удалось импортировать.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ImportError {
    #[error("это не копия ViTune, ViMusic или Melogold")]
    NotABackup,
    #[error("копия слишком старая")]
    TooOld,
    #[error("копии InnerTune и Metrolist не поддерживаются")]
    Unsupported,
    #[error("файл не читается: {0}")]
    Unreadable(String),
}

fn unreadable(error: impl std::fmt::Display) -> ImportError {
    ImportError::Unreadable(error.to_string())
}

/// «Импорт копии» `path` в библиотеку.
pub fn import(library: &Library, path: &Path, app_version: &str) -> Result<ImportSummary, ImportError> {
    let directory = std::env::temp_dir().join(format!("melogold-import-{}", melogold_core::ids::new_uuid()));
    std::fs::create_dir_all(&directory).map_err(unreadable)?;
    let result = import_in(library, path, &directory, app_version);
    let _ = std::fs::remove_dir_all(&directory);
    result
}

fn import_in(library: &Library, path: &Path, directory: &Path, app_version: &str) -> Result<ImportSummary, ImportError> {
    let mut copy = directory.join("backup.db");
    std::fs::copy(path, &copy).map_err(unreadable)?;
    if !is_sqlite(&copy) {
        return Err(ImportError::NotABackup);
    }
    let (mut version, mut tables) = {
        let connection = open(&copy).map_err(unreadable)?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(unreadable)?;
        (version, table_names(&connection).map_err(unreadable)?)
    };
    // Старая копия Windows или база Linux — своя схема: перевести в формат копии и читать его.
    if !tables.contains("Song") && tables.contains("tracks") && tables.contains("play_events") {
        let converted = directory.join("converted.db");
        convert(&copy, &converted, "linux", app_version).map_err(unreadable)?;
        copy = converted;
        version = i64::from(FORMAT_USER_VERSION);
        tables.insert("Song".into());
    }
    if !tables.contains("Song") {
        return Err(if tables.contains("song") { ImportError::Unsupported } else { ImportError::NotABackup });
    }
    if (1..MIN_VERSION).contains(&version) {
        return Err(ImportError::TooOld);
    }
    let bundle = {
        let connection = open(&copy).map_err(unreadable)?;
        Reader::new(&connection).and_then(|r| r.read()).map_err(unreadable)?
    };
    let summary = library.database().write(|t| merge(t, &bundle, version)).map_err(unreadable)?;
    library.reload_overrides();
    library.notify(Change(u32::MAX));
    Ok(summary)
}

fn open(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    // Своя копия: из WAL в один файл.
    let _: String = connection.query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))?;
    Ok(connection)
}

fn is_sqlite(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 16];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut header)).is_ok() && &header == b"SQLite format 3\0"
}

fn table_names(c: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut statement = c.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let rows = statement.query_map([], |r| r.get(0))?;
    rows.collect()
}

// ── чтение копии ──

struct Song {
    id: String,
    title: String,
    artists_text: Option<String>,
    duration_text: Option<String>,
    thumbnail_url: Option<String>,
    liked_at: Option<i64>,
    total_play_ms: i64,
    blacklisted: bool,
    explicit: bool,
}

struct Event {
    song_id: String,
    timestamp: i64,
    play_time: i64,
    sync_id: Option<String>,
    device_id: Option<String>,
}

struct LyricsRow {
    song_id: String,
    plain: Option<String>,
    synced: Option<String>,
    start_time: Option<i64>,
    plain_source: Option<String>,
    synced_source: Option<String>,
}

struct Album {
    id: String,
    title: Option<String>,
    thumbnail_url: Option<String>,
    year: Option<String>,
    authors_text: Option<String>,
    bookmarked_at: Option<i64>,
}

struct Artist {
    id: String,
    name: Option<String>,
    thumbnail_url: Option<String>,
    bookmarked_at: Option<i64>,
}

struct Playlist {
    name: String,
    browse_id: Option<String>,
    thumbnail: Option<String>,
    sync_id: Option<String>,
    song_ids: Vec<String>,
}

struct Override {
    video_id: String,
    title: Option<String>,
    artists_text: Option<String>,
    album_title: Option<String>,
    updated_at: i64,
}

#[derive(Default)]
struct Bundle {
    songs: Vec<Song>,
    local_skipped: usize,
    events: Vec<Event>,
    lyrics: Vec<LyricsRow>,
    albums: Vec<Album>,
    artists: Vec<Artist>,
    song_albums: Vec<(String, String)>,
    song_artists: Vec<(String, String)>,
    playlists: Vec<Playlist>,
    searches: Vec<String>,
    overrides: Vec<Override>,
}

/// Строка по именам колонок; недостающие колонки читаются как NULL.
struct Row(HashMap<String, Value>);

impl Row {
    fn string(&self, name: &str) -> Option<String> {
        match self.0.get(name)? {
            Value::Text(s) => Some(s.clone()),
            Value::Integer(i) => Some(i.to_string()),
            Value::Real(f) => Some(f.to_string()),
            Value::Blob(b) => String::from_utf8(b.clone()).ok(),
            Value::Null => None,
        }
    }

    fn long(&self, name: &str) -> Option<i64> {
        match self.0.get(name)? {
            Value::Integer(i) => Some(*i),
            Value::Real(f) => Some(*f as i64),
            Value::Text(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}

struct Reader<'a> {
    db: &'a Connection,
    tables: HashSet<String>,
}

impl<'a> Reader<'a> {
    fn new(db: &'a Connection) -> rusqlite::Result<Reader<'a>> {
        Ok(Reader { db, tables: table_names(db)? })
    }

    fn columns(&self, table: &str) -> rusqlite::Result<HashSet<String>> {
        let mut statement = self.db.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let rows = statement.query_map([], |r| r.get::<_, String>(1))?;
        rows.collect()
    }

    fn select(&self, table: &str, wanted: &[&str], order: Option<&str>) -> rusqlite::Result<Vec<Row>> {
        if !self.tables.contains(table) {
            return Ok(Vec::new());
        }
        let have = self.columns(table)?;
        let list: Vec<String> =
            wanted.iter().map(|w| if have.contains(*w) { format!("\"{w}\"") } else { format!("NULL AS \"{w}\"") }).collect();
        let sql = format!("SELECT {} FROM \"{table}\"{}", list.join(", "), order.map(|o| format!(" ORDER BY {o}")).unwrap_or_default());
        let mut statement = self.db.prepare(&sql)?;
        let rows = statement.query_map([], |r| {
            let mut map = HashMap::new();
            for (index, name) in wanted.iter().enumerate() {
                map.insert((*name).to_owned(), r.get::<_, Value>(index)?);
            }
            Ok(Row(map))
        })?;
        rows.collect()
    }

    fn read(&self) -> rusqlite::Result<Bundle> {
        let mut bundle = Bundle::default();
        for row in self.select(
            "Song",
            &["id", "title", "artistsText", "durationText", "thumbnailUrl", "likedAt", "totalPlayTimeMs", "blacklisted", "explicit"],
            None,
        )? {
            let Some(id) = row.string("id") else { continue };
            if !VIDEO_ID.is_match(&id) {
                bundle.local_skipped += 1;
                continue;
            }
            let title = row.string("title").filter(|t| !t.trim().is_empty()).unwrap_or_else(|| id.clone());
            bundle.songs.push(Song {
                title,
                artists_text: row.string("artistsText"),
                duration_text: row.string("durationText"),
                thumbnail_url: row.string("thumbnailUrl"),
                liked_at: row.long("likedAt").filter(|l| *l > 0),
                total_play_ms: row.long("totalPlayTimeMs").unwrap_or(0).max(0),
                blacklisted: row.long("blacklisted") == Some(1),
                explicit: row.long("explicit") == Some(1),
                id,
            });
        }
        for row in self.select("Event", &["songId", "timestamp", "playTime", "syncId", "deviceId"], Some("timestamp"))? {
            if let (Some(song_id), Some(timestamp)) = (row.string("songId"), row.long("timestamp")) {
                bundle.events.push(Event {
                    song_id,
                    timestamp,
                    play_time: row.long("playTime").unwrap_or(0).clamp(1, MAX_PLAY_TIME_MS),
                    sync_id: row.string("syncId").filter(|s| !s.is_empty()),
                    device_id: row.string("deviceId").filter(|s| !s.is_empty()),
                });
            }
        }
        for row in self.select("Lyrics", &["songId", "fixed", "synced", "startTime", "fixedSource", "syncedSource"], None)? {
            let Some(song_id) = row.string("songId") else { continue };
            let plain = row.string("fixed").filter(|t| !t.is_empty());
            let synced = row.string("synced").filter(|t| !t.is_empty());
            if plain.is_none() && synced.is_none() {
                continue;
            }
            bundle.lyrics.push(LyricsRow {
                plain_source: plain.as_ref().and_then(|_| source_from_backup(row.string("fixedSource").as_deref())),
                synced_source: synced.as_ref().and_then(|_| source_from_backup(row.string("syncedSource").as_deref())),
                song_id,
                plain,
                synced,
                start_time: row.long("startTime"),
            });
        }
        for row in self.select("Album", &["id", "title", "thumbnailUrl", "year", "authorsText", "bookmarkedAt"], None)? {
            if let Some(id) = row.string("id") {
                bundle.albums.push(Album {
                    id,
                    title: row.string("title"),
                    thumbnail_url: row.string("thumbnailUrl"),
                    year: row.string("year"),
                    authors_text: row.string("authorsText"),
                    bookmarked_at: row.long("bookmarkedAt"),
                });
            }
        }
        for row in self.select("Artist", &["id", "name", "thumbnailUrl", "bookmarkedAt"], None)? {
            if let Some(id) = row.string("id") {
                bundle.artists.push(Artist {
                    id,
                    name: row.string("name"),
                    thumbnail_url: row.string("thumbnailUrl"),
                    bookmarked_at: row.long("bookmarkedAt"),
                });
            }
        }
        for row in self.select("SongAlbumMap", &["songId", "albumId"], None)? {
            if let (Some(song), Some(album)) = (row.string("songId"), row.string("albumId")) {
                bundle.song_albums.push((song, album));
            }
        }
        for row in self.select("SongArtistMap", &["songId", "artistId"], None)? {
            if let (Some(song), Some(artist)) = (row.string("songId"), row.string("artistId")) {
                bundle.song_artists.push((song, artist));
            }
        }
        // ViMusic v11 называл связь с плейлистом SongInPlaylist.
        let map_table = if self.tables.contains("SongPlaylistMap") { "SongPlaylistMap" } else { "SongInPlaylist" };
        let mut items: HashMap<i64, Vec<String>> = HashMap::new();
        for row in self.select(map_table, &["songId", "playlistId", "position"], Some("position, rowid"))? {
            if let (Some(playlist), Some(song)) = (row.long("playlistId"), row.string("songId")) {
                let list = items.entry(playlist).or_default();
                if !list.contains(&song) {
                    list.push(song);
                }
            }
        }
        for row in self.select("Playlist", &["id", "name", "browseId", "thumbnail", "syncId"], Some("rowid"))? {
            if let Some(id) = row.long("id") {
                bundle.playlists.push(Playlist {
                    name: row.string("name").unwrap_or_default(),
                    browse_id: row.string("browseId"),
                    thumbnail: row.string("thumbnail"),
                    sync_id: row.string("syncId").filter(|s| !s.is_empty()),
                    song_ids: items.remove(&id).unwrap_or_default(),
                });
            }
        }
        bundle.searches = self
            .select("SearchQuery", &["query"], Some("rowid DESC"))?
            .into_iter()
            .filter_map(|r| r.string("query"))
            .take(SEARCH_QUERIES)
            .collect();
        for row in self.select("TrackOverride", &["videoId", "title", "artistsText", "albumTitle", "updatedAt"], None)? {
            let Some(video_id) = row.string("videoId").filter(|v| VIDEO_ID.is_match(v)) else { continue };
            let clean = |v: Option<String>| v.map(|s| truncate_utf16(s.trim(), OVERRIDE_MAX).to_owned()).filter(|s| !s.is_empty());
            let (title, artists_text, album_title) =
                (clean(row.string("title")), clean(row.string("artistsText")), clean(row.string("albumTitle")));
            if title.is_none() && artists_text.is_none() && album_title.is_none() {
                continue;
            }
            bundle.overrides.push(Override { video_id, title, artists_text, album_title, updated_at: row.long("updatedAt").unwrap_or(0) });
        }
        Ok(bundle)
    }
}

// ── слияние ──

fn merge(t: &Transaction, bundle: &Bundle, version: i64) -> rusqlite::Result<ImportSummary> {
    let now = now_ms();
    let mut summary = ImportSummary { version, local_skipped: bundle.local_skipped, ..Default::default() };
    let mut known: HashSet<String> = HashSet::new();
    // Треки, которые библиотека показывает: прослушанные, в Избранном, в плейлистах. ViTune хранит и
    // треки всех открытых альбомов — они приходят (страницы альбомов, тексты), но в итог не входят.
    let shown: HashSet<&str> = bundle
        .events
        .iter()
        .map(|e| e.song_id.as_str())
        .chain(bundle.playlists.iter().flat_map(|p| p.song_ids.iter().map(String::as_str)))
        .collect();
    let mut visible: HashSet<String> = HashSet::new();

    for song in &bundle.songs {
        let liked_at = song.liked_at.filter(|l| SANE.contains(l));
        if song.liked_at.is_some() && liked_at.is_none() {
            summary.dates_skipped += 1;
        }
        if shown.contains(song.id.as_str()) || liked_at.is_some() || song.total_play_ms > 0 {
            visible.insert(song.id.clone());
        }
        let local: Option<(String, Option<i64>)> =
            t.query_row("SELECT title, liked_at FROM tracks WHERE video_id = ?1", [&song.id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let duration_ms = parse_duration(song.duration_text.as_deref());
        match local {
            None => {
                t.execute(
                    "INSERT INTO tracks (video_id, title, artists_text, duration_ms, duration_text, thumbnail_url, explicit, metadata_stub, liked_at, total_play_ms, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        song.id,
                        song.title,
                        song.artists_text,
                        duration_ms,
                        song.duration_text,
                        song.thumbnail_url,
                        song.explicit as i64,
                        (song.title == song.id) as i64,
                        liked_at,
                        song.total_play_ms,
                        now
                    ],
                )?;
                if visible.contains(&song.id) {
                    summary.tracks += 1;
                }
                if liked_at.is_some() {
                    summary.favorites += 1;
                }
            }
            Some((_, local_liked)) => {
                // Заглушка, названная своим id, узнаёт настоящее название; пустые поля заполняются.
                t.execute(
                    "UPDATE tracks SET
                         title = CASE WHEN title = video_id AND ?2 <> video_id THEN ?2 ELSE title END,
                         metadata_stub = CASE WHEN title = video_id AND ?2 <> video_id THEN 0 ELSE metadata_stub END,
                         artists_text = COALESCE(artists_text, ?3),
                         duration_text = COALESCE(duration_text, ?4),
                         duration_ms = COALESCE(duration_ms, ?5),
                         thumbnail_url = COALESCE(thumbnail_url, ?6),
                         liked_at = CASE WHEN liked_at IS NULL THEN ?7 WHEN ?7 IS NULL THEN liked_at ELSE MIN(liked_at, ?7) END,
                         total_play_ms = MAX(total_play_ms, ?8),
                         explicit = MAX(explicit, ?9)
                     WHERE video_id = ?1",
                    params![
                        song.id,
                        song.title,
                        song.artists_text,
                        song.duration_text,
                        duration_ms,
                        song.thumbnail_url,
                        liked_at,
                        song.total_play_ms,
                        song.explicit as i64
                    ],
                )?;
                if local_liked.is_none() && liked_at.is_some() {
                    summary.favorites += 1;
                }
            }
        }
        if song.blacklisted {
            t.execute(
                "INSERT OR IGNORE INTO content_blocks (type, key, level, title, subtitle, thumbnail_url, blocked_at) VALUES ('track', ?1, 'hide', ?2, ?3, ?4, ?5)",
                params![song.id, song.title, song.artists_text, song.thumbnail_url, now],
            )?;
        }
        known.insert(song.id.clone());
    }

    let is_track = |known: &mut HashSet<String>, id: &str| -> rusqlite::Result<bool> {
        if known.contains(id) {
            return Ok(true);
        }
        let exists = t.query_row("SELECT 1 FROM tracks WHERE video_id = ?1", [id], |_| Ok(())).optional()?.is_some();
        if exists {
            known.insert(id.to_owned());
        }
        Ok(exists)
    };

    // Альбомы и исполнители: недостающие добавляются, закладка — самая ранняя.
    for album in &bundle.albums {
        t.execute(
            "INSERT OR IGNORE INTO albums (browse_id, title, artists_text, year, thumbnail_url) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![album.id, album.title, album.authors_text, album.year, album.thumbnail_url],
        )?;
        let Some(at) = album.bookmarked_at.filter(|a| SANE.contains(a)) else { continue };
        let before: Option<i64> = t.query_row("SELECT bookmarked_at FROM albums WHERE browse_id = ?1", [&album.id], |r| r.get(0))?;
        if before.is_none() {
            summary.saved += 1;
        }
        t.execute("UPDATE albums SET bookmarked_at = MIN(COALESCE(bookmarked_at, ?1), ?1) WHERE browse_id = ?2", params![at, album.id])?;
    }
    for artist in &bundle.artists {
        t.execute(
            "INSERT OR IGNORE INTO artists (browse_id, name, thumbnail_url) VALUES (?1, ?2, ?3)",
            params![artist.id, artist.name, artist.thumbnail_url],
        )?;
        let Some(at) = artist.bookmarked_at.filter(|a| SANE.contains(a)) else { continue };
        let before: Option<i64> = t.query_row("SELECT bookmarked_at FROM artists WHERE browse_id = ?1", [&artist.id], |r| r.get(0))?;
        if before.is_none() {
            summary.saved += 1;
        }
        t.execute("UPDATE artists SET bookmarked_at = MIN(COALESCE(bookmarked_at, ?1), ?1) WHERE browse_id = ?2", params![at, artist.id])?;
    }
    // Трек в альбоме и исполнители трека — только если обе стороны есть; известное здесь не затирается.
    let albums: HashMap<&str, &Album> = bundle.albums.iter().map(|a| (a.id.as_str(), a)).collect();
    for (song, album_id) in &bundle.song_albums {
        let Some(album) = albums.get(album_id.as_str()) else { continue };
        if !is_track(&mut known, song)? {
            continue;
        }
        t.execute(
            "UPDATE tracks SET album_id = COALESCE(album_id, ?1), album_title = COALESCE(album_title, ?2) WHERE video_id = ?3",
            params![album_id, album.title, song],
        )?;
    }
    let artists: HashMap<&str, &Artist> = bundle.artists.iter().map(|a| (a.id.as_str(), a)).collect();
    let mut by_song: Vec<(&str, Vec<&str>)> = Vec::new();
    for (song, artist) in &bundle.song_artists {
        if !artists.contains_key(artist.as_str()) {
            continue;
        }
        match by_song.iter_mut().find(|(s, _)| *s == song.as_str()) {
            Some((_, list)) => list.push(artist),
            None => by_song.push((song, vec![artist])),
        }
    }
    for (song, ids) in by_song {
        if !is_track(&mut known, song)? {
            continue;
        }
        let refs: Vec<melogold_core::music::ArtistRef> = ids
            .iter()
            .map(|id| melogold_core::music::ArtistRef { id: Some((*id).to_owned()), name: artists[id].name.clone().unwrap_or_default() })
            .collect();
        let json = serde_json::to_string(&refs).unwrap_or_default();
        t.execute("UPDATE tracks SET artists_json = COALESCE(artists_json, ?1) WHERE video_id = ?2", params![json, song])?;
    }

    // Прослушивания: тот же трек в тот же момент — один раз, какой бы ни был id.
    let mut played_at: HashSet<String> = {
        let mut statement = t.prepare("SELECT video_id || ':' || played_at FROM play_events")?;
        let rows = statement.query_map([], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for event in &bundle.events {
        if !is_track(&mut known, &event.song_id)? {
            continue;
        }
        if !SANE.contains(&event.timestamp) {
            summary.dates_skipped += 1;
            continue;
        }
        if !played_at.insert(format!("{}:{}", event.song_id, event.timestamp)) {
            summary.plays_known += 1;
            continue;
        }
        let id = event.sync_id.clone().unwrap_or_else(|| import_ids::event_id(&event.song_id, event.timestamp, event.play_time));
        // С deviceId — прослушивание другого устройства аккаунта: оно уже на сервере.
        let inserted = t.execute(
            "INSERT OR IGNORE INTO play_events (event_id, video_id, played_at, play_time_ms, synced, device_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, event.song_id, event.timestamp, event.play_time, event.device_id.is_some() as i64, event.device_id],
        )?;
        if inserted > 0 {
            summary.plays += 1;
        } else {
            summary.plays_known += 1;
        }
    }

    // Тексты: только пустые стороны здесь («не искали» или «не нашли»).
    for lyrics in &bundle.lyrics {
        if !is_track(&mut known, &lyrics.song_id)? {
            continue;
        }
        let local: Option<(Option<String>, Option<String>)> = t
            .query_row("SELECT synced, plain FROM lyrics WHERE video_id = ?1", [&lyrics.song_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let offset = -lyrics.start_time.unwrap_or(0);
        let Some((synced_here, plain_here)) = local else {
            t.execute(
                "INSERT INTO lyrics (video_id, synced, plain, source, plain_source, offset_ms, fetched_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    lyrics.song_id,
                    lyrics.synced,
                    lyrics.plain,
                    lyrics.synced_source,
                    lyrics.plain_source,
                    if lyrics.synced.is_some() { offset } else { 0 },
                    now
                ],
            )?;
            if visible.contains(&lyrics.song_id) {
                summary.lyrics += 1;
            }
            continue;
        };
        let take_plain = plain_here.as_deref().is_none_or(str::is_empty) && lyrics.plain.is_some();
        let take_synced = synced_here.as_deref().is_none_or(str::is_empty) && lyrics.synced.is_some();
        if take_plain {
            t.execute(
                "UPDATE lyrics SET plain = ?1, plain_source = ?2, plain_ref = NULL WHERE video_id = ?3",
                params![lyrics.plain, lyrics.plain_source, lyrics.song_id],
            )?;
        }
        if take_synced {
            t.execute(
                "UPDATE lyrics SET synced = ?1, source = ?2, offset_ms = ?3, synced_ref = NULL WHERE video_id = ?4",
                params![lyrics.synced, lyrics.synced_source, offset, lyrics.song_id],
            )?;
        }
        if (take_plain || take_synced) && visible.contains(&lyrics.song_id) {
            summary.lyrics += 1;
        }
    }

    // Плейлисты: тот же (id сервера), иначе с той же ссылкой YouTube или единственный с тем же
    // именем — недостающие треки дописываются в конец; иначе новый.
    let locals: Vec<(i64, Option<String>, Option<String>, String)> = {
        let mut statement = t.prepare("SELECT id, sync_id, browse_id, name FROM playlists ORDER BY id")?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut taken: HashSet<i64> = HashSet::new();
    for imported in &bundle.playlists {
        let mut tracks_of = Vec::new();
        for id in &imported.song_ids {
            if is_track(&mut known, id)? {
                tracks_of.push(id.clone());
            }
        }
        let free: Vec<&(i64, Option<String>, Option<String>, String)> = locals.iter().filter(|p| !taken.contains(&p.0)).collect();
        let mut matched = free.iter().find(|p| imported.sync_id.is_some() && p.1 == imported.sync_id).map(|p| p.0);
        if matched.is_none() {
            matched = free.iter().find(|p| imported.browse_id.is_some() && p.2 == imported.browse_id).map(|p| p.0);
        }
        if matched.is_none() {
            let wanted = import_ids::norm(&imported.name);
            let same: Vec<i64> = free.iter().filter(|p| import_ids::norm(&p.3) == wanted).map(|p| p.0).collect();
            if same.len() == 1 {
                matched = Some(same[0]);
            }
        }
        let playlist_id = match matched {
            Some(id) => id,
            None => {
                let name =
                    if imported.name.trim().is_empty() { "—".to_owned() } else { truncate_utf16(&imported.name, NAME_MAX).to_owned() };
                t.execute(
                    "INSERT INTO playlists (name, browse_id, thumbnail_url, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![name, imported.browse_id, imported.thumbnail, now],
                )?;
                t.last_insert_rowid()
            }
        };
        taken.insert(playlist_id);
        let have: HashSet<String> = {
            let mut statement = t.prepare("SELECT video_id FROM playlist_items WHERE playlist_id = ?1")?;
            let rows = statement.query_map([playlist_id], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let start: i64 =
            t.query_row("SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_items WHERE playlist_id = ?1", [playlist_id], |r| r.get(0))?;
        let added: Vec<&String> = tracks_of.iter().filter(|id| !have.contains(*id)).collect();
        for (index, video_id) in added.iter().enumerate() {
            t.execute(
                "INSERT INTO playlist_items (playlist_id, video_id, position, added_at) VALUES (?1, ?2, ?3, ?4)",
                params![playlist_id, video_id, start + index as i64, now],
            )?;
        }
        if matched.is_none() || !added.is_empty() {
            summary.playlists += 1;
        }
    }

    // Поиск: последние запросы, новые — сверху.
    for (index, query) in bundle.searches.iter().enumerate() {
        t.execute(
            "INSERT OR IGNORE INTO search_history (query, searched_at) VALUES (?1, ?2)",
            params![truncate_utf16(query, 200), now - index as i64],
        )?;
    }

    // Свои названия: у каждой правки побеждает более поздняя.
    for edit in &bundle.overrides {
        let here: Option<i64> =
            t.query_row("SELECT updated_at FROM track_overrides WHERE video_id = ?1", [&edit.video_id], |r| r.get(0)).optional()?;
        if here.is_some_and(|at| at >= edit.updated_at) || !is_track(&mut known, &edit.video_id)? {
            continue;
        }
        t.execute(
            "INSERT OR REPLACE INTO track_overrides (video_id, title, artists_text, album_title, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![edit.video_id, edit.title, edit.artists_text, edit.album_title, edit.updated_at.max(1)],
        )?;
    }

    // Накопленное время — на сервер заново (play.baseline atLeast) при следующей синхронизации.
    t.execute("INSERT OR REPLACE INTO sync_state (key, value) VALUES ('historyMerge', '1')", [])?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use melogold_core::lyrics::sync_rules::StoredLyrics;
    use melogold_core::music::Track;

    const RICK: &str = "dQw4w9WgXcQ";
    const OTHER: &str = "a1B2c3D4e5F";
    const LIKED_AT: i64 = 1_726_000_000_000;
    const PLAYED_AT: i64 = 1_726_000_100_000;
    const PLAYLIST_SYNC_ID: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
    const EVENT_SYNC_ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dir() -> Dir {
        let path = std::env::temp_dir().join(format!("melogold-backup-test-{}", melogold_core::ids::new_uuid()));
        std::fs::create_dir_all(&path).unwrap();
        Dir(path)
    }

    fn library(dir: &Dir) -> std::sync::Arc<Library> {
        Library::open(Database::open(&dir.0.join(format!("library-{}.db", melogold_core::ids::new_uuid()))).unwrap())
    }

    fn scalar<T: rusqlite::types::FromSql>(library: &Library, sql: &str) -> T {
        library.database().read(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
    }

    fn vitune30(dir: &Dir) -> PathBuf {
        let file = dir.0.join(format!("vitune-{}.db", melogold_core::ids::new_uuid()));
        let c = Connection::open(&file).unwrap();
        c.execute_batch(&format!(
            "CREATE TABLE Song (id TEXT NOT NULL PRIMARY KEY, title TEXT NOT NULL, artistsText TEXT, durationText TEXT,
                thumbnailUrl TEXT, likedAt INTEGER, totalPlayTimeMs INTEGER NOT NULL, loudnessBoost REAL,
                blacklisted INTEGER NOT NULL DEFAULT 0, explicit INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE Event (id INTEGER PRIMARY KEY AUTOINCREMENT, songId TEXT NOT NULL, timestamp INTEGER NOT NULL, playTime INTEGER NOT NULL);
            CREATE TABLE Lyrics (songId TEXT NOT NULL PRIMARY KEY, fixed TEXT, synced TEXT, startTime INTEGER);
            CREATE TABLE Album (id TEXT NOT NULL PRIMARY KEY, title TEXT, thumbnailUrl TEXT, year TEXT, authorsText TEXT,
                shareUrl TEXT, timestamp INTEGER, bookmarkedAt INTEGER, description TEXT, otherInfo TEXT);
            CREATE TABLE Artist (id TEXT NOT NULL PRIMARY KEY, name TEXT, thumbnailUrl TEXT, timestamp INTEGER, bookmarkedAt INTEGER);
            CREATE TABLE SongAlbumMap (songId TEXT NOT NULL, albumId TEXT NOT NULL, position INTEGER, PRIMARY KEY (songId, albumId));
            CREATE TABLE SongArtistMap (songId TEXT NOT NULL, artistId TEXT NOT NULL, PRIMARY KEY (songId, artistId));
            CREATE TABLE Playlist (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, browseId TEXT, thumbnail TEXT);
            CREATE TABLE SongPlaylistMap (songId TEXT NOT NULL, playlistId INTEGER NOT NULL, position INTEGER NOT NULL, PRIMARY KEY (songId, playlistId));
            CREATE TABLE SearchQuery (id INTEGER PRIMARY KEY AUTOINCREMENT, query TEXT NOT NULL);
            INSERT INTO Song VALUES ('{RICK}', 'Never Gonna Give You Up', 'Rick Astley', '3:33', 'https://i.ytimg.com/vi/{RICK}/hq.jpg', {LIKED_AT}, 600000, NULL, 0, 0);
            INSERT INTO Song VALUES ('{OTHER}', '', NULL, NULL, NULL, NULL, 0, NULL, 0, 1);
            INSERT INTO Song VALUES ('local:42', 'My file.mp3', NULL, NULL, NULL, NULL, 90000, NULL, 0, 0);
            INSERT INTO Event (songId, timestamp, playTime) VALUES ('{RICK}', {PLAYED_AT}, 215000);
            INSERT INTO Event (songId, timestamp, playTime) VALUES ('{RICK}', {}, 385000);
            INSERT INTO Event (songId, timestamp, playTime) VALUES ('{OTHER}', {}, 0);
            INSERT INTO Event (songId, timestamp, playTime) VALUES ('{OTHER}', 5, 1000);
            INSERT INTO Event (songId, timestamp, playTime) VALUES ('local:42', {PLAYED_AT}, 90000);
            INSERT INTO Lyrics VALUES ('{RICK}', '', '[00:01.00]Never gonna give you up', NULL);
            INSERT INTO Album (id, title, bookmarkedAt) VALUES ('MPREb_album1', 'Whenever You Need Somebody', {LIKED_AT});
            INSERT INTO SongAlbumMap VALUES ('{RICK}', 'MPREb_album1', 1);
            INSERT INTO Playlist (name) VALUES ('Дорога');
            INSERT INTO SongPlaylistMap VALUES ('{OTHER}', 1, 0);
            INSERT INTO SongPlaylistMap VALUES ('{RICK}', 1, 1);
            INSERT INTO SongPlaylistMap VALUES ('local:42', 1, 2);
            INSERT INTO SearchQuery (query) VALUES ('rick astley');
            PRAGMA user_version = 30;",
            PLAYED_AT + 1_000_000,
            PLAYED_AT + 2_000_000,
        ))
        .unwrap();
        file
    }

    #[test]
    fn a_vitune_v30_backup_merges_once() {
        let dir = dir();
        let library = library(&dir);
        let first = import(&library, &vitune30(&dir), "test").unwrap();
        assert_eq!(
            (
                first.version,
                first.tracks,
                first.local_skipped,
                first.plays,
                first.dates_skipped,
                first.favorites,
                first.lyrics,
                first.playlists,
                first.saved
            ),
            (30, 2, 1, 3, 1, 1, 1, 1, 1)
        );
        assert_eq!(library.track(RICK).unwrap().unwrap().title, "Never Gonna Give You Up");
        assert_eq!(library.favorites().unwrap().iter().map(|t| t.video_id.as_str()).collect::<Vec<_>>(), [RICK]);
        let first_id: String =
            scalar(&library, &format!("SELECT event_id FROM play_events WHERE video_id = '{RICK}' ORDER BY played_at LIMIT 1"));
        assert_eq!(first_id, import_ids::event_id(RICK, PLAYED_AT, 215_000));
        assert_eq!(scalar::<i64>(&library, "SELECT COUNT(*) FROM play_events WHERE synced = 1 OR device_id IS NOT NULL"), 0);
        assert_eq!(library.lyrics(RICK).unwrap().unwrap().synced.as_deref(), Some("[00:01.00]Never gonna give you up"));
        assert_eq!(library.saved_albums().unwrap().len(), 1);
        let road = library.playlists().unwrap().into_iter().find(|p| p.name == "Дорога").unwrap();
        let order: Vec<String> = library.playlist_tracks(road.id).unwrap().into_iter().map(|t| t.video_id).collect();
        assert_eq!(order, [OTHER, RICK]);
        assert_eq!(scalar::<String>(&library, "SELECT value FROM sync_state WHERE key = 'historyMerge'"), "1");

        let again = import(&library, &vitune30(&dir), "test").unwrap();
        assert_eq!((again.tracks, again.plays, again.plays_known, again.favorites, again.playlists), (0, 0, 3, 0, 0));
        assert_eq!(library.play_count().unwrap(), 3);
    }

    #[test]
    fn a_copy_of_melogold_goes_round() {
        let dir = dir();
        let here = library(&dir);
        let rick =
            Track { video_id: RICK.into(), title: "Never Gonna Give You Up".into(), duration_ms: Some(213_000), ..Default::default() };
        here.set_liked(std::slice::from_ref(&rick), true).unwrap();
        here.save_lyrics(
            RICK,
            &StoredLyrics { plain: Some("Мои слова".into()), plain_source: Some(sources::USER.into()), ..Default::default() },
        )
        .unwrap();
        here.set_override(RICK, Some("Свой Рик"), None, None).unwrap();
        let playlist = here.create_playlist("Дорога", &[rick]).unwrap();
        here.database()
            .write(|t| {
                t.execute("UPDATE playlists SET sync_id = ?1 WHERE id = ?2", params![PLAYLIST_SYNC_ID, playlist])?;
                t.execute(
                    "INSERT INTO play_events (event_id, video_id, played_at, play_time_ms, synced) VALUES (?1, ?2, ?3, 215000, 1)",
                    params![EVENT_SYNC_ID, RICK, PLAYED_AT],
                )
            })
            .unwrap();

        let copy = dir.0.join("Melogold_backup.db");
        export(&here, &copy, "linux", "0.1.0").unwrap();
        {
            let c = Connection::open(&copy).unwrap();
            assert_eq!(
                c.query_row("SELECT value FROM MelogoldBackup WHERE key = 'platform'", [], |r| r.get::<_, String>(0)).unwrap(),
                "linux"
            );
            assert_eq!(c.query_row("SELECT durationText FROM Song", [], |r| r.get::<_, String>(0)).unwrap(), "3:33");
            assert_eq!(c.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).unwrap(), 31);
        }

        // Другое устройство: тот же плейлист переименован там, больше ничего.
        let there = library(&dir);
        let theirs = there.create_playlist("Road trip", &[]).unwrap();
        there
            .database()
            .write(|t| t.execute("UPDATE playlists SET sync_id = ?1 WHERE id = ?2", params![PLAYLIST_SYNC_ID, theirs]))
            .unwrap();
        let summary = import(&there, &copy, "0.1.0").unwrap();
        assert_eq!((summary.tracks, summary.plays), (1, 1));
        assert_eq!(there.lyrics(RICK).unwrap().unwrap().plain_source.as_deref(), Some("user"));
        assert_eq!(there.playlists().unwrap().iter().map(|p| p.id).collect::<Vec<_>>(), [theirs]);
        assert_eq!(there.playlist_tracks(theirs).unwrap().iter().map(|t| t.video_id.as_str()).collect::<Vec<_>>(), [RICK]);
        assert_eq!(scalar::<String>(&there, "SELECT event_id FROM play_events"), EVENT_SYNC_ID);
        assert_eq!(there.track_override(RICK).and_then(|o| o.title).as_deref(), Some("Свой Рик"));
    }

    #[test]
    fn a_library_file_is_converted() {
        let dir = dir();
        let old_path = dir.0.join("old.db");
        {
            let old = Library::open(Database::open(&old_path).unwrap());
            let rick = Track { video_id: RICK.into(), title: "Never Gonna Give You Up".into(), ..Default::default() };
            old.record_play(&rick, 60_000, PLAYED_AT).unwrap();
            old.set_liked(&[rick], true).unwrap();
        }
        let library = library(&dir);
        let summary = import(&library, &old_path, "test").unwrap();
        assert_eq!((summary.tracks, summary.plays, summary.favorites), (1, 1, 1));
    }

    #[test]
    fn foreign_files_are_not_backups() {
        let dir = dir();
        let text = dir.0.join("hello.db");
        std::fs::write(&text, "hello").unwrap();
        assert_eq!(import(&library(&dir), &text, "test"), Err(ImportError::NotABackup));
        let innertune = dir.0.join("innertune.db");
        Connection::open(&innertune).unwrap().execute_batch("CREATE TABLE song (id TEXT PRIMARY KEY); PRAGMA user_version = 20;").unwrap();
        assert_eq!(import(&library(&dir), &innertune, "test"), Err(ImportError::Unsupported));
        let ancient = dir.0.join("ancient.db");
        Connection::open(&ancient)
            .unwrap()
            .execute_batch("CREATE TABLE Song (id TEXT PRIMARY KEY, title TEXT); PRAGMA user_version = 5;")
            .unwrap();
        assert_eq!(import(&library(&dir), &ancient, "test"), Err(ImportError::TooOld));
    }

    /// Настоящая копия по пути из `MELOGOLD_IMPORT_SAMPLE` (в репозитории её нет: это чья-то история).
    #[test]
    fn a_real_backup_when_given() {
        let Some(path) = std::env::var_os("MELOGOLD_IMPORT_SAMPLE").map(PathBuf::from).filter(|p| p.exists()) else { return };
        let dir = dir();
        let sample = library(&dir);
        let summary = import(&sample, &path, "test").unwrap();
        println!("IMPORT SAMPLE: {summary:?}");
        assert_eq!(
            (summary.tracks, summary.plays, summary.favorites, summary.lyrics, summary.saved, summary.local_skipped),
            (195, 16_046, 21, 173, 3, 16)
        );
        let copy = dir.0.join("round.db");
        export(&sample, &copy, "linux", "test").unwrap();
        let again = import(&library(&dir), &copy, "test").unwrap();
        assert_eq!((again.plays, again.favorites), (summary.plays, summary.favorites));
    }
}
