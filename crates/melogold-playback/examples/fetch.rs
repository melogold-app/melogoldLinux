//! Живая проверка потока: `cargo run -p melogold-playback --example fetch -- <videoId> <файл>`.
//! Резолвит трек клиентами из config/stream-clients.json и качает его кусками по 1 МБ.

use std::io::Write;
use std::time::Instant;

use melogold_innertube::InnerTube;
use melogold_playback::resolver::Resolver;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("warn,melogold=debug").init();
    let mut args = std::env::args().skip(1);
    let video_id = args.next().unwrap_or_else(|| "dQw4w9WgXcQ".into());
    let path = args.next().unwrap_or_else(|| format!("/tmp/{video_id}.m4a"));
    let clients = melogold_playback::stream_clients::parse(include_str!("../../../config/stream-clients.json")).unwrap();
    let resolver = Resolver::new(InnerTube::new("en", "US"), clients);
    let started = Instant::now();
    let info = match resolver.resolve(&video_id).await {
        Ok(info) => info,
        Err(error) => {
            eprintln!("не получен: {:?} {} страна {:?} открыт в {:?}", error.kind, error.message, error.country, error.open_countries);
            std::process::exit(1);
        }
    };
    println!(
        "адрес за {} мс: {} itag {} {} {:?} байт, громкость {:?}, длительность {:?}",
        started.elapsed().as_millis(),
        info.source,
        info.itag,
        info.mime_type,
        info.content_length,
        info.loudness_db,
        info.duration_ms
    );
    let http = reqwest::Client::new();
    let total = info.content_length.unwrap_or(u64::MAX);
    let mut file = std::fs::File::create(&path).unwrap();
    let mut offset = 0u64;
    let started = Instant::now();
    while offset < total {
        let end = (offset + (1 << 20) - 1).min(total - 1);
        let mut request = http.get(&info.url).header("Range", format!("bytes={offset}-{end}"));
        if let Some(ua) = &info.user_agent {
            request = request.header("User-Agent", ua);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        if !status.is_success() || bytes.is_empty() {
            eprintln!("HTTP {status} на {offset}");
            break;
        }
        file.write_all(&bytes).unwrap();
        offset += bytes.len() as u64;
    }
    let seconds = started.elapsed().as_secs_f64();
    println!("скачано {offset} байт за {seconds:.1} с ({:.0} КБ/с) → {path}", offset as f64 / 1024.0 / seconds);
}
