//! Управление музыкой на другом устройстве через сервер (задание 0011, API §4.9 «Пульт» и §6, DESIGN §3.12,
//! образец — Android `sync/remote`). Здесь всё, что проверяется без экрана и без плеера:
//!
//! * [`Reporter`] — сообщает серверу, что играет это устройство (`PUT /playback/state`): при смене трека,
//!   паузе и воспроизведении, перемотке, смене очереди и громкости, не чаще раза в секунду;
//! * [`RemoteControl`] — это устройство как пульт другого: что там играет, команды по одной, ошибки словами;
//! * [`incoming_of`] — что просит `playback.command` от плеера этого устройства;
//! * часы сервера, позиция от `at`, «уступить место», ограничитель уведомлений.

use std::future::Future;
use std::ops::Range;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use melogold_core::iso;
use melogold_core::music::Track;
use melogold_core::text::now_ms;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::account::Account;
use crate::api::ApiError;
use crate::dto::{
    PlaybackCommandPayload, PlaybackHandoff, PlaybackHandoffInput, PlaybackPut, PlaybackPutResult, PlaybackState, PlaybackStateResponse,
    PlaybackSummary, RemoteCommand, RemoteCommandResult, RemoteDevice, RemoteDeviceList, TrackInput,
};

/// Сколько треков очереди несёт состояние и сколько из них — до текущего (DESIGN §3.12.1).
pub const QUEUE_WINDOW: usize = 200;
const BEFORE_CURRENT: usize = 50;
/// Тело `PUT /playback/state` — до 128 КиБ (API §1.9); клиент держится ниже 120.
const BODY_LIMIT: usize = 120 * 1024;
/// Уведомление «Управляет «…»» — не чаще раза в 30 с.
const NOTICE_INTERVAL: Duration = Duration::from_secs(30);
const HANDOFF_MAX_AGE_MS: i64 = 5 * 60_000;
const MAX_EXTRAPOLATION_MS: i64 = 3 * 60_000;
const LAST_SECOND_MS: i64 = 1_000;

/// Действия команд `POST /playback/commands`.
pub mod action {
    pub const PLAY: &str = "play";
    pub const PAUSE: &str = "pause";
    pub const TOGGLE: &str = "toggle";
    pub const NEXT: &str = "next";
    pub const PREVIOUS: &str = "previous";
    pub const SEEK: &str = "seek";
    pub const VOLUME: &str = "volume";
    pub const PLAY_QUEUE: &str = "play_queue";
    pub const STOP: &str = "stop";
}

/// Идентификатор видео YouTube; файлы этого устройства (`local:…`) другим не отдаются.
fn is_video_id(id: &str) -> bool {
    id.len() == 11 && !id.starts_with("local:")
}

/// Места очереди, которые несёт состояние, когда текущий трек — `index`: `max` штук от 50 до него (у длинной
/// очереди `[index − 50, index + 149]`), у короткой — все. Текущий трек всегда внутри.
pub fn queue_window(size: usize, index: usize, max: usize) -> Range<usize> {
    if size == 0 || index >= size || max == 0 {
        return 0..0;
    }
    let before = BEFORE_CURRENT.min(max / 2);
    let start = index.saturating_sub(before);
    let end = (start + max).min(size);
    start..end
}

// ── часы сервера и позиция от `at` ──

/// Часы сервера, какими их видит это устройство: `at` в состояниях — время сервера (API §1.5), поэтому
/// устройства с разными часами всё равно согласны, где в треке. Каждое `serverTime` сдвигает смещение.
#[derive(Default)]
pub struct ServerClock {
    offset_ms: AtomicI64,
}

impl ServerClock {
    /// Время сервера сейчас, мс эпохи.
    pub fn now(&self) -> i64 {
        self.now_from(now_ms())
    }

    pub fn now_from(&self, local_ms: i64) -> i64 {
        local_ms + self.offset_ms.load(Ordering::Relaxed)
    }

    /// Пришло `serverTime` (или `at` события): смещение — время сервера минус время здесь.
    pub fn update(&self, server_time: &str, local_ms: i64) {
        if let Some(server) = iso::parse(server_time) {
            self.offset_ms.store(server - local_ms, Ordering::Relaxed);
        }
    }
}

/// Где трек сейчас: `position_ms` была верна в `at_ms` (время сервера) и идёт дальше, пока `playing`. Не дальше
/// конца: последнюю секунду называет сам плеер.
pub fn extrapolate_position(position_ms: i64, at_ms: i64, playing: bool, now_server_ms: i64, duration_ms: Option<i64>) -> i64 {
    let moved = if playing { (now_server_ms - at_ms).clamp(0, MAX_EXTRAPOLATION_MS) } else { 0 };
    let position = position_ms + moved;
    match duration_ms.filter(|d| *d > 0) {
        Some(duration) => position.min((duration - LAST_SECOND_MS).max(0)),
        None => position,
    }
}

/// Уступает ли это устройство место другому, которое забрало его сессию («Слушать здесь», DESIGN §3.12.6):
/// `handoffFrom` называет это устройство и его нынешнюю сессию и моложе 5 минут.
pub fn should_give_way(from: Option<&PlaybackHandoff>, my_device_id: Option<&str>, my_session_id: &str, server_now_ms: i64) -> bool {
    let (Some(from), Some(me)) = (from, my_device_id) else { return false };
    if from.device_id != me || from.session_id != my_session_id {
        return false;
    }
    iso::parse(&from.at).is_some_and(|at| server_now_ms - at < HANDOFF_MAX_AGE_MS)
}

/// Можно ли показать «Управляет «Pixel 7 Pro»» сейчас: не чаще раза в 30 с, чтобы ползунок громкости на другом
/// устройстве не превращался в поток уведомлений.
pub struct NoticeThrottle {
    interval: Duration,
    last: Mutex<Option<Instant>>,
}

impl Default for NoticeThrottle {
    fn default() -> Self {
        NoticeThrottle::new(NOTICE_INTERVAL)
    }
}

impl NoticeThrottle {
    pub fn new(interval: Duration) -> NoticeThrottle {
        NoticeThrottle { interval, last: Mutex::new(None) }
    }

    pub fn allow(&self, now: Instant) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
        if last.is_some_and(|before| now.duration_since(before) < self.interval) {
            return false;
        }
        *last = Some(now);
        true
    }
}

// ── выполнение команд ──

/// Чего `playback.command` просит от плеера этого устройства.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    Seek(i64),
    /// 0..100.
    Volume(u8),
    PlayQueue {
        tracks: Vec<Track>,
        index: usize,
    },
}

/// Команда → просьба к плееру. Команда без того, что нужно её действию, и незнакомое действие остаются без
/// внимания: сервер уже проверил, а новая версия может знать то, чего эта не знает.
pub fn incoming_of(command: &PlaybackCommandPayload) -> Option<Incoming> {
    Some(match command.action.as_str() {
        action::PLAY => Incoming::Play,
        action::PAUSE => Incoming::Pause,
        action::TOGGLE => Incoming::Toggle,
        action::NEXT => Incoming::Next,
        action::PREVIOUS => Incoming::Previous,
        action::STOP => Incoming::Stop,
        action::SEEK => Incoming::Seek(command.position_ms?.max(0)),
        action::VOLUME => Incoming::Volume(command.volume?.clamp(0, 100) as u8),
        action::PLAY_QUEUE => {
            let queue = command.queue.as_ref().filter(|q| !q.is_empty())?;
            let index = usize::try_from(command.index?).ok().filter(|i| *i < queue.len())?;
            Incoming::PlayQueue { tracks: queue.iter().map(|t| t.to_track()).collect(), index }
        }
        _ => return None,
    })
}

// ── это устройство сообщает, что играет ──

/// Что плеер говорит о себе сейчас. `tracks` — вся очередь в порядке проигрывания.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerSnapshot {
    pub tracks: Vec<Track>,
    pub index: usize,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub playing: bool,
    /// 0..100.
    pub volume: Option<u8>,
}

/// Что сервер сделал с состоянием.
#[derive(Clone, Debug, PartialEq)]
pub enum PutOutcome {
    Applied,
    /// Чужое состояние новее (`newer_state`): делать нечего.
    Newer,
    /// Сессию этого устройства забрало другое («Слушать здесь»).
    HandedOff(Option<Box<PlaybackState>>),
    /// `409 playback_queue_required`: у сервера нет очереди этой сессии.
    QueueRequired,
    /// `413`: тело велико.
    TooLarge,
    /// Нет сети, сервер занят: попробовать позже.
    Unreachable,
}

/// Что ответил `PUT /playback/state`, как это понимает [`Reporter`].
pub fn put_outcome(result: Result<PlaybackPutResult, ApiError>) -> PutOutcome {
    match result {
        Ok(result) if result.applied => PutOutcome::Applied,
        Ok(result) if result.reason.as_deref() == Some("handed_off") => PutOutcome::HandedOff(result.state.map(Box::new)),
        Ok(_) => PutOutcome::Newer,
        Err(error) if error.code == "playback_queue_required" => PutOutcome::QueueRequired,
        Err(error) if error.status == 413 => PutOutcome::TooLarge,
        Err(error) if error.is_transient() => PutOutcome::Unreachable,
        // Отказ, который повтор не вылечит (старый протокол, конец сессии): делать нечего.
        Err(_) => PutOutcome::Newer,
    }
}

/// Что докладчику нужно снаружи: плеер, сервер, часы.
pub trait ReporterPort: Send + Sync + 'static {
    /// Плеер сейчас; `None` — сказать нечего (очереди нет).
    fn snapshot(&self) -> impl Future<Output = Option<PlayerSnapshot>> + Send;
    fn send(&self, put: PlaybackPut) -> impl Future<Output = PutOutcome> + Send;
    /// Время сервера сейчас (`at`).
    fn server_now_ms(&self) -> i64;
}

/// Порт на аккаунте: часы сервера учатся у каждого ответа.
pub struct AccountReporterPort<S> {
    pub account: Arc<Account>,
    pub clock: Arc<ServerClock>,
    pub snapshot: S,
}

impl<S> ReporterPort for AccountReporterPort<S>
where
    S: Fn() -> Option<PlayerSnapshot> + Send + Sync + 'static,
{
    async fn snapshot(&self) -> Option<PlayerSnapshot> {
        (self.snapshot)()
    }

    async fn send(&self, put: PlaybackPut) -> PutOutcome {
        let result = self.account.put_playback(put).await;
        if let Ok(ok) = &result {
            self.clock.update(&ok.server_time, now_ms());
        }
        put_outcome(result)
    }

    fn server_now_ms(&self) -> i64 {
        self.clock.now()
    }
}

/// Интервалы докладчика: пауза между запросами, сердцебиение, повтор при отсутствии сети.
#[derive(Clone, Copy, Debug)]
pub struct ReporterTiming {
    pub min_interval: Duration,
    pub heartbeat: Duration,
    pub retry: Duration,
}

impl Default for ReporterTiming {
    fn default() -> Self {
        ReporterTiming { min_interval: Duration::from_secs(1), heartbeat: Duration::from_secs(60), retry: Duration::from_secs(15) }
    }
}

/// Устаревшее состояние без сети не отправляется: старше 10 минут о нём говорить не стоит.
const STALE: Duration = Duration::from_secs(10 * 60);
const MAX_ATTEMPTS: u32 = 3;

/// Что у сервера уже есть от очереди: чьей сессии, какой версии и какие места очереди.
struct Sent {
    session_id: String,
    version: i64,
    kept: Vec<usize>,
}

struct ReporterState {
    session_id: String,
    queue_version: i64,
    last_queue_ids: Option<Vec<String>>,
    sent: Option<Sent>,
    last_sent_at: Option<Instant>,
    has_played: bool,
    last_playing: bool,
    pending_handoff: Option<PlaybackHandoffInput>,
    window: usize,
    paused: bool,
}

struct ReporterInner<P> {
    port: P,
    handle: Handle,
    timing: ReporterTiming,
    new_session_id: Box<dyn Fn() -> String + Send + Sync>,
    on_handed_off: HandoffListener,
    state: Mutex<ReporterState>,
    signal: Notify,
    jobs: Mutex<Vec<JoinHandle<()>>>,
}

/// Сообщает серверу, что играет это устройство (API §4.9, DESIGN §3.12, Android `PlaybackReporter`):
///
/// * при [`changed`](Self::changed) (трек, пауза, перемотка, очередь, громкость) и раз в минуту, пока играет, —
///   не чаще [`ReporterTiming::min_interval`]; частые изменения — один запрос с последним состоянием;
/// * только после того, как в этой сессии реально звучал звук (DESIGN §3.12.2), и не пока
///   [`set_paused`](Self::set_paused): это устройство — пульт другого, и его состояние затёрло бы чужое;
/// * очередь идёт, когда она (или сессия) новее того, что принял сервер; `queueVersion` растёт с каждой её
///   сменой; окно [`QUEUE_WINDOW`] треков вокруг текущего, файлы этого устройства не берутся;
/// * позиция между запросами не шлётся: другие считают её от `at`, `positionMs` и `playing`.
pub struct Reporter<P: ReporterPort>(Arc<ReporterInner<P>>);

impl<P: ReporterPort> Clone for Reporter<P> {
    fn clone(&self) -> Self {
        Reporter(Arc::clone(&self.0))
    }
}

impl<P: ReporterPort> Reporter<P> {
    pub fn new(port: P, handle: Handle, on_handed_off: impl Fn(Option<&PlaybackState>) + Send + Sync + 'static) -> Reporter<P> {
        Reporter::with_options(port, handle, ReporterTiming::default(), Box::new(melogold_core::ids::new_uuid), Box::new(on_handed_off))
    }

    pub fn with_options(
        port: P,
        handle: Handle,
        timing: ReporterTiming,
        new_session_id: Box<dyn Fn() -> String + Send + Sync>,
        on_handed_off: HandoffListener,
    ) -> Reporter<P> {
        let session_id = new_session_id();
        let reporter = Reporter(Arc::new(ReporterInner {
            port,
            handle: handle.clone(),
            timing,
            new_session_id,
            on_handed_off,
            state: Mutex::new(ReporterState {
                session_id,
                queue_version: 0,
                last_queue_ids: None,
                sent: None,
                last_sent_at: None,
                has_played: false,
                last_playing: false,
                pending_handoff: None,
                window: QUEUE_WINDOW,
                paused: false,
            }),
            signal: Notify::new(),
            jobs: Mutex::default(),
        }));
        let this = reporter.clone();
        let sender = handle.spawn(async move {
            loop {
                this.0.signal.notified().await;
                let wait = {
                    let state = this.state();
                    state.last_sent_at.map(|at| (at + this.0.timing.min_interval).saturating_duration_since(Instant::now()))
                };
                if let Some(wait) = wait.filter(|w| !w.is_zero()) {
                    tokio::time::sleep(wait).await;
                }
                this.flush(0).await;
            }
        });
        let this = reporter.clone();
        let heartbeat = handle.spawn(async move {
            loop {
                tokio::time::sleep(this.0.timing.heartbeat).await;
                let due = {
                    let state = this.state();
                    state.last_playing && !state.paused
                };
                if due {
                    this.changed();
                }
            }
        });
        reporter.0.jobs.lock().unwrap_or_else(|p| p.into_inner()).extend([sender, heartbeat]);
        reporter
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ReporterState> {
        self.0.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Номер сессии этого устройства: по нему другие узнают, что состояние — его.
    pub fn session_id(&self) -> String {
        self.state().session_id.clone()
    }

    /// Что-то изменилось: скоро уйдёт состояние.
    pub fn changed(&self) {
        self.0.signal.notify_one();
    }

    /// Звук зазвучал: с этой минуты сессия стоит рассказа.
    pub fn sound_played(&self) {
        let first = {
            let mut state = self.state();
            !std::mem::replace(&mut state.has_played, true)
        };
        if first {
            self.changed();
        }
    }

    /// Пока `true`, ничего не шлётся: это устройство управляет другим.
    pub fn set_paused(&self, paused: bool) {
        let resumed = {
            let mut state = self.state();
            std::mem::replace(&mut state.paused, paused) && !paused
        };
        if resumed {
            self.changed();
        }
    }

    /// «Слушать здесь» на этом устройстве: следующее состояние несёт `from` и очередь, в своей сессии.
    pub fn take_over(&self, from: PlaybackHandoffInput) {
        {
            let mut state = self.state();
            state.pending_handoff = Some(from);
            self.renew_session(&mut state);
            state.has_played = true;
        }
        self.changed();
    }

    /// Другая сессия: новый номер, очередь пойдёт снова.
    pub fn new_session(&self) {
        let mut state = self.state();
        self.renew_session(&mut state);
    }

    fn renew_session(&self, state: &mut ReporterState) {
        state.session_id = (self.0.new_session_id)();
        state.sent = None;
    }

    /// Остановить фоновые задачи (выход).
    pub fn stop(&self) {
        for job in self.0.jobs.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
            job.abort();
        }
    }

    async fn flush(&self, attempt: u32) {
        {
            let state = self.state();
            if state.paused || !state.has_played {
                return;
            }
        }
        let Some(snapshot) = self.0.port.snapshot().await else { return };
        let built = {
            let mut state = self.state();
            state.last_playing = snapshot.playing;
            // Новая очередь — новая версия.
            let ids: Vec<String> = snapshot.tracks.iter().map(|t| t.video_id.clone()).collect();
            if state.last_queue_ids.as_ref() != Some(&ids) {
                if state.last_queue_ids.is_some() {
                    state.queue_version += 1;
                }
                state.last_queue_ids = Some(ids);
            }
            let Some(built) = build(&mut state, &snapshot, self.0.port.server_now_ms()) else { return };
            state.last_sent_at = Some(Instant::now());
            built
        };
        let Built { put, kept } = built;
        match self.0.port.send(put.clone()).await {
            PutOutcome::Applied => {
                let mut state = self.state();
                if let Some(kept) = kept {
                    state.sent = Some(Sent { session_id: put.session_id.clone(), version: put.queue_version, kept });
                }
                if put.handoff_from.is_some() {
                    state.pending_handoff = None;
                }
                state.window = QUEUE_WINDOW;
            }
            PutOutcome::Newer => {}
            PutOutcome::HandedOff(current) => {
                {
                    let mut state = self.state();
                    self.renew_session(&mut state);
                    state.pending_handoff = None;
                }
                (self.0.on_handed_off)(current.as_deref());
            }
            PutOutcome::QueueRequired if attempt < MAX_ATTEMPTS => {
                self.state().sent = None;
                Box::pin(self.flush(attempt + 1)).await;
            }
            PutOutcome::TooLarge if attempt < MAX_ATTEMPTS => {
                {
                    let mut state = self.state();
                    if state.window <= 1 {
                        return;
                    }
                    state.window = (state.window / 2).max(1);
                    state.sent = None;
                }
                Box::pin(self.flush(attempt + 1)).await;
            }
            PutOutcome::Unreachable => {
                // Последнее состояние помнится и уходит, когда сеть вернётся, если оно моложе 10 минут.
                let (this, failed_at) = (self.clone(), Instant::now());
                let retry = self.0.timing.retry;
                let job = self.0.handle.spawn(async move {
                    tokio::time::sleep(retry).await;
                    if failed_at.elapsed() <= STALE + retry {
                        this.changed();
                    }
                });
                let mut jobs = self.0.jobs.lock().unwrap_or_else(|p| p.into_inner());
                jobs.retain(|j| !j.is_finished());
                jobs.push(job);
            }
            PutOutcome::QueueRequired | PutOutcome::TooLarge => {}
        }
    }
}

struct Built {
    put: PlaybackPut,
    /// Места очереди, что ушли в состоянии (если очередь шла).
    kept: Option<Vec<usize>>,
}

/// Запрос для `snapshot`; `None` — сказать нечего (текущий трек — не видео YouTube). Очередь идёт, когда у
/// сервера нет очереди этой сессии в этой версии или в ней нет этого трека.
fn build(state: &mut ReporterState, snapshot: &PlayerSnapshot, server_now_ms: i64) -> Option<Built> {
    let tracks = &snapshot.tracks;
    if snapshot.index >= tracks.len() || !is_video_id(&tracks[snapshot.index].video_id) {
        return None;
    }
    let have = state
        .sent
        .as_ref()
        .filter(|s| s.session_id == state.session_id && s.version == state.queue_version && s.kept.contains(&snapshot.index));
    let with_queue = have.is_none() || state.pending_handoff.is_some();
    let mut kept = have.map(|s| s.kept.clone());
    let mut queue: Option<Vec<TrackInput>> = None;
    let mut size = state.window;
    if with_queue {
        let places = kept_places(tracks, snapshot.index, size);
        queue = Some(places.iter().map(|i| TrackInput::from_track(&tracks[*i])).collect());
        kept = Some(places);
    }
    let mut kept_places_now = kept?;
    let mut put = put_of(state, snapshot, &kept_places_now, queue.clone(), server_now_ms);
    // Окно вокруг текущего сжимается, пока тело не влезет.
    while queue.is_some() && body_size(&put) > BODY_LIMIT && size > 1 {
        size = (size / 2).max(1);
        kept_places_now = kept_places(tracks, snapshot.index, size);
        queue = Some(kept_places_now.iter().map(|i| TrackInput::from_track(&tracks[*i])).collect());
        put = put_of(state, snapshot, &kept_places_now, queue.clone(), server_now_ms);
    }
    Some(Built { put, kept: queue.is_some().then_some(kept_places_now) })
}

fn kept_places(tracks: &[Track], index: usize, size: usize) -> Vec<usize> {
    queue_window(tracks.len(), index, size).filter(|i| is_video_id(&tracks[*i].video_id)).collect()
}

fn body_size(put: &PlaybackPut) -> usize {
    serde_json::to_vec(put).map(|v| v.len()).unwrap_or(0)
}

fn put_of(
    state: &ReporterState,
    snapshot: &PlayerSnapshot,
    kept: &[usize],
    queue: Option<Vec<TrackInput>>,
    server_now_ms: i64,
) -> PlaybackPut {
    PlaybackPut {
        session_id: state.session_id.clone(),
        queue_version: state.queue_version,
        at: iso::format(server_now_ms),
        index: kept.iter().position(|i| *i == snapshot.index).unwrap_or(0) as i64,
        position_ms: snapshot.position_ms.max(0),
        duration_ms: snapshot.duration_ms.filter(|d| *d > 0),
        playing: snapshot.playing,
        queue,
        handoff_from: state.pending_handoff.clone(),
        volume: snapshot.volume.map(|v| i64::from(v.min(100))),
    }
}

// ── это устройство — пульт другого ──

/// Устройство, которым управляет это.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTarget {
    pub device_id: String,
    pub name: String,
    pub platform: String,
}

/// Что играет управляемое устройство, как говорят последние события и чтение. `position_ms` была верна в
/// `at_ms` (время сервера); [`position_at`](Self::position_at) считает дальше, пока `playing`.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteNow {
    pub device_id: String,
    pub track: Option<Track>,
    pub index: i64,
    pub queue_length: i64,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub playing: bool,
    pub at_ms: i64,
    pub volume: Option<i64>,
    pub rev: i64,
}

/// Длительность играющего: из состояния, а если устройство её не прислало — из самого трека
/// (`durationMs`, иначе текст «3:45», «1:02:03»). Телефон присылает состояние без длительности, и
/// пульт показывал «0:08 / 0:00» с ползунком в конце (задание 0022, Windows `Remote.DurationOf`).
pub fn duration_of(state_duration_ms: Option<i64>, track: Option<&Track>) -> Option<i64> {
    state_duration_ms
        .filter(|d| *d > 0)
        .or_else(|| track.and_then(|t| t.duration_ms).filter(|d| *d > 0))
        .or_else(|| track.and_then(|t| melogold_core::text::parse_duration(t.duration_text.as_deref())))
        .filter(|d| *d > 0)
}

impl RemoteNow {
    pub fn position_at(&self, now_server_ms: i64) -> i64 {
        extrapolate_position(self.position_ms, self.at_ms, self.playing, now_server_ms, self.duration_ms)
    }

    pub fn from_summary(summary: &PlaybackSummary, fallback_at_ms: i64) -> RemoteNow {
        let track = summary.track.as_ref().map(|t| t.to_track());
        RemoteNow {
            device_id: summary.device_id.clone(),
            duration_ms: duration_of(summary.duration_ms, track.as_ref()),
            track,
            index: summary.index,
            queue_length: summary.queue_length,
            position_ms: summary.position_ms,
            playing: summary.playing,
            at_ms: iso::parse(&summary.at).unwrap_or(fallback_at_ms),
            volume: summary.volume,
            rev: summary.rev,
        }
    }

    pub fn from_state(state: &PlaybackState, fallback_at_ms: i64) -> RemoteNow {
        let track = usize::try_from(state.index).ok().and_then(|i| state.queue.get(i)).map(|t| t.to_track());
        RemoteNow {
            device_id: state.device_id.clone(),
            duration_ms: duration_of(state.duration_ms, track.as_ref()),
            track,
            index: state.index,
            queue_length: state.queue.len() as i64,
            position_ms: state.position_ms,
            playing: state.playing,
            at_ms: iso::parse(&state.at).unwrap_or(fallback_at_ms),
            volume: state.volume,
            rev: state.rev,
        }
    }
}

/// Что человеку скажут, если команда не дошла до устройства.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteNotice {
    /// `409 device_offline`: «„MacBook Air“ не в сети»; пульт выключается.
    Offline(String),
    /// `409 remote_control_disabled`: «На „MacBook Air“ управление выключено»; пульт выключается.
    Disabled(String),
    /// Устройства уже нет на аккаунте (`404`) или что-то ещё пошло не так.
    Failed,
}

/// Пульт целиком, для окна: кем управляем и что там играет.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemoteView {
    pub target: Option<RemoteTarget>,
    pub now: Option<RemoteNow>,
}

/// Что пульту нужно от сервера.
pub trait RemotePort: Send + Sync + 'static {
    fn devices(&self) -> impl Future<Output = Result<RemoteDeviceList, ApiError>> + Send;
    fn playback_state(&self) -> impl Future<Output = Result<PlaybackStateResponse, ApiError>> + Send;
    fn send(&self, command: RemoteCommand) -> impl Future<Output = Result<RemoteCommandResult, ApiError>> + Send;
}

pub struct AccountRemotePort {
    pub account: Arc<Account>,
    pub clock: Arc<ServerClock>,
}

impl RemotePort for AccountRemotePort {
    async fn devices(&self) -> Result<RemoteDeviceList, ApiError> {
        let list = self.account.remote_devices().await?;
        self.clock.update(&list.server_time, now_ms());
        Ok(list)
    }

    async fn playback_state(&self) -> Result<PlaybackStateResponse, ApiError> {
        let state = self.account.playback_state().await?;
        self.clock.update(&state.server_time, now_ms());
        Ok(state)
    }

    async fn send(&self, command: RemoteCommand) -> Result<RemoteCommandResult, ApiError> {
        self.account.send_command(command).await
    }
}

type HandoffListener = Box<dyn Fn(Option<&PlaybackState>) + Send + Sync>;
type ViewListener = Box<dyn Fn(&RemoteView) + Send + Sync>;
type NoticeListener = Box<dyn Fn(&RemoteNotice) + Send + Sync>;

struct ControlInner<P> {
    port: P,
    clock: Arc<ServerClock>,
    new_command_id: Box<dyn Fn() -> String + Send + Sync>,
    view: Mutex<RemoteView>,
    listeners: Mutex<Vec<ViewListener>>,
    notice_listeners: Mutex<Vec<NoticeListener>>,
    commands: mpsc::UnboundedSender<RemoteCommand>,
    handle: Handle,
}

/// Это устройство как пульт другого устройства аккаунта (API §4.9 «Пульт», Android `RemoteControl`): кем
/// управляем, что там играет (`playback.updated` и `GET /playback/state`), команды по одной в том порядке,
/// в каком человек их дал. Пока устройство выбрано, свой плеер состояние не шлёт.
pub struct RemoteControl<P: RemotePort>(Arc<ControlInner<P>>);

impl<P: RemotePort> Clone for RemoteControl<P> {
    fn clone(&self) -> Self {
        RemoteControl(Arc::clone(&self.0))
    }
}

impl<P: RemotePort> RemoteControl<P> {
    pub fn new(port: P, clock: Arc<ServerClock>, handle: Handle) -> RemoteControl<P> {
        RemoteControl::with_ids(port, clock, handle, Box::new(melogold_core::ids::new_uuid))
    }

    pub fn with_ids(
        port: P,
        clock: Arc<ServerClock>,
        handle: Handle,
        new_command_id: Box<dyn Fn() -> String + Send + Sync>,
    ) -> RemoteControl<P> {
        let (commands, mut queue) = mpsc::unbounded_channel::<RemoteCommand>();
        let control = RemoteControl(Arc::new(ControlInner {
            port,
            clock,
            new_command_id,
            view: Mutex::default(),
            listeners: Mutex::default(),
            notice_listeners: Mutex::default(),
            commands,
            handle: handle.clone(),
        }));
        // По одной, в порядке нажатий.
        let this = control.clone();
        handle.spawn(async move {
            while let Some(command) = queue.recv().await {
                this.deliver(command).await;
            }
        });
        control
    }

    pub fn view(&self) -> RemoteView {
        self.0.view.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn active(&self) -> bool {
        self.view().target.is_some()
    }

    pub fn subscribe(&self, listener: impl Fn(&RemoteView) + Send + Sync + 'static) {
        self.0.listeners.lock().unwrap_or_else(|p| p.into_inner()).push(Box::new(listener));
    }

    pub fn subscribe_notices(&self, listener: impl Fn(&RemoteNotice) + Send + Sync + 'static) {
        self.0.notice_listeners.lock().unwrap_or_else(|p| p.into_inner()).push(Box::new(listener));
    }

    pub fn clock(&self) -> &ServerClock {
        &self.0.clock
    }

    fn set_view(&self, change: impl FnOnce(&mut RemoteView) -> bool) {
        let snapshot = {
            let mut view = self.0.view.lock().unwrap_or_else(|p| p.into_inner());
            if !change(&mut view) {
                return;
            }
            view.clone()
        };
        for listener in self.0.listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            listener(&snapshot);
        }
    }

    fn notice(&self, notice: RemoteNotice) {
        for listener in self.0.notice_listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            listener(&notice);
        }
    }

    /// Другие устройства аккаунта, на которых можно включить музыку: в сети ли, что играют.
    ///
    /// Часы сюда не входят (задание 0020, доктрина §4.8): звук на часах играет только
    /// приложение, открытое на самих часах, и включать на них музыку отсюда бессмысленно.
    /// Фильтр — здесь, где список приходит от сервера, а не в окне: так его не обойдёт ни
    /// один показ. Фильтр истории по устройствам часы показывает по-прежнему — он не отсюда.
    pub async fn devices(&self) -> Result<Vec<RemoteDevice>, ApiError> {
        let devices = self.0.port.devices().await?.devices;
        Ok(devices.into_iter().filter(|device| can_play(&device.platform)).collect())
    }

    /// Плеер становится пультом устройства `device`.
    pub fn connect(&self, device: &RemoteDevice) {
        let target = RemoteTarget { device_id: device.device_id.clone(), name: device.name.clone(), platform: device.platform.clone() };
        let now =
            device.playing.as_ref().filter(|p| p.device_id == device.device_id).map(|p| RemoteNow::from_summary(p, self.0.clock.now()));
        self.set_view(|view| {
            *view = RemoteView { target: Some(target), now };
            true
        });
        let this = self.clone();
        self.0.handle.spawn(async move { this.refresh().await });
    }

    /// Плеер снова управляет этим устройством; выбранное играет дальше.
    pub fn disconnect(&self) {
        self.set_view(|view| {
            let changed = view.target.is_some() || view.now.is_some();
            *view = RemoteView::default();
            changed
        });
    }

    /// Прочитать состояние заново (после переподключения потока событий, выбора устройства).
    pub async fn refresh(&self) {
        let Some(target) = self.view().target else { return };
        let Ok(response) = self.0.port.playback_state().await else { return };
        let Some(state) = response.state.filter(|s| s.device_id == target.device_id) else { return };
        let now = RemoteNow::from_state(&state, self.0.clock.now());
        self.set_view(|view| {
            if view.target.as_ref() != Some(&target) {
                return false;
            }
            view.now = Some(now);
            true
        });
    }

    /// `playback.updated`: чужое состояние или «очищено». Событие старше уже известного — прошлое.
    pub fn on_updated(&self, cleared: bool, state: Option<&PlaybackSummary>) {
        self.set_view(|view| {
            let Some(target) = &view.target else { return false };
            if cleared {
                view.now = None;
                return true;
            }
            let Some(state) = state.filter(|s| s.device_id == target.device_id) else { return false };
            if view.now.as_ref().is_some_and(|known| state.rev <= known.rev) {
                return false;
            }
            view.now = Some(RemoteNow::from_summary(state, self.0.clock.now()));
            true
        });
    }

    // ── команды ──

    pub fn play(&self) {
        self.command(action::PLAY, None, None, |now| now.playing = true);
    }

    pub fn pause(&self) {
        self.command(action::PAUSE, None, None, |now| now.playing = false);
    }

    pub fn toggle(&self) {
        self.command(action::TOGGLE, None, None, |now| now.playing = !now.playing);
    }

    pub fn next(&self) {
        self.command(action::NEXT, None, None, |_| {});
    }

    pub fn previous(&self) {
        self.command(action::PREVIOUS, None, None, |_| {});
    }

    pub fn stop(&self) {
        self.command(action::STOP, None, None, |now| now.playing = false);
    }

    pub fn seek_to(&self, position_ms: i64) {
        let at = self.0.clock.now();
        self.command(action::SEEK, Some(position_ms.max(0)), None, move |now| {
            now.position_ms = position_ms.max(0);
            now.at_ms = at;
        });
    }

    /// Громкость устройства, 0..100.
    pub fn set_volume(&self, percent: i64) {
        let percent = percent.clamp(0, 100);
        self.command(action::VOLUME, None, Some(percent), move |now| now.volume = Some(percent));
    }

    /// Нажатие по треку в списке, пока пульт включён: очередь — окно из 200 треков вокруг него, файлы этого
    /// устройства не берутся. `false` — устройство не выбрано и плеер играет здесь; `true` — команда принята.
    pub fn play_queue(&self, tracks: &[Track], index: usize) -> bool {
        let Some(target) = self.view().target else { return false };
        if index >= tracks.len() {
            return true;
        }
        let kept = kept_places(tracks, index, QUEUE_WINDOW);
        let Some(at) = kept.iter().position(|i| *i == index) else {
            self.notice(RemoteNotice::Failed);
            return true;
        };
        self.enqueue(RemoteCommand {
            command_id: (self.0.new_command_id)(),
            target_device_id: target.device_id,
            action: action::PLAY_QUEUE.into(),
            queue: Some(kept.iter().map(|i| TrackInput::from_track(&tracks[*i])).collect()),
            index: Some(at as i64),
            ..Default::default()
        });
        true
    }

    fn command(&self, action: &str, position_ms: Option<i64>, volume: Option<i64>, optimistic: impl FnOnce(&mut RemoteNow)) {
        let Some(target) = self.view().target else { return };
        // Что видит человек, не ждёт устройство: его поправит то, что устройство сообщит.
        let server_now = self.0.clock.now();
        self.set_view(|view| {
            let Some(now) = view.now.as_mut() else { return false };
            now.position_ms = now.position_at(server_now);
            now.at_ms = server_now;
            optimistic(now);
            true
        });
        self.enqueue(RemoteCommand {
            command_id: (self.0.new_command_id)(),
            target_device_id: target.device_id,
            action: action.into(),
            position_ms,
            volume,
            ..Default::default()
        });
    }

    fn enqueue(&self, command: RemoteCommand) {
        let _ = self.0.commands.send(command);
    }

    async fn deliver(&self, command: RemoteCommand) {
        let Some(target) = self.view().target.filter(|t| t.device_id == command.target_device_id) else { return };
        if let Err(error) = self.0.port.send(command).await {
            match error.code.as_str() {
                "device_offline" => {
                    self.notice(RemoteNotice::Offline(target.name));
                    self.disconnect();
                }
                "remote_control_disabled" => {
                    self.notice(RemoteNotice::Disabled(target.name));
                    self.disconnect();
                }
                "device_not_found" => {
                    self.notice(RemoteNotice::Failed);
                    self.disconnect();
                }
                _ => self.notice(RemoteNotice::Failed),
            }
        }
    }

    /// «Слушать здесь»: состояние управляемого устройства целиком (с очередью), чтобы играть его здесь.
    /// `None` — забирать нечего или сервер не ответил (тогда пульт остаётся и человеку говорят «не получилось»).
    pub async fn state_to_take(&self) -> Option<PlaybackState> {
        let target = self.view().target?;
        match self.0.port.playback_state().await {
            Ok(response) => response.state.filter(|s| s.device_id == target.device_id && !s.queue.is_empty()),
            Err(_) => {
                self.notice(RemoteNotice::Failed);
                None
            }
        }
    }
}

/// Может ли устройство с платформой `platform` играть музыку по команде пульта: всё, кроме часов
/// (`watchos`, без учёта регистра — как `melogold_core::devices::kind`).
pub fn can_play(platform: &str) -> bool {
    melogold_core::devices::kind(Some(platform)) != melogold_core::devices::DeviceKind::Watch
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::dto::TrackDto;

    fn id(i: usize) -> String {
        format!("{i:011}")
    }

    fn track(i: usize) -> Track {
        Track { video_id: id(i), title: format!("Трек {i}"), artists_text: Some("Кино".into()), ..Default::default() }
    }

    fn snapshot(size: usize, index: usize, playing: bool) -> PlayerSnapshot {
        PlayerSnapshot {
            tracks: (0..size).map(track).collect(),
            index,
            position_ms: 83_000,
            duration_ms: Some(213_000),
            playing,
            volume: Some(70),
        }
    }

    // ── окно очереди, позиция, часы ──

    #[test]
    fn queue_window_is_fifty_before_and_the_rest_after() {
        assert_eq!(queue_window(10, 3, 200), 0..10, "короткая очередь — вся");
        assert_eq!(queue_window(1000, 500, 200), 450..650);
        assert_eq!(queue_window(1000, 10, 200), 0..200, "у начала окно сдвигается");
        assert_eq!(queue_window(1000, 990, 200), 940..1000, "у конца — только то, что есть");
        assert_eq!(queue_window(1000, 500, 2), 499..501);
        assert_eq!(queue_window(1000, 500, 1), 500..501);
        assert_eq!(queue_window(0, 0, 200), 0..0);
        assert_eq!(queue_window(5, 9, 200), 0..0);
        assert!(queue_window(1000, 999, 200).contains(&999), "текущий трек всегда внутри");
    }

    #[test]
    fn remote_position_counts_on_from_at() {
        // Играет: позиция идёт от `at`.
        assert_eq!(extrapolate_position(83_000, 1_000_000, true, 1_004_500, Some(213_000)), 87_500);
        // Пауза: стоит там, где была.
        assert_eq!(extrapolate_position(83_000, 1_000_000, false, 1_004_500, Some(213_000)), 83_000);
        // Время сервера чуть позади: назад не идёт.
        assert_eq!(extrapolate_position(83_000, 1_000_000, true, 999_000, Some(213_000)), 83_000);
        // Не дальше конца: последняя секунда — за плеером.
        assert_eq!(extrapolate_position(212_000, 1_000_000, true, 1_030_000, Some(213_000)), 212_000);
        // Без длительности — как есть; больше трёх минут не додумывается.
        assert_eq!(extrapolate_position(0, 0, true, 10_000_000, None), 180_000);
    }

    #[test]
    fn remote_now_uses_at_of_the_state_not_the_local_clock() {
        let summary = PlaybackSummary {
            device_id: "d".into(),
            position_ms: 10_000,
            duration_ms: Some(200_000),
            playing: true,
            at: "2026-09-30T10:00:00.000Z".into(),
            ..Default::default()
        };
        let now = RemoteNow::from_summary(&summary, 0);
        let at = iso::parse("2026-09-30T10:00:00.000Z").unwrap();
        assert_eq!(now.at_ms, at);
        assert_eq!(now.position_at(at + 2_500), 12_500);
    }

    #[test]
    fn server_clock_follows_server_time() {
        let clock = ServerClock::default();
        assert_eq!(clock.now_from(1_000), 1_000, "пока ничего не пришло — смещения нет");
        let local = iso::parse("2026-09-30T10:00:00.000Z").unwrap();
        clock.update("2026-09-30T10:00:05.000Z", local);
        assert_eq!(clock.now_from(local + 100), local + 5_100, "часы сервера впереди на 5 с");
        clock.update("не время", local);
        assert_eq!(clock.now_from(local), local + 5_000, "непонятное время смещение не трогает");
    }

    #[test]
    fn give_way_only_to_a_fresh_takeover_of_this_session() {
        let at = "2026-09-30T10:00:00.000Z";
        let server_now = iso::parse(at).unwrap() + 60_000;
        let from = |device: &str, session: &str| PlaybackHandoff { device_id: device.into(), session_id: session.into(), at: at.into() };
        assert!(should_give_way(Some(&from("me", "s1")), Some("me"), "s1", server_now));
        assert!(!should_give_way(Some(&from("other", "s1")), Some("me"), "s1", server_now), "чужое устройство");
        assert!(!should_give_way(Some(&from("me", "s0")), Some("me"), "s1", server_now), "чужая сессия");
        assert!(!should_give_way(Some(&from("me", "s1")), Some("me"), "s1", server_now + 5 * 60_000), "старше 5 минут");
        assert!(!should_give_way(None, Some("me"), "s1", server_now));
        assert!(!should_give_way(Some(&from("me", "s1")), None, "s1", server_now));
    }

    #[tokio::test(start_paused = true)]
    async fn notice_at_most_once_in_thirty_seconds() {
        let throttle = NoticeThrottle::default();
        let start = Instant::now();
        assert!(throttle.allow(start));
        assert!(!throttle.allow(start + Duration::from_secs(29)));
        assert!(throttle.allow(start + Duration::from_secs(30)));
        assert!(!throttle.allow(start + Duration::from_secs(45)));
    }

    // ── выполнение каждой команды ──

    fn payload(action: &str) -> PlaybackCommandPayload {
        PlaybackCommandPayload { command_id: "c".into(), from_device_id: "f".into(), action: action.into(), ..Default::default() }
    }

    #[test]
    fn every_command_becomes_a_request_to_the_player() {
        assert_eq!(incoming_of(&payload("play")), Some(Incoming::Play));
        assert_eq!(incoming_of(&payload("pause")), Some(Incoming::Pause));
        assert_eq!(incoming_of(&payload("toggle")), Some(Incoming::Toggle));
        assert_eq!(incoming_of(&payload("next")), Some(Incoming::Next));
        assert_eq!(incoming_of(&payload("previous")), Some(Incoming::Previous));
        assert_eq!(incoming_of(&payload("stop")), Some(Incoming::Stop));
        assert_eq!(incoming_of(&PlaybackCommandPayload { position_ms: Some(83_000), ..payload("seek") }), Some(Incoming::Seek(83_000)));
        assert_eq!(incoming_of(&PlaybackCommandPayload { volume: Some(40), ..payload("volume") }), Some(Incoming::Volume(40)));
        let dto = |i: usize| TrackDto { video_id: id(i), title: format!("Трек {i}"), ..Default::default() };
        let queue = PlaybackCommandPayload { queue: Some(vec![dto(1), dto(2), dto(3)]), index: Some(1), ..payload("play_queue") };
        match incoming_of(&queue) {
            Some(Incoming::PlayQueue { tracks, index }) => {
                assert_eq!((tracks.len(), index), (3, 1));
                assert_eq!(tracks[1].title, "Трек 2");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_command_without_what_it_needs_is_left_alone() {
        assert_eq!(incoming_of(&payload("seek")), None);
        assert_eq!(incoming_of(&payload("volume")), None);
        assert_eq!(incoming_of(&payload("play_queue")), None);
        let dto = TrackDto { video_id: id(1), ..Default::default() };
        assert_eq!(
            incoming_of(&PlaybackCommandPayload { queue: Some(vec![dto.clone()]), index: Some(1), ..payload("play_queue") }),
            None,
            "индекс вне очереди"
        );
        assert_eq!(incoming_of(&PlaybackCommandPayload { queue: Some(vec![dto.clone()]), index: Some(-1), ..payload("play_queue") }), None);
        assert_eq!(incoming_of(&PlaybackCommandPayload { queue: Some(vec![]), index: Some(0), ..payload("play_queue") }), None);
        assert_eq!(incoming_of(&payload("teleport")), None, "незнакомое действие");
    }

    #[test]
    fn seek_and_volume_are_kept_in_range() {
        assert_eq!(incoming_of(&PlaybackCommandPayload { position_ms: Some(-5), ..payload("seek") }), Some(Incoming::Seek(0)));
        assert_eq!(incoming_of(&PlaybackCommandPayload { volume: Some(250), ..payload("volume") }), Some(Incoming::Volume(100)));
        assert_eq!(incoming_of(&PlaybackCommandPayload { volume: Some(-3), ..payload("volume") }), Some(Incoming::Volume(0)));
    }

    #[test]
    fn put_outcomes() {
        let ok = |applied: bool, reason: Option<&str>| {
            Ok(PlaybackPutResult { applied, reason: reason.map(str::to_owned), ..Default::default() })
        };
        assert_eq!(put_outcome(ok(true, None)), PutOutcome::Applied);
        assert_eq!(put_outcome(ok(false, Some("newer_state"))), PutOutcome::Newer);
        assert!(matches!(put_outcome(ok(false, Some("handed_off"))), PutOutcome::HandedOff(None)));
        assert_eq!(put_outcome(Err(ApiError::new(409, "playback_queue_required", ""))), PutOutcome::QueueRequired);
        assert_eq!(put_outcome(Err(ApiError::new(413, "payload_too_large", ""))), PutOutcome::TooLarge);
        assert_eq!(put_outcome(Err(ApiError::new(0, "network", ""))), PutOutcome::Unreachable);
        assert_eq!(put_outcome(Err(ApiError::new(503, "server_busy", ""))), PutOutcome::Unreachable);
        assert_eq!(put_outcome(Err(ApiError::new(429, "rate_limited", ""))), PutOutcome::Unreachable);
        assert_eq!(put_outcome(Err(ApiError::new(400, "invalid_request", ""))), PutOutcome::Newer);
    }

    // ── докладчик ──

    #[derive(Default)]
    struct Fake {
        snapshot: Mutex<Option<PlayerSnapshot>>,
        outcomes: Mutex<VecDeque<PutOutcome>>,
        puts: Mutex<Vec<(Instant, PlaybackPut)>>,
    }

    struct FakePort(Arc<Fake>);

    impl ReporterPort for FakePort {
        async fn snapshot(&self) -> Option<PlayerSnapshot> {
            self.0.snapshot.lock().unwrap().clone()
        }

        async fn send(&self, put: PlaybackPut) -> PutOutcome {
            self.0.puts.lock().unwrap().push((Instant::now(), put));
            self.0.outcomes.lock().unwrap().pop_front().unwrap_or(PutOutcome::Applied)
        }

        fn server_now_ms(&self) -> i64 {
            iso::parse("2026-09-30T10:00:00.000Z").unwrap()
        }
    }

    fn reporter(fake: &Arc<Fake>) -> Reporter<FakePort> {
        let counter = Arc::new(Mutex::new(0));
        Reporter::with_options(
            FakePort(Arc::clone(fake)),
            Handle::current(),
            ReporterTiming::default(),
            Box::new(move || {
                let mut n = counter.lock().unwrap();
                *n += 1;
                format!("session-{n}")
            }),
            Box::new(|_| {}),
        )
    }

    async fn settle() {
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
    }

    async fn advance(ms: u64) {
        tokio::time::advance(Duration::from_millis(ms)).await;
        settle().await;
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_told_before_a_sound_played() {
        let fake = Arc::new(Fake::default());
        *fake.snapshot.lock().unwrap() = Some(snapshot(3, 0, true));
        let reporter = reporter(&fake);
        reporter.changed();
        settle().await;
        assert!(fake.puts.lock().unwrap().is_empty());
        reporter.sound_played();
        settle().await;
        assert_eq!(fake.puts.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn puts_at_most_once_a_second_and_a_burst_is_one_request_with_the_last_state() {
        let fake = Arc::new(Fake::default());
        *fake.snapshot.lock().unwrap() = Some(snapshot(3, 0, true));
        let reporter = reporter(&fake);
        reporter.sound_played();
        settle().await;
        assert_eq!(fake.puts.lock().unwrap().len(), 1);
        // Десять изменений подряд за долю секунды: ушло ещё ровно одно состояние, и не раньше секунды.
        for i in 1..=10 {
            *fake.snapshot.lock().unwrap() = Some(PlayerSnapshot { volume: Some(70 + i), ..snapshot(3, 0, true) });
            reporter.changed();
            advance(50).await;
        }
        assert_eq!(fake.puts.lock().unwrap().len(), 1, "секунда ещё не прошла");
        advance(600).await;
        let puts = fake.puts.lock().unwrap().clone();
        assert_eq!(puts.len(), 2);
        assert!(puts[1].0.duration_since(puts[0].0) >= Duration::from_secs(1));
        assert_eq!(puts[1].1.volume, Some(80), "последнее состояние из всех");
        // Дальше — тоже не чаще.
        for _ in 0..5 {
            reporter.changed();
            advance(200).await;
        }
        let puts = fake.puts.lock().unwrap().clone();
        for pair in puts.windows(2) {
            assert!(pair[1].0.duration_since(pair[0].0) >= Duration::from_secs(1), "интервал меньше секунды");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn queue_goes_with_the_first_state_and_again_only_when_it_changes() {
        let fake = Arc::new(Fake::default());
        *fake.snapshot.lock().unwrap() = Some(snapshot(3, 0, true));
        let reporter = reporter(&fake);
        reporter.sound_played();
        settle().await;
        // Пауза, перемотка, громкость — без очереди и с той же версией.
        *fake.snapshot.lock().unwrap() = Some(PlayerSnapshot { playing: false, position_ms: 90_000, ..snapshot(3, 0, false) });
        reporter.changed();
        advance(1_100).await;
        // Очередь сменилась: новая версия и очередь снова.
        *fake.snapshot.lock().unwrap() = Some(snapshot(5, 1, true));
        reporter.changed();
        advance(1_100).await;
        let puts: Vec<PlaybackPut> = fake.puts.lock().unwrap().iter().map(|(_, p)| p.clone()).collect();
        assert_eq!(puts.len(), 3);
        assert_eq!(puts[0].queue.as_ref().map(Vec::len), Some(3));
        assert_eq!((puts[0].queue_version, puts[0].index), (0, 0));
        assert_eq!(puts[1].queue, None, "очередь не менялась: сервер её уже принял");
        assert_eq!((puts[1].queue_version, puts[1].playing), (0, false));
        assert_eq!(puts[2].queue.as_ref().map(Vec::len), Some(5));
        assert_eq!((puts[2].queue_version, puts[2].index), (1, 1));
        assert_eq!(puts[0].session_id, puts[2].session_id);
        assert_eq!(puts[0].at, "2026-09-30T10:00:00.000Z");
    }

    #[tokio::test(start_paused = true)]
    async fn queue_is_a_window_of_two_hundred_around_the_current_track() {
        let fake = Arc::new(Fake::default());
        *fake.snapshot.lock().unwrap() = Some(snapshot(1000, 500, true));
        let reporter = reporter(&fake);
        reporter.sound_played();
        settle().await;
        let put = fake.puts.lock().unwrap()[0].1.clone();
        let queue = put.queue.unwrap();
        assert_eq!(queue.len(), 200);
        assert_eq!(queue[0].video_id, id(450));
        assert_eq!(put.index, 50, "индекс — внутри окна");
        assert_eq!(queue[put.index as usize].video_id, id(500));
    }

    #[tokio::test(start_paused = true)]
    async fn queue_required_sends_it_again_and_local_files_are_left_out() {
        let fake = Arc::new(Fake::default());
        let mut shot = snapshot(3, 1, true);
        shot.tracks[0].video_id = "local:/music/a.mp3".into();
        *fake.snapshot.lock().unwrap() = Some(shot);
        fake.outcomes.lock().unwrap().push_back(PutOutcome::Applied);
        let reporter = reporter(&fake);
        reporter.sound_played();
        settle().await;
        let first = fake.puts.lock().unwrap()[0].1.clone();
        assert_eq!(first.queue.as_ref().map(Vec::len), Some(2), "файл этого устройства в очередь не идёт");
        assert_eq!(first.index, 0);
        // Сервер потерял очередь: следующее состояние снова с ней.
        fake.outcomes.lock().unwrap().push_back(PutOutcome::QueueRequired);
        reporter.changed();
        advance(1_100).await;
        let puts = fake.puts.lock().unwrap().clone();
        assert!(puts.len() >= 3);
        assert!(puts.last().unwrap().1.queue.is_some(), "после queue_required очередь идёт снова");
    }

    #[tokio::test(start_paused = true)]
    async fn a_local_file_alone_says_nothing() {
        let fake = Arc::new(Fake::default());
        let mut shot = snapshot(1, 0, true);
        shot.tracks[0].video_id = "local:/music/a.mp3".into();
        *fake.snapshot.lock().unwrap() = Some(shot);
        let reporter = reporter(&fake);
        reporter.sound_played();
        settle().await;
        assert!(fake.puts.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn paused_reporter_is_silent_while_this_device_is_a_remote() {
        let fake = Arc::new(Fake::default());
        *fake.snapshot.lock().unwrap() = Some(snapshot(3, 0, true));
        let reporter = reporter(&fake);
        reporter.sound_played();
        settle().await;
        reporter.set_paused(true);
        reporter.changed();
        advance(2_000).await;
        assert_eq!(fake.puts.lock().unwrap().len(), 1);
        reporter.set_paused(false);
        advance(2_000).await;
        assert_eq!(fake.puts.lock().unwrap().len(), 2, "после пульта состояние уходит сразу");
    }

    #[tokio::test(start_paused = true)]
    async fn takeover_carries_the_handoff_and_the_queue_in_a_new_session() {
        let fake = Arc::new(Fake::default());
        *fake.snapshot.lock().unwrap() = Some(snapshot(3, 2, true));
        let reporter = reporter(&fake);
        reporter.sound_played();
        settle().await;
        let before = reporter.session_id();
        reporter.take_over(PlaybackHandoffInput { device_id: "other".into(), session_id: "s-other".into() });
        advance(1_100).await;
        let puts = fake.puts.lock().unwrap().clone();
        let last = &puts.last().unwrap().1;
        assert_eq!(last.handoff_from.as_ref().map(|h| h.device_id.as_str()), Some("other"));
        assert!(last.queue.is_some());
        assert_ne!(last.session_id, before);
        // После принятого состояния handoff больше не шлётся.
        reporter.changed();
        advance(1_100).await;
        assert!(fake.puts.lock().unwrap().last().unwrap().1.handoff_from.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn handed_off_starts_a_new_session_and_tells_the_owner() {
        let fake = Arc::new(Fake::default());
        *fake.snapshot.lock().unwrap() = Some(snapshot(3, 0, true));
        let told = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&told);
        let reporter = Reporter::with_options(
            FakePort(Arc::clone(&fake)),
            Handle::current(),
            ReporterTiming::default(),
            Box::new(melogold_core::ids::new_uuid),
            Box::new(move |_| *counter.lock().unwrap() += 1),
        );
        let before = reporter.session_id();
        fake.outcomes.lock().unwrap().push_back(PutOutcome::HandedOff(None));
        reporter.sound_played();
        settle().await;
        assert_eq!(*told.lock().unwrap(), 1);
        assert_ne!(reporter.session_id(), before);
    }

    #[tokio::test(start_paused = true)]
    async fn without_network_the_last_state_goes_again_in_fifteen_seconds() {
        let fake = Arc::new(Fake::default());
        *fake.snapshot.lock().unwrap() = Some(snapshot(3, 0, true));
        fake.outcomes.lock().unwrap().push_back(PutOutcome::Unreachable);
        let reporter = reporter(&fake);
        reporter.sound_played();
        settle().await;
        assert_eq!(fake.puts.lock().unwrap().len(), 1);
        advance(14_000).await;
        assert_eq!(fake.puts.lock().unwrap().len(), 1);
        advance(2_000).await;
        assert_eq!(fake.puts.lock().unwrap().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_every_minute_while_playing() {
        let fake = Arc::new(Fake::default());
        *fake.snapshot.lock().unwrap() = Some(snapshot(3, 0, true));
        let reporter = reporter(&fake);
        reporter.sound_played();
        settle().await;
        advance(61_000).await;
        assert_eq!(fake.puts.lock().unwrap().len(), 2, "раз в минуту, пока играет");
        *fake.snapshot.lock().unwrap() = Some(snapshot(3, 0, false));
        reporter.changed();
        advance(1_100).await;
        let count = fake.puts.lock().unwrap().len();
        advance(120_000).await;
        assert_eq!(fake.puts.lock().unwrap().len(), count, "на паузе сердцебиения нет");
    }

    // ── пульт ──

    #[derive(Default)]
    struct RemoteFake {
        devices: Mutex<Vec<RemoteDevice>>,
        state: Mutex<Option<PlaybackState>>,
        sent: Mutex<Vec<RemoteCommand>>,
        errors: Mutex<VecDeque<ApiError>>,
    }

    struct RemoteFakePort(Arc<RemoteFake>);

    impl RemotePort for RemoteFakePort {
        async fn devices(&self) -> Result<RemoteDeviceList, ApiError> {
            Ok(RemoteDeviceList { devices: self.0.devices.lock().unwrap().clone(), ..Default::default() })
        }

        async fn playback_state(&self) -> Result<PlaybackStateResponse, ApiError> {
            Ok(PlaybackStateResponse { state: self.0.state.lock().unwrap().clone(), server_time: String::new() })
        }

        async fn send(&self, command: RemoteCommand) -> Result<RemoteCommandResult, ApiError> {
            if let Some(error) = self.0.errors.lock().unwrap().pop_front() {
                return Err(error);
            }
            self.0.sent.lock().unwrap().push(command);
            Ok(RemoteCommandResult { delivered: true })
        }
    }

    fn device(playing: Option<PlaybackSummary>) -> RemoteDevice {
        RemoteDevice {
            device_id: "mac".into(),
            name: "MacBook Air".into(),
            platform: "macos".into(),
            online: true,
            controllable: true,
            playing,
            volume: Some(40),
        }
    }

    fn summary(rev: i64, playing: bool, position_ms: i64) -> PlaybackSummary {
        PlaybackSummary {
            rev,
            device_id: "mac".into(),
            queue_length: 12,
            track: Some(TrackDto {
                video_id: id(1),
                title: "Группа крови".into(),
                artists_text: Some("Кино".into()),
                ..Default::default()
            }),
            position_ms,
            duration_ms: Some(285_000),
            playing,
            at: "2026-09-30T10:00:00.000Z".into(),
            volume: Some(40),
            ..Default::default()
        }
    }

    fn control(fake: &Arc<RemoteFake>) -> RemoteControl<RemoteFakePort> {
        let counter = Arc::new(Mutex::new(0));
        RemoteControl::with_ids(
            RemoteFakePort(Arc::clone(fake)),
            Arc::new(ServerClock::default()),
            Handle::current(),
            Box::new(move || {
                let mut n = counter.lock().unwrap();
                *n += 1;
                format!("cmd-{n}")
            }),
        )
    }

    #[test]
    fn a_state_without_duration_takes_it_from_the_track() {
        let track = |ms: Option<i64>, text: Option<&str>| Track {
            video_id: "v".into(),
            duration_ms: ms,
            duration_text: text.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(duration_of(Some(200_000), Some(&track(Some(1), None))), Some(200_000), "своя длительность состояния — первой");
        assert_eq!(duration_of(None, Some(&track(Some(225_000), None))), Some(225_000));
        assert_eq!(duration_of(Some(0), Some(&track(None, Some("3:45")))), Some(225_000));
        assert_eq!(duration_of(None, Some(&track(None, Some("1:02:03")))), Some(3_723_000));
        assert_eq!(duration_of(None, Some(&track(None, Some("live")))), None);
        assert_eq!(duration_of(None, None), None);

        // Телефон прислал состояние без длительности, трек — «3:45»: позиция не уходит за конец.
        let summary = PlaybackSummary {
            device_id: "phone".into(),
            track: Some(crate::dto::TrackDto { video_id: "v".into(), duration_text: Some("3:45".into()), ..Default::default() }),
            position_ms: 8_000,
            duration_ms: None,
            playing: true,
            at: "2026-10-01T10:00:00.000Z".into(),
            ..Default::default()
        };
        let at = iso::parse(&summary.at).unwrap();
        let now = RemoteNow::from_summary(&summary, at);
        assert_eq!(now.duration_ms, Some(225_000));
        assert_eq!(now.position_at(at), 8_000);
        assert!(now.position_at(at + 3_600_000) < 225_000, "позиция не дальше длительности трека");
    }

    #[tokio::test]
    async fn a_watch_is_not_a_place_to_send_music() {
        let fake = Arc::new(RemoteFake::default());
        *fake.devices.lock().unwrap() = vec![
            RemoteDevice { device_id: "mac".into(), name: "MacBook Air".into(), platform: "macos".into(), ..Default::default() },
            RemoteDevice { device_id: "watch".into(), name: "Apple Watch".into(), platform: "watchOS".into(), ..Default::default() },
        ];
        let names: Vec<String> = control(&fake).devices().await.unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["MacBook Air"]);
        assert!(can_play("android") && can_play("linux") && can_play("") && !can_play("watchos"));
    }

    #[tokio::test(start_paused = true)]
    async fn connect_shows_what_the_device_plays_and_events_move_it() {
        let fake = Arc::new(RemoteFake::default());
        let control = control(&fake);
        let views = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&views);
        control.subscribe(move |v| sink.lock().unwrap().push(v.clone()));
        control.connect(&device(Some(summary(10, true, 5_000))));
        settle().await;
        let view = control.view();
        assert_eq!(view.target.as_ref().map(|t| t.name.as_str()), Some("MacBook Air"));
        assert_eq!(view.now.as_ref().and_then(|n| n.track.as_ref()).map(|t| t.title.as_str()), Some("Группа крови"));
        // Событие новее — принимается, старее — прошлое, чужого устройства — не наше.
        control.on_updated(false, Some(&summary(11, false, 9_000)));
        assert_eq!(control.view().now.unwrap().position_ms, 9_000);
        control.on_updated(false, Some(&summary(9, true, 1_000)));
        assert_eq!(control.view().now.unwrap().rev, 11, "событие старее известного — прошлое");
        control.on_updated(false, Some(&PlaybackSummary { device_id: "other".into(), ..summary(50, true, 1) }));
        assert_eq!(control.view().now.unwrap().rev, 11);
        control.on_updated(true, None);
        assert!(control.view().now.is_none(), "очищено");
        control.disconnect();
        assert_eq!(control.view(), RemoteView::default());
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_reads_the_state_of_the_controlled_device_only() {
        let fake = Arc::new(RemoteFake::default());
        let control = control(&fake);
        *fake.state.lock().unwrap() = Some(PlaybackState {
            rev: 5,
            device_id: "mac".into(),
            index: 1,
            position_ms: 42_000,
            playing: true,
            at: "2026-09-30T10:00:00.000Z".into(),
            queue: vec![
                TrackDto { video_id: id(1), title: "Первый".into(), ..Default::default() },
                TrackDto { video_id: id(2), title: "Второй".into(), ..Default::default() },
            ],
            ..Default::default()
        });
        control.connect(&device(None));
        settle().await;
        let now = control.view().now.expect("состояние прочитано");
        assert_eq!(now.track.map(|t| t.title), Some("Второй".into()));
        assert_eq!(now.position_ms, 42_000);
        // Состояние другого устройства не наше.
        control.disconnect();
        fake.state.lock().unwrap().as_mut().unwrap().device_id = "pixel".into();
        control.connect(&device(None));
        settle().await;
        assert!(control.view().now.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn commands_go_one_after_another_with_their_arguments() {
        let fake = Arc::new(RemoteFake::default());
        let control = control(&fake);
        control.connect(&device(Some(summary(1, true, 5_000))));
        settle().await;
        control.pause();
        control.next();
        control.previous();
        control.seek_to(90_000);
        control.set_volume(35);
        control.toggle();
        control.play();
        control.stop();
        settle().await;
        let sent = fake.sent.lock().unwrap().clone();
        let actions: Vec<&str> = sent.iter().map(|c| c.action.as_str()).collect();
        assert_eq!(actions, ["pause", "next", "previous", "seek", "volume", "toggle", "play", "stop"], "порядок нажатий");
        assert_eq!(sent[3].position_ms, Some(90_000));
        assert_eq!(sent[4].volume, Some(35));
        assert!(sent.iter().all(|c| c.target_device_id == "mac"));
        let ids: std::collections::HashSet<&str> = sent.iter().map(|c| c.command_id.as_str()).collect();
        assert_eq!(ids.len(), sent.len(), "у каждой команды свой commandId");
    }

    #[tokio::test(start_paused = true)]
    async fn the_screen_does_not_wait_for_the_device() {
        let fake = Arc::new(RemoteFake::default());
        let control = control(&fake);
        control.connect(&device(Some(summary(1, true, 5_000))));
        control.pause();
        assert!(!control.view().now.unwrap().playing, "пауза видна сразу");
        control.set_volume(15);
        assert_eq!(control.view().now.unwrap().volume, Some(15));
        control.seek_to(60_000);
        assert_eq!(control.view().now.unwrap().position_ms, 60_000);
        control.set_volume(500);
        assert_eq!(control.view().now.unwrap().volume, Some(100), "громкость не выше 100");
    }

    #[tokio::test(start_paused = true)]
    async fn tapping_a_track_plays_that_list_on_the_device() {
        let fake = Arc::new(RemoteFake::default());
        let control = control(&fake);
        let tracks: Vec<Track> = (0..500).map(track).collect();
        assert!(!control.play_queue(&tracks, 3), "устройство не выбрано — играем здесь");
        control.connect(&device(None));
        assert!(control.play_queue(&tracks, 300));
        settle().await;
        let sent = fake.sent.lock().unwrap().clone();
        let command = sent.last().unwrap();
        assert_eq!(command.action, "play_queue");
        let queue = command.queue.as_ref().unwrap();
        assert_eq!(queue.len(), 200);
        assert_eq!(queue[command.index.unwrap() as usize].video_id, id(300), "индекс — трек, по которому нажали");
        assert!(control.play_queue(&tracks, 9_999), "индекс вне списка принят и ничего не делает");
    }

    #[tokio::test(start_paused = true)]
    async fn offline_and_disabled_turn_the_remote_off_with_words() {
        let fake = Arc::new(RemoteFake::default());
        let control = control(&fake);
        let notices = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&notices);
        control.subscribe_notices(move |n| sink.lock().unwrap().push(n.clone()));
        for (code, expected) in [
            ("device_offline", RemoteNotice::Offline("MacBook Air".into())),
            ("remote_control_disabled", RemoteNotice::Disabled("MacBook Air".into())),
            ("device_not_found", RemoteNotice::Failed),
        ] {
            control.connect(&device(None));
            fake.errors.lock().unwrap().push_back(ApiError::new(409, code, ""));
            control.pause();
            settle().await;
            assert_eq!(notices.lock().unwrap().last(), Some(&expected), "{code}");
            assert!(!control.active(), "{code}: пульт выключен");
        }
        // Другая ошибка пульт не выключает.
        control.connect(&device(None));
        fake.errors.lock().unwrap().push_back(ApiError::new(500, "internal", ""));
        control.pause();
        settle().await;
        assert!(control.active());
        assert_eq!(notices.lock().unwrap().last(), Some(&RemoteNotice::Failed));
    }

    #[tokio::test(start_paused = true)]
    async fn a_command_to_a_device_left_behind_is_not_sent() {
        let fake = Arc::new(RemoteFake::default());
        let control = control(&fake);
        control.connect(&device(None));
        control.pause();
        control.disconnect();
        settle().await;
        assert!(fake.sent.lock().unwrap().is_empty(), "отключились раньше, чем команда ушла");
    }
}
