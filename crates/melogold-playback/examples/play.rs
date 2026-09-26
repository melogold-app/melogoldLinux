//! Живая проверка вывода без звука: `MELOGOLD_AUDIO_SINK=fakesink cargo run -p melogold-playback --example play -- <videoId>`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use melogold_innertube::InnerTube;
use melogold_playback::output::Output;
use melogold_playback::reader::RangeReader;
use melogold_playback::resolver::Resolver;
use melogold_playback::song_cache::SongCache;
use melogold_playback::stream::TrackStream;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("warn,melogold=info").init();
    gst::init().unwrap();
    let video_id = std::env::args().nth(1).unwrap_or_else(|| "dQw4w9WgXcQ".into());
    let clients = melogold_playback::stream_clients::parse(include_str!("../../../config/stream-clients.json")).unwrap();
    let client = InnerTube::new("en", "US");
    client.ensure_visitor_data().await.unwrap();
    let resolver = Arc::new(Resolver::new(client, clients));
    let cache = SongCache::new(std::env::temp_dir().join("melogold-play-example"), 0);
    let pressed = Instant::now();
    let info = cache.complete(&video_id).map(Ok).unwrap_or(resolver.resolve(&video_id).await).unwrap();
    let entry = cache.entry(&info);
    let reader = RangeReader::new(reqwest::Client::new(), Arc::clone(&resolver), info, Some((Arc::clone(&cache), entry)));
    let stream = TrackStream::open(reader).await.unwrap();
    println!("заголовки за {} мс, длительность {:?}", pressed.elapsed().as_millis(), stream.duration());
    let output = Output::new(stream, Duration::ZERO, 1.0, |e| eprintln!("сбой чтения: {e}")).unwrap();
    output.set_volume(0.8, false);
    output.play();
    let bus = output.bus().unwrap();
    let mut first_sound = None;
    let wait = |seconds: f64| {
        let until = Instant::now() + Duration::from_secs_f64(seconds);
        while Instant::now() < until {
            if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(50)) {
                match message.view() {
                    gst::MessageView::Error(e) => println!("ошибка: {} {:?}", e.error(), e.debug()),
                    gst::MessageView::Eos(_) => println!("конец трека"),
                    _ => {}
                }
            }
        }
    };
    while first_sound.is_none() && pressed.elapsed() < Duration::from_secs(10) {
        if output.position().is_some_and(|p| p > Duration::ZERO) {
            first_sound = Some(pressed.elapsed());
        }
        wait(0.02);
    }
    println!("от нажатия до звука: {:?}", first_sound);
    wait(2.0);
    println!("через 2 с: {:?}", output.position());
    let t = Instant::now();
    output.seek(Duration::from_secs(150), 1.0);
    wait(1.0);
    println!("перемотка на 150 с: через 1 с позиция {:?} (с перемотки {} мс)", output.position(), t.elapsed().as_millis());
    output.seek(Duration::from_secs(30), 1.5);
    wait(2.0);
    println!("перемотка на 30 с и 1,5×: через 2 с позиция {:?}", output.position());
    output.seek(Duration::from_secs(209), 1.0);
    wait(5.0);
    println!("после конца: {:?}", output.position());
    drop(output);
    println!("в кэше целиком: {}", cache.is_complete(&video_id));
}
