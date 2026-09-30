//! Разбор страниц каталога на настоящих ответах YouTube (фикстуры Android).

use melogold_core::music::MusicItem;
use melogold_innertube::music::{parse_album, parse_artist, parse_browse_shelves, parse_playlist, parse_playlist_continuation};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

#[test]
fn explore_has_trending_moods_and_releases() {
    for name in ["ytm/explore.ru.json", "ytm/explore.en.json"] {
        let shelves = parse_browse_shelves(&fixture(name)).unwrap();
        for shelf in &shelves {
            println!("{name}: {:?} — {} шт., все: {:?}", shelf.title, shelf.items.len(), shelf.more_browse_id);
        }
        assert!(shelves.iter().any(|s| s.more_browse_id.as_deref() == Some("FEmusic_new_releases_albums")), "новые релизы");
        assert!(shelves.iter().any(|s| s.items.iter().all(|i| matches!(i, MusicItem::Mood(_)))), "настроения");
        assert!(shelves.iter().any(|s| !s.items.is_empty() && s.items.iter().all(|i| matches!(i, MusicItem::Track(_)))), "треки в тренде");
    }
}

#[test]
fn moods_and_mood_page() {
    let moods = parse_browse_shelves(&fixture("ytm/moods.en.json")).unwrap();
    assert!(moods.iter().flat_map(|s| &s.items).filter(|i| matches!(i, MusicItem::Mood(m) if m.params.is_some())).count() >= 20);
    let mood = parse_browse_shelves(&fixture("ytm/mood.ru.json")).unwrap();
    assert!(mood.iter().flat_map(|s| &s.items).filter(|i| matches!(i, MusicItem::Playlist(_))).count() >= 5);
    let releases = parse_browse_shelves(&fixture("ytm/new-releases-albums.en.json")).unwrap();
    assert!(releases.iter().flat_map(|s| &s.items).filter(|i| matches!(i, MusicItem::Album(_))).count() >= 20);
}

#[test]
fn album_page() {
    let album = parse_album("MPREb_x", &fixture("ytm/album.ru.json")).unwrap();
    println!(
        "альбом «{}» · {:?} · {:?} · {:?} · {} треков · {:?}",
        album.album.title,
        album.album.artists_text,
        album.album.year,
        album.album.type_text,
        album.tracks.len(),
        album.album.playlist_id
    );
    assert!(!album.album.title.is_empty());
    assert!(album.tracks.len() >= 5);
    assert!(album.album.year.is_some());
    assert!(album.album.playlist_id.as_deref().is_some_and(|p| p.starts_with("OLAK5uy_")));
    assert!(album.tracks.iter().all(|t| t.album_id.as_deref() == Some("MPREb_x") && t.artists_text.is_some() && t.duration_ms.is_some()));
}

#[test]
fn artist_and_ugc_channel() {
    let artist = parse_artist("UCx", &fixture("ytm/artist.en.json")).unwrap();
    println!(
        "исполнитель {} · {:?} · полок {} · песни {:?}",
        artist.name,
        artist.subscribers_text,
        artist.shelves.len(),
        artist.songs_playlist_id
    );
    assert!(!artist.name.is_empty());
    assert!(artist.shelves.len() >= 3);
    assert!(artist.songs_playlist_id.is_some());
    assert!(artist.shelves[0].items.iter().all(|i| matches!(i, MusicItem::Track(_))), "первая полка — популярные песни");
    // Канал без музыкального профиля: YTM не даёт полок — нужен канал обычного YouTube.
    assert!(parse_artist("UCy", &fixture("ytm/artist-ugc-channel.ru.json")).is_none());
}

#[test]
fn playlists_and_continuations() {
    let playlist = parse_playlist("VLx", &fixture("ytm/playlist.ru.json")).unwrap();
    println!("плейлист «{}» · {:?} · {} треков", playlist.playlist.title, playlist.count_text, playlist.tracks.len());
    assert!(!playlist.tracks.is_empty());
    let editorial = parse_playlist("VLy", &fixture("ytm/playlist-editorial.en.json")).unwrap();
    assert!(!editorial.tracks.is_empty());
    let long = parse_playlist("VLPL85973FA7E35D0D96", &fixture("ytm/playlist-long.p00.ru.json")).unwrap();
    assert_eq!(long.playlist.playlist_id, "PL85973FA7E35D0D96");
    assert!(long.tracks.len() >= 90, "{}", long.tracks.len());
    assert!(long.continuation.is_some(), "длинный плейлист догружается продолжениями (грабли §9 п. 8)");
    let next = parse_playlist_continuation(&fixture("ytm/playlist-long.p01.ru.json"));
    assert!(next.items.len() >= 90);
    assert!(next.continuation.is_some());
}

#[test]
fn channel_videos_and_resolve() {
    let response = fixture("web/channel-videos.en.json");
    let page = melogold_innertube::music::parse_channel("UCy_vnPBNh9FqtyH9Qc-aiSA", &response);
    println!("канал {} · {:?} · {} видео", page.name, page.subscribers_text, page.videos.len());
    assert!(!page.name.is_empty());
    assert!(page.videos.len() >= 10);
    assert!(page.videos.iter().all(|v| v.artists_text.as_deref() == Some(page.name.as_str())));
    assert!(page.continuation.is_some());
    let resolved = fixture("web/resolve-url-handle-direct.en.json");
    assert!(resolved["endpoint"]["browseEndpoint"]["browseId"].as_str().is_some_and(|id| id.starts_with("UC")));
}

#[test]
fn artist_shelves_have_more_links_and_grid_page_parses() {
    let artist = parse_artist("UCRr1xG_2WIDs18a6cIiCxeA", &fixture("ytm/artist-daft-punk.ru.json")).unwrap();
    for s in &artist.shelves {
        println!("{:?}: {} шт., все: {:?} {:?}", s.title, s.items.len(), s.more_browse_id, s.more_params);
    }
    let singles =
        artist.shelves.iter().find(|s| s.more_browse_id.as_deref().is_some_and(|b| b.starts_with("MPAD"))).expect("у синглов есть «Все»");
    assert!(singles.more_params.is_some());
    let grid = parse_browse_shelves(&fixture("ytm/artist-singles-all.ru.json")).unwrap();
    assert_eq!(grid.len(), 1);
    assert!(grid[0].items.len() >= 20 && grid[0].items.iter().all(|i| matches!(i, MusicItem::Album(_))));
    assert!(grid[0].items.len() > singles.items.len());
}

#[test]
fn grid_continuation_pages_are_parsed() {
    let card = |id: &str| {
        serde_json::json!({"musicTwoRowItemRenderer": {"title": {"runs": [{"text": id}]}, "navigationEndpoint": {"browseEndpoint": {
            "browseId": id, "browseEndpointContextSupportedConfigs": {"browseEndpointContextMusicConfig": {"pageType": "MUSIC_PAGE_TYPE_ALBUM"}}}}}})
    };
    let old = serde_json::json!({"continuationContents": {"gridContinuation": {"items": [card("MPREb_a")],
        "continuations": [{"nextContinuationData": {"continuation": "T1"}}]}}});
    let page = parse_playlist_continuation(&old);
    assert_eq!((page.items.len(), page.continuation.as_deref()), (1, Some("T1")));
    let new = serde_json::json!({"onResponseReceivedActions": [{"appendContinuationItemsAction": {"continuationItems": [
        card("MPREb_b"), {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "T2"}}}}]}}]});
    let page = parse_playlist_continuation(&new);
    assert_eq!((page.items.len(), page.continuation.as_deref()), (1, Some("T2")));
}
