//! Ссылки других сервисов против настоящих страниц (задание 0010). Не запускается обычным `cargo test`:
//!
//! ```sh
//! cargo test -p melogold-innertube --test live_external -- --ignored --nocapture
//! ```

use melogold_core::music_services::parse;
use melogold_innertube::external::{ExternalResolver, Resolution};
use melogold_innertube::music::YouTubeMusic;
use melogold_innertube::InnerTube;

#[tokio::test]
#[ignore = "нужна сеть"]
async fn spotify_and_yandex_tracks_are_found_in_youtube_music() {
    let resolver = ExternalResolver::with_key(YouTubeMusic::new(InnerTube::new(&std::env::var("HL").unwrap_or("en".into()), "US")), None);
    for url in ["https://open.spotify.com/track/4PTG3Z6ehGkBFwjybzWkR8", "https://music.yandex.ru/track/609676"] {
        match resolver.resolve(&parse(url).unwrap()).await {
            Resolution::Track(track) => {
                println!("{url} → {} — {:?}", track.title, track.artists_text);
                assert!(track.title.to_lowercase().contains("never gonna give you up"), "{}", track.title);
            }
            other => panic!("{url}: {other:?}"),
        }
    }
}
