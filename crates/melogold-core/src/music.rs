//! Модели каталога YouTube Music и YouTube (Windows `Music/Models.cs`, Android `core/data`).
//!
//! Трек — любое видео YouTube: песня YTM, клип, обычное видео, трансляция (docs/PROMPT.md §1:
//! обычный YouTube — такой же источник). `video_type` — как в `TrackDto` сервера:
//! `song | video | ugc | live | podcast_episode`, строкой (грабли §9 п. 12).

use serde::{Deserialize, Serialize};

/// Исполнитель или канал в подписи трека: `id` — browseId (`UC…`), может отсутствовать.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistRef {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub artists: Vec<ArtistRef>,
    /// Подпись исполнителей как в YouTube («A, B и C»); у видео — имя канала.
    pub artists_text: Option<String>,
    pub album_id: Option<String>,
    pub album_title: Option<String>,
    pub duration_ms: Option<i64>,
    pub duration_text: Option<String>,
    pub thumbnail_url: Option<String>,
    pub explicit: bool,
    pub video_type: Option<String>,
    /// Только для показа: «1,2 млн просмотров» — числа из строк YouTube не разбираются.
    pub views_text: Option<String>,
    pub unavailable: bool,
}

impl Track {
    pub fn is_video(&self) -> bool {
        matches!(self.video_type.as_deref(), Some("video" | "ugc" | "live" | "podcast_episode"))
    }

    pub fn is_live(&self) -> bool {
        self.video_type.as_deref() == Some("live")
    }

    /// «Исполнитель · Альбом».
    pub fn subtitle(&self) -> String {
        join_non_empty(&[self.artists_text.as_deref(), self.album_title.as_deref()])
    }

    /// Первый исполнитель со ссылкой: «Открыть исполнителя» или «Открыть канал».
    pub fn artist_id(&self) -> Option<&str> {
        self.artists.iter().find_map(|artist| artist.id.as_deref())
    }
}

/// Альбом, сингл или EP (`MPREb_…`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AlbumItem {
    pub browse_id: String,
    pub title: String,
    pub artists: Vec<ArtistRef>,
    pub artists_text: Option<String>,
    pub year: Option<String>,
    /// «Альбом», «Сингл», «EP» — как пришло от YouTube.
    pub type_text: Option<String>,
    pub thumbnail_url: Option<String>,
    /// `OLAK5uy_…` — плейлист альбома для «Слушать».
    pub playlist_id: Option<String>,
    pub explicit: bool,
}

impl AlbumItem {
    pub fn subtitle(&self) -> String {
        join_non_empty(&[self.type_text.as_deref(), self.artists_text.as_deref(), self.year.as_deref()])
    }
}

/// Исполнитель YouTube Music или канал YouTube (`UC…`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ArtistItem {
    pub browse_id: String,
    pub name: String,
    pub subtitle: Option<String>,
    pub thumbnail_url: Option<String>,
    /// Канал обычного YouTube без музыкального профиля.
    pub is_channel: bool,
}

/// Плейлист YouTube; `playlist_id` без префикса `VL`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlaylistItem {
    pub playlist_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub thumbnail_url: Option<String>,
}

impl PlaylistItem {
    pub fn browse_id(&self) -> String {
        format!("VL{}", self.playlist_id)
    }
}

/// Плитка «Настроения и жанры».
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MoodItem {
    pub title: String,
    pub browse_id: String,
    pub params: Option<String>,
    /// Цвет полоски плитки, ARGB.
    pub color: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MusicItem {
    Track(Track),
    Album(AlbumItem),
    Artist(ArtistItem),
    Playlist(PlaylistItem),
    Mood(MoodItem),
}

impl MusicItem {
    /// Ключ для отбрасывания повторов в выдаче.
    pub fn key(&self) -> String {
        match self {
            MusicItem::Track(t) => format!("t:{}", t.video_id),
            MusicItem::Album(a) => format!("a:{}", a.browse_id),
            MusicItem::Artist(r) => format!("r:{}", r.browse_id),
            MusicItem::Playlist(p) => format!("p:{}", p.playlist_id),
            MusicItem::Mood(m) => format!("m:{}{}", m.browse_id, m.params.as_deref().unwrap_or_default()),
        }
    }

    pub fn as_track(&self) -> Option<&Track> {
        match self {
            MusicItem::Track(track) => Some(track),
            _ => None,
        }
    }
}

/// Полка страницы: заголовок, элементы и переход «Все ›».
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Shelf {
    pub title: Option<String>,
    pub items: Vec<MusicItem>,
    pub more_browse_id: Option<String>,
    pub more_params: Option<String>,
}

impl Shelf {
    pub fn tracks(&self) -> impl Iterator<Item = &Track> {
        self.items.iter().filter_map(MusicItem::as_track)
    }
}

/// Выдача «Всё»: лучший результат и смешанный список.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchSummary {
    pub top: Option<MusicItem>,
    pub items: Vec<MusicItem>,
}

/// Страница выдачи с продолжением.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemsPage {
    pub items: Vec<MusicItem>,
    pub continuation: Option<String>,
}

/// Очередь из «Далее» (радио или плейлист), вкладки текста и похожих.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NextPage {
    pub tracks: Vec<Track>,
    pub continuation: Option<String>,
    pub playlist_id: Option<String>,
    pub lyrics_browse_id: Option<String>,
    pub related_browse_id: Option<String>,
}

/// Убирает повторы, сохраняя порядок первого появления.
pub fn distinct(items: Vec<MusicItem>) -> Vec<MusicItem> {
    let mut seen = std::collections::HashSet::new();
    items.into_iter().filter(|item| seen.insert(item.key())).collect()
}

fn join_non_empty(parts: &[Option<&str>]) -> String {
    parts.iter().flatten().map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_skips_missing_parts() {
        let track = Track { artists_text: Some("Rick Astley".into()), ..Default::default() };
        assert_eq!(track.subtitle(), "Rick Astley");
        let track = Track { artists_text: Some("A".into()), album_title: Some("B".into()), ..Default::default() };
        assert_eq!(track.subtitle(), "A · B");
    }

    #[test]
    fn video_types() {
        for (kind, video) in [("song", false), ("video", true), ("ugc", true), ("live", true)] {
            let track = Track { video_type: Some(kind.into()), ..Default::default() };
            assert_eq!(track.is_video(), video, "{kind}");
        }
    }

    #[test]
    fn items_round_trip_through_json() {
        let item = MusicItem::Track(Track { video_id: "dQw4w9WgXcQ".into(), title: "Never".into(), ..Default::default() });
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"kind\":\"track\""), "{json}");
        assert_eq!(serde_json::from_str::<MusicItem>(&json).unwrap(), item);
    }
}
