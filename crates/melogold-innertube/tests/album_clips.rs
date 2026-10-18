//! Клипы в списке альбома заменяются песнями из плейлиста альбома (`music::prefer_album_audio`).

use melogold_innertube::music::{parse_album, parse_playlist, prefer_album_audio};
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
