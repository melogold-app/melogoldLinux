//! «Сохранить файлом» на живом потоке: `cargo run -p melogold-playback --example save_m4a -- <fmp4> <m4a>`.
//! Берёт поток, скачанный примером `fetch`, и пишет обычный .m4a с тегами (проверка — `ffprobe`).

use melogold_playback::mp4_writer::{self, Tags};

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("файл потока (пример fetch)");
    let output = args.next().expect("куда писать .m4a");
    let bytes = std::fs::read(&input).expect("поток читается");
    let tags =
        Tags { title: Some("Проверка".into()), artist: Some("Melogold".into()), album: Some("Тест".into()), cover: None };
    let m4a = mp4_writer::from_fragmented(&bytes, &tags).expect("m4a собирается");
    std::fs::write(&output, &m4a).expect("m4a пишется");
    println!("{} → {} ({} КБ)", input, output, m4a.len() / 1024);
}
