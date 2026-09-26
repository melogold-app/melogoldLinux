//! Чтение диапазонов потока (docs/PROMPT.md §4 «Байты», грабли §9 п. 1, 3), порт Windows
//! `HttpRangeReader.cs`.
//!
//! Сначала — кэш на диске: что там есть, в сеть не ходит; прочитанное из сети ложится туда. 403 и
//! истёкший адрес — не пропуск трека, а свежий адрес и повтор (до двух раз подряд); сетевая ошибка —
//! повтор через 1 и 3 с. Параллельные чтения, получившие 403 на один адрес, обновляют его один раз.
//! Короткие диапазоны googlevideo не душит: одним запросом файл шёл бы около 35 КБ/с.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::resolver::{Resolver, StreamError, StreamErrorKind, StreamInfo};
use crate::song_cache::{Entry, SongCache};

pub struct RangeReader {
    http: reqwest::Client,
    resolver: Arc<Resolver>,
    info: Mutex<StreamInfo>,
    /// Растёт при каждом обновлении адреса: чтение обновляет его, только если адрес ещё тот, с которым оно получило 403.
    generation: AtomicU64,
    refresh_lock: tokio::sync::Mutex<()>,
    refreshes: AtomicU32,
    total: Mutex<Option<u64>>,
    cache: Option<Arc<Entry>>,
    songs: Option<Arc<SongCache>>,
    cancelled: AtomicBool,
    cancel: tokio::sync::Notify,
}

impl RangeReader {
    pub fn new(http: reqwest::Client, resolver: Arc<Resolver>, info: StreamInfo, cache: Option<(Arc<SongCache>, Arc<Entry>)>) -> Arc<Self> {
        let total = info.content_length;
        let (songs, cache) = cache.map(|(s, e)| (Some(s), Some(e))).unwrap_or((None, None));
        Arc::new(Self {
            http,
            resolver,
            info: Mutex::new(info),
            generation: AtomicU64::new(0),
            refresh_lock: tokio::sync::Mutex::new(()),
            refreshes: AtomicU32::new(0),
            total: Mutex::new(total),
            cache,
            songs,
            cancelled: AtomicBool::new(false),
            cancel: tokio::sync::Notify::new(),
        })
    }

    pub fn info(&self) -> StreamInfo {
        self.info.lock().map(|i| i.clone()).unwrap_or_default()
    }

    pub fn total(&self) -> Option<u64> {
        self.total.lock().ok().and_then(|t| *t)
    }

    /// Трек больше не нужен: текущие и будущие чтения прерываются, кэш трека отпускается.
    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::SeqCst) {
            self.cancel.notify_waiters();
            if let Some(entry) = &self.cache {
                entry.release();
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn cancelled_error() -> StreamError {
        StreamError::new(StreamErrorKind::Network, "cancelled")
    }

    pub async fn read(&self, start: u64, length: usize) -> Result<Vec<u8>, StreamError> {
        if self.is_cancelled() {
            return Err(Self::cancelled_error());
        }
        tokio::select! {
            result = self.read_inner(start, length) => result,
            () = self.cancel.notified() => Err(Self::cancelled_error()),
        }
    }

    async fn read_inner(&self, start: u64, length: usize) -> Result<Vec<u8>, StreamError> {
        let mut network_retries = 0;
        loop {
            let (current, generation) = (self.info(), self.generation.load(Ordering::SeqCst));
            let mut end = start + length as u64 - 1;
            if let Some(total) = self.total() {
                end = end.min(total.saturating_sub(1));
            }
            if end < start || length == 0 {
                return Ok(Vec::new());
            }
            let wanted = (end - start + 1) as usize;
            if let Some(cached) = self.cache.as_ref().and_then(|c| c.try_read(start, wanted)) {
                return Ok(cached);
            }
            if current.url.is_empty() {
                // Трек открыт из кэша, а диапазона на диске уже нет (кэш очистили): нужен адрес.
                self.refresh(generation).await?;
                continue;
            }
            let mut request = self.http.get(&current.url).header("Range", format!("bytes={start}-{end}")).timeout(Duration::from_secs(20));
            if let Some(user_agent) = &current.user_agent {
                request = request.header("User-Agent", user_agent);
            }
            let failure = match request.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    match status {
                        401 | 403 | 410 => {
                            let _guard = self.refresh_lock.lock().await;
                            // Адрес уже обновило другое чтение — повторить с ним.
                            if self.generation.load(Ordering::SeqCst) == generation {
                                if self.refreshes.fetch_add(1, Ordering::SeqCst) + 1 > 2 {
                                    return Err(StreamError::new(
                                        StreamErrorKind::Extractor,
                                        format!("googlevideo {status} after fresh URLs"),
                                    ));
                                }
                                tracing::info!(трек = %current.video_id, код = status, "адрес потока истёк — берём свежий");
                                self.refresh(generation).await?;
                            }
                            continue;
                        }
                        416 => return Ok(Vec::new()),
                        200..=299 => {
                            let full = response
                                .headers()
                                .get("Content-Range")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|v| v.rsplit('/').next())
                                .and_then(|v| v.parse::<u64>().ok());
                            match response.bytes().await {
                                Ok(bytes) => {
                                    // Свежий адрес работает: следующий 403 (через часы) снова может его обновить.
                                    if self.generation.load(Ordering::SeqCst) == generation {
                                        self.refreshes.store(0, Ordering::SeqCst);
                                    }
                                    if let Some(full) = full {
                                        if let Ok(mut total) = self.total.lock() {
                                            *total = Some(full);
                                        }
                                    }
                                    if let Some(cache) = &self.cache {
                                        if cache.write(start, &bytes, self.total()) {
                                            if let Some(songs) = &self.songs {
                                                songs.completed(&current.video_id);
                                            }
                                        }
                                    }
                                    return Ok(bytes.to_vec());
                                }
                                Err(error) => error.to_string(),
                            }
                        }
                        _ => format!("HTTP {status}"),
                    }
                }
                Err(error) => error.to_string(),
            };
            network_retries += 1;
            if network_retries > 2 {
                return Err(StreamError::new(StreamErrorKind::Network, format!("Network error reading the stream: {failure}")));
            }
            tokio::time::sleep(Duration::from_millis(if network_retries == 1 { 1000 } else { 3000 })).await;
        }
    }

    async fn refresh(&self, seen: u64) -> Result<(), StreamError> {
        let video_id = self.info().video_id;
        self.resolver.invalidate(&video_id);
        let fresh = self.resolver.resolve(&video_id).await?;
        if let Ok(mut info) = self.info.lock() {
            // Сведения из кэша (громкость, длительность) остаются, если свежий ответ их не дал.
            let loudness = info.loudness_db;
            *info = StreamInfo { loudness_db: fresh.loudness_db.or(loudness), ..fresh };
        }
        let _ = self.generation.compare_exchange(seen, seen + 1, Ordering::SeqCst, Ordering::SeqCst);
        Ok(())
    }
}
