//! Лучший результат поиска (задание 0018, доктрина §4.6; Windows `SearchTopResult.cs`): первым,
//! крупной карточкой. YouTube Music сам ставит его первой полкой выдачи «Всё»
//! (`musicCardShelfRenderer`); если её нет, а имя исполнителя из выдачи совпадает с запросом — без
//! учёта регистра, ё/е и знаков, — лучшим становится он.

use crate::music::{MusicItem, SearchSummary};

/// Лучший результат выдачи «Всё» по запросу `query`; `None` — его нет.
pub fn pick(summary: &SearchSummary, query: &str) -> Option<MusicItem> {
    if let Some(top) = &summary.top {
        return Some(top.clone());
    }
    let wanted = normalize(query);
    if wanted.is_empty() {
        return None;
    }
    summary.items.iter().find(|item| matches!(item, MusicItem::Artist(artist) if normalize(&artist.name) == wanted)).cloned()
}

/// «Ёлка-Палка!» → «елка палка», «AC/DC» → «ac dc»: строчные, ё → е, буквы и цифры, один пробел
/// между словами.
pub fn normalize(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut space = false;
    for c in text.to_lowercase().chars().map(|c| if c == 'ё' { 'е' } else { c }) {
        if c.is_alphanumeric() {
            if space && !result.is_empty() {
                result.push(' ');
            }
            result.push(c);
            space = false;
        } else {
            space = true;
        }
    }
    result
}

/// Тот же объект: лучший результат строкой ниже не повторяется (трек — по videoId, альбом и
/// исполнитель — по browseId, плейлист — по playlistId).
pub fn same(a: &MusicItem, b: Option<&MusicItem>) -> bool {
    b.is_some_and(|b| a.key() == b.key())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::{AlbumItem, ArtistItem, Track};

    fn artist(id: &str, name: &str) -> MusicItem {
        MusicItem::Artist(ArtistItem { browse_id: id.into(), name: name.into(), ..Default::default() })
    }

    #[test]
    fn words_are_compared_without_case_yo_and_signs() {
        assert_eq!(normalize("Ёлка-Палка!"), "елка палка");
        assert_eq!(normalize("AC/DC"), "ac dc");
        assert_eq!(normalize("  Michael   Jackson "), "michael jackson");
        assert_eq!(normalize("—"), "");
    }

    #[test]
    fn the_card_of_youtube_music_wins() {
        let card = MusicItem::Album(AlbumItem { browse_id: "MPRE".into(), title: "OK Computer".into(), ..Default::default() });
        let summary = SearchSummary { top: Some(card.clone()), items: vec![artist("UC1", "OK Computer")] };
        assert_eq!(pick(&summary, "ok computer"), Some(card));
    }

    #[test]
    fn without_a_card_the_artist_named_like_the_query_goes_first() {
        let track = MusicItem::Track(Track { video_id: "v".into(), title: "Кино".into(), ..Default::default() });
        let summary = SearchSummary { top: None, items: vec![track, artist("UCx", "Ёлка"), artist("UCk", "Кино")] };
        assert_eq!(pick(&summary, "КИНО"), Some(artist("UCk", "Кино")));
        assert_eq!(pick(&summary, "елка"), Some(artist("UCx", "Ёлка")));
        assert_eq!(pick(&summary, "Кино Группа крови"), None, "частичное совпадение — не лучший результат");
        assert_eq!(pick(&summary, "  "), None);
    }

    #[test]
    fn the_same_object_is_recognised_by_its_id() {
        let a = artist("UCk", "Кино");
        assert!(same(&a, Some(&artist("UCk", "КИНО"))));
        assert!(!same(&a, Some(&artist("UCz", "Кино"))));
        assert!(!same(&a, None));
    }
}
