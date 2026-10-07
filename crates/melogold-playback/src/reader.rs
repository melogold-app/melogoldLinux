//! Чтение диапазонов потока (docs/PROMPT.md §4 «Байты», грабли §9 п. 1, 3), порт Windows
//! `HttpRangeReader.cs`.
//!
//! Сначала — кэш на диске: что там есть, в сеть не ходит; прочитанное из сети ложится туда. 403 и
//! истёкший адрес — не пропуск трека, а свежий адрес и повтор: до четырёх раз подряд, перед вторым,
//! третьим и четвёртым — паузы 1,5, 4 и 8 с ([`FRESH_URL_DELAYS`]). Адрес googlevideo привязан к IP:
//! после смены сети или сервера VPN и свежий адрес, взятый в ту же секунду, ещё получает 403, и песня
//! обрывалась на середине (задание 0021, Windows `HttpRangeReader.cs`). Сетевая ошибка — повтор
//! через 1 и 3 с. Параллельные чтения, получившие 403 на один адрес, обновляют его один раз.
//! Короткие диапазоны googlevideo не душит: одним запросом файл шёл бы около 35 КБ/с.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::resolver::{Intent, Resolver, StreamError, StreamErrorKind, StreamInfo};
use crate::song_cache::{Entry, SongCache};

/// Паузы перед каждым свежим адресом подряд на 401/403/410: первый — сразу. Удачное чтение
/// сбрасывает счёт. Тесты ставят нулевые ([`RangeReader::set_fresh_url_delays`]).
pub const FRESH_URL_DELAYS: [Duration; 4] = [Duration::ZERO, Duration::from_millis(1500), Duration::from_secs(4), Duration::from_secs(8)];

pub struct RangeReader {
    http: reqwest::Client,
    resolver: Arc<Resolver>,
    info: Mutex<StreamInfo>,
    /// Растёт при каждом обновлении адреса: чтение обновляет его, только если адрес ещё тот, с которым оно получило 403.
    generation: AtomicU64,
    refresh_lock: tokio::sync::Mutex<()>,
    refreshes: AtomicU32,
    fresh_url_delays: Mutex<Vec<Duration>>,
    total: Mutex<Option<u64>>,
    cache: Option<Arc<Entry>>,
    songs: Option<Arc<SongCache>>,
    cancelled: AtomicBool,
    /// Читает заготовка или загрузка: закрытый адрес YouTube не пробуется (задание 0013).
    background: AtomicBool,
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
            fresh_url_delays: Mutex::new(FRESH_URL_DELAYS.to_vec()),
            total: Mutex::new(total),
            cache,
            songs,
            cancelled: AtomicBool::new(false),
            background: AtomicBool::new(false),
            cancel: tokio::sync::Notify::new(),
        })
    }

    /// Свои паузы перед свежими адресами (тесты — нулевые); их число — сколько адресов подряд.
    pub fn set_fresh_url_delays(&self, delays: Vec<Duration>) {
        if let Ok(mut current) = self.fresh_url_delays.lock() {
            *current = delays;
        }
    }

    /// Заготовка и загрузки: свежий адрес не спрашивать, пока YouTube не пускает адрес.
    pub fn set_background(&self) {
        self.background.store(true, Ordering::SeqCst);
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
                                let attempt = self.refreshes.fetch_add(1, Ordering::SeqCst) as usize;
                                let delays = self.fresh_url_delays.lock().map(|d| d.clone()).unwrap_or_default();
                                let Some(wait) = delays.get(attempt).copied() else {
                                    // Место и источник — в тексте: по журналу видно, на каком байте и у
                                    // какого клиента оборвалось.
                                    let total = self.total().map_or_else(|| "?".to_string(), |t| t.to_string());
                                    return Err(StreamError::new(
                                        StreamErrorKind::Extractor,
                                        format!(
                                            "googlevideo {status} after {} fresh URLs at byte {start} of {total} ({})",
                                            delays.len(),
                                            current.source
                                        ),
                                    ));
                                };
                                tracing::info!(
                                    трек = %current.video_id,
                                    код = status,
                                    попытка = attempt + 1,
                                    пауза_мс = wait.as_millis() as u64,
                                    "адрес потока не пускает — берём свежий"
                                );
                                if !wait.is_zero() {
                                    tokio::time::sleep(wait).await;
                                }
                                // Свежий адрес — сразу в новом сеансе YouTube: в «помеченном» сеансе
                                // свежие адреса тоже не пускают дальше первого мегабайта
                                // (`Resolver::renew_visitor`). Истёкшему адресу новый сеанс не мешает.
                                self.resolver.renew_visitor();
                                self.refresh(generation).await?;
                            }
                            continue;
                        }
                        // googlevideo считает запросы с адреса так же, как YouTube: 429 — проверка «вы не бот».
                        429 => {
                            self.resolver.close_address();
                            return Err(StreamError::new(StreamErrorKind::BotCheck, "googlevideo 429"));
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
        let intent = if self.background.load(Ordering::SeqCst) { Intent::Background } else { Intent::User };
        let fresh = self.resolver.resolve_as(&video_id, intent).await?;
        if let Ok(mut info) = self.info.lock() {
            // Сведения из кэша (громкость, длительность) остаются, если свежий ответ их не дал.
            let loudness = info.loudness_db;
            *info = StreamInfo { loudness_db: fresh.loudness_db.or(loudness), ..fresh };
        }
        let _ = self.generation.compare_exchange(seen, seen + 1, Ordering::SeqCst, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;

    use melogold_innertube::player::{AudioFormat, PlayerResponse};

    use super::*;
    use crate::resolver::fake;

    const BODY: &[u8] = b"0123456789abcdefghijklmnopqrstuv";

    /// Подставной googlevideo: `answer(номер запроса)` — `true` отдаёт диапазон (206), `false` — 403.
    fn googlevideo(answer: impl Fn(usize) -> bool + Send + 'static) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut head = [0u8; 4096];
                let read = stream.read(&mut head).unwrap_or(0);
                let head = String::from_utf8_lossy(&head[..read]).to_string();
                let range =
                    head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("range: bytes=").map(str::to_string)).unwrap_or_default();
                let (from, to) = range.split_once('-').unwrap_or(("0", "0"));
                let (from, to): (usize, usize) = (from.parse().unwrap_or(0), to.trim().parse().unwrap_or(0));
                let index = seen.fetch_add(1, Ordering::SeqCst);
                if answer(index) {
                    let to = to.min(BODY.len() - 1);
                    let part = &BODY[from..=to];
                    let _ = write!(
                        stream,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{to}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        BODY.len(),
                        part.len()
                    );
                    let _ = stream.write_all(part);
                } else {
                    let _ = write!(stream, "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
        });
        (base, count)
    }

    /// YouTube, который на каждый запрос `player` даёт новый адрес того же подставного googlevideo.
    fn reader(base: &str) -> (Arc<RangeReader>, Arc<fake::FakeApi>) {
        let first = format!("{base}/videoplayback?expire=4102444800&itag=140&n=0");
        let base = base.to_owned();
        let api = fake::FakeApi::new(move |_, index| {
            Ok(PlayerResponse {
                status: "OK".into(),
                reason: None,
                audio_formats: vec![AudioFormat {
                    itag: 140,
                    url: format!("{base}/videoplayback?expire=4102444800&itag=140&n={}", index + 1),
                    mime_type: "audio/mp4; codecs=\"mp4a.40.2\"".into(),
                    content_length: Some(BODY.len() as u64),
                    bitrate: Some(128_000),
                    loudness_db: None,
                }],
                loudness_db: None,
                duration_ms: Some(180_000),
            })
        });
        let resolver = fake::resolver(&api, fake::two_clients());
        let info = StreamInfo {
            video_id: "kagerou".into(),
            url: first,
            itag: 140,
            content_length: Some(BODY.len() as u64),
            source: "VISIONOS".into(),
            ..Default::default()
        };
        let reader = RangeReader::new(reqwest::Client::new(), resolver, info, None);
        reader.set_fresh_url_delays(vec![Duration::ZERO; 4]);
        (reader, api)
    }

    fn start(answer: impl Fn(usize) -> bool + Send + 'static) -> (Arc<RangeReader>, Arc<fake::FakeApi>, Arc<AtomicUsize>) {
        let (base, count) = googlevideo(answer);
        let (reader, api) = reader(&base);
        (reader, api, count)
    }

    #[tokio::test]
    async fn three_forbidden_answers_then_the_range_plays() {
        let (reader, api, requests) = start(|index| index >= 3);
        assert_eq!(reader.read(4, 8).await.unwrap(), &BODY[4..12]);
        assert_eq!(api.player_calls(), 3, "свежих адресов — три");
        assert_eq!(requests.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn a_forbidden_answer_renews_the_youtube_session() {
        let (reader, _, _) = start(|index| index >= 1);
        reader.resolver.client().set_visitor_data(Some("старый сеанс".into()));
        assert_eq!(reader.read(0, 4).await.unwrap(), &BODY[0..4]);
        assert_eq!(reader.resolver.client().visitor_data(), None, "свежий адрес взят в старом сеансе YouTube");
    }

    #[tokio::test]
    async fn always_forbidden_fails_after_four_fresh_urls_and_says_where() {
        let (reader, api, _) = start(|_| false);
        let error = reader.read(16, 8).await.unwrap_err();
        assert_eq!(api.player_calls(), 4);
        assert!(error.message.contains("after 4 fresh URLs"), "{}", error.message);
        assert!(error.message.contains("at byte 16 of 32"), "{}", error.message);
        assert!(error.message.contains("VISIONOS"), "{}", error.message);
    }

    #[tokio::test]
    async fn forbidden_every_other_read_never_runs_out() {
        // Удачное чтение сбрасывает счёт: 403 через раз не копится до отказа.
        let (reader, _, _) = start(|index| index % 2 == 1);
        for round in 0..10 {
            let start = (round * 3) % 24;
            assert_eq!(reader.read(start as u64, 4).await.unwrap(), &BODY[start..start + 4], "чтение {round}");
        }
    }

    #[test]
    fn the_default_pauses_are_those_of_windows() {
        let millis: Vec<u128> = FRESH_URL_DELAYS.iter().map(Duration::as_millis).collect();
        assert_eq!(millis, [0, 1500, 4000, 8000]);
    }
}
