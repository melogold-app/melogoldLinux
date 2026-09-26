//! Загрузки (docs/PROMPT.md §4 «Загрузки», задание Windows 0003, Windows `TrackDownloads.cs`).
//!
//! «Скачать» оставляет трек насовсем — в своей папке того же формата, что кэш музыки; ни лимит, ни
//! «Очистить кэш» её не трогают. Трек, целиком лежащий в кэше, копируется сразу и без сети;
//! остальные качаются кусками по 1 МБ, по два трека сразу, с долей скачанного. Список загрузок — в
//! библиотеке: прерванные продолжаются при запуске.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use melogold_core::music::Track;
use melogold_data::Library;

use crate::reader::RangeReader;
use crate::resolver::{Resolver, StreamError, StreamErrorKind};
use crate::song_cache::SongCache;

const CHUNK: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DownloadState {
    Queued,
    /// Доля скачанного 0…1, если длина известна.
    Downloading(Option<f64>),
    Completed,
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
        for track in &wanted {
            self.start(&track.video_id);
        }
        wanted.len()
    }

    /// «Скачать снова» после сбоя.
    pub fn retry(self: &Arc<Self>, video_id: &str) {
        self.start(video_id);
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

    fn start(self: &Arc<Self>, video_id: &str) {
        let Ok(mut active) = self.active.lock() else { return };
        if active.contains_key(video_id) {
            return;
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
        let info = self.resolver.resolve(video_id).await?;
        let entry = target.entry(&info);
        let reader = RangeReader::new(self.http.clone(), Arc::clone(&self.resolver), info, Some((Arc::clone(target), entry)));
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
}
