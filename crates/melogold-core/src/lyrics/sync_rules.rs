//! Свой текст и его синхронизация (docs/LYRICS-SYNC.md сервера §3, Windows `LyricsSyncRules.cs`).
//! Свой — хотя бы одна сторона из источника `user` или `file`, **или** выбранный в «Найти другой
//! текст» (`chosen`) и непустой: он уходит на сервер целиком, обеими сторонами, с настоящими
//! источниками. Найденный автоматически без выбора своим не считается (задание 0002).

use sha2::{Digest, Sha256};

use super::{detect, Format};

pub mod sources {
    pub const YOUTUBE_MUSIC: &str = "youtube_music";
    pub const LRCLIB: &str = "lrclib";
    pub const KUGOU: &str = "kugou";
    pub const FILE: &str = "file";
    pub const USER: &str = "user";
    /// Общий текст другого пользователя с сервера: показывается, но своим не становится.
    pub const MELOGOLD: &str = "melogold";
}

/// Лимиты сервера (API §4.10, в единицах UTF-16).
pub const PLAIN_MAX: usize = 50_000;
pub const SYNCED_MAX: usize = 200_000;
pub const LANGUAGE_MAX: usize = 35;
pub const START_TIME_MAX_MS: i64 = 86_400_000;

/// Текст трека, как его хранит устройство.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredLyrics {
    /// Синхронный текст (LRC или TTML); пустая строка — «искали, нет».
    pub synced: Option<String>,
    pub plain: Option<String>,
    pub synced_source: Option<String>,
    pub plain_source: Option<String>,
    /// Сдвиг, как `[offset:]` в LRC: положительный — текст раньше, отрицательный — позже.
    pub offset_ms: i64,
    pub language: Option<String>,
    /// Выбран в «Найти другой текст» или пришёл своей версией с сервера (задание 0002).
    pub chosen: bool,
    /// Ссылки у поставщика найденного текста (задание 0006): id LrcLib, browseId YouTube Music (`MPLYt…`),
    /// `id:accesskey` KuGou — для синхронной и обычной стороны.
    pub synced_ref: Option<String>,
    pub plain_ref: Option<String>,
}

/// Текст в форме сервера (`LyricsPut` и `LyricsText`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LyricsPayload {
    pub plain: Option<String>,
    pub plain_source: Option<String>,
    pub synced: Option<String>,
    pub synced_format: Option<String>,
    pub synced_source: Option<String>,
    pub start_time_ms: Option<i64>,
    pub language: Option<String>,
}

/// Что отправить на сервер по итогам сравнения со снимком.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LyricsSend {
    /// Своего текста нет в снимке или он изменился: `PUT`.
    Put { video_id: String, payload: LyricsPayload, hash: String },
    /// Свой текст был на сервере, а здесь его больше нет: `DELETE`.
    Delete { video_id: String },
    /// Текст сервер не принял (слишком большой), и здесь его больше нет: просто забыть.
    Forget { video_id: String },
}

/// Что снимок знает о своей версии на сервере: `rev` и хэш содержимого; `rev` [`REJECTED`] — сервер отказал.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricsSnapshot {
    pub rev: i64,
    pub hash: String,
}

pub const REJECTED: i64 = -1;

fn text(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}

fn is_own_source(source: &Option<String>) -> bool {
    matches!(source.as_deref(), Some(sources::USER | sources::FILE))
}

/// Источники, которые принимает сервер; общий текст (`melogold`) уходит без источника.
fn server_source(source: &Option<String>) -> Option<String> {
    source
        .as_deref()
        .filter(|s| [sources::USER, sources::FILE, sources::YOUTUBE_MUSIC, sources::LRCLIB, sources::KUGOU].contains(s))
        .map(str::to_owned)
}

/// Свой текст — набранный, импортированный или выбранный вместо найденного: он уходит на сервер.
pub fn is_own(lyrics: &StoredLyrics) -> bool {
    (lyrics.chosen && (text(&lyrics.synced).is_some() || text(&lyrics.plain).is_some()))
        || (is_own_source(&lyrics.synced_source) && text(&lyrics.synced).is_some())
        || (is_own_source(&lyrics.plain_source) && text(&lyrics.plain).is_some())
}

/// Свой только потому, что выбран, а не набран и не импортирован. Надгробие такой текст не удаляет,
/// а делает найденным (клиенты до задания 0002 удаляли с сервера выбранное).
pub fn is_chosen_only(lyrics: &StoredLyrics) -> bool {
    lyrics.chosen
        && !(is_own_source(&lyrics.synced_source) && text(&lyrics.synced).is_some())
        && !(is_own_source(&lyrics.plain_source) && text(&lyrics.plain).is_some())
}

/// Содержимое для `PUT`: пустые стороны не отправляются, источник — только вместе со своей стороной,
/// формат — по содержимому; сдвиг «позже» — как `startTimeMs` (где в треке начинается текст), сдвиг
/// «раньше» сервер не хранит — он остаётся на этом устройстве.
pub fn to_payload(lyrics: &StoredLyrics) -> LyricsPayload {
    let mut synced = text(&lyrics.synced).map(str::to_owned);
    let format = synced.as_deref().and_then(|s| match detect(s) {
        Format::Ttml => Some("ttml".to_owned()),
        Format::Lrc => Some("lrc".to_owned()),
        Format::Plain => None,
    });
    // Синхронный текст, который не разбирается ни как LRC, ни как TTML, серверу не нужен.
    if format.is_none() {
        synced = None;
    }
    let plain = text(&lyrics.plain).map(str::to_owned);
    LyricsPayload {
        plain_source: plain.as_ref().and_then(|_| server_source(&lyrics.plain_source)),
        plain,
        synced_source: synced.as_ref().and_then(|_| server_source(&lyrics.synced_source)),
        start_time_ms: if synced.is_none() || lyrics.offset_ms >= 0 { None } else { Some((-lyrics.offset_ms).min(START_TIME_MAX_MS)) },
        synced,
        synced_format: format,
        language: text(&lyrics.language).filter(|l| l.encode_utf16().count() <= LANGUAGE_MAX).map(str::to_owned),
    }
}

/// Сторона длиннее лимита сервера: такой текст не отправляется, он остаётся только здесь.
pub fn too_large(payload: &LyricsPayload) -> bool {
    payload.plain.as_ref().is_some_and(|p| p.encode_utf16().count() > PLAIN_MAX)
        || payload.synced.as_ref().is_some_and(|s| s.encode_utf16().count() > SYNCED_MAX)
}

/// Версия с сервера совпадает с тем, что уже лежит здесь (в том числе эхо своей же отправки).
pub fn same_content(local: Option<&StoredLyrics>, incoming: &StoredLyrics) -> bool {
    local.is_some_and(|local| hash(&to_payload(local)) == hash(&to_payload(incoming)))
}

/// Версия с сервера — как строка здесь: отсутствующая сторона — «искали, нет» (пустая), источники
/// как есть, и она — своя, из какого бы источника ни была (грабля §9 п. 14).
pub fn from_payload(payload: &LyricsPayload) -> StoredLyrics {
    StoredLyrics {
        synced: Some(payload.synced.clone().unwrap_or_default()),
        plain: Some(payload.plain.clone().unwrap_or_default()),
        synced_source: payload.synced.as_ref().and(payload.synced_source.clone()),
        plain_source: payload.plain.as_ref().and(payload.plain_source.clone()),
        offset_ms: -payload.start_time_ms.unwrap_or(0),
        language: payload.language.clone(),
        chosen: true,
        synced_ref: None,
        plain_ref: None,
    }
}

/// SHA-256 полей в постоянном порядке (тот же, что у Windows и Android).
pub fn hash(payload: &LyricsPayload) -> String {
    let field = |value: &Option<String>| value.clone().unwrap_or_else(|| "\u{0}".into());
    let text = [
        field(&payload.plain),
        field(&payload.plain_source),
        field(&payload.synced),
        field(&payload.synced_format),
        field(&payload.synced_source),
        payload.start_time_ms.map(|v| v.to_string()).unwrap_or_else(|| "\u{0}".into()),
        field(&payload.language),
    ]
    .join("\u{1f}");
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Отправка: свой текст, которого нет в снимке или чей хэш изменился, — `PUT`; строка снимка, у
/// которой больше нет своего текста, — `DELETE`. Отклонённый повторно не отправляется, пока не изменится.
pub fn plan_sends(
    own: &std::collections::BTreeMap<String, StoredLyrics>,
    snapshot: &std::collections::BTreeMap<String, LyricsSnapshot>,
) -> Vec<LyricsSend> {
    let mut sends = Vec::new();
    for (video_id, lyrics) in own {
        let payload = to_payload(lyrics);
        if payload.plain.is_none() && payload.synced.is_none() {
            continue;
        }
        let hash = hash(&payload);
        if snapshot.get(video_id).is_some_and(|known| known.hash == hash) {
            continue;
        }
        sends.push(LyricsSend::Put { video_id: video_id.clone(), payload, hash });
    }
    for (video_id, known) in snapshot {
        if own.contains_key(video_id) {
            continue;
        }
        sends.push(if known.rev == REJECTED {
            LyricsSend::Forget { video_id: video_id.clone() }
        } else {
            LyricsSend::Delete { video_id: video_id.clone() }
        });
    }
    sends
}

/// Надгробие с сервера: свой текст здесь удаляется, только если он не менялся с прошлого синка.
pub fn delete_on_tombstone(local: Option<&StoredLyrics>, snapshot: Option<&LyricsSnapshot>) -> bool {
    match (local, snapshot) {
        (Some(local), Some(snapshot)) => is_own(local) && hash(&to_payload(local)) == snapshot.hash,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn lrclib(chosen: bool) -> StoredLyrics {
        StoredLyrics {
            synced: Some("[00:01.00]Строка\n".into()),
            plain: Some("Строка".into()),
            synced_source: Some(sources::LRCLIB.into()),
            plain_source: Some(sources::LRCLIB.into()),
            chosen,
            ..Default::default()
        }
    }

    #[test]
    fn chosen_lrclib_is_own_and_keeps_its_source() {
        assert!(is_own(&lrclib(true)));
        assert!(!is_own(&lrclib(false)), "найденный автоматически — не свой");
        let payload = to_payload(&lrclib(true));
        assert_eq!(payload.synced_source.as_deref(), Some("lrclib"));
        assert_eq!(payload.synced_format.as_deref(), Some("lrc"));
        let mut own = BTreeMap::new();
        own.insert("v".to_owned(), lrclib(true));
        assert!(matches!(plan_sends(&own, &BTreeMap::new())[..], [LyricsSend::Put { .. }]));
        assert!(plan_sends(&BTreeMap::new(), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn server_version_with_lrclib_stays_own_and_is_not_deleted() {
        // Грабля Android и Apple: версия с сервера с источником lrclib считалась найденной и удалялась.
        let incoming = from_payload(&to_payload(&lrclib(true)));
        assert!(incoming.chosen && is_own(&incoming));
        let mut own = BTreeMap::new();
        own.insert("v".to_owned(), incoming.clone());
        let mut snapshot = BTreeMap::new();
        snapshot.insert("v".to_owned(), LyricsSnapshot { rev: 3, hash: hash(&to_payload(&incoming)) });
        assert!(plan_sends(&own, &snapshot).is_empty(), "не PUT и не DELETE");
        assert!(delete_on_tombstone(Some(&incoming), snapshot.get("v")));
        assert!(is_chosen_only(&incoming));
    }

    #[test]
    fn later_offset_goes_as_start_time_and_limits_hold() {
        let lyrics = StoredLyrics { offset_ms: -1500, ..lrclib(true) };
        assert_eq!(to_payload(&lyrics).start_time_ms, Some(1500));
        assert_eq!(to_payload(&StoredLyrics { offset_ms: 700, ..lrclib(true) }).start_time_ms, None);
        let huge = LyricsPayload { plain: Some("я".repeat(PLAIN_MAX + 1)), ..Default::default() };
        assert!(too_large(&huge));
    }
}
