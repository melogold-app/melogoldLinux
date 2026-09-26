//! Свои название, исполнитель и альбом трека поверх того, что говорит YouTube (задание 0005,
//! Android `TrackOverride`, API §4.8 `track.override.set`): собранный из разрозненных видео альбом
//! выглядит одним альбомом на всех устройствах. Правка применяется в одном месте — [`TrackOverride::apply`]
//! при показе трека; в базе у трека остаётся то, что дал YouTube.

use melogold_core::music::Track;

/// Самое длинное поле — 500 единиц UTF-16 (API §1.4, §4.8).
pub const TEXT_MAX: usize = 500;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackOverride {
    pub video_id: String,
    pub title: Option<String>,
    pub artists_text: Option<String>,
    pub album_title: Option<String>,
    pub updated_at: i64,
}

impl TrackOverride {
    /// Правка целиком, как её хранит сервер: поле обрезано по краям и до 500 единиц UTF-16, пустое — без правки.
    pub fn new(video_id: &str, title: Option<&str>, artists_text: Option<&str>, album_title: Option<&str>, updated_at: i64) -> Self {
        TrackOverride {
            video_id: video_id.to_owned(),
            title: clean(title),
            artists_text: clean(artists_text),
            album_title: clean(album_title),
            updated_at,
        }
    }

    /// Все поля пустые — правки нет.
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.artists_text.is_none() && self.album_title.is_none()
    }

    /// Трек, каким его показывать: правленые поля вместо YouTube, остальное — как было.
    pub fn apply(&self, track: &Track) -> Track {
        let mut shown = track.clone();
        if let Some(title) = &self.title {
            shown.title = title.clone();
        }
        if let Some(artists) = &self.artists_text {
            shown.artists_text = Some(artists.clone());
        }
        if let Some(album) = &self.album_title {
            shown.album_title = Some(album.clone());
        }
        shown
    }

    /// Совпадают ли поля (время правки не в счёт): так сравниваются правка и снимок сервера.
    pub fn same_fields(&self, other: &TrackOverride) -> bool {
        self.title == other.title && self.artists_text == other.artists_text && self.album_title == other.album_title
    }
}

/// Поле как у сервера: обрезано по краям, не длиннее 500 единиц UTF-16 (суррогатная пара не рвётся), пустое — `None`.
pub fn clean(value: Option<&str>) -> Option<String> {
    let text = value?.trim();
    if text.is_empty() {
        return None;
    }
    let mut units = 0;
    let mut end = text.len();
    for (index, ch) in text.char_indices() {
        if units + ch.len_utf16() > TEXT_MAX {
            end = index;
            break;
        }
        units += ch.len_utf16();
    }
    Some(text[..end].trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_fields_keep_youtube() {
        let track = Track {
            video_id: "v".into(),
            title: "Artist — Song (live, fan upload)".into(),
            artists_text: Some("Fan Channel".into()),
            album_title: None,
            ..Default::default()
        };
        let edit = TrackOverride::new("v", Some("  Song "), Some(""), Some("Lost Album"), 1);
        let shown = edit.apply(&track);
        assert_eq!(shown.title, "Song");
        assert_eq!(shown.artists_text.as_deref(), Some("Fan Channel"));
        assert_eq!(shown.album_title.as_deref(), Some("Lost Album"));
        assert!(TrackOverride::new("v", Some("\t"), None, Some("x"), 1).title.is_none());
        assert!(TrackOverride::new("v", Some(" "), None, Some("  "), 1).is_empty());
    }

    #[test]
    fn fields_are_cut_at_500_utf16_units() {
        assert_eq!(clean(Some(&"я".repeat(600))).map(|s| s.encode_utf16().count()), Some(500));
        // Эмодзи — две единицы UTF-16: на границе он не рвётся пополам.
        let text = format!("{}😀", "a".repeat(499));
        assert_eq!(clean(Some(&text)).map(|s| s.encode_utf16().count()), Some(499));
    }
}
