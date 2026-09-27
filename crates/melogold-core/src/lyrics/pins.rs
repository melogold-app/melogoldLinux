//! Закреплённый текст (задание 0006, Windows 0012, Android `LyricsPins.kt`): прослушал трек 30 с с
//! найденным автоматически текстом и не менял — текст закрепляется ссылкой у поставщика, и все
//! устройства аккаунта показывают его, а не ищут свой. Первое закрепление — общее: уже
//! закреплённый текст не перезакрепляется.
//!
//! Порядок выбора текста трека: свой → закреплённый → поиск → общий с сервера.

use super::sync_rules::{is_own, sources, StoredLyrics};

/// Столько прослушал трек с найденным автоматически текстом — текст закрепляется.
pub const PIN_AFTER_MS: i64 = 30_000;

/// Ссылка на текст у поставщика: `lrclib` — id записи, `youtube_music` — browseId `MPLYt…`,
/// `kugou` — `id:accesskey`. `start_time_ms` — сдвиг «позже» (где в треке начинается текст).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricsPin {
    pub video_id: String,
    pub source: String,
    pub reference: String,
    pub start_time_ms: Option<i64>,
    pub updated_at: i64,
}

impl LyricsPin {
    /// Те же поля, что знает сервер (время правки не в счёт).
    pub fn same_fields(&self, other: &LyricsPin) -> bool {
        self.source == other.source && self.reference == other.reference && self.start_time_ms == other.start_time_ms
    }
}

const PIN_SOURCES: [&str; 3] = [sources::YOUTUBE_MUSIC, sources::LRCLIB, sources::KUGOU];
const VIDEO_ID_LENGTH: usize = 11;

fn filled(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|v| !v.is_empty())
}

/// Сдвиг «позже» синхронного текста как `startTimeMs`: «раньше» остаётся на устройстве.
pub fn start_time_of(lyrics: &StoredLyrics) -> Option<i64> {
    (lyrics.offset_ms < 0).then_some(-lyrics.offset_ms)
}

/// Показывает ли `lyrics` текст, на который ссылается `pin` (любая сторона).
pub fn shows(lyrics: &StoredLyrics, pin: &LyricsPin) -> bool {
    (filled(&lyrics.synced)
        && lyrics.synced_ref.as_deref() == Some(pin.reference.as_str())
        && lyrics.synced_source.as_deref() == Some(pin.source.as_str()))
        || (filled(&lyrics.plain)
            && lyrics.plain_ref.as_deref() == Some(pin.reference.as_str())
            && lyrics.plain_source.as_deref() == Some(pin.source.as_str()))
}

/// Закрепление найденного автоматически текста: сторона, которая на экране (синхронная, иначе
/// обычная), её поставщик и номер у него. `None` — свой текст или текст без ссылки.
pub fn pin_of(video_id: &str, lyrics: &StoredLyrics, now: i64) -> Option<LyricsPin> {
    if is_own(lyrics) || video_id.len() != VIDEO_ID_LENGTH {
        return None;
    }
    let synced = filled(&lyrics.synced);
    let (source, reference) = if synced {
        (&lyrics.synced_source, &lyrics.synced_ref)
    } else if filled(&lyrics.plain) {
        (&lyrics.plain_source, &lyrics.plain_ref)
    } else {
        return None;
    };
    let source = source.as_deref().filter(|s| PIN_SOURCES.contains(s))?;
    let reference = reference.as_deref().map(str::trim).filter(|r| !r.is_empty())?;
    Some(LyricsPin {
        video_id: video_id.to_owned(),
        source: source.to_owned(),
        reference: reference.to_owned(),
        start_time_ms: if synced { start_time_of(lyrics) } else { None },
        updated_at: now,
    })
}

/// Нужно ли достать закреплённый текст по ссылке вместо того, что лежит здесь: свой текст важнее
/// закрепления, а текст, который уже показывает закреплённый, доставать заново незачем.
pub fn needs_pinned(stored: Option<&StoredLyrics>, pin: Option<&LyricsPin>) -> bool {
    match (stored, pin) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(stored), Some(pin)) => !is_own(stored) && !shows(stored, pin),
    }
}

/// Закреплённый текст, как он ложится здесь: найденный автоматически, со сдвигом закрепления.
pub fn from_pinned(mut lyrics: StoredLyrics, pin: &LyricsPin, current: Option<&StoredLyrics>) -> StoredLyrics {
    lyrics.offset_ms = -pin.start_time_ms.unwrap_or(0);
    lyrics.language = lyrics.language.or_else(|| current.and_then(|c| c.language.clone()));
    lyrics.chosen = false;
    lyrics
}

/// После сдвига текста: закрепление этого текста забирает сдвиг «позже». `None` — закрепление не меняется.
pub fn shifted(pin: &LyricsPin, lyrics: &StoredLyrics, now: i64) -> Option<LyricsPin> {
    if is_own(lyrics) || !shows(lyrics, pin) || lyrics.offset_ms > 0 {
        return None;
    }
    let start_time_ms = start_time_of(lyrics);
    (start_time_ms != pin.start_time_ms).then(|| LyricsPin { start_time_ms, updated_at: now, ..pin.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(source: &str, reference: &str) -> StoredLyrics {
        StoredLyrics {
            synced: Some("[00:01.00]Строка".into()),
            plain: Some("Строка".into()),
            synced_source: Some(source.into()),
            plain_source: Some(sources::YOUTUBE_MUSIC.into()),
            synced_ref: Some(reference.into()),
            plain_ref: Some("MPLYt_plain".into()),
            ..Default::default()
        }
    }

    #[test]
    fn pins_the_side_on_screen() {
        let pin = pin_of("dQw4w9WgXcQ", &found(sources::LRCLIB, "33476831"), 1).unwrap();
        assert_eq!((pin.source.as_str(), pin.reference.as_str(), pin.start_time_ms), ("lrclib", "33476831", None));
        let mut plain_only = found(sources::LRCLIB, "33476831");
        plain_only.synced = Some(String::new());
        let pin = pin_of("dQw4w9WgXcQ", &plain_only, 1).unwrap();
        assert_eq!((pin.source.as_str(), pin.reference.as_str()), ("youtube_music", "MPLYt_plain"));
        let mut later = found(sources::KUGOU, "12:ab");
        later.offset_ms = -1500;
        assert_eq!(pin_of("dQw4w9WgXcQ", &later, 1).unwrap().start_time_ms, Some(1500));
    }

    #[test]
    fn own_and_unreferenced_lyrics_are_not_pinned() {
        let mut chosen = found(sources::LRCLIB, "1");
        chosen.chosen = true;
        assert_eq!(pin_of("dQw4w9WgXcQ", &chosen, 1), None);
        assert_eq!(pin_of("dQw4w9WgXcQ", &found(sources::USER, "1"), 1), None);
        assert_eq!(pin_of("dQw4w9WgXcQ", &found(sources::MELOGOLD, "1"), 1), None);
        let mut no_ref = found(sources::LRCLIB, "1");
        no_ref.synced_ref = None;
        assert_eq!(pin_of("dQw4w9WgXcQ", &no_ref, 1), None);
        assert_eq!(pin_of("local:1", &found(sources::LRCLIB, "1"), 1), None);
    }

    #[test]
    fn order_own_then_pinned_then_search() {
        let pin = LyricsPin {
            video_id: "dQw4w9WgXcQ".into(),
            source: "lrclib".into(),
            reference: "7".into(),
            start_time_ms: Some(800),
            updated_at: 1,
        };
        // Ничего нет — закреплённый.
        assert!(needs_pinned(None, Some(&pin)));
        // Найденный здесь другой текст уступает закреплённому.
        assert!(needs_pinned(Some(&found(sources::LRCLIB, "9")), Some(&pin)));
        // Уже тот же — не доставать.
        assert!(!needs_pinned(Some(&found(sources::LRCLIB, "7")), Some(&pin)));
        // Свой важнее закрепления.
        let mut own = found(sources::LRCLIB, "9");
        own.chosen = true;
        assert!(!needs_pinned(Some(&own), Some(&pin)));
        // Без закрепления — обычный поиск.
        assert!(!needs_pinned(None, None));
        let placed = from_pinned(found(sources::LRCLIB, "7"), &pin, None);
        assert_eq!((placed.offset_ms, placed.chosen), (-800, false));
    }

    #[test]
    fn later_shift_moves_the_pin_earlier_stays_here() {
        let mut lyrics = found(sources::LRCLIB, "7");
        let pin = pin_of("dQw4w9WgXcQ", &lyrics, 1).unwrap();
        lyrics.offset_ms = -500;
        assert_eq!(shifted(&pin, &lyrics, 2).unwrap().start_time_ms, Some(500));
        lyrics.offset_ms = 500;
        assert_eq!(shifted(&pin, &lyrics, 2), None);
        let other = found(sources::LRCLIB, "8");
        assert_eq!(shifted(&pin, &other, 2), None);
    }
}
