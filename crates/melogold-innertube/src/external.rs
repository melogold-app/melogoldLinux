//! Ссылка другого сервиса → то же самое в YouTube Music (задание 0010, Android `ExternalLinkResolver`):
//!
//! 1. song.link, если в сборке задан ключ (`MELOGOLD_SONGLINK_KEY`): без ключа он отвечает
//!    `401 PUBLIC_API_ACCESS_DEPRECATED` и к нему не ходят;
//! 2. начало страницы самой ссылки (до 400 КБ, User-Agent браузера): название и исполнитель из `<title>`,
//!    `og:title`, `og:description` по правилам сервиса и поиск по ним. Плейлисты — не здесь.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use melogold_core::music::{AlbumItem, ArtistItem, MusicItem, Track};
use melogold_core::music_services::{parse, parse_song_link, search_text, MusicLinkKind, MusicServiceLink};

use crate::music::{MusicFilter, YouTubeMusic};

/// Сколько читать от страницы: заголовок — в самом начале.
const MAX_PAGE_BYTES: usize = 400_000;
/// Сервисы отдают заголовки браузеру; настольный Spotify без скриптов — пустая страница «Web Player»,
/// мобильный получает лёгкую страницу с названием (проверено 2026-09-30, как у Android).
const BROWSER: &str =
    "Mozilla/5.0 (Linux; Android 16; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36";
const SONG_LINK: &str = "https://api.song.link/v1-alpha.1/links";
/// Около десяти запросов в минуту с одного адреса.
const SONG_LINK_PAUSE: Duration = Duration::from_millis(1500);
const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);

/// Ключ song.link из сборки; без него в song.link не ходим.
pub fn song_link_key() -> Option<&'static str> {
    option_env!("MELOGOLD_SONGLINK_KEY").filter(|key| !key.trim().is_empty())
}

/// Куда ведёт ссылка другого сервиса.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    Track(Box<Track>),
    Album(AlbumItem),
    Artist(ArtistItem),
    /// song.link назвал ссылку на YouTube: открывается как любая ссылка YouTube.
    YouTube(String),
    /// Ссылка ничего годного не говорит или сети нет.
    NotFound,
}

pub struct ExternalResolver {
    music: YouTubeMusic,
    http: reqwest::Client,
    key: Option<String>,
    last_request: tokio::sync::Mutex<Option<Instant>>,
    cache: Mutex<Vec<(String, String, Instant)>>,
}

impl ExternalResolver {
    pub fn new(music: YouTubeMusic) -> ExternalResolver {
        ExternalResolver::with_key(music, song_link_key().map(str::to_owned))
    }

    pub fn with_key(music: YouTubeMusic, key: Option<String>) -> ExternalResolver {
        let http = reqwest::Client::builder()
            .user_agent(BROWSER)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(12))
            .build()
            .expect("HTTP-клиент собирается");
        ExternalResolver { music, http, key, last_request: tokio::sync::Mutex::new(None), cache: Mutex::default() }
    }

    pub async fn resolve(&self, link: &MusicServiceLink) -> Resolution {
        if matches!(link.kind, MusicLinkKind::Track | MusicLinkKind::Album | MusicLinkKind::Unknown) && self.key.is_some() {
            if let Some(answer) = self.song_link(&link.url).await {
                if let Some(url) = answer.youtube_url {
                    return Resolution::YouTube(url);
                }
                if let Some(text) = answer.search_text() {
                    return self.search(if answer.is_album { MusicLinkKind::Album } else { MusicLinkKind::Track }, &text).await;
                }
            }
        }
        let Some((final_url, html)) = self.fetch_page(&link.url).await else { return Resolution::NotFound };
        // Короткая ссылка: настоящий вид — по адресу, куда она привела, или по `og:type`.
        let mut refined = link.clone();
        if refined.kind == MusicLinkKind::Unknown {
            refined.kind = parse(&final_url).map(|l| l.kind).filter(|k| *k != MusicLinkKind::Unknown).unwrap_or(MusicLinkKind::Track);
        }
        match search_text(&refined, &html) {
            Some(text) => self.search(refined.kind, &text).await,
            None => Resolution::NotFound,
        }
    }

    /// Поиск по словам: трек — первая песня (иначе клип), альбом, исполнитель.
    async fn search(&self, kind: MusicLinkKind, text: &str) -> Resolution {
        let first = |filter: MusicFilter| async move { self.music.search(text, filter).await.ok().map(|page| page.items) };
        match kind {
            MusicLinkKind::Album => {
                let items = first(MusicFilter::Albums).await.unwrap_or_default();
                items
                    .into_iter()
                    .find_map(|i| if let MusicItem::Album(a) = i { Some(Resolution::Album(a)) } else { None })
                    .unwrap_or(Resolution::NotFound)
            }
            MusicLinkKind::Artist => {
                let items = first(MusicFilter::Artists).await.unwrap_or_default();
                items
                    .into_iter()
                    .find_map(|i| if let MusicItem::Artist(a) = i { Some(Resolution::Artist(a)) } else { None })
                    .unwrap_or(Resolution::NotFound)
            }
            _ => {
                for filter in [MusicFilter::Songs, MusicFilter::Videos] {
                    let items = first(filter).await.unwrap_or_default();
                    if let Some(track) = items.into_iter().find_map(|i| if let MusicItem::Track(t) = i { Some(t) } else { None }) {
                        return Resolution::Track(Box::new(track));
                    }
                }
                Resolution::NotFound
            }
        }
    }

    /// Начало страницы и адрес, на котором она оказалась после переходов; ошибка — ничто.
    async fn fetch_page(&self, url: &str) -> Option<(String, String)> {
        let mut response = self.http.get(url).header(reqwest::header::ACCEPT_LANGUAGE, "en").send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let final_url = response.url().to_string();
        let mut bytes = Vec::new();
        while bytes.len() < MAX_PAGE_BYTES {
            match response.chunk().await {
                Ok(Some(chunk)) => bytes.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(_) => return None,
            }
        }
        bytes.truncate(MAX_PAGE_BYTES);
        Some((final_url, String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Запросы к song.link — по одному, с паузой; ответы сутки лежат в памяти.
    async fn song_link(&self, url: &str) -> Option<melogold_core::music_services::SongLinkAnswer> {
        let key = self.key.as_deref()?;
        if let Some(body) = self.cached(url) {
            return parse_song_link(&body);
        }
        let mut last = self.last_request.lock().await;
        if let Some(at) = *last {
            let wait = SONG_LINK_PAUSE.saturating_sub(at.elapsed());
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
        }
        *last = Some(Instant::now());
        let response = self.http.get(SONG_LINK).query(&[("url", url), ("key", key), ("songIfSingle", "true")]).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let body = response.text().await.ok()?;
        let answer = parse_song_link(&body)?;
        self.cache.lock().unwrap_or_else(|p| p.into_inner()).push((url.to_owned(), body, Instant::now()));
        Some(answer)
    }

    fn cached(&self, url: &str) -> Option<String> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.retain(|(_, _, at)| at.elapsed() < CACHE_TTL);
        cache.iter().find(|(key, ..)| key == url).map(|(_, body, _)| body.clone())
    }
}
