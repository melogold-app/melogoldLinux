//! Клипы в списке альбома заменяются песнями из плейлиста альбома (`music::prefer_album_audio`), а
//! если плейлист их не дал — песнями из поиска (`music::song_for_clip`).

use melogold_core::music::{ArtistRef, MusicItem, Track};
use melogold_innertube::music::{parse_album, parse_playlist, parse_search, prefer_album_audio, song_for_clip};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/album-clips/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).expect("JSON")
}

const ALBUM: &str = "MPREb_rMqWqQe5N3L";
const PLAYLIST: &str = "VLOLAK5uy_np90iV2eD-YiScFePYgA7-i3c9kOYXMpc";

/// 18.10.2026: LEMONHEAD показывал 2:11, а играл клип на 1:25 — текст песни расходился со звуком.
#[test]
fn album_clips_become_the_album_songs() {
    let mut album = parse_album(ALBUM, &fixture("call-me-if-you-get-lost.album.en.json")).expect("альбом");
    assert_eq!(album.tracks.len(), 16);
    assert!(album.tracks.iter().all(|t| t.video_type.as_deref() == Some("video")), "в записи все строки — клипы");
    assert_eq!(album.album.playlist_id.as_deref(), PLAYLIST.strip_prefix("VL"), "плейлист альбома берётся из строк");

    let audio = parse_playlist(PLAYLIST, &fixture("call-me-if-you-get-lost.playlist.en.json")).expect("плейлист").tracks;
    assert_eq!(prefer_album_audio(&mut album.tracks, &audio), 16);

    let lemonhead = album.tracks.iter().find(|t| t.title.starts_with("LEMONHEAD")).expect("LEMONHEAD");
    assert_eq!(lemonhead.video_id, "QjckqsLmb1o", "песня, а не клип a5JCltuobqQ");
    assert_eq!(lemonhead.video_type.as_deref(), Some("song"));
    assert_eq!(lemonhead.duration_ms, Some(131_000));
    assert_eq!(lemonhead.album_id.as_deref(), Some(ALBUM), "альбом остаётся тем, который открыт");
    assert!(album.tracks.iter().all(|t| t.video_type.as_deref() == Some("song")));
}

/// Плейлист без пары для строки (другой порядок, лишний трек) — строка находится по названию,
/// а без пары остаётся клипом: лучше сыграть клип, чем чужую песню.
#[test]
fn a_shuffled_or_short_playlist_matches_by_title_only() {
    let mut album = parse_album(ALBUM, &fixture("call-me-if-you-get-lost.album.en.json")).expect("альбом");
    let mut audio = parse_playlist(PLAYLIST, &fixture("call-me-if-you-get-lost.playlist.en.json")).expect("плейлист").tracks;
    audio.reverse();
    let lemonhead_index = audio.iter().position(|t| t.title.starts_with("LEMONHEAD")).expect("в плейлисте");
    audio.remove(lemonhead_index);

    assert_eq!(prefer_album_audio(&mut album.tracks, &audio), 15);
    let lemonhead = album.tracks.iter().find(|t| t.title.starts_with("LEMONHEAD")).expect("LEMONHEAD");
    assert_eq!(lemonhead.video_id, "a5JCltuobqQ", "без пары — прежний клип");
    let corso = album.tracks.iter().find(|t| t.title == "CORSO").expect("CORSO");
    assert_eq!(corso.video_type.as_deref(), Some("song"));
}

const ESTATE_SALE: &str = "MPREb_VpXYEs5HeNs";

/// 20.10.2026: у переиздания «CALL ME IF YOU GET LOST: The Estate Sale» плейлист альбома пуст, и
/// LEMONHEAD снова играл клип на 1:26 вместо песни на 2:11. Песню находит поиск «Песни»: то же
/// название, исполнитель и длительность строки альбома.
#[test]
fn estate_sale_clips_find_their_songs_by_search() {
    let album = parse_album(ESTATE_SALE, &fixture("estate-sale.album.en.json")).expect("альбом");
    let clips: Vec<_> = album.tracks.iter().filter(|t| t.video_type.as_deref() == Some("video")).collect();
    assert_eq!(clips.len(), 16, "первые 16 строк — клипы");
    let playlist_id = album.album.playlist_id.clone().expect("плейлист альбома");
    assert!(parse_playlist(&format!("VL{playlist_id}"), &fixture("estate-sale.playlist.en.json")).is_none(), "плейлист пуст");

    for (title, file, clip_id, song_id, duration) in [
        ("LEMONHEAD (feat. 42 Dugg)", "estate-sale.songs-lemonhead.en.json", "a5JCltuobqQ", "QjckqsLmb1o", 131_000),
        ("CORSO", "estate-sale.songs-corso.en.json", "Lqvwm3zAILI", "qz-k3DoMxkw", 147_000),
    ] {
        let clip = album.tracks.iter().find(|t| t.title == title).expect(title);
        assert_eq!((clip.video_id.as_str(), clip.duration_ms), (clip_id, Some(duration)), "в строке клип, длительность песни");
        let found: Vec<Track> = songs(file);
        let song = song_for_clip(clip, &found).expect(title);
        assert_eq!((song.video_id.as_str(), song.duration_ms), (song_id, Some(duration)));
    }
}

/// Пара строгая: другая длительность, другой исполнитель или строка без длительности — клип
/// остаётся клипом.
#[test]
fn a_song_from_search_must_match_title_artist_and_duration() {
    let album = parse_album(ESTATE_SALE, &fixture("estate-sale.album.en.json")).expect("альбом");
    let lemonhead = album.tracks.iter().find(|t| t.title.starts_with("LEMONHEAD")).expect("LEMONHEAD").clone();
    let found = songs("estate-sale.songs-lemonhead.en.json");

    let clip_length = Track { duration_ms: Some(86_000), ..lemonhead.clone() };
    assert!(song_for_clip(&clip_length, &found).is_none(), "длительность клипа, а не песни");
    let no_length = Track { duration_ms: None, ..lemonhead.clone() };
    assert!(song_for_clip(&no_length, &found).is_none());
    let rounded = Track { duration_ms: Some(132_500), ..lemonhead.clone() };
    assert_eq!(song_for_clip(&rounded, &found).map(|s| s.video_id.as_str()), Some("QjckqsLmb1o"), "округление в 1,5 с");

    let mut stranger = found.clone();
    for song in &mut stranger {
        song.artists = vec![ArtistRef { id: Some("UCother".into()), name: "Other".into() }];
        song.artists_text = Some("Other".into());
    }
    assert!(song_for_clip(&lemonhead, &stranger).is_none(), "чужой исполнитель");
}

fn songs(name: &str) -> Vec<Track> {
    parse_search(&fixture(name))
        .items
        .into_iter()
        .filter_map(|item| match item {
            MusicItem::Track(track) => Some(track),
            _ => None,
        })
        .collect()
}
