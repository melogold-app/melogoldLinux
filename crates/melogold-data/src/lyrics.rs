//! Тексты песен в библиотеке (Windows `Library.cs` «Тексты», `SyncStore.cs`): кэш найденного, свои
//! тексты, закрепления (задание 0006) и снимок сервера для синхронизации. У каждой стороны текста
//! `None` — ещё не искали, пустая строка — искали и не нашли.

use std::collections::BTreeMap;

use melogold_core::lyrics::pins::{self, LyricsPin};
use melogold_core::lyrics::sync_rules::{LyricsSnapshot, StoredLyrics};
use melogold_core::text::now_ms;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::database::DbError;
use crate::library::{Change, Library};
use crate::sync_store::SyncTx;

const COLUMNS: &str = "synced, plain, source, plain_source, offset_ms, language, chosen, synced_ref, plain_ref";

/// Свой текст (как `sync_rules::is_own`): набранный или из файла, или выбранный и непустой.
const OWN: &str = "(source IN ('user', 'file') AND COALESCE(synced, '') <> '')
    OR (plain_source IN ('user', 'file') AND COALESCE(plain, '') <> '')
    OR (chosen = 1 AND (COALESCE(synced, '') <> '' OR COALESCE(plain, '') <> ''))";

/// Найденное в сети — кэш: «Очистить» свои и выбранные тексты не трогает.
const FETCHED_ONLY: &str =
    "COALESCE(source, '') NOT IN ('file', 'user') AND COALESCE(plain_source, '') NOT IN ('file', 'user') AND chosen = 0";

fn read(r: &Row, o: usize) -> rusqlite::Result<StoredLyrics> {
    Ok(StoredLyrics {
        synced: r.get(o)?,
        plain: r.get(o + 1)?,
        synced_source: r.get(o + 2)?,
        plain_source: r.get(o + 3)?,
        offset_ms: r.get(o + 4)?,
        language: r.get(o + 5)?,
        chosen: r.get::<_, i64>(o + 6)? != 0,
        synced_ref: r.get(o + 7)?,
        plain_ref: r.get(o + 8)?,
    })
}

fn get(c: &Connection, video_id: &str) -> rusqlite::Result<Option<StoredLyrics>> {
    c.query_row(&format!("SELECT {COLUMNS} FROM lyrics WHERE video_id = ?1"), [video_id], |r| read(r, 0)).optional()
}

fn write(c: &Connection, video_id: &str, lyrics: &StoredLyrics) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO lyrics (video_id, synced, plain, source, plain_source, offset_ms, language, chosen, synced_ref, plain_ref, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            video_id,
            lyrics.synced,
            lyrics.plain,
            lyrics.synced_source,
            lyrics.plain_source,
            lyrics.offset_ms,
            lyrics.language,
            lyrics.chosen as i64,
            lyrics.synced_ref,
            lyrics.plain_ref,
            now_ms()
        ],
    )?;
    Ok(())
}

fn pin(c: &Connection, video_id: &str) -> rusqlite::Result<Option<LyricsPin>> {
    c.query_row("SELECT video_id, source, ref, start_time_ms, updated_at FROM lyrics_pins WHERE video_id = ?1", [video_id], read_pin)
        .optional()
}

fn read_pin(r: &Row) -> rusqlite::Result<LyricsPin> {
    Ok(LyricsPin { video_id: r.get(0)?, source: r.get(1)?, reference: r.get(2)?, start_time_ms: r.get(3)?, updated_at: r.get(4)? })
}

fn write_pin(c: &Connection, pin: &LyricsPin) -> rusqlite::Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO lyrics_pins (video_id, source, ref, start_time_ms, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![pin.video_id, pin.source, pin.reference, pin.start_time_ms, pin.updated_at],
    )?;
    Ok(())
}

impl Library {
    /// Текст трека из кэша.
    pub fn lyrics(&self, video_id: &str) -> Result<Option<StoredLyrics>, DbError> {
        self.database().read(|c| get(c, video_id))
    }

    /// Записать текст; свой через 2 с уходит на сервер ([`Change::LYRICS`]).
    pub fn save_lyrics(&self, video_id: &str, lyrics: &StoredLyrics) -> Result<(), DbError> {
        self.database().write(|t| write(t, video_id, lyrics))?;
        self.notify(Change::LYRICS);
        Ok(())
    }

    /// Размер найденных в сети текстов, байт (кэш: их можно найти снова).
    pub fn fetched_lyrics_size(&self) -> Result<i64, DbError> {
        self.database().read(|c| {
            c.query_row(
                &format!(
                    "SELECT COALESCE(SUM(LENGTH(COALESCE(synced, '')) + LENGTH(COALESCE(plain, ''))), 0) FROM lyrics WHERE {FETCHED_ONLY}"
                ),
                [],
                |r| r.get(0),
            )
        })
    }

    /// Очистка кэша: найденные в сети тексты забываются, свои, выбранные и из файлов остаются.
    pub fn clear_fetched_lyrics(&self) -> Result<(), DbError> {
        self.database().write(|t| t.execute(&format!("DELETE FROM lyrics WHERE {FETCHED_ONLY}"), []).map(|_| ()))?;
        self.notify(Change::LYRICS);
        Ok(())
    }

    pub fn lyrics_pin(&self, video_id: &str) -> Result<Option<LyricsPin>, DbError> {
        self.database().read(|c| pin(c, video_id))
    }

    /// Трек проиграл 30 с: найденный автоматически текст закрепляется, если у трека ещё нет ни
    /// своего текста, ни закрепления (первое закрепление — общее). `true` — закрепил.
    pub fn pin_played(&self, video_id: &str) -> Result<bool, DbError> {
        let pinned = self.database().write(|t| {
            if pin(t, video_id)?.is_some() {
                return Ok(false);
            }
            let Some(new) = get(t, video_id)?.and_then(|lyrics| pins::pin_of(video_id, &lyrics, now_ms())) else {
                return Ok(false);
            };
            write_pin(t, &new)?;
            Ok(true)
        })?;
        if pinned {
            self.notify(Change::LYRICS);
        }
        Ok(pinned)
    }

    /// Текст сдвинули: закрепление этого текста забирает сдвиг «позже».
    pub fn shift_pin(&self, video_id: &str, lyrics: &StoredLyrics) -> Result<(), DbError> {
        let changed = self.database().write(|t| {
            let Some(shifted) = pin(t, video_id)?.and_then(|current| pins::shifted(&current, lyrics, now_ms())) else {
                return Ok(false);
            };
            write_pin(t, &shifted)?;
            Ok(true)
        })?;
        if changed {
            self.notify(Change::LYRICS);
        }
        Ok(())
    }
}

impl SyncTx<'_> {
    // ── свои тексты (API §4.10) ──

    pub fn own_lyrics(&self) -> rusqlite::Result<BTreeMap<String, StoredLyrics>> {
        let mut statement = self.t.prepare(&format!("SELECT video_id, {COLUMNS} FROM lyrics WHERE {OWN}"))?;
        let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, read(r, 1)?)))?;
        rows.collect()
    }

    pub fn lyrics(&self, video_id: &str) -> rusqlite::Result<Option<StoredLyrics>> {
        get(self.t, video_id)
    }

    pub fn save_lyrics(&self, video_id: &str, lyrics: &StoredLyrics) -> rusqlite::Result<()> {
        write(self.t, video_id, lyrics)?;
        self.changed(Change::LYRICS);
        Ok(())
    }

    pub fn delete_lyrics(&self, video_id: &str) -> rusqlite::Result<()> {
        self.t.execute("DELETE FROM lyrics WHERE video_id = ?1", [video_id])?;
        self.changed(Change::LYRICS);
        Ok(())
    }

    /// Снимок своих версий на сервере.
    pub fn synced_lyrics(&self) -> rusqlite::Result<BTreeMap<String, LyricsSnapshot>> {
        let mut statement = self.t.prepare("SELECT video_id, rev, hash FROM synced_lyrics")?;
        let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, LyricsSnapshot { rev: r.get(1)?, hash: r.get(2)? })))?;
        rows.collect()
    }

    pub fn synced_lyrics_of(&self, video_id: &str) -> rusqlite::Result<Option<LyricsSnapshot>> {
        self.t
            .query_row("SELECT rev, hash FROM synced_lyrics WHERE video_id = ?1", [video_id], |r| {
                Ok(LyricsSnapshot { rev: r.get(0)?, hash: r.get(1)? })
            })
            .optional()
    }

    pub fn set_synced_lyrics(&self, video_id: &str, rev: i64, hash: &str) -> rusqlite::Result<()> {
        self.t.execute("INSERT OR REPLACE INTO synced_lyrics (video_id, rev, hash) VALUES (?1, ?2, ?3)", params![video_id, rev, hash])?;
        Ok(())
    }

    pub fn forget_synced_lyrics(&self, video_id: &str) -> rusqlite::Result<()> {
        self.t.execute("DELETE FROM synced_lyrics WHERE video_id = ?1", [video_id])?;
        Ok(())
    }

    // ── закрепления (задание 0006) ──

    pub fn lyrics_pins(&self) -> rusqlite::Result<BTreeMap<String, LyricsPin>> {
        self.pin_rows("SELECT video_id, source, ref, start_time_ms, updated_at FROM lyrics_pins")
    }

    /// Снимок сервера: какие закрепления он знает (поля без времени).
    pub fn synced_lyrics_pins(&self) -> rusqlite::Result<BTreeMap<String, LyricsPin>> {
        self.pin_rows("SELECT video_id, source, ref, start_time_ms, 0 FROM synced_lyrics_pins")
    }

    fn pin_rows(&self, sql: &str) -> rusqlite::Result<BTreeMap<String, LyricsPin>> {
        let mut statement = self.t.prepare(sql)?;
        let rows = statement.query_map([], |r| read_pin(r).map(|p| (p.video_id.clone(), p)))?;
        rows.collect()
    }

    /// Закрепление с сервера: и здесь, и в снимке — такое, как у сервера; `None` — снято.
    pub fn apply_lyrics_pin(&self, video_id: &str, pin: Option<&LyricsPin>) -> rusqlite::Result<()> {
        match pin {
            None => {
                self.t.execute("DELETE FROM lyrics_pins WHERE video_id = ?1", [video_id])?;
                self.t.execute("DELETE FROM synced_lyrics_pins WHERE video_id = ?1", [video_id])?;
            }
            Some(pin) => {
                write_pin(self.t, pin)?;
                self.t.execute(
                    "INSERT OR REPLACE INTO synced_lyrics_pins (video_id, source, ref, start_time_ms) VALUES (?1, ?2, ?3, ?4)",
                    params![pin.video_id, pin.source, pin.reference, pin.start_time_ms],
                )?;
            }
        }
        self.changed(Change::LYRICS);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use melogold_core::lyrics::sync_rules::sources;

    fn found() -> StoredLyrics {
        StoredLyrics {
            synced: Some("[00:01.00]Строка".into()),
            plain: Some(String::new()),
            synced_source: Some(sources::LRCLIB.into()),
            synced_ref: Some("33476831".into()),
            ..Default::default()
        }
    }

    #[test]
    fn lyrics_round_trip_and_cache_clear() {
        let lib = Library::open(Database::in_memory().unwrap());
        assert_eq!(lib.lyrics("dQw4w9WgXcQ").unwrap(), None);
        lib.save_lyrics("dQw4w9WgXcQ", &found()).unwrap();
        assert_eq!(lib.lyrics("dQw4w9WgXcQ").unwrap(), Some(found()));
        let own = StoredLyrics { plain: Some("Своё".into()), plain_source: Some(sources::USER.into()), ..Default::default() };
        lib.save_lyrics("fJ9rUzIMcZQ", &own).unwrap();
        assert!(lib.fetched_lyrics_size().unwrap() > 0);
        lib.clear_fetched_lyrics().unwrap();
        assert_eq!(lib.lyrics("dQw4w9WgXcQ").unwrap(), None);
        assert_eq!(lib.lyrics("fJ9rUzIMcZQ").unwrap(), Some(own.clone()));
        let own_rows = lib.sync(|tx| tx.own_lyrics()).unwrap();
        assert_eq!(own_rows.keys().collect::<Vec<_>>(), ["fJ9rUzIMcZQ"]);
    }

    #[test]
    fn first_pin_is_everyones() {
        let lib = Library::open(Database::in_memory().unwrap());
        // Текста нет — закреплять нечего.
        assert!(!lib.pin_played("dQw4w9WgXcQ").unwrap());
        lib.save_lyrics("dQw4w9WgXcQ", &found()).unwrap();
        assert!(lib.pin_played("dQw4w9WgXcQ").unwrap());
        let pin = lib.lyrics_pin("dQw4w9WgXcQ").unwrap().unwrap();
        assert_eq!((pin.source.as_str(), pin.reference.as_str()), ("lrclib", "33476831"));
        // Другой найденный текст не перезакрепляет.
        let other = StoredLyrics { synced_ref: Some("1".into()), ..found() };
        lib.save_lyrics("dQw4w9WgXcQ", &other).unwrap();
        assert!(!lib.pin_played("dQw4w9WgXcQ").unwrap());
        assert_eq!(lib.lyrics_pin("dQw4w9WgXcQ").unwrap().unwrap().reference, "33476831");
        // Сдвиг «позже» закреплённого текста уходит в закрепление.
        let later = StoredLyrics { offset_ms: -700, ..found() };
        lib.shift_pin("dQw4w9WgXcQ", &later).unwrap();
        assert_eq!(lib.lyrics_pin("dQw4w9WgXcQ").unwrap().unwrap().start_time_ms, Some(700));
        // С сервера: снято.
        lib.sync(|tx| tx.apply_lyrics_pin("dQw4w9WgXcQ", None)).unwrap();
        assert_eq!(lib.lyrics_pin("dQw4w9WgXcQ").unwrap(), None);
    }
}
