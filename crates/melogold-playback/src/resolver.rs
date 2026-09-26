//! Поток трека без Python и yt-dlp (docs/PROMPT.md §4, Windows `StreamResolver.cs`).
//!
//! Запрос InnerTube `player` клиентами, которые отдают прямые ссылки без PO-токена (сейчас
//! VISIONOS), по очереди из списка — его можно заменить свежим из репозитория без релиза. Формат —
//! itag 140 (AAC в m4a). Адреса кэшируются (LRU 64) до `expire − 5 мин`; кэш сбрасывается при 403
//! и смене сети. Одновременно — не больше двух извлечений, у каждого сторож 20 с. Не получилось —
//! один запрос WEB объясняет почему (задание 0001), и в журнал уходит одна строка.

use std::collections::VecDeque;
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use melogold_core::text::now_ms;
use melogold_innertube::player::{self, Playability};
use melogold_innertube::{ClientProfile, ErrorKind, InnerTube};

const CACHE_SIZE: usize = 64;
const WATCHDOG: Duration = Duration::from_secs(20);

/// Адрес аудиопотока трека и то, как по нему ходить.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StreamInfo {
    pub video_id: String,
    /// Пусто у трека, открытого из кэша или загрузок: адрес понадобится, только если байтов не хватит.
    pub url: String,
    pub itag: u32,
    pub mime_type: String,
    pub content_length: Option<u64>,
    pub bitrate: Option<u32>,
    /// `expire` адреса минус 5 минут, мс Unix.
    pub expires_at_ms: i64,
    /// Клиент InnerTube, который дал адрес («Сведения о потоке»).
    pub source: String,
    /// User-Agent, с которым googlevideo отдаёт этот адрес; `None` — любой.
    pub user_agent: Option<String>,
    pub loudness_db: Option<f64>,
    pub duration_ms: Option<i64>,
}

impl StreamInfo {
    pub fn codec(&self) -> &str {
        if self.mime_type.contains("opus") {
            "Opus"
        } else if self.mime_type.contains("mp4a") {
            "AAC"
        } else {
            &self.mime_type
        }
    }
}

/// Класс ошибки получения потока (REWRITE §4.10.3): по нему повторы и текст карточки.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamErrorKind {
    Network,
    Timeout,
    BotCheck,
    Geo,
    Unavailable,
    Age,
    Extractor,
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct StreamError {
    pub kind: StreamErrorKind,
    pub message: String,
    /// Трек закрыт в стране: где YouTube видит устройство (`RU`); `None` — не сказал.
    pub country: Option<String>,
    /// В скольких странах правообладатель открыл трек; `None` — неизвестно.
    pub open_countries: Option<usize>,
}

impl StreamError {
    pub fn new(kind: StreamErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into(), country: None, open_countries: None }
    }

    /// Сколько раз повторить, прежде чем пропустить трек: сеть — 2, таймаут, бот и прочее — 1,
    /// гео, возраст, недоступно — 0.
    pub fn retries(&self) -> u32 {
        match self.kind {
            StreamErrorKind::Network => 2,
            StreamErrorKind::Timeout | StreamErrorKind::BotCheck | StreamErrorKind::Extractor => 1,
            _ => 0,
        }
    }

    /// Пропуск без повторов: причина в самом видео.
    pub fn is_final(&self) -> bool {
        matches!(self.kind, StreamErrorKind::Geo | StreamErrorKind::Unavailable | StreamErrorKind::Age)
    }
}

pub struct Resolver {
    client: InnerTube,
    clients: RwLock<Vec<ClientProfile>>,
    cache: Mutex<VecDeque<StreamInfo>>,
    slots: tokio::sync::Semaphore,
}

impl Resolver {
    pub fn new(client: InnerTube, clients: Vec<ClientProfile>) -> Self {
        Self { client, clients: RwLock::new(clients), cache: Mutex::default(), slots: tokio::sync::Semaphore::new(2) }
    }

    pub fn set_clients(&self, clients: Vec<ClientProfile>) {
        if let Ok(mut current) = self.clients.write() {
            *current = clients;
        }
    }

    pub fn client(&self) -> &InnerTube {
        &self.client
    }

    pub async fn resolve(&self, video_id: &str) -> Result<StreamInfo, StreamError> {
        if let Some(cached) = self.cached(video_id) {
            return Ok(cached);
        }
        let _slot = self.slots.acquire().await.map_err(|_| StreamError::new(StreamErrorKind::Extractor, "resolver closed"))?;
        if let Some(cached) = self.cached(video_id) {
            return Ok(cached);
        }
        let clients = self.clients.read().map(|c| c.clone()).unwrap_or_default();
        let mut last = StreamError::new(StreamErrorKind::Extractor, "no stream clients");
        for profile in &clients {
            match tokio::time::timeout(WATCHDOG, self.ask_client(profile, video_id)).await {
                Ok(Ok(info)) => {
                    self.remember(info.clone());
                    return Ok(info);
                }
                // Причина в самом видео: другой клиент ответит тем же.
                Ok(Err(error)) if error.is_final() => return Err(self.explain(video_id, error).await),
                Ok(Err(error)) => last = error,
                Err(_) => last = StreamError::new(StreamErrorKind::Timeout, format!("{}: timeout", profile.name)),
            }
        }
        Err(self.explain(video_id, last).await)
    }

    /// Забыть адрес трека (403 при чтении): следующий резолв спросит заново.
    pub fn invalidate(&self, video_id: &str) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.retain(|info| info.video_id != video_id);
        }
    }

    /// Сеть сменилась: адреса привязаны к адресу клиента — сбрасываются все.
    pub fn invalidate_all(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
    }

    fn cached(&self, video_id: &str) -> Option<StreamInfo> {
        let mut cache = self.cache.lock().ok()?;
        let index = cache.iter().position(|info| info.video_id == video_id)?;
        let info = cache.remove(index)?;
        if info.expires_at_ms <= now_ms() {
            return None;
        }
        cache.push_front(info.clone());
        Some(info)
    }

    fn remember(&self, info: StreamInfo) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.retain(|known| known.video_id != info.video_id);
            cache.push_front(info);
            cache.truncate(CACHE_SIZE);
        }
    }

    async fn ask_client(&self, profile: &ClientProfile, video_id: &str) -> Result<StreamInfo, StreamError> {
        #[cfg(debug_assertions)]
        if fake_geo().is_some_and(|(id, _)| id == video_id) {
            return Err(StreamError::new(
                StreamErrorKind::Unavailable,
                format!("{}: UNPLAYABLE Video unavailable (MELOGOLD_FAKE_GEO)", profile.name),
            ));
        }
        let to_stream_error = |error: melogold_innertube::YouTubeError| {
            let kind = if error.kind == ErrorKind::Blocked { StreamErrorKind::BotCheck } else { StreamErrorKind::Network };
            StreamError::new(kind, error.message)
        };
        self.client.ensure_visitor_data().await.map_err(to_stream_error)?;
        let response = player::player(&self.client, profile, video_id, WATCHDOG).await.map_err(to_stream_error)?;
        if response.status != "OK" {
            let text = format!("{} {}", response.status, response.reason.as_deref().unwrap_or_default());
            return Err(StreamError::new(classify(&text), format!("{}: {}", profile.name, text.trim())));
        }
        let chosen = response
            .choose()
            .ok_or_else(|| StreamError::new(StreamErrorKind::Extractor, format!("{}: no audio with a plain URL", profile.name)))?;
        Ok(StreamInfo {
            video_id: video_id.to_owned(),
            url: chosen.url.clone(),
            itag: chosen.itag,
            mime_type: chosen.mime_type.clone(),
            content_length: chosen.content_length,
            bitrate: chosen.bitrate,
            expires_at_ms: expires_at(&chosen.url, now_ms()),
            source: profile.name.clone(),
            user_agent: profile.media_user_agent.clone(),
            loudness_db: chosen.loudness_db.or(response.loudness_db),
            duration_ms: response.duration_ms,
        })
    }

    /// Поток не получен: один раз спросить YouTube клиентом WEB, почему, и уточнить ошибку. Сеть
    /// и таймаут не уточняются — тогда YouTube не ответит и WEB. В журнал — одна строка на отказ.
    async fn explain(&self, video_id: &str, failure: StreamError) -> StreamError {
        if matches!(failure.kind, StreamErrorKind::Network | StreamErrorKind::Timeout) {
            tracing::warn!(трек = video_id, итог = ?failure.kind, клиент = %failure.message, "поток не получен");
            return failure;
        }
        #[allow(unused_mut)]
        let mut playability = player::playability(&self.client, video_id).await.ok();
        #[cfg(debug_assertions)]
        if let (Some((id, country)), Some(p)) = (fake_geo(), playability.as_mut()) {
            if id == video_id {
                p.country = Some(country);
            }
        }
        let result = diagnose(playability.as_ref(), &failure).unwrap_or_else(|| failure.clone());
        tracing::warn!(
            трек = video_id,
            итог = ?result.kind,
            статус = playability.as_ref().and_then(|p| p.status.as_deref()).unwrap_or("-"),
            причина = playability.as_ref().and_then(|p| p.reason.as_deref()).unwrap_or(""),
            страна = playability.as_ref().and_then(|p| p.country.as_deref()).unwrap_or("-"),
            открыт_в = playability.as_ref().map(|p| p.available_countries.len()).unwrap_or(0),
            клиент = %failure.message,
            "поток не получен"
        );
        result
    }
}

/// Только отладочная сборка: `MELOGOLD_FAKE_GEO=<videoId>:<страна>` — поток этого трека «не получен»,
/// а YouTube «видит» устройство в этой стране. Чтобы увидеть карточку задания 0001 не из России.
#[cfg(debug_assertions)]
fn fake_geo() -> Option<(String, String)> {
    let value = std::env::var("MELOGOLD_FAKE_GEO").ok()?;
    let (id, country) = value.split_once(':')?;
    Some((id.to_owned(), country.to_owned()))
}

/// `expire` из адреса минус 5 минут; нет параметра — через 5 часов.
pub fn expires_at(url: &str, now: i64) -> i64 {
    let expire =
        url::Url::parse(url).ok().and_then(|u| u.query_pairs().find(|(k, _)| k == "expire").and_then(|(_, v)| v.parse::<i64>().ok()));
    match expire {
        Some(seconds) => (seconds * 1000 - 5 * 60_000).max(now + 60_000),
        None => now + 5 * 3_600_000,
    }
}

const GEO_PHRASES: &[&str] = &["available in your country", "not made this video available in your country", "blocked it in your country"];
const AGE_PHRASES: &[&str] = &["confirm your age", "age-restricted", "inappropriate for some users"];
const GONE_PHRASES: &[&str] =
    &["private video", "has been removed", "account associated with this video has been terminated", "no longer available"];

/// Итог диагноза (задание 0001 §2 п. 3), по порядку: страна вне списка открытых — закрыт в стране
/// (страна и число); фразы о стране — закрыт в стране; о возрасте — возраст; об удалении — удалён.
/// `None` — диагноз ничего не добавил, остаётся прежняя ошибка.
pub fn diagnose(playability: Option<&Playability>, failure: &StreamError) -> Option<StreamError> {
    let reason = playability.and_then(|p| p.reason.clone()).unwrap_or_default();
    let (reason_lower, message_lower) = (reason.to_lowercase(), failure.message.to_lowercase());
    let says = |phrases: &[&str]| phrases.iter().any(|p| message_lower.contains(p) || reason_lower.contains(p));
    let explained = if reason.is_empty() { failure.message.clone() } else { reason.clone() };
    let open = playability.map(|p| p.available_countries.len()).filter(|n| *n > 0);
    let country = playability.and_then(|p| p.country.clone());
    if playability.is_some_and(Playability::is_blocked_here) {
        return Some(StreamError {
            kind: StreamErrorKind::Geo,
            message: format!("Closed in {}, open in {} countries", country.as_deref().unwrap_or("?"), open.unwrap_or(0)),
            country,
            open_countries: open,
        });
    }
    if says(GEO_PHRASES) {
        return Some(StreamError {
            kind: StreamErrorKind::Geo,
            message: format!("Closed in the country: {explained}"),
            country,
            open_countries: open,
        });
    }
    if says(AGE_PHRASES) {
        return Some(StreamError::new(StreamErrorKind::Age, format!("Age check: {explained}")));
    }
    if says(GONE_PHRASES) {
        return Some(StreamError::new(StreamErrorKind::Unavailable, format!("Removed or private: {explained}")));
    }
    None
}

/// Класс ошибки по тексту YouTube (таблица REWRITE §4.10.3).
pub fn classify(message: &str) -> StreamErrorKind {
    let text = message.to_lowercase();
    let has = |s: &str| text.contains(s);
    if has("not a bot") || has("confirm you’re not") || has("confirm you're not") || has("sign in to confirm") {
        StreamErrorKind::BotCheck
    } else if has("country") || has("region") || has("стране") || has("регион") {
        StreamErrorKind::Geo
    } else if (has("age") && (has("confirm") || has("restricted"))) || has("возраст") {
        StreamErrorKind::Age
    } else if has("unavailable")
        || has("removed")
        || has("private")
        || has("недоступно")
        || text.starts_with("error")
        || text.starts_with("unplayable")
    {
        StreamErrorKind::Unavailable
    } else if has("login_required") || has("sign in") {
        StreamErrorKind::BotCheck
    } else {
        StreamErrorKind::Extractor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(country: Option<&str>, open: usize, with_russia: bool, reason: Option<&str>) -> Playability {
        let mut countries: Vec<String> = (0..open).map(|i| format!("C{i}")).collect();
        if with_russia {
            countries.push("RU".into());
        }
        Playability {
            status: Some("UNPLAYABLE".into()),
            reason: reason.map(str::to_owned),
            country: country.map(str::to_owned),
            available_countries: countries,
        }
    }

    #[test]
    fn closed_in_russia_gives_country_and_count() {
        let failure = StreamError::new(StreamErrorKind::Unavailable, "VISIONOS: UNPLAYABLE Video unavailable");
        let result = diagnose(Some(&answer(Some("RU"), 122, false, Some("Video unavailable"))), &failure).unwrap();
        assert_eq!(result.kind, StreamErrorKind::Geo);
        assert_eq!(result.country.as_deref(), Some("RU"));
        assert_eq!(result.open_countries, Some(122));
    }

    #[test]
    fn open_here_keeps_the_old_error() {
        let failure = StreamError::new(StreamErrorKind::Extractor, "VISIONOS: no audio");
        let mut open = answer(Some("RU"), 122, true, None);
        open.status = Some("OK".into());
        assert_eq!(diagnose(Some(&open), &failure), None);
        assert_eq!(diagnose(None, &failure), None);
    }

    #[test]
    fn phrases_about_country_age_and_removal() {
        let cases = [
            ("The uploader has not made this video available in your country", StreamErrorKind::Geo),
            ("Video blocked it in your country on copyright grounds", StreamErrorKind::Geo),
            ("Sign in to confirm your age", StreamErrorKind::Age),
            ("This video may be inappropriate for some users.", StreamErrorKind::Age),
            ("Private video", StreamErrorKind::Unavailable),
            ("This video has been removed by the uploader", StreamErrorKind::Unavailable),
            (
                "This video is no longer available because the YouTube account associated with this video has been terminated.",
                StreamErrorKind::Unavailable,
            ),
        ];
        let failure = StreamError::new(StreamErrorKind::Extractor, "x");
        for (reason, kind) in cases {
            let from_reason = diagnose(Some(&Playability { reason: Some(reason.into()), ..Default::default() }), &failure);
            assert_eq!(from_reason.as_ref().map(|e| e.kind), Some(kind), "{reason}");
            let from_client = diagnose(None, &StreamError::new(StreamErrorKind::Extractor, format!("IOS: UNPLAYABLE {reason}")));
            assert_eq!(from_client.map(|e| e.kind), Some(kind), "{reason}");
            if kind == StreamErrorKind::Geo {
                assert_eq!(from_reason.unwrap().open_countries, None);
            }
        }
        let geo =
            diagnose(Some(&answer(Some("RU"), 0, false, Some("The uploader has not made this video available in your country"))), &failure)
                .unwrap();
        assert_eq!((geo.kind, geo.country.as_deref(), geo.open_countries), (StreamErrorKind::Geo, Some("RU"), None));
    }

    #[test]
    fn classify_by_youtube_text() {
        assert_eq!(classify("LOGIN_REQUIRED Sign in to confirm you’re not a bot"), StreamErrorKind::BotCheck);
        assert_eq!(classify("UNPLAYABLE Video unavailable"), StreamErrorKind::Unavailable);
        assert_eq!(classify("ERROR"), StreamErrorKind::Unavailable);
        assert_eq!(classify("UNPLAYABLE This video is not available in your country"), StreamErrorKind::Geo);
        assert_eq!(classify("something else"), StreamErrorKind::Extractor);
    }

    #[test]
    fn expiry_minus_five_minutes() {
        let now = 1_000_000;
        assert_eq!(expires_at("https://rr1.googlevideo.com/videoplayback?expire=2000&itag=140", now), 2_000_000 - 300_000);
        assert_eq!(expires_at("https://x/videoplayback?itag=140", now), now + 5 * 3_600_000);
        assert_eq!(expires_at("https://x/videoplayback?expire=1", now), now + 60_000);
    }
}
