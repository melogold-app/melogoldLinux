//! Вывод звука на GStreamer (docs/PROMPT.md §3 «Звук»).
//!
//! ```text
//! appsrc (кадры AAC из fMP4) → avdec_aac → audioconvert → audioresample → scaletempo → volume → вывод
//! ```
//!
//! Кадры в `appsrc` кладёт отдельный поток: берёт фрагменты у [`TrackStream`] (диск, кэш, сеть) и
//! отдаёт их с метками времени. Перемотка приходит в `appsrc` как `seek-data` — поток начинает с
//! фрагмента, в котором звучит нужное место (по `sidx`). Скорость — перемоткой с `rate`,
//! `scaletempo` держит тон. `MELOGOLD_AUDIO_SINK=fakesink` — проверки без звука.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use gst::prelude::*;
use gst_app::{AppSrc, AppStreamType};

use crate::resolver::StreamError;
use crate::stream::{FragmentData, TrackStream};

/// Что поток кормления должен делать дальше.
#[derive(Default)]
struct FeedState {
    /// Растёт при каждой перемотке: кадры старой позиции в новый сегмент не попадают.
    generation: u64,
    target: Duration,
    stopped: bool,
}

#[derive(Default)]
struct Control {
    state: Mutex<FeedState>,
    changed: Condvar,
}

impl Control {
    fn lock(&self) -> std::sync::MutexGuard<'_, FeedState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn seek(&self, target: Duration) {
        let mut state = self.lock();
        state.generation += 1;
        state.target = target;
        self.changed.notify_all();
    }

    fn stop(&self) {
        self.lock().stopped = true;
        self.changed.notify_all();
    }

    /// Ждать перемотки или остановки; `None` — остановлено.
    fn wait_change(&self, seen: u64) -> Option<(u64, Duration)> {
        let mut state = self.lock();
        while !state.stopped && state.generation == seen {
            state = self.changed.wait(state).unwrap_or_else(|p| p.into_inner());
        }
        (!state.stopped).then_some((state.generation, state.target))
    }

    fn current(&self) -> Option<u64> {
        let state = self.lock();
        (!state.stopped).then_some(state.generation)
    }
}

pub struct Output {
    pipeline: gst::Pipeline,
    volume: gst::Element,
    control: Arc<Control>,
    feeder: Option<JoinHandle<()>>,
    stream: Arc<TrackStream>,
}

impl Output {
    /// Собрать конвейер трека; `start` — с какого места, `rate` — скорость. Ошибки чтения
    /// фрагментов уходят в `failed`.
    pub fn new(
        stream: Arc<TrackStream>,
        start: Duration,
        rate: f64,
        failed: impl Fn(StreamError) + Send + 'static,
    ) -> Result<Output, String> {
        let track = &stream.index().track;
        let caps = gst::Caps::builder("audio/mpeg")
            .field("mpegversion", 4i32)
            .field("stream-format", "raw")
            .field("codec_data", gst::Buffer::from_slice(track.audio_specific_config.clone()))
            .field("rate", track.sample_rate as i32)
            .field("channels", track.channels as i32)
            .build();
        let appsrc = AppSrc::builder()
            .caps(&caps)
            .format(gst::Format::Time)
            .stream_type(AppStreamType::Seekable)
            .is_live(false)
            .block(true)
            .max_bytes(384 * 1024)
            .build();
        appsrc.set_duration(gst::ClockTime::from_nseconds(stream.duration().as_nanos() as u64));

        let make = |name: &str| gst::ElementFactory::make(name).build().map_err(|_| format!("нет элемента GStreamer {name}"));
        let decoder = ["avdec_aac", "fdkaacdec", "faad"]
            .iter()
            .find_map(|name| gst::ElementFactory::make(name).build().ok())
            .ok_or_else(|| "нет декодера AAC: avdec_aac (gst-libav), fdkaacdec или faad".to_owned())?;
        let convert = make("audioconvert")?;
        let resample = make("audioresample")?;
        let tempo = make("scaletempo")?;
        let convert_out = make("audioconvert")?;
        let volume = make("volume")?;
        let sink = audio_sink()?;

        let pipeline = gst::Pipeline::with_name("melogold");
        let elements = [appsrc.upcast_ref::<gst::Element>(), &decoder, &convert, &resample, &tempo, &convert_out, &volume, &sink];
        pipeline.add_many(elements).map_err(|e| e.to_string())?;
        gst::Element::link_many(elements).map_err(|e| e.to_string())?;

        let control = Arc::new(Control::default());
        let seek_control = Arc::clone(&control);
        appsrc.set_callbacks(
            gst_app::AppSrcCallbacks::builder()
                .seek_data(move |_, offset| {
                    seek_control.seek(Duration::from_nanos(offset));
                    true
                })
                .build(),
        );

        // Начальная позиция и скорость — перемоткой ещё до запуска: basesrc помнит её и выполнит
        // при старте, и первый же сегмент начнётся с нужного места.
        if start > Duration::ZERO || (rate - 1.0).abs() > f64::EPSILON {
            pipeline.set_state(gst::State::Ready).map_err(|e| e.to_string())?;
            let seek = gst::event::Seek::new(
                rate,
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::SeekType::Set,
                gst::ClockTime::from_nseconds(start.as_nanos() as u64),
                gst::SeekType::None,
                gst::ClockTime::NONE,
            );
            appsrc.send_event(seek);
        }

        let feeder_stream = Arc::clone(&stream);
        let feeder_control = Arc::clone(&control);
        let feeder = std::thread::Builder::new()
            .name("melogold-feeder".into())
            .spawn(move || feed(feeder_stream, appsrc, feeder_control, start, failed))
            .map_err(|e| e.to_string())?;

        Ok(Output { pipeline, volume, control, feeder: Some(feeder), stream })
    }

    pub fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    pub fn stream(&self) -> &Arc<TrackStream> {
        &self.stream
    }

    pub fn play(&self) {
        let _ = self.pipeline.set_state(gst::State::Playing);
    }

    pub fn pause(&self) {
        let _ = self.pipeline.set_state(gst::State::Paused);
    }

    pub fn position(&self) -> Option<Duration> {
        self.pipeline.query_position::<gst::ClockTime>().map(|t| Duration::from_nanos(t.nseconds()))
    }

    pub fn seek(&self, position: Duration, rate: f64) {
        let result = self.pipeline.seek(
            rate,
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            gst::ClockTime::from_nseconds(position.as_nanos() as u64),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
        if let Err(error) = result {
            tracing::warn!(%error, "перемотка не удалась");
        }
    }

    /// Громкость 0…1 (кубическая шкала, как у ползунков GNOME) и «без звука».
    pub fn set_volume(&self, cubic: f64, muted: bool) {
        let linear = gst_audio::StreamVolume::convert_volume(
            gst_audio::StreamVolumeFormat::Cubic,
            gst_audio::StreamVolumeFormat::Linear,
            cubic.clamp(0.0, 1.0),
        );
        self.volume.set_property("volume", linear.clamp(0.0, 10.0));
        self.volume.set_property("mute", muted);
    }

    pub fn bus(&self) -> Option<gst::Bus> {
        self.pipeline.bus()
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.control.stop();
        self.stream.reader().cancel();
        let _ = self.pipeline.set_state(gst::State::Null);
        if let Some(feeder) = self.feeder.take() {
            let _ = feeder.join();
        }
    }
}

fn audio_sink() -> Result<gst::Element, String> {
    if let Ok(name) = std::env::var("MELOGOLD_AUDIO_SINK") {
        let sink = gst::ElementFactory::make(&name).build().map_err(|_| format!("нет вывода {name}"))?;
        if sink.has_property("sync") {
            sink.set_property("sync", true);
        }
        return Ok(sink);
    }
    // PulseAudio-вывод работает и на PipeWire (pipewire-pulse): у потока — имя и роль «музыка».
    if let Ok(sink) = gst::ElementFactory::make("pulsesink").build() {
        sink.set_property("client-name", "Melogold");
        let properties = gst::Structure::builder("props")
            .field("media.role", "music")
            .field("application.name", "Melogold")
            .field("application.id", melogold_core::app_info::APP_ID)
            .field("application.icon_name", melogold_core::app_info::APP_ID)
            .build();
        sink.set_property("stream-properties", properties);
        return Ok(sink);
    }
    gst::ElementFactory::make("autoaudiosink").build().map_err(|_| "нет вывода звука (pulsesink, autoaudiosink)".to_owned())
}

/// Поток кормления: фрагмент за фрагментом, кадр за кадром, с учётом перемоток.
fn feed(stream: Arc<TrackStream>, appsrc: AppSrc, control: Arc<Control>, start: Duration, failed: impl Fn(StreamError)) {
    let index = stream.index().clone();
    let timescale = u64::from(index.track.timescale.max(1));
    let Some(mut generation) = control.current() else { return };
    let mut target = start;
    'position: loop {
        let mut number = index.fragment_at(target);
        let mut discont = true;
        let mut first = true;
        loop {
            if control.current() != Some(generation) {
                match control.current() {
                    Some(fresh) => {
                        generation = fresh;
                        target = control.lock().target;
                        continue 'position;
                    }
                    None => return,
                }
            }
            if number >= index.fragments.len() {
                let _ = appsrc.end_of_stream();
                match control.wait_change(generation) {
                    Some((fresh, to)) => {
                        (generation, target) = (fresh, to);
                        continue 'position;
                    }
                    None => return,
                }
            }
            stream.prefetch(number + 1);
            stream.prefetch(number + 2);
            // Первый фрагмент после старта или перемотки: сначала то, что уже есть в заголовке.
            let mut pushed = 0usize;
            if first {
                if let Some(partial) = stream.from_head(number) {
                    match push(&appsrc, &partial, 0, target, timescale, &mut discont, &control, generation) {
                        Pushed::All(count) => pushed = count,
                        Pushed::Interrupted => continue,
                        Pushed::Stopped => return,
                    }
                }
            }
            first = false;
            let data = match stream.fragment_blocking(number) {
                Ok(data) => data,
                Err(error) => {
                    if control.current().is_none() || stream.reader().is_cancelled() {
                        return;
                    }
                    failed(error);
                    match control.wait_change(generation) {
                        Some((fresh, to)) => {
                            (generation, target) = (fresh, to);
                            continue 'position;
                        }
                        None => return,
                    }
                }
            };
            match push(&appsrc, &data, pushed, target, timescale, &mut discont, &control, generation) {
                Pushed::All(_) => number += 1,
                Pushed::Interrupted => continue,
                Pushed::Stopped => return,
            }
        }
    }
}

enum Pushed {
    /// Отдано всё доступное; число — сколько кадров фрагмента уже отдано.
    All(usize),
    /// Перемотка: начать заново с новой позиции.
    Interrupted,
    Stopped,
}

#[allow(clippy::too_many_arguments)]
fn push(
    appsrc: &AppSrc,
    data: &FragmentData,
    skip: usize,
    target: Duration,
    timescale: u64,
    discont: &mut bool,
    control: &Control,
    generation: u64,
) -> Pushed {
    let ns = |ticks: u64| ticks.saturating_mul(1_000_000_000) / timescale;
    let target_ns = target.as_nanos() as u64;
    // Кадры задолго до нужного места не нужны: декодеру хватает двух для разгона.
    let first_needed = data.samples.iter().position(|s| ns(s.time_ticks + s.duration_ticks) > target_ns).unwrap_or(data.samples.len());
    let begin = skip.max(first_needed.saturating_sub(2));
    let mut count = skip;
    for (number, sample) in data.samples.iter().enumerate().skip(begin) {
        if sample.offset + sample.size > data.available {
            break;
        }
        if control.current() != Some(generation) {
            return if control.current().is_some() { Pushed::Interrupted } else { Pushed::Stopped };
        }
        let mut buffer = gst::Buffer::from_slice(data.bytes[sample.offset..sample.offset + sample.size].to_vec());
        {
            let buffer = buffer.get_mut().expect("новый буфер");
            buffer.set_pts(gst::ClockTime::from_nseconds(ns(sample.time_ticks)));
            buffer.set_duration(gst::ClockTime::from_nseconds(ns(sample.duration_ticks)));
            if *discont {
                buffer.set_flags(gst::BufferFlags::DISCONT);
                *discont = false;
            }
        }
        match appsrc.push_buffer(buffer) {
            Ok(_) => count = number + 1,
            Err(gst::FlowError::Flushing) => {
                // Идёт перемотка: дождаться её и начать с новой позиции.
                return match control.wait_change(generation) {
                    Some(_) => Pushed::Interrupted,
                    None => Pushed::Stopped,
                };
            }
            Err(_) => return Pushed::Stopped,
        }
    }
    Pushed::All(count)
}
