//! Плеер (docs/PROMPT.md §4, Windows `PlayerEngine.cs`): очередь, поток, вывод, ошибки,
//! автовоспроизведение. Живёт отдельной задачей tokio: окно и MPRIS шлют ей команды
//! ([`Command`]) и получают события ([`Event`]); позицию спрашивают напрямую у конвейера.
//!
//! Ошибки — по классам: сеть — 2 повтора (через 1 и 3 с), таймаут и бот — 1, гео, возраст,
//! недоступно — сразу пропуск с причиной; после трёх пропусков подряд воспроизведение
//! останавливается. Адреса двух следующих треков резолвятся заранее, у ближайшего сразу
//! открывается начало звука — переход по очереди не ждёт сети.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gst::prelude::*;
use melogold_core::music::Track;
use melogold_core::queue::{PlayQueue, QueueItem, QueueSnapshot, RepeatMode};
use melogold_innertube::music::YouTubeMusic;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::output::Output;
use crate::reader::RangeReader;
use crate::resolver::{Resolver, StreamError, StreamErrorKind, StreamInfo};
use crate::song_cache::SongCache;
use crate::stream::TrackStream;

/// Меньше — не прослушивание (DESIGN §3.11.1).
const MIN_PLAY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    /// Получаем поток: на кнопке — крутилка, через 3 с подпись «Получаем поток…».
    Resolving,
    Buffering,
    Playing,
    Paused,
    /// Остановлено с ошибкой: причина словами и «Повторить».
    Error,
}

/// Причина ошибки для текста (REWRITE §3.10.9).
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerError {
    pub kind: StreamErrorKind,
    pub message: String,
    pub track: Track,
    /// Трек закрыт в стране: где YouTube видит устройство (задание 0001).
    pub country: Option<String>,
    pub open_countries: Option<usize>,
}

impl PlayerError {
    fn from(error: &StreamError, track: &Track) -> Self {
        Self {
            kind: error.kind,
            message: error.message.clone(),
            track: track.clone(),
            country: error.country.clone(),
            open_countries: error.open_countries,
        }
    }
}

/// Что сейчас играет и как — снимок для окна и MPRIS.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub status: Status,
    /// Играет или собирается играть, как только будет поток.
    pub playing: bool,
    pub track: Option<Track>,
    pub item_id: Option<i64>,
    pub duration: Option<Duration>,
    pub error: Option<PlayerError>,
    pub stream: Option<StreamInfo>,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub has_previous: bool,
    pub has_next: bool,
    /// Громкость 0…1, «без звука» и скорость — для MPRIS.
    pub volume: f64,
    pub muted: bool,
    pub speed: f64,
}

/// Очередь в порядке проигрывания (панель очереди).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueueView {
    pub items: Vec<QueueItem>,
    pub current: Option<usize>,
    /// С какого места в `items` начинается блок «Далее — похожие».
    pub autoplay_from: Option<usize>,
}

#[derive(Clone, Debug)]
pub enum Event {
    State(Box<State>),
    Queue(QueueView),
    /// Трек пропущен из-за ошибки: «Пропущен „…“: причина».
    Skipped(PlayerError),
    /// Очередь заменена, в прежней было два и больше пользовательских треков: «Отменить».
    QueueReplaced(Box<QueueSnapshot>),
    /// Позиция сменилась скачком (MPRIS `Seeked`).
    Seeked(Duration),
    /// Сеанс трека закончился: ≥ 5 с реального звучания — одно прослушивание (история, срез 4).
    Listened {
        track: Track,
        played: Duration,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub volume: f64,
    pub muted: bool,
    pub speed: f64,
    pub normalize: bool,
    pub autoplay: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { volume: 0.8, muted: false, speed: 1.0, normalize: true, autoplay: true }
    }
}

#[derive(Debug)]
pub enum Command {
    /// Играть список с выбранного трека (альбом, плейлист, Избранное…).
    PlayList {
        tracks: Vec<Track>,
        start: usize,
        shuffle: bool,
    },
    /// Одиночный трек и дальше похожие (поиск, «Недавние», ссылка); `start` — позиция из `t=`.
    PlaySingle {
        track: Track,
        start: Duration,
    },
    PlayNext(Vec<Track>),
    AddToEnd(Vec<Track>),
    TogglePlay,
    Play,
    Pause,
    Next,
    Previous,
    Seek(Duration),
    /// Сдвиг от текущей позиции (Shift+← →, MPRIS `Seek`).
    SeekBy(i64),
    JumpTo(i64),
    Remove(i64),
    Move {
        id: i64,
        to: usize,
    },
    ClearQueue,
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    Settings(Settings),
    Retry,
    /// «Отменить» после замены очереди: прежняя очередь, трек и позиция.
    RestoreQueue(Box<QueueSnapshot>),
    SaveQueue,
    Shutdown,
    // ── внутренние ──
    Loaded {
        generation: u64,
        result: Box<Result<Arc<TrackStream>, StreamError>>,
    },
    Preloaded {
        video_id: String,
        stream: Arc<TrackStream>,
    },
    FeedFailed {
        generation: u64,
        error: StreamError,
    },
    Bus {
        generation: u64,
        message: gst::Message,
    },
    AutoplayLoaded {
        tracks: Vec<Track>,
        continuation: Option<String>,
        playlist_id: Option<String>,
        seed: Option<String>,
    },
    AutoplayFailed,
}

/// Общее окна и движка: позиция спрашивается у конвейера без очереди команд.
#[derive(Default)]
struct Shared {
    pipeline: Mutex<Option<gst::Pipeline>>,
    /// Позиция, пока конвейера нет: восстановленная очередь, загрузка трека.
    pending: Mutex<Duration>,
    subscribers: Mutex<Vec<async_channel::Sender<Event>>>,
    state: Mutex<State>,
}

#[derive(Clone)]
pub struct PlayerHandle {
    commands: UnboundedSender<Command>,
    shared: Arc<Shared>,
}

impl PlayerHandle {
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// Позиция текущего трека (прямо у GStreamer, дёшево); без конвейера — с какого места он начнётся.
    pub fn position(&self) -> Option<Duration> {
        match self.shared.pipeline.lock().ok()?.clone() {
            Some(pipeline) => pipeline.query_position::<gst::ClockTime>().map(|t| Duration::from_nanos(t.nseconds())),
            None => self.shared.pending.lock().ok().map(|p| *p),
        }
    }

    /// Последний снимок состояния.
    pub fn state(&self) -> State {
        self.shared.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Новый получатель событий (окно, MPRIS).
    pub fn subscribe(&self) -> async_channel::Receiver<Event> {
        let (sender, receiver) = async_channel::unbounded();
        if let Ok(mut subscribers) = self.shared.subscribers.lock() {
            subscribers.push(sender);
        }
        receiver
    }
}

pub struct Deps {
    pub resolver: Arc<Resolver>,
    pub music: YouTubeMusic,
    pub songs: Arc<SongCache>,
    pub http: reqwest::Client,
    pub settings: Settings,
    /// Куда сохранять очередь для восстановления после перезапуска.
    pub queue_path: Option<PathBuf>,
}

/// Запустить движок на рантайме tokio.
pub fn start(runtime: &tokio::runtime::Handle, deps: Deps) -> PlayerHandle {
    let (commands, receiver) = unbounded_channel();
    let shared = Arc::new(Shared::default());
    let handle = PlayerHandle { commands: commands.clone(), shared: Arc::clone(&shared) };
    let engine = Engine::new(deps, commands, shared);
    runtime.spawn(engine.run(receiver));
    handle
}

#[derive(Default)]
struct Autoplay {
    loading: bool,
    continuation: Option<String>,
    playlist_id: Option<String>,
    seed: Option<String>,
    seeds_without_new: u32,
}

struct Engine {
    deps: Deps,
    tx: UnboundedSender<Command>,
    shared: Arc<Shared>,
    queue: PlayQueue,
    settings: Settings,
    status: Status,
    play_when_ready: bool,
    output: Option<Output>,
    stream: Option<StreamInfo>,
    duration: Option<Duration>,
    generation: u64,
    error: Option<PlayerError>,
    skips_in_row: u32,
    played_session: HashSet<String>,
    preloaded: HashMap<String, Arc<TrackStream>>,
    preloading: HashSet<String>,
    autoplay: Autoplay,
    pending_start: Duration,
    /// Когда нажали «играть» — для замера «от нажатия до звука» (приёмка среза 2).
    load_started: Option<Instant>,
    listened: Duration,
    listening_since: Option<Instant>,
    listened_track: Option<Track>,
    last_saved: Instant,
}

impl Engine {
    fn new(deps: Deps, tx: UnboundedSender<Command>, shared: Arc<Shared>) -> Engine {
        let settings = deps.settings;
        Engine {
            deps,
            tx,
            shared,
            queue: PlayQueue::new(),
            settings,
            status: Status::Idle,
            play_when_ready: false,
            output: None,
            stream: None,
            duration: None,
            generation: 0,
            error: None,
            skips_in_row: 0,
            played_session: HashSet::new(),
            preloaded: HashMap::new(),
            preloading: HashSet::new(),
            autoplay: Autoplay::default(),
            pending_start: Duration::ZERO,
            load_started: None,
            listened: Duration::ZERO,
            listening_since: None,
            listened_track: None,
            last_saved: Instant::now(),
        }
    }

    async fn run(mut self, mut receiver: UnboundedReceiver<Command>) {
        self.restore_saved();
        self.emit_queue();
        self.emit_state();
        while let Some(command) = receiver.recv().await {
            if matches!(command, Command::Shutdown) {
                self.finish_listening();
                self.save_queue();
                self.release_output();
                break;
            }
            self.handle(command);
            // Позиция в сохранённой очереди — не реже раза в 10 с, пока играет.
            if self.status == Status::Playing && self.last_saved.elapsed() > Duration::from_secs(10) {
                self.save_queue();
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::PlayList { tracks, start, shuffle } => {
                if tracks.is_empty() {
                    return;
                }
                self.before_replace();
                self.queue.set_list(tracks, start, shuffle);
                self.queue_changed_then_load(Duration::ZERO);
            }
            Command::PlaySingle { track, start } => {
                self.before_replace();
                self.queue.set_single(track);
                self.queue_changed_then_load(start);
            }
            Command::PlayNext(tracks) => {
                let was_empty = self.queue.current().is_none();
                self.queue.play_next(tracks);
                self.after_queue_change();
                if was_empty {
                    self.load_current(true, Duration::ZERO);
                }
            }
            Command::AddToEnd(tracks) => {
                let was_empty = self.queue.current().is_none();
                self.queue.add_to_end(tracks);
                self.after_queue_change();
                if was_empty {
                    self.load_current(true, Duration::ZERO);
                }
            }
            Command::TogglePlay => {
                if self.is_playing() {
                    self.pause();
                } else {
                    self.play();
                }
            }
            Command::Play => self.play(),
            Command::Pause => self.pause(),
            Command::Next => self.next(true),
            Command::Previous => self.previous(),
            Command::Seek(position) => self.seek(position),
            Command::SeekBy(ms) => {
                let position = self.position().unwrap_or_default().as_millis() as i64 + ms;
                let limit = self.duration.map(|d| d.as_millis() as i64).unwrap_or(i64::MAX);
                self.seek(Duration::from_millis(position.clamp(0, limit.saturating_sub(500).max(0)) as u64));
            }
            Command::JumpTo(id) => {
                self.finish_listening();
                if self.queue.jump_to(id) {
                    self.queue_changed_then_load(Duration::ZERO);
                }
            }
            Command::Remove(id) => {
                self.queue.remove(id);
                self.after_queue_change();
            }
            Command::Move { id, to } => {
                self.queue.move_item(id, to);
                self.after_queue_change();
            }
            Command::ClearQueue => {
                self.queue.clear_except_current();
                self.after_queue_change();
            }
            Command::SetRepeat(mode) => {
                self.queue.set_repeat(mode);
                self.after_queue_change();
            }
            Command::SetShuffle(on) => {
                self.queue.shuffle(on);
                self.after_queue_change();
            }
            Command::Settings(settings) => {
                let speed_changed = (settings.speed - self.settings.speed).abs() > f64::EPSILON;
                self.settings = settings;
                self.apply_volume();
                self.emit_state();
                if speed_changed {
                    if let (Some(output), Some(position)) = (&self.output, self.position()) {
                        output.seek(position, self.rate());
                    }
                }
                if settings.autoplay {
                    self.maybe_autoplay();
                }
            }
            Command::Retry => {
                let position = self.position().unwrap_or(self.pending_start);
                self.load_current(true, position);
            }
            Command::RestoreQueue(snapshot) => {
                self.finish_listening();
                let position = Duration::from_millis(snapshot.position_ms.max(0) as u64);
                self.queue.restore(*snapshot);
                self.reset_autoplay();
                self.queue_changed_then_load(position);
            }
            Command::SaveQueue => self.save_queue(),
            Command::Shutdown => {}
            Command::Loaded { generation, result } => {
                if generation == self.generation {
                    self.on_loaded(*result);
                }
            }
            Command::Preloaded { video_id, stream } => {
                self.preloading.remove(&video_id);
                let upcoming: Vec<String> = self.upcoming_ids(2);
                if upcoming.contains(&video_id) {
                    self.preloaded.insert(video_id, stream);
                } else {
                    stream.reader().cancel();
                }
            }
            Command::FeedFailed { generation, error } => {
                if generation == self.generation {
                    if let Some(track) = self.current_track() {
                        self.skip_after_error(PlayerError::from(&error, &track));
                    }
                }
            }
            Command::Bus { generation, message } => {
                if generation == self.generation {
                    self.on_bus(message);
                }
            }
            Command::AutoplayLoaded { tracks, continuation, playlist_id, seed } => {
                self.on_autoplay(tracks, continuation, playlist_id, seed)
            }
            Command::AutoplayFailed => self.autoplay.loading = false,
        }
    }

    // ── состояние ──

    fn current_track(&self) -> Option<Track> {
        self.queue.current().map(|item| item.track.clone())
    }

    fn is_playing(&self) -> bool {
        matches!(self.status, Status::Playing | Status::Buffering) || (self.status == Status::Resolving && self.play_when_ready)
    }

    fn position(&self) -> Option<Duration> {
        self.output.as_ref().and_then(Output::position)
    }

    fn rate(&self) -> f64 {
        self.settings.speed.clamp(0.5, 2.0)
    }

    fn broadcast(&self, event: Event) {
        if let Ok(mut subscribers) = self.shared.subscribers.lock() {
            subscribers.retain(|s| s.try_send(event.clone()).is_ok() || !s.is_closed());
        }
    }

    fn emit_state(&self) {
        let state = State {
            status: self.status,
            playing: self.is_playing(),
            track: self.current_track(),
            item_id: self.queue.current().map(|i| i.id),
            duration: self.duration.or_else(|| self.current_track().and_then(|t| t.duration_ms).map(|ms| Duration::from_millis(ms as u64))),
            error: self.error.clone(),
            stream: self.stream.clone(),
            repeat: self.queue.repeat(),
            shuffle: self.queue.is_shuffled(),
            has_previous: self.queue.current().is_some(),
            has_next: self.queue.peek_next(true).is_some(),
            volume: self.settings.volume,
            muted: self.settings.muted,
            speed: self.settings.speed,
        };
        if let Ok(mut shared) = self.shared.state.lock() {
            if *shared == state {
                return;
            }
            *shared = state.clone();
        }
        self.broadcast(Event::State(Box::new(state)));
    }

    fn emit_queue(&self) {
        let order = self.queue.play_order();
        let items: Vec<QueueItem> = order.iter().map(|&i| self.queue.items()[i].clone()).collect();
        let current = self.queue.current_index().and_then(|c| order.iter().position(|&i| i == c));
        let autoplay_from = items.iter().position(|i| i.from_autoplay);
        self.broadcast(Event::Queue(QueueView { items, current, autoplay_from }));
    }

    fn set_status(&mut self, status: Status) {
        self.status = status;
        self.update_listening();
        self.emit_state();
    }

    fn after_queue_change(&mut self) {
        self.emit_queue();
        self.emit_state();
        self.prefetch_upcoming();
        self.maybe_autoplay();
        self.save_queue();
    }

    /// Очередь сменилась и сразу грузится другой трек: состояние скажет загрузка («Получаем поток»),
    /// а не «играет» новый трек со старой позицией.
    fn queue_changed_then_load(&mut self, start: Duration) {
        self.emit_queue();
        self.save_queue();
        self.load_current(true, start);
    }

    /// Замена очереди: при двух и больше пользовательских треках — «Очередь заменена · Отменить».
    fn before_replace(&mut self) {
        self.finish_listening();
        self.reset_autoplay();
        if self.queue.user_added_count() >= 2 {
            let position = self.position().unwrap_or_default().as_millis() as i64;
            self.broadcast(Event::QueueReplaced(Box::new(self.queue.snapshot(position))));
        }
    }

    // ── команды ──

    fn play(&mut self) {
        if self.current_track().is_none() {
            return;
        }
        if matches!(self.status, Status::Error | Status::Idle) || self.output.is_none() {
            let start = self.position().unwrap_or(self.pending_start);
            self.load_current(true, start);
            return;
        }
        self.play_when_ready = true;
        if let Some(output) = &self.output {
            output.play();
        }
        self.emit_state();
    }

    fn pause(&mut self) {
        self.play_when_ready = false;
        if let Some(output) = &self.output {
            output.pause();
        }
        if self.status == Status::Resolving || self.output.is_none() {
            self.emit_state();
        }
    }

    fn next(&mut self, user_action: bool) {
        self.finish_listening();
        self.skips_in_row = 0;
        if !self.queue.move_next(user_action) {
            // Конец очереди без автовоспроизведения: остановка на последнем треке в начале.
            if let Some(output) = &self.output {
                output.pause();
                output.seek(Duration::ZERO, self.rate());
            }
            self.play_when_ready = false;
            self.set_status(Status::Paused);
            return;
        }
        self.queue_changed_then_load(Duration::ZERO);
    }

    /// «Предыдущий»: с позиции больше 3 с — в начало трека (REWRITE §4.10.4).
    fn previous(&mut self) {
        if self.position().is_some_and(|p| p > Duration::from_secs(3)) || !self.queue.move_previous() {
            self.seek(Duration::ZERO);
            return;
        }
        self.finish_listening();
        self.queue_changed_then_load(Duration::ZERO);
    }

    fn seek(&mut self, position: Duration) {
        match &self.output {
            Some(output) => {
                output.seek(position, self.rate());
                self.broadcast(Event::Seeked(position));
            }
            None => self.pending_start = position,
        }
        self.emit_state();
    }

    /// Громкость с нормализацией: `loudnessDb` — насколько трек громче эталона YouTube; громкие
    /// приглушаются, тихие не усиливаются.
    fn apply_volume(&self) {
        let Some(output) = &self.output else { return };
        let mut volume = self.settings.volume;
        if self.settings.normalize {
            if let Some(loudness) = self.stream.as_ref().and_then(|s| s.loudness_db).filter(|l| *l > 0.0) {
                // Кубическая шкала громкости: множитель амплитуды — кубический корень.
                volume *= 10f64.powf(-loudness / 20.0).cbrt();
            }
        }
        output.set_volume(volume, self.settings.muted);
    }

    // ── загрузка трека ──

    fn set_pending(&self, position: Duration) {
        if let Ok(mut pending) = self.shared.pending.lock() {
            *pending = position;
        }
    }

    fn release_output(&mut self) {
        if let Ok(mut pipeline) = self.shared.pipeline.lock() {
            *pipeline = None;
        }
        if let Some(output) = self.output.take() {
            // Остановка конвейера ждёт поток кормления: не в потоке движка.
            tokio::task::spawn_blocking(move || drop(output));
        }
    }

    fn load_current(&mut self, play: bool, start: Duration) {
        self.generation += 1;
        let generation = self.generation;
        let Some(track) = self.current_track() else {
            self.release_output();
            self.set_status(Status::Idle);
            return;
        };
        let start = if start.is_zero() { std::mem::take(&mut self.pending_start) } else { start };
        self.pending_start = start;
        self.set_pending(start);
        self.load_started = play.then(Instant::now);
        self.play_when_ready = play;
        self.error = None;
        self.release_output();
        self.stream = None;
        self.duration = None;
        self.set_status(Status::Resolving);

        let preloaded = self.preloaded.remove(&track.video_id);
        let (resolver, songs, http, tx) =
            (Arc::clone(&self.deps.resolver), Arc::clone(&self.deps.songs), self.deps.http.clone(), self.tx.clone());
        tokio::spawn(async move {
            let mut attempt = 0;
            let result = loop {
                let opened = match &preloaded {
                    Some(stream) if attempt == 0 && !stream.reader().is_cancelled() => Ok(Arc::clone(stream)),
                    _ => open(&resolver, &songs, &http, &track.video_id).await,
                };
                match opened {
                    Ok(stream) => break Ok(stream),
                    Err(error) if attempt < error.retries() => {
                        attempt += 1;
                        tracing::info!(трек = %track.video_id, попытка = attempt, итог = ?error.kind, "повтор получения потока");
                        tokio::time::sleep(Duration::from_millis(if attempt == 1 { 1000 } else { 3000 })).await;
                    }
                    Err(error) => break Err(error),
                }
            };
            let _ = tx.send(Command::Loaded { generation, result: Box::new(result) });
        });
    }

    fn on_loaded(&mut self, result: Result<Arc<TrackStream>, StreamError>) {
        let Some(track) = self.current_track() else { return };
        let stream = match result {
            Ok(stream) => stream,
            Err(error) => {
                self.skip_after_error(PlayerError::from(&error, &track));
                return;
            }
        };
        let generation = self.generation;
        let failed = {
            let tx = self.tx.clone();
            move |error| {
                let _ = tx.send(Command::FeedFailed { generation, error });
            }
        };
        let start = std::mem::take(&mut self.pending_start);
        let output = match Output::new(Arc::clone(&stream), start, self.rate(), failed) {
            Ok(output) => output,
            Err(message) => {
                tracing::error!(%message, "вывод звука не собрался");
                self.skip_after_error(PlayerError::from(&StreamError::new(StreamErrorKind::Extractor, message), &track));
                return;
            }
        };
        if let Some(bus) = output.bus() {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                use futures_util::StreamExt;
                let mut messages = bus.stream();
                while let Some(message) = messages.next().await {
                    if tx.send(Command::Bus { generation, message }).is_err() {
                        break;
                    }
                }
            });
        }
        if let Ok(mut pipeline) = self.shared.pipeline.lock() {
            *pipeline = Some(output.pipeline().clone());
        }
        self.stream = Some(stream.info());
        self.duration = Some(stream.duration());
        self.output = Some(output);
        self.apply_volume();
        self.skips_in_row = 0;
        self.played_session.insert(track.video_id.clone());
        let output = self.output.as_ref().expect("только что собран");
        if self.play_when_ready {
            output.play();
            self.set_status(Status::Buffering);
        } else {
            output.pause();
            self.set_status(Status::Paused);
        }
        self.prefetch_upcoming();
        self.maybe_autoplay();
    }

    fn on_bus(&mut self, message: gst::Message) {
        match message.view() {
            gst::MessageView::StateChanged(changed) => {
                let from_pipeline =
                    message.src().is_some_and(|src| self.output.as_ref().is_some_and(|o| src == o.pipeline().upcast_ref::<gst::Object>()));
                if !from_pipeline {
                    return;
                }
                let status = match changed.current() {
                    gst::State::Playing => Status::Playing,
                    gst::State::Paused if self.play_when_ready => Status::Buffering,
                    gst::State::Paused => Status::Paused,
                    _ => return,
                };
                if status == Status::Playing {
                    self.error = None;
                    if let Some(started) = self.load_started.take() {
                        let track = self.current_track().map(|t| t.video_id).unwrap_or_default();
                        tracing::info!(трек = %track, мс = started.elapsed().as_millis() as u64, "от нажатия до звука");
                    }
                }
                self.set_status(status);
            }
            gst::MessageView::Eos(_) => {
                self.finish_listening();
                self.next(false);
            }
            gst::MessageView::Error(error) => {
                let Some(track) = self.current_track() else { return };
                tracing::warn!(трек = %track.video_id, ошибка = %error.error(), подробно = ?error.debug(), "GStreamer");
                // Поток уже повторял сам; сюда доходит то, что не лечится повтором — свежий адрес один раз.
                self.deps.resolver.invalidate(&track.video_id);
                let position = self.position().unwrap_or_default();
                if self.error.is_none() {
                    self.error = Some(PlayerError::from(&StreamError::new(StreamErrorKind::Network, error.error().to_string()), &track));
                    let play = self.play_when_ready;
                    self.load_current(play, position);
                } else {
                    self.skip_after_error(PlayerError::from(
                        &StreamError::new(StreamErrorKind::Extractor, error.error().to_string()),
                        &track,
                    ));
                }
            }
            _ => {}
        }
    }

    /// Пропуск включён всегда; после трёх пропусков подряд — остановка с причиной (REWRITE §3.10.9).
    fn skip_after_error(&mut self, error: PlayerError) {
        tracing::warn!(трек = %error.track.video_id, итог = ?error.kind, "трек пропущен: {}", error.message);
        self.error = Some(error.clone());
        self.skips_in_row += 1;
        if self.skips_in_row >= 3 || self.queue.peek_next(true).is_none() {
            self.play_when_ready = false;
            self.release_output();
            self.set_status(Status::Error);
            return;
        }
        self.broadcast(Event::Skipped(error));
        self.queue.move_next(true);
        self.queue_changed_then_load(Duration::ZERO);
    }

    // ── упреждающая загрузка и автовоспроизведение ──

    fn upcoming_ids(&self, count: usize) -> Vec<String> {
        self.queue.upcoming(count).into_iter().map(|i| self.queue.items()[i].track.video_id.clone()).collect()
    }

    /// Адреса двух следующих треков — заранее, а у ближайшего ещё и начало звука (docs/PROMPT.md §4).
    fn prefetch_upcoming(&mut self) {
        let upcoming = self.upcoming_ids(2);
        self.preloaded.retain(|video_id, stream| {
            let keep = upcoming.contains(video_id);
            if !keep {
                stream.reader().cancel();
            }
            keep
        });
        if let Some(next) = upcoming.first() {
            if !self.preloaded.contains_key(next) && self.preloading.insert(next.clone()) {
                let (resolver, songs, http, tx, video_id) =
                    (Arc::clone(&self.deps.resolver), Arc::clone(&self.deps.songs), self.deps.http.clone(), self.tx.clone(), next.clone());
                tokio::spawn(async move {
                    if let Ok(stream) = open(&resolver, &songs, &http, &video_id).await {
                        let _ = tx.send(Command::Preloaded { video_id, stream });
                    }
                });
            }
        }
        for video_id in upcoming.into_iter().skip(1) {
            if self.deps.songs.is_complete(&video_id) {
                continue;
            }
            let resolver = Arc::clone(&self.deps.resolver);
            tokio::spawn(async move {
                // Ошибка всплывёт при переходе на трек — там её и обработаем.
                let _ = resolver.resolve(&video_id).await;
            });
        }
    }

    fn reset_autoplay(&mut self) {
        self.autoplay = Autoplay::default();
    }

    /// Автовоспроизведение похожих (REWRITE §4.10.5): догружается, когда впереди ≤ 3 треков; без
    /// повторов (очередь, прослушанное за сессию); по продолжению, а без него — от последнего трека.
    fn maybe_autoplay(&mut self) {
        if self.autoplay.loading || !self.settings.autoplay || self.queue.repeat() == RepeatMode::All || self.queue.current().is_none() {
            return;
        }
        // Список, который пользователь запустил сам, играет до конца: похожие — только после него.
        if self.queue.upcoming(usize::MAX).len() > 3 || self.autoplay.seeds_without_new >= 3 {
            return;
        }
        self.autoplay.loading = true;
        let music = self.deps.music.clone();
        let tx = self.tx.clone();
        let continuation = self.autoplay.continuation.clone();
        let playlist_id = self.autoplay.playlist_id.clone();
        let seed = if continuation.is_some() {
            None
        } else {
            let items = self.queue.items();
            let from_autoplay = items.iter().rev().find(|i| i.from_autoplay && Some(&i.track.video_id) != self.autoplay.seed.as_ref());
            let last_user = items[..self.queue.autoplay_start()].last();
            from_autoplay.or(last_user).or(self.queue.current()).map(|i| i.track.video_id.clone())
        };
        tokio::spawn(async move {
            let page = match (&continuation, &seed) {
                (Some(token), _) => music.next_continuation(token, playlist_id.as_deref()).await,
                (None, Some(seed)) => music.next(seed, Some(&format!("RDAMVM{seed}"))).await,
                (None, None) => {
                    let _ = tx.send(Command::AutoplayFailed);
                    return;
                }
            };
            let _ = tx.send(match page {
                Ok(page) => {
                    Command::AutoplayLoaded { tracks: page.tracks, continuation: page.continuation, playlist_id: page.playlist_id, seed }
                }
                Err(error) => {
                    tracing::info!(%error, "похожие не загрузились");
                    Command::AutoplayFailed
                }
            });
        });
    }

    fn on_autoplay(&mut self, tracks: Vec<Track>, continuation: Option<String>, playlist_id: Option<String>, seed: Option<String>) {
        self.autoplay.loading = false;
        if seed.is_some() {
            self.autoplay.seed = seed;
        }
        let known: HashSet<&str> = self.queue.items().iter().map(|i| i.track.video_id.as_str()).collect();
        let mut seen = HashSet::new();
        let fresh: Vec<Track> = tracks
            .into_iter()
            .filter(|t| !t.unavailable && !known.contains(t.video_id.as_str()) && !self.played_session.contains(&t.video_id))
            .filter(|t| seen.insert(t.video_id.clone()))
            .take(25)
            .collect();
        self.autoplay.continuation = if continuation == self.autoplay.continuation { None } else { continuation };
        if playlist_id.is_some() {
            self.autoplay.playlist_id = playlist_id;
        }
        if fresh.is_empty() {
            self.autoplay.continuation = None;
            self.autoplay.seeds_without_new += 1;
            return;
        }
        self.autoplay.seeds_without_new = 0;
        self.queue.append_autoplay(fresh);
        self.emit_queue();
        self.emit_state();
        self.prefetch_upcoming();
        self.save_queue();
    }

    // ── учёт прослушиваний ──

    fn update_listening(&mut self) {
        if self.status == Status::Playing {
            let current = self.current_track();
            if self.listened_track.as_ref().map(|t| &t.video_id) != current.as_ref().map(|t| &t.video_id) {
                self.finish_listening();
                self.listened_track = current;
            }
            self.listening_since.get_or_insert_with(Instant::now);
        } else if let Some(since) = self.listening_since.take() {
            self.listened += since.elapsed();
        }
    }

    /// Сеанс трека закончился (переход, остановка): ≥ 5 с реального звучания — одно прослушивание.
    fn finish_listening(&mut self) {
        if let Some(since) = self.listening_since.take() {
            self.listened += since.elapsed();
        }
        let played = std::mem::take(&mut self.listened).mul_f64(self.rate());
        if let Some(track) = self.listened_track.take() {
            if played >= MIN_PLAY {
                self.broadcast(Event::Listened { track, played });
            }
        }
    }

    // ── очередь после перезапуска ──

    fn save_queue(&mut self) {
        let Some(path) = &self.deps.queue_path else { return };
        self.last_saved = Instant::now();
        let position = self.position().map(|p| p.as_millis() as i64).unwrap_or(self.pending_start.as_millis() as i64);
        let snapshot = self.queue.snapshot(position);
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            if let Ok(text) = serde_json::to_string(&snapshot) {
                let temp = path.with_extension("json.tmp");
                if std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, &path)).is_err() {
                    tracing::warn!("очередь не сохранилась");
                }
            }
        });
    }

    /// После перезапуска очередь и позиция восстанавливаются, без автостарта (docs/PROMPT.md §4).
    fn restore_saved(&mut self) {
        let Some(path) = &self.deps.queue_path else { return };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let Ok(snapshot) = serde_json::from_str::<QueueSnapshot>(&text) else {
            tracing::warn!("сохранённая очередь не читается");
            return;
        };
        self.pending_start = Duration::from_millis(snapshot.position_ms.max(0) as u64);
        self.set_pending(self.pending_start);
        self.queue.restore(snapshot);
        if self.queue.current().is_some() {
            self.status = Status::Paused;
        }
    }
}

/// Открыть трек: скачанный или целиком закэшированный играет без запросов, иначе адрес и начало звука.
async fn open(
    resolver: &Arc<Resolver>,
    songs: &Arc<SongCache>,
    http: &reqwest::Client,
    video_id: &str,
) -> Result<Arc<TrackStream>, StreamError> {
    let info = match songs.complete(video_id) {
        Some(info) => info,
        None => resolver.resolve(video_id).await?,
    };
    let entry = songs.entry(&info);
    let reader = RangeReader::new(http.clone(), Arc::clone(resolver), info, Some((Arc::clone(songs), entry)));
    let result = TrackStream::open(Arc::clone(&reader)).await;
    if result.is_err() {
        reader.cancel();
    }
    result
}
