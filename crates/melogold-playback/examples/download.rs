//! Живая проверка загрузок: `cargo run -p melogold-playback --example download -- <videoId> <папка>`.
//! Скачивает трек в папку загрузок, как «Скачать», и ждёт конца; второй запуск берёт из кэша без сети.

use std::sync::Arc;
use std::time::{Duration, Instant};

use melogold_core::music::Track;
use melogold_data::{Database, Library};
use melogold_innertube::InnerTube;
use melogold_playback::downloads::{DownloadState, Downloads};
use melogold_playback::resolver::Resolver;
use melogold_playback::song_cache::SongCache;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("warn,melogold=info").init();
    let mut args = std::env::args().skip(1);
    let video_id = args.next().unwrap_or_else(|| "dQw4w9WgXcQ".into());
    let dir = std::path::PathBuf::from(args.next().expect("папка для проверки"));
    let clients = melogold_playback::stream_clients::parse(include_str!("../../../config/stream-clients.json")).unwrap();
    let resolver = Arc::new(Resolver::new(InnerTube::new("en", "US"), clients));
    let library = Library::open(Database::open(&dir.join("library.db")).unwrap());
    let songs = SongCache::new(dir.join("songs"), 512 * 1024 * 1024);
    let downloads = Downloads::new(
        SongCache::new(dir.join("downloads"), 0),
        songs,
        resolver,
        library,
        reqwest::Client::new(),
        tokio::runtime::Handle::current(),
    );
    let started = Instant::now();
    let track = Track { video_id: video_id.clone(), title: video_id.clone(), ..Default::default() };
    println!("в очередь: {}", downloads.download(&[track]));
    loop {
        let state = downloads.state(&video_id);
        println!("{:>5.1} с  {state:?}", started.elapsed().as_secs_f64());
        match state {
            Some(DownloadState::Completed) => break,
            Some(DownloadState::Failed) | None => std::process::exit(1),
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    println!("скачано: {} байт, в списке загрузок: {:?}", downloads.size(), downloads.store().is_complete(&video_id));
}
