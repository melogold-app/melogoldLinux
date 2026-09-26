//! Живая проверка движка без звука:
//! `MELOGOLD_AUDIO_SINK=fakesink cargo run -p melogold-playback --example engine`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use melogold_core::music::Track;
use melogold_innertube::music::YouTubeMusic;
use melogold_innertube::InnerTube;
use melogold_playback::engine::{self, Command, Deps, Event, Status};
use melogold_playback::resolver::Resolver;
use melogold_playback::song_cache::SongCache;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("warn,melogold=info").init();
    gst::init().unwrap();
    let client = InnerTube::new("en", "US");
    let _ = client.ensure_visitor_data().await;
    let clients = melogold_playback::stream_clients::parse(include_str!("../../../config/stream-clients.json")).unwrap();
    let deps = Deps {
        resolver: Arc::new(Resolver::new(client.clone(), clients)),
        music: YouTubeMusic::new(client),
        songs: SongCache::new(std::env::temp_dir().join("melogold-engine-example"), 256 << 20),
        http: reqwest::Client::new(),
        settings: Default::default(),
        queue_path: None,
    };
    let player = engine::start(&tokio::runtime::Handle::current(), deps);
    let events = player.subscribe();
    let track = Track { video_id: "dQw4w9WgXcQ".into(), title: "Never Gonna Give You Up".into(), ..Default::default() };
    let pressed = Instant::now();
    player.send(Command::PlaySingle { track, start: Duration::ZERO });
    let mut next_pressed: Option<Instant> = None;
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut queue_len = 0;
    while Instant::now() < deadline {
        let Ok(Ok(event)) = tokio::time::timeout(Duration::from_millis(500), events.recv()).await else { continue };
        match event {
            Event::State(state) => {
                let title = state.track.as_ref().map(|t| t.title.clone()).unwrap_or_default();
                println!("[{:>5} мс] {:?} «{}» позиция {:?}", pressed.elapsed().as_millis(), state.status, title, player.position());
                if state.status == Status::Playing {
                    match next_pressed {
                        None if queue_len > 1 => {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            println!("позиция через 2 с: {:?}; «Следующий»", player.position());
                            next_pressed = Some(Instant::now());
                            player.send(Command::Next);
                        }
                        Some(at) if title != "Never Gonna Give You Up" => {
                            println!("следующий трек зазвучал через {} мс после нажатия", at.elapsed().as_millis());
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Event::Queue(view) => {
                queue_len = view.items.len();
                println!("очередь: {} треков, похожие с {:?}", view.items.len(), view.autoplay_from);
            }
            Event::Skipped(error) => println!("пропущен: {:?}", error),
            other => println!("{other:?}"),
        }
    }
    player.send(Command::Shutdown);
    tokio::time::sleep(Duration::from_millis(500)).await;
}
