//! Разбор настоящих ответов YouTube (фикстуры Android, `tests/fixtures/README.md`).

use melogold_core::music::MusicItem;
use melogold_innertube::music::{parse_next, parse_search, parse_search_summary, parse_web_search};
use melogold_innertube::player::PlayerResponse;
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

fn describe(item: &MusicItem) -> String {
    match item {
        MusicItem::Track(t) => format!(
            "трек {} «{}» · {:?} · {:?} · {:?} · {:?}",
            t.video_id, t.title, t.artists_text, t.album_title, t.duration_text, t.video_type
        ),
        MusicItem::Album(a) => format!("альбом {} «{}» · {:?} · {:?} · {:?}", a.browse_id, a.title, a.type_text, a.artists_text, a.year),
        MusicItem::Artist(a) => format!("исполнитель {} «{}» · {:?} · канал {}", a.browse_id, a.name, a.subtitle, a.is_channel),
        MusicItem::Playlist(p) => format!("плейлист {} «{}» · {:?}", p.playlist_id, p.title, p.subtitle),
        MusicItem::Mood(m) => format!("настроение «{}»", m.title),
    }
}

#[test]
fn search_all_has_top_result_and_mixed_items() {
    for name in ["ytm/search-all.ru.json", "ytm/search-all.en.json"] {
        let summary = parse_search_summary(&fixture(name));
        println!("{name}: лучший — {}", summary.top.as_ref().map(describe).unwrap_or_default());
        for item in summary.items.iter().take(8) {
            println!("  {}", describe(item));
        }
        assert!(summary.top.is_some(), "{name}: нет лучшего результата");
        assert!(summary.items.len() >= 5, "{name}: {} элементов", summary.items.len());
        assert!(summary.items.iter().any(|i| matches!(i, MusicItem::Track(_))));
        assert!(summary.items.iter().any(|i| matches!(i, MusicItem::Album(_) | MusicItem::Artist(_) | MusicItem::Playlist(_))));
    }
    // Лучший результат — исполнитель: у его песен в карточке исполнитель подразумевается, а в
    // подписи остаётся только «94M plays». Это не исполнитель, а число прослушиваний.
    let summary = parse_search_summary(&fixture("ytm/search-all.en.json"));
    let giorgio = summary.items.iter().filter_map(MusicItem::as_track).find(|t| t.video_id == "ZFZM6jDTWd4").unwrap();
    assert_eq!(giorgio.artists_text.as_deref(), Some("Daft Punk"));
    assert_eq!(giorgio.artists[0].id.as_deref(), Some("UCRr1xG_2WIDs18a6cIiCxeA"));
    assert_eq!(giorgio.views_text.as_deref(), Some("94M plays"));
}

#[test]
fn songs_have_artist_album_and_duration() {
    for name in ["ytm/search-songs.ru.json", "ytm/search-songs.en.json"] {
        let page = parse_search(&fixture(name));
        let tracks: Vec<_> = page.items.iter().filter_map(MusicItem::as_track).collect();
        println!("{name}: {} треков, продолжение {}", tracks.len(), page.continuation.is_some());
        for track in tracks.iter().take(3) {
            println!("  {}", describe(&MusicItem::Track((*track).clone())));
        }
        assert!(tracks.len() >= 10);
        assert!(page.continuation.is_some());
        for track in &tracks {
            assert_eq!(track.video_id.len(), 11, "{:?}", track.video_id);
            assert_eq!(track.video_type.as_deref(), Some("song"), "{}", track.title);
            assert!(track.duration_ms.is_some(), "{}: нет длительности", track.title);
            assert!(track.artists.iter().any(|a| a.id.is_some()), "{}: нет ссылки на исполнителя", track.title);
            assert!(track.album_id.as_deref().is_some_and(|a| a.starts_with("MPREb_")), "{}: нет альбома", track.title);
            assert!(track.thumbnail_url.is_some());
        }
    }
}

#[test]
fn filters_return_their_kind() {
    let albums = parse_search(&fixture("ytm/search-albums.ru.json"));
    assert!(albums.items.len() >= 5 && albums.items.iter().all(|i| matches!(i, MusicItem::Album(_))));
    let artists = parse_search(&fixture("ytm/search-artists.en.json"));
    assert!(artists.items.len() >= 5 && artists.items.iter().all(|i| matches!(i, MusicItem::Artist(_))));
    let playlists = parse_search(&fixture("ytm/search-community-playlists.ru.json"));
    assert!(playlists.items.len() >= 5 && playlists.items.iter().all(|i| matches!(i, MusicItem::Playlist(_))));
    println!("{}", describe(&albums.items[0]));
    println!("{}", describe(&artists.items[0]));
    println!("{}", describe(&playlists.items[0]));
}

#[test]
fn radio_queue_with_continuation_and_tabs() {
    let page = parse_next(&fixture("ytm/next-radio.ru.json"));
    println!("радио: {} треков, плейлист {:?}, текст {:?}", page.tracks.len(), page.playlist_id, page.lyrics_browse_id);
    assert!(page.tracks.len() >= 10);
    assert!(page.continuation.is_some());
    assert!(page.playlist_id.as_deref().is_some_and(|p| p.starts_with("RD")));
    assert!(page.related_browse_id.is_some());
    assert!(page.tracks.iter().all(|t| !t.title.is_empty() && t.duration_ms.is_some()));
}

#[test]
fn web_search_skips_shorts_and_marks_videos() {
    for name in ["web/search-videos.ru.json", "web/search-videos.en.json"] {
        let page = parse_web_search(&fixture(name));
        let tracks: Vec<_> = page.items.iter().filter_map(MusicItem::as_track).collect();
        println!("{name}: {} видео", tracks.len());
        for track in tracks.iter().take(3) {
            println!("  {}", describe(&MusicItem::Track((*track).clone())));
        }
        assert!(tracks.len() >= 10);
        assert!(page.continuation.is_some());
        assert!(tracks.iter().all(|t| t.is_video() && t.artists_text.is_some()));
    }
    let channels = parse_web_search(&fixture("web/search-channels.ru.json"));
    assert!(channels.items.iter().filter(|i| matches!(i, MusicItem::Artist(a) if a.is_channel)).count() >= 3);
    let playlists = parse_web_search(&fixture("web/search-playlists.en.json"));
    assert!(playlists.items.iter().filter(|i| matches!(i, MusicItem::Playlist(_))).count() >= 3);
    let live = parse_web_search(&fixture("web/search-live.en.json"));
    assert!(live.items.iter().filter_map(MusicItem::as_track).any(|t| t.is_live()));
}

#[test]
fn player_response_status_loudness_duration() {
    // У фикстуры ссылки потоков вырезаны при обезличивании: проверяется остальное.
    let response = PlayerResponse::parse(&fixture("ytm/player-ios.en.json"));
    println!("player: {} · громкость {:?} · {:?} мс", response.status, response.loudness_db, response.duration_ms);
    assert_eq!(response.status, "OK");
    // trackAbsoluteLoudnessLkfs −13,62 − loudnessTargetLkfs −14 = 0,38; у формата — тоже 0,38.
    assert!((response.loudness_db.unwrap() - 0.38).abs() < 0.01);
    assert!(response.audio_formats.is_empty(), "ссылки вырезаны — форматов с адресом нет");
    assert!(response.duration_ms.is_some_and(|ms| ms > 60_000));
}

// ── лучший результат поиска (задание 0018) ──

fn top_of(name: &str, query: &str) -> Option<MusicItem> {
    melogold_core::search_top::pick(&parse_search_summary(&fixture(&format!("search/{name}.json"))), query)
}

#[test]
fn an_artist_query_puts_the_artist_first() {
    for (name, query, artist) in
        [("kino", "Кино", "Кино"), ("michael-jackson", "Michael Jackson", "Michael Jackson"), ("tkay-maidza", "Tkay Maidza", "Tkay Maidza")]
    {
        match top_of(name, query) {
            Some(MusicItem::Artist(found)) => {
                assert_eq!(found.name, artist, "{name}");
                assert!(found.browse_id.starts_with("UC"), "{name}: {}", found.browse_id);
                assert!(found.thumbnail_url.is_some(), "{name}: у исполнителя нет фото");
            }
            other => panic!("{name}: ждал исполнителя, вышло {:?}", other.as_ref().map(describe)),
        }
    }
}

#[test]
fn an_album_and_a_track_query_put_them_first() {
    match top_of("ok-computer", "OK Computer") {
        Some(MusicItem::Album(album)) => {
            assert_eq!(album.title, "OK Computer");
            assert!(album.artists_text.as_deref().is_some_and(|a| a.contains("Radiohead")), "{:?}", album.artists_text);
        }
        other => panic!("ждал альбом, вышло {:?}", other.as_ref().map(describe)),
    }
    match top_of("bohemian-rhapsody", "Bohemian Rhapsody") {
        Some(MusicItem::Track(track)) => {
            assert!(track.title.contains("Bohemian Rhapsody"), "{}", track.title);
            assert!(!track.video_id.is_empty());
        }
        other => panic!("ждал трек, вышло {:?}", other.as_ref().map(describe)),
    }
}

#[test]
fn the_top_result_is_not_repeated_among_the_items() {
    for name in ["kino", "michael-jackson", "tkay-maidza", "ok-computer", "bohemian-rhapsody"] {
        let summary = parse_search_summary(&fixture(&format!("search/{name}.json")));
        let Some(top) = &summary.top else { panic!("{name}: нет карточки") };
        let rest: Vec<&MusicItem> = summary.items.iter().filter(|i| !melogold_core::search_top::same(i, Some(top))).collect();
        assert!(rest.len() < summary.items.len() || !summary.items.iter().any(|i| i.key() == top.key()), "{name}");
        assert!(rest.iter().all(|i| i.key() != top.key()), "{name}: повтор лучшего результата строкой");
    }
}

#[test]
fn without_the_card_the_artist_is_raised_by_the_name_rule() {
    // Тот же ответ, но полка карточки заменена обычной полкой с её строками.
    let mut response = fixture("search/kino.json");
    let sections = response
        .pointer_mut("/contents/tabbedSearchResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents")
        .and_then(Value::as_array_mut)
        .expect("секции выдачи");
    for section in sections.iter_mut() {
        if let Some(card) = section.get("musicCardShelfRenderer").cloned() {
            *section = serde_json::json!({ "musicShelfRenderer": { "contents": card.get("contents").cloned().unwrap_or_default() } });
        }
    }
    let mut summary = parse_search_summary(&response);
    assert!(summary.top.is_none(), "карточка осталась");
    // Исполнитель был только в самой карточке, а её строки — его песни. Как у Windows: если его нет
    // среди остальной выдачи, ставим его туда, как прислал бы YouTube без карточки.
    let artists: Vec<String> =
        summary.items.iter().filter_map(|i| if let MusicItem::Artist(a) = i { Some(a.name.clone()) } else { None }).collect();
    if !artists.iter().any(|a| melogold_core::search_top::normalize(a) == "кино") {
        let kino = melogold_core::music::ArtistItem { browse_id: "UCkino".into(), name: "КИНО".into(), ..Default::default() };
        summary.items.insert(0, MusicItem::Artist(kino));
    }
    match melogold_core::search_top::pick(&summary, "кино!") {
        Some(MusicItem::Artist(artist)) => assert_eq!(melogold_core::search_top::normalize(&artist.name), "кино"),
        other => panic!("правило по имени не подняло исполнителя: {:?}", other.as_ref().map(describe)),
    }
    // Не совпадает ни с кем — лучшего нет.
    summary.items.clear();
    assert!(melogold_core::search_top::pick(&summary, "Кино").is_none());
}

// ── страница исполнителя (задание 0019) ──

#[test]
fn an_artist_page_has_a_wide_banner_and_what_about_shows() {
    for (name, id) in [("kino", "UCkino"), ("michael-jackson", "UCmj")] {
        let artist = melogold_innertube::music::parse_artist(id, &fixture(&format!("artist/{name}.json"))).expect(name);
        println!(
            "{name}: «{}» · фото {:?} · слушатели {:?} · подписчики {:?} · просмотры {:?}",
            artist.name, artist.thumbnail_url, artist.monthly_listeners_text, artist.subscriber_count, artist.views_text
        );
        assert!(!artist.name.is_empty(), "{name}: нет имени");
        // Шапка — широкий баннер, а не квадрат: пропорции из адреса около 2,4 : 1.
        let ratio = melogold_core::thumbnails::aspect(artist.thumbnail_url.as_deref()).expect("пропорции баннера");
        assert!((2.0..2.8).contains(&ratio), "{name}: {ratio}");
        assert!(artist.monthly_listeners_text.is_some() || artist.subscriber_count.is_some(), "{name}: ни слушателей, ни подписчиков");
        assert!(!artist.shelves.is_empty(), "{name}: нет полок");
    }
}
