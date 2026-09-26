//! Проверка GStreamer на фрагментированном MP4 YouTube (грабли §9 п. 4): длительность и перемотка.
//! `cargo run -p melogold-playback --example probe -- <файл.m4a>`; звук — в fakesink.

use gst::prelude::*;

fn main() {
    gst::init().unwrap();
    let path = std::env::args().nth(1).expect("файл");
    let pipeline = gst::parse::launch(&format!(
        "filesrc location={path} ! qtdemux name=demux ! avdec_aac ! audioconvert ! scaletempo ! audioresample ! fakesink name=sink sync=true"
    ))
    .unwrap()
    .downcast::<gst::Pipeline>()
    .unwrap();
    let bus = pipeline.bus().unwrap();
    let started = std::time::Instant::now();
    pipeline.set_state(gst::State::Paused).unwrap();
    let _ = pipeline.state(gst::ClockTime::from_seconds(10));
    println!("PAUSED за {} мс", started.elapsed().as_millis());
    let duration = pipeline.query_duration::<gst::ClockTime>();
    println!("длительность: {duration:?}");
    let sink = pipeline.by_name("sink").unwrap();
    for target in [150u64, 30, 200] {
        let t = std::time::Instant::now();
        pipeline.seek_simple(gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE, gst::ClockTime::from_seconds(target)).unwrap();
        let _ = pipeline.state(gst::ClockTime::from_seconds(10));
        let position = pipeline.query_position::<gst::ClockTime>();
        let sample = sink.property::<Option<gst::Sample>>("last-sample");
        let pts = sample.and_then(|s| s.buffer().and_then(|b| b.pts()));
        println!("перемотка на {target} с за {} мс: позиция {position:?}, первый буфер {pts:?}", t.elapsed().as_millis());
    }
    // Скорость 1,5× без смены тона: seek с rate.
    pipeline
        .seek(
            1.5,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::ClockTime::from_seconds(10),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        )
        .unwrap();
    pipeline.set_state(gst::State::Playing).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("после 2 с на 1,5×: позиция {:?}", pipeline.query_position::<gst::ClockTime>());
    while let Some(message) = bus.pop() {
        if let gst::MessageView::Error(e) = message.view() {
            println!("ошибка: {} {:?}", e.error(), e.debug());
        }
    }
    pipeline.set_state(gst::State::Null).unwrap();
}
