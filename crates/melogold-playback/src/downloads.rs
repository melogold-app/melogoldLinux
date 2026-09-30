//! Загрузки (docs/PROMPT.md §4 «Загрузки», задание Windows 0003, Windows `TrackDownloads.cs`).
//!
//! «Скачать» оставляет трек насовсем — в своей папке того же формата, что кэш музыки; ни лимит, ни
//! «Очистить кэш» её не трогают. Трек, целиком лежащий в кэше, копируется сразу и без сети;
//! остальные качаются кусками по 1 МБ, по два трека сразу, с долей скачанного. Список загрузок — в
//! библиотеке: прерванные продолжаются при запуске.
//!
//! Проверка «вы не бот» (задание 0013) — не попытка и не сбой: вся очередь встаёт в ожидание с
//! причиной (как ожидание Wi‑Fi), порядок и скачанные куски остаются. Пока YouTube не пускает
//! адрес, загрузки в него не ходят. Очередь идёт дальше по «Повторить» / «Возобновить» или при
//! смене сети — первым уходит один пробный запрос, остальные ждут его исхода.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use melogold_core::music::Track;
use melogold_data::Library;

use crate::reader::RangeReader;
use crate::resolver::{Intent, Resolver, StreamError, StreamErrorKind};
use crate::song_cache::SongCache;

const CHUNK: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DownloadState {
    Queued,
    /// Доля скачанного 0…1, если длина известна.
    Downloading(Option<f64>),
    Completed,
    /// Ждёт: YouTube не пускает адрес (проверка «вы не бот»). Не сбой; причина — в тексте плашки.
    Waiting,
    Failed,
}

type Listener = Box<dyn Fn(&str) + Send + Sync>;

pub struct Downloads {
    store: Arc<SongCache>,
    songs: Arc<SongCache>,
    resolver: Arc<Resolver>,
    library: Arc<Library>,
    http: reqwest::Client,
    runtime: tokio::runtime::Handle,
    slots: Arc<tokio::sync::Semaphore>,
    active: Mutex<HashMap<String, (DownloadState, tokio::task::AbortHandle)>>,
    /// Порядок очереди: кто раньше попросил, тот раньше и качается.
    order: Mutex<Vec<String>>,
    /// Ждут снятия проверки «вы не бот», в порядке очереди.
    waiting: Mutex<Vec<String>>,
    /// Запросы адресов идут по одному: после проверки «вы не бот» остальные не спрашивают YouTube.
    gate: tokio::sync::Mutex<()>,
    /// Пользователь просил («Скачать», «Повторить», «Возобновить»): следующий запрос адреса — пробный.
    probe: std::sync::atomic::AtomicBool,
    failed: Mutex<HashSet<String>>,
    listener: Mutex<Option<Listener>>,
}

impl Downloads {
    pub fn new(
        store: Arc<SongCache>,
        songs: Arc<SongCache>,
        resolver: Arc<Resolver>,
        library: Arc<Library>,
        http: reqwest::Client,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Downloads> {
        Arc::new(Downloads {
            store,
            songs,
            resolver,
            library,
            http,
            runtime,
            slots: Arc::new(tokio::sync::Semaphore::new(2)),
            active: Mutex::default(),
            order: Mutex::default(),
            waiting: Mutex::default(),
            gate: tokio::sync::Mutex::new(()),
            probe: std::sync::atomic::AtomicBool::new(false),
            failed: Mutex::default(),
            listener: Mutex::default(),
        })
    }

    /// Скачанное лежит здесь: плеер играет отсюда без сети.
    pub fn store(&self) -> &Arc<SongCache> {
        &self.store
    }

    /// У трека изменилась загрузка — метки «есть без сети» обновляются.
    pub fn set_listener(&self, listener: impl Fn(&str) + Send + Sync + 'static) {
        if let Ok(mut slot) = self.listener.lock() {
            *slot = Some(Box::new(listener));
        }
    }

    fn changed(&self, video_id: &str) {
        if let Ok(slot) = self.listener.lock() {
            if let Some(listener) = slot.as_ref() {
                listener(video_id);
            }
        }
    }

    /// Загрузка трека; `None` — трек не скачивали.
    pub fn state(&self, video_id: &str) -> Option<DownloadState> {
        if let Some((state, _)) = self.active.lock().ok()?.get(video_id) {
            return Some(*state);
        }
        if self.waiting.lock().ok()?.iter().any(|id| id == video_id) {
            return Some(DownloadState::Waiting);
        }
        if self.store.is_complete(video_id) {
            return Some(DownloadState::Completed);
        }
        self.failed.lock().ok()?.contains(video_id).then_some(DownloadState::Failed)
    }

    pub fn size(&self) -> u64 {
        self.store.size()
    }

    /// «Скачать»: треки — в список загрузок и в очередь; трансляции и уже скачанное пропускаются.
    pub fn download(self: &Arc<Self>, tracks: &[Track]) -> usize {
        let wanted = wanted(tracks, |id| self.state(id));
        if let Err(error) = self.library.add_downloads(&wanted) {
            tracing::warn!(%error, "загрузки не записались в библиотеку");
        }
        self.probe.store(true, std::sync::atomic::Ordering::SeqCst);
        for track in &wanted {
            self.start(&track.video_id);
        }
        wanted.len()
    }

    /// «Скачать снова» после сбоя или «Повторить» у ожидающей: у ожидающих идёт вся очередь.
    pub fn retry(self: &Arc<Self>, video_id: &str) {
        self.probe.store(true, std::sync::atomic::Ordering::SeqCst);
        if self.state(video_id) == Some(DownloadState::Waiting) {
            self.resume_waiting();
        } else {
            self.start(video_id);
        }
    }

    /// «Возобновить», смена сети: очередь, вставшая на проверке «вы не бот», идёт дальше по порядку.
    pub fn resume_waiting(self: &Arc<Self>) {
        let ids = self.waiting.lock().map(|mut w| std::mem::take(&mut *w)).unwrap_or_default();
        for id in &ids {
            self.changed(id);
        }
        for id in ids {
            self.start(&id);
        }
    }

    /// Загрузки ждут проверки «вы не бот».
    pub fn is_waiting(&self) -> bool {
        self.waiting.lock().is_ok_and(|w| !w.is_empty())
    }

    /// Сеть сменилась: адрес выхода другой — отметка «закрыт» снимается, ожидающие идут дальше.
    pub fn network_changed(self: &Arc<Self>) {
        self.resolver.network_changed();
        self.resume_waiting();
    }

    /// При запуске: то, что скачивали и не докачали, продолжается.
    pub fn resume(self: &Arc<Self>) {
        match self.library.download_ids() {
            Ok(ids) => {
                for video_id in ids.iter().filter(|id| !self.store.is_complete(id)) {
                    self.start(video_id);
                }
            }
            Err(error) => tracing::warn!(%error, "список загрузок не читается"),
        }
    }

    /// «Отменить загрузку» и «Удалить загрузку»: из списка, байты — с диска.
    pub fn remove(&self, video_id: &str) {
        if let Some((_, handle)) = self.active.lock().ok().and_then(|mut a| a.remove(video_id)) {
            handle.abort();
        }
        if let Ok(mut failed) = self.failed.lock() {
            failed.remove(video_id);
        }
        self.forget(video_id);
        let _ = self.library.remove_download(video_id);
        self.store.remove(video_id, false);
        self.changed(video_id);
    }

    /// «Удалить все загрузки» (Настройки › Хранилище).
    pub fn remove_all(&self) {
        let mut ids: HashSet<String> = self.library.download_ids().unwrap_or_default();
        ids.extend(self.store.video_ids());
        ids.extend(self.active.lock().map(|a| a.keys().cloned().collect::<Vec<_>>()).unwrap_or_default());
        for video_id in ids {
            self.remove(&video_id);
        }
        let _ = self.library.remove_all_downloads();
    }

    /// Из порядка очереди и из ожидающих.
    fn forget(&self, video_id: &str) {
        if let Ok(mut order) = self.order.lock() {
            order.retain(|id| id != video_id);
        }
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.retain(|id| id != video_id);
        }
    }

    fn start(self: &Arc<Self>, video_id: &str) {
        let Ok(mut active) = self.active.lock() else { return };
        if active.contains_key(video_id) {
            return;
        }
        if let Ok(mut waiting) = self.waiting.lock() {
            waiting.retain(|id| id != video_id);
        }
        if let Ok(mut order) = self.order.lock() {
            if !order.iter().any(|id| id == video_id) {
                order.push(video_id.to_owned());
            }
        }
        if let Ok(mut failed) = self.failed.lock() {
            failed.remove(video_id);
        }
        let (this, id) = (Arc::clone(self), video_id.to_owned());
        let task = self.runtime.spawn(async move { this.run(id).await });
        active.insert(video_id.to_owned(), (DownloadState::Queued, task.abort_handle()));
        drop(active);
        self.changed(video_id);
    }

    async fn run(self: Arc<Self>, video_id: String) {
        let Ok(_slot) = Arc::clone(&self.slots).acquire_owned().await else { return };
        let result = if self.store.is_complete(&video_id) || self.store.copy_from(&self.songs, &video_id) {
            Ok(())
        } else {
            self.fetch(&video_id, &self.store, true).await
        };
        if let Err(error) = &result {
            if StreamError::stops_queue(error.kind) {
                // Не сбой и не попытка: вся очередь ждёт, скачанные куски остаются.
                tracing::warn!(трек = %video_id, "загрузки ждут: YouTube просит проверку «не бот»");
                self.wait_all(&video_id);
                return;
            }
        }
        self.forget(&video_id);
        match result {
            Ok(()) if self.store.is_complete(&video_id) => {
                // Дубль в кэше плеера больше не нужен: место — новым трекам.
                self.songs.remove(&video_id, true);
                tracing::info!(трек = %video_id, "скачано");
            }
            Ok(()) => self.fail(&video_id, "загрузка не целиком"),
            Err(error) => self.fail(&video_id, &error.message),
        }
        if let Ok(mut active) = self.active.lock() {
            active.remove(&video_id);
        }
        self.changed(&video_id);
    }

    /// Все идущие и стоящие в очереди — в ожидание, в порядке очереди; `first` — тот, кто упёрся.
    fn wait_all(&self, first: &str) {
        let running: Vec<String> = self
            .active
            .lock()
            .map(|mut a| {
                a.drain()
                    .map(|(id, (_, handle))| {
                        handle.abort();
                        id
                    })
                    .collect()
            })
            .unwrap_or_default();
        let order = self.order.lock().map(|o| o.clone()).unwrap_or_default();
        let mut ids: Vec<String> = order.into_iter().filter(|id| running.contains(id)).collect();
        for id in running {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if let Some(position) = ids.iter().position(|id| id == first) {
            let id = ids.remove(position);
            ids.insert(0, id);
        }
        if let Ok(mut waiting) = self.waiting.lock() {
            for id in &ids {
                if !waiting.contains(id) {
                    waiting.push(id.clone());
                }
            }
        }
        for id in &ids {
            self.changed(id);
        }
    }

    fn fail(&self, video_id: &str, reason: &str) {
        tracing::warn!(трек = %video_id, %reason, "загрузка не удалась");
        if let Ok(mut failed) = self.failed.lock() {
            failed.insert(video_id.to_owned());
        }
    }

    fn report(&self, video_id: &str, progress: Option<f64>) {
        if let Ok(mut active) = self.active.lock() {
            if let Some(entry) = active.get_mut(video_id) {
                entry.0 = DownloadState::Downloading(progress);
            }
        }
        self.changed(video_id);
    }

    /// Все байты трека (для «Сохранить файлом»): из загрузок, из кэша или из сети в кэш музыки —
    /// заодно он будет играть без сети.
    pub async fn read_whole(self: &Arc<Self>, video_id: &str) -> Result<Vec<u8>, StreamError> {
        if let Some(bytes) = self.store.read_complete(video_id).or_else(|| self.songs.read_complete(video_id)) {
            return Ok(bytes);
        }
        self.fetch(video_id, &self.songs, false).await?;
        self.songs.read_complete(video_id).ok_or_else(|| StreamError::new(StreamErrorKind::Network, "the track is not complete"))
    }

    /// Весь поток — кусками в `target`; что уже на диске (прерванная загрузка), в сеть не идёт.
    async fn fetch(&self, video_id: &str, target: &Arc<SongCache>, report: bool) -> Result<(), StreamError> {
        let info = if report {
            // Адреса очереди — по одному: если первый упрётся в проверку «вы не бот», остальные
            // увидят закрытый адрес и не пойдут в YouTube. Пробным становится только запрос,
            // за которым стоит действие пользователя.
            let _turn = self.gate.lock().await;
            let asked = self.probe.swap(false, std::sync::atomic::Ordering::SeqCst);
            self.resolver.resolve_as(video_id, if asked { Intent::User } else { Intent::Background }).await?
        } else {
            self.resolver.resolve(video_id).await?
        };
        let entry = target.entry(&info);
        let reader = RangeReader::new(self.http.clone(), Arc::clone(&self.resolver), info, Some((Arc::clone(target), entry)));
        if report {
            reader.set_background();
        }
        let mut position = 0u64;
        let mut reported = Instant::now() - Duration::from_secs(1);
        let result = loop {
            let bytes = match reader.read(position, CHUNK).await {
                Ok(bytes) => bytes,
                Err(error) => break Err(error),
            };
            if bytes.is_empty() {
                break Ok(());
            }
            position += bytes.len() as u64;
            let total = reader.total();
            if total.is_some_and(|t| position >= t) {
                break Ok(());
            }
            if report && reported.elapsed() > Duration::from_millis(300) {
                reported = Instant::now();
                self.report(video_id, total.filter(|t| *t > 0).map(|t| position as f64 / t as f64));
            }
        };
        reader.cancel();
        result
    }
}

/// Что из `tracks` качать: трансляции, скачанное и уже идущее пропускаются, сбой — качается снова.
fn wanted(tracks: &[Track], state: impl Fn(&str) -> Option<DownloadState>) -> Vec<Track> {
    let mut seen = HashSet::new();
    tracks
        .iter()
        .filter(|t| !t.is_live() && seen.insert(t.video_id.as_str()))
        .filter(|t| !matches!(state(&t.video_id), Some(DownloadState::Completed | DownloadState::Downloading(_) | DownloadState::Queued)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, live: bool) -> Track {
        Track { video_id: id.into(), title: id.into(), video_type: live.then(|| "live".into()), ..Default::default() }
    }

    #[test]
    fn download_skips_downloaded_going_and_live() {
        let tracks = [
            track("new", false),
            track("done", false),
            track("going", false),
            track("queued", false),
            track("failed", false),
            track("live", true),
            track("new", false),
        ];
        let state = |id: &str| match id {
            "done" => Some(DownloadState::Completed),
            "going" => Some(DownloadState::Downloading(Some(0.5))),
            "queued" => Some(DownloadState::Queued),
            "failed" => Some(DownloadState::Failed),
            _ => None,
        };
        let ids: Vec<String> = wanted(&tracks, state).into_iter().map(|t| t.video_id).collect();
        assert_eq!(ids, ["new", "failed"]);
    }

    use crate::resolver::fake::{self, FakeApi};

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("не дождались: {what}");
    }

    /// Три загрузки и проверка «вы не бот»: один запрос, остальные ждут с причиной, это не сбой.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bot_check_puts_the_whole_queue_on_hold_with_one_request() {
        let dir = std::env::temp_dir().join(format!("melogold-downloads-test-{}", std::process::id()));
        let api = FakeApi::new(|_, _| fake::bot_check());
        let resolver = fake::resolver(&api, fake::two_clients());
        let library = Library::open(melogold_data::Database::in_memory().unwrap());
        let downloads = Downloads::new(
            SongCache::new(dir.join("store"), 0),
            SongCache::new(dir.join("songs"), 0),
            Arc::clone(&resolver),
            library,
            reqwest::Client::new(),
            tokio::runtime::Handle::current(),
        );
        let tracks = [track("a", false), track("b", false), track("c", false)];
        assert_eq!(downloads.download(&tracks), 3);
        until("все три ждут", || tracks.iter().all(|t| downloads.state(&t.video_id) == Some(DownloadState::Waiting))).await;
        assert_eq!(api.player_calls(), 1, "один запрос, остальные ждут");
        assert_eq!(api.playabilities.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(downloads.is_waiting() && resolver.is_address_closed());
        assert_eq!(*downloads.waiting.lock().unwrap(), ["a", "b", "c"], "порядок очереди сохранён");
        assert!(downloads.failed.lock().unwrap().is_empty(), "не сбой");

        // «Повторить» — первым один пробный запрос.
        downloads.retry("b");
        until("снова ждут", || api.player_calls() == 2 && downloads.is_waiting()).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(api.player_calls(), 2);
        assert!(tracks.iter().all(|t| downloads.state(&t.video_id) == Some(DownloadState::Waiting)));

        // Смена сети снимает отметку: идёт один запрос, остальные снова ждут.
        downloads.network_changed();
        until("после смены сети", || api.player_calls() == 3 && downloads.is_waiting()).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(api.player_calls(), 3);
        for t in &tracks {
            downloads.remove(&t.video_id);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
