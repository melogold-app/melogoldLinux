//! Приёмка среза 2 (docs/PROMPT.md §4, §7): от нажатия до звука, 10 треков, без кэша и без звука.
//! `MELOGOLD_AUDIO_SINK=fakesink cargo run --release -p melogold-playback --example latency -- "запрос"`
//! Цель: медиана ≤ 3 с, p90 ≤ 6 с на хорошей сети.

use std::sync::Arc;
use std::time::{Duration, Instant};

use melogold_core::music::MusicItem;
use melogold_innertube::music::{MusicFilter, YouTubeMusic};
use melogold_innertube::InnerTube;
use melogold_playback::engine::{self, Command, Deps, Event, Status};
use melogold_playback::resolver::Resolver;
use melogold_playback::song_cache::SongCache;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    gst::init().unwrap();
    let query = std::env::args().nth(1).unwrap_or_else(|| "Daft Punk".into());
    let client = InnerTube::new("en", "US");
    let music = YouTubeMusic::new(client.clone());
    let tracks: Vec<_> = music
        .search(&query, MusicFilter::Songs)
        .await
        .unwrap()
        .items
        .into_iter()
        .filter_map(|i| match i {
            MusicItem::Track(t) => Some(t),
            _ => None,
        })
        .take(10)
        .collect();
    let cache_dir = std::env::temp_dir().join(format!("melogold-latency-{}", std::process::id()));
    let clients = melogold_playback::stream_clients::parse(include_str!("../../../config/stream-clients.json")).unwrap();
    let deps = Deps {
        resolver: Arc::new(Resolver::new(client, clients)),
        music,
        songs: SongCache::new(cache_dir.clone(), 0),
        http: reqwest::Client::new(),
        settings: engine::Settings { autoplay: false, ..Default::default() },
        queue_path: None,
    };
    let player = engine::start(&tokio::runtime::Handle::current(), deps);
    let events = player.subscribe();
    let mut times = Vec::new();
    for track in tracks {
        let title = track.title.clone();
        let pressed = Instant::now();
        player.send(Command::PlaySingle { track, start: Duration::ZERO });
        let result = loop {
            match tokio::time::timeout(Duration::from_secs(20), events.recv()).await {
                Ok(Ok(Event::State(state))) if state.status == Status::Playing => break Some(pressed.elapsed()),
                Ok(Ok(Event::State(state))) if state.status == Status::Error => break None,
                Ok(Ok(_)) => continue,
                _ => break None,
            }
        };
        match result {
            Some(elapsed) => {
                println!("{:>5} мс  {title}", elapsed.as_millis());
                times.push(elapsed);
            }
            None => println!("  сбой  {title}"),
        }
        player.send(Command::Pause);
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    times.sort();
    if !times.is_empty() {
        let pick = |q: f64| times[((times.len() as f64 - 1.0) * q).round() as usize];
        println!("медиана {} мс, p90 {} мс (цель: ≤ 3000 и ≤ 6000)", pick(0.5).as_millis(), pick(0.9).as_millis());
    }
    let _ = std::fs::remove_dir_all(cache_dir);
}
