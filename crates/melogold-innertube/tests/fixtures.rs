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
