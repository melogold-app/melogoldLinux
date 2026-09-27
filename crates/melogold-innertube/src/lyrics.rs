//! Поставщики текстов и их цепочка (docs/PROMPT.md §4 «Тексты», Android `LyricsFetcher.kt`, Windows
//! `LyricsProviders.cs`, `LyricsFetcher.cs`). У песен: timed-текст YouTube Music → LrcLib → KuGou; у
//! видео LrcLib первым (видео может идти не в такт песне). Название сначала проходит TitleCleaner:
//! названия YouTube как есть ничего не находят. У найденного текста хранится ссылка у поставщика
//! (задание 0006): по ней закреплённый текст достаётся на других устройствах.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use melogold_core::lyrics::sync_rules::{sources, LyricsPayload, StoredLyrics};
use melogold_core::music::Track;
use melogold_core::title_cleaner;
use serde::Deserialize;

use crate::music::YouTubeMusic;

// ── LrcLib ──

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LrcLibTrack {
    pub id: i64,
    pub track_name: String,
    pub artist_name: String,
    pub album_name: Option<String>,
    pub duration: f64,
    pub plain_lyrics: Option<String>,
    pub synced_lyrics: Option<String>,
}

/// Ошибка поставщика: сеть — такой пустой итог не кэшируется как «текста нет».
#[derive(Clone, Debug)]
pub enum ProviderError {
    Network(String),
    Other(String),
}

fn network(error: reqwest::Error) -> ProviderError {
    if error.is_decode() {
        ProviderError::Other(error.to_string())
    } else {
        ProviderError::Network(error.to_string())
    }
}

/// LrcLib (Android `providers/lrclib`): поиск по названию и исполнителю без альбома — YouTube Music
/// называет альбомы по-своему, и одно слово мимо ничего не находит.
#[derive(Clone)]
pub struct LrcLib {
    http: reqwest::Client,
}

pub const LRCLIB_URL: &str = "https://lrclib.net";

impl LrcLib {
    pub fn new(user_agent: &str) -> LrcLib {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(value) = reqwest::header::HeaderValue::from_str(user_agent) {
            headers.insert("Lrclib-Client", value);
        }
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("HTTP-клиент LrcLib");
        LrcLib { http }
    }

    pub async fn search(&self, artist: &str, title: &str) -> Result<Vec<LrcLibTrack>, ProviderError> {
        let response = self
            .http
            .get(format!("{LRCLIB_URL}/api/search"))
            .query(&[("track_name", title), ("artist_name", artist)])
            .send()
            .await
            .map_err(network)?;
        json(response).await
    }

    /// Треки по запросу, у которых есть хоть какой-то текст («Найти другой текст»).
    pub async fn search_query(&self, query: &str) -> Result<Vec<LrcLibTrack>, ProviderError> {
        let response = self.http.get(format!("{LRCLIB_URL}/api/search")).query(&[("q", query)]).send().await.map_err(network)?;
        let tracks: Vec<LrcLibTrack> = json(response).await?;
        Ok(tracks.into_iter().filter(|t| has_text(&t.synced_lyrics) || has_text(&t.plain_lyrics)).collect())
    }

    /// Запись по номеру (закреплённый текст): `None` — такой нет.
    pub async fn get(&self, id: i64) -> Result<Option<LrcLibTrack>, ProviderError> {
        let response = self.http.get(format!("{LRCLIB_URL}/api/get/{id}")).send().await.map_err(network)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        json(response).await.map(Some)
    }

    /// Текст версии трека, ближайшей по длительности; синхронный — только в пределах max(3 с, 10 %).
    /// Вместе с номером записи — ссылкой у поставщика.
    pub async fn best(&self, artist: &str, title: &str, duration_ms: i64, synced: bool) -> Result<Option<(String, String)>, ProviderError> {
        let tracks: Vec<LrcLibTrack> = self
            .search(artist, title)
            .await?
            .into_iter()
            .filter(|t| if synced { has_text(&t.synced_lyrics) } else { has_text(&t.plain_lyrics) })
            .collect();
        Ok(best_matching(&tracks, title, duration_ms).and_then(|t| {
            let text = if synced { t.synced_lyrics.clone() } else { t.plain_lyrics.clone() }?;
            Some((text, t.id.to_string()))
        }))
    }
}

async fn json<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T, ProviderError> {
    let text = response.error_for_status().map_err(network)?.text().await.map_err(network)?;
    serde_json::from_str(&text).map_err(|e| ProviderError::Other(e.to_string()))
}

fn has_text(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|v| !v.trim().is_empty())
}

/// Версия с ближайшей длительностью, если она близко (3 с или 10 %): синхронный текст записи другой
/// длины (живой, сокращённой) уезжает. Без длительности — с ближайшим по длине названием.
pub fn best_matching<'a>(tracks: &'a [LrcLibTrack], title: &str, duration_ms: i64) -> Option<&'a LrcLibTrack> {
    let seconds = (duration_ms / 1000) as f64;
    if seconds <= 0.0 {
        let length = title.chars().count() as i64;
        return tracks.iter().min_by_key(|t| (t.track_name.chars().count() as i64 - length).abs());
    }
    let closest = tracks.iter().min_by(|a, b| (a.duration - seconds).abs().total_cmp(&(b.duration - seconds).abs()))?;
    ((closest.duration - seconds).abs() <= (seconds * 0.1).max(3.0)).then_some(closest)
}

// ── KuGou ──

#[derive(Deserialize, Default)]
#[serde(default)]
struct SongInfo {
    duration: i64,
    hash: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SongData {
    info: Vec<SongInfo>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SongResponse {
    data: SongData,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct Candidate {
    id: serde_json::Value,
    accesskey: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CandidatesResponse {
    candidates: Vec<Candidate>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DownloadResponse {
    content: String,
}

/// KuGou (Android `providers/kugou`): последняя линия синхронного текста.
#[derive(Clone)]
pub struct KuGou {
    http: reqwest::Client,
}

impl Default for KuGou {
    fn default() -> Self {
        KuGou::new()
    }
}

impl KuGou {
    pub fn new() -> KuGou {
        let http = reqwest::Client::builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("HTTP-клиент KuGou");
        KuGou { http }
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str, query: &[(&str, &str)]) -> Result<T, ProviderError> {
        // KuGou отвечает JSON-ом с типом text/plain или text/html.
        let text = self.http.get(url).query(query).send().await.map_err(network)?.text().await.map_err(network)?;
        serde_json::from_str(&text).map_err(|e| ProviderError::Other(e.to_string()))
    }

    /// LRC трека и ссылка `id:accesskey`: сначала песня с совпадающей длительностью (допуск до 5 с), потом поиск текста по словам.
    pub async fn lyrics(&self, artist: &str, title: &str, duration_seconds: i64) -> Result<Option<(String, String)>, ProviderError> {
        let keyword = keyword(artist, title);
        let songs: SongResponse = self
            .get(
                "https://mobileservice.kugou.com/api/v3/search/song",
                &[("version", "9108"), ("plat", "0"), ("pagesize", "8"), ("showtype", "0"), ("keyword", &keyword)],
            )
            .await?;
        let infos = songs.data.info;
        for tolerance in 0..=5 {
            for info in infos.iter().filter(|i| (i.duration - duration_seconds).abs() <= tolerance) {
                let by_hash: CandidatesResponse = self
                    .get("https://krcs.kugou.com/search", &[("ver", "1"), ("man", "yes"), ("client", "mobi"), ("hash", &info.hash)])
                    .await?;
                if let Some(candidate) = by_hash.candidates.first() {
                    return self.download(candidate).await;
                }
            }
            if infos.is_empty() {
                break;
            }
        }
        let by_keyword: CandidatesResponse =
            self.get("https://krcs.kugou.com/search", &[("ver", "1"), ("man", "yes"), ("client", "mobi"), ("keyword", &keyword)]).await?;
        match by_keyword.candidates.first() {
            Some(candidate) => self.download(candidate).await,
            None => Ok(None),
        }
    }

    /// Текст по ссылке `id:accesskey` (закреплённый).
    pub async fn by_ref(&self, reference: &str) -> Result<Option<String>, ProviderError> {
        let Some((id, key)) = reference.split_once(':') else { return Ok(None) };
        let candidate = Candidate { id: serde_json::Value::String(id.to_owned()), accesskey: key.to_owned() };
        Ok(self.download(&candidate).await?.map(|(text, _)| text))
    }

    async fn download(&self, candidate: &Candidate) -> Result<Option<(String, String)>, ProviderError> {
        let id = match &candidate.id {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let response: DownloadResponse = self
            .get(
                "https://krcs.kugou.com/download",
                &[("ver", "1"), ("man", "yes"), ("client", "pc"), ("fmt", "lrc"), ("id", &id), ("accesskey", &candidate.accesskey)],
            )
            .await?;
        let bytes =
            base64::engine::general_purpose::STANDARD.decode(response.content.trim()).map_err(|e| ProviderError::Other(e.to_string()))?;
        let text = normalize_kugou(&String::from_utf8_lossy(&bytes));
        Ok((!text.trim().is_empty()).then(|| (text, format!("{id}:{}", candidate.accesskey))))
    }
}

fn keyword(artist: &str, title: &str) -> String {
    let (title, featuring) = match title.find(" (feat. ") {
        Some(from) => match title[from..].find(')') {
            Some(to) => (format!("{}{}", &title[..from], &title[from + to + 1..]), title[from + 8..from + to].to_owned()),
            None => (title.to_owned(), String::new()),
        },
        None => (title.to_owned(), String::new()),
    };
    let artist = if featuring.is_empty() { artist.to_owned() } else { format!("{artist}, {featuring}") };
    format!("{} - {title}", artist.replace(", ", "、").replace(" & ", "、").replace('.', ""))
}

/// Без служебных тегов и титров в начале (авторы, композитор), как у Android.
pub fn normalize_kugou(value: &str) -> String {
    const META: [&str; 10] = ["[ti:", "[ar:", "[al:", "[by:", "[hash:", "[sign:", "[qq:", "[total:", "[offset:", "[id:"];
    const CREDITS: [&str; 6] = ["]Written by：", "]Lyrics by：", "]Composed by：", "]Producer：", "]作曲 : ", "]作词 : "];
    let text = value.replace("\r\n", "\n");
    let text = text.trim();
    let (mut to_drop, mut maybe_to_drop) = (0usize, 0usize);
    for line in text.split('\n') {
        let credit = CREDITS.iter().any(|m| line.char_indices().nth(9).is_some_and(|(i, _)| line[i..].starts_with(m)));
        if META.iter().any(|p| line.starts_with(p)) || credit {
            to_drop += line.len() + 1 + maybe_to_drop;
            maybe_to_drop = 0;
        } else if maybe_to_drop == 0 {
            maybe_to_drop = line.len() + 1;
        } else {
            maybe_to_drop = 0;
            break;
        }
    }
    let drop = (to_drop + maybe_to_drop).min(text.len());
    let drop = (0..=drop).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
    text[drop..].replace("&apos;", "'")
}

// ── цепочка ──

/// Что нашла цепочка. `any_failure` — хоть один поставщик не ответил из-за сети (такой пустой итог
/// не кэшируется как «текста нет»); `mine` — своя версия с сервера (своя и здесь).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FetchResult {
    pub lyrics: StoredLyrics,
    pub any_failure: bool,
    pub mine: bool,
}

/// Текст с сервера Melogold: своя версия или общая другого пользователя; `None` — нет.
pub type Community =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<Option<(LyricsPayload, bool)>, ProviderError>> + Send>> + Send + Sync>;

#[derive(Clone)]
pub struct LyricsFetcher {
    pub music: YouTubeMusic,
    pub lrclib: LrcLib,
    pub kugou: KuGou,
    pub community: Option<Community>,
}

impl LyricsFetcher {
    /// Стороны, которые уже есть в `current`, заново не ищутся.
    /// `edited` — свои исполнитель и название трека (задание 0005): LrcLib спрашивается сначала по ним,
    /// потом по оригиналу — у загрузок фанатов оригинальное «Artist — Song (live, fan upload)» не находится.
    pub async fn fetch(
        &self,
        track: &Track,
        edited: Option<(String, String)>,
        duration_ms: i64,
        current: Option<&StoredLyrics>,
    ) -> FetchResult {
        let raw_artist = track.artists_text.clone().unwrap_or_default();
        let raw_title = track.title.clone();
        let is_song = track.album_id.is_some() || track.album_title.is_some();
        let clean = title_cleaner::clean(
            &raw_title,
            (!raw_artist.is_empty()).then_some(raw_artist.as_str()),
            if is_song { Some("song") } else { None },
        );
        let artist = clean.artist.clone().unwrap_or_else(|| raw_artist.clone());
        let title = if clean.title.trim().is_empty() { raw_title.clone() } else { clean.title.clone() };
        let failed = std::sync::atomic::AtomicBool::new(false);
        let note = |error: &ProviderError| {
            if let ProviderError::Network(message) = error {
                tracing::debug!(%message, "поставщик текста не ответил");
                failed.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        };

        // LrcLib: своё название, затем очищенное, а синхронный — ещё и как у трека, если очистка его изменила.
        let mut plain_queries: Vec<(String, String)> = Vec::new();
        let push = |queries: &mut Vec<(String, String)>, pair: (String, String)| {
            if !pair.1.trim().is_empty() && !queries.contains(&pair) {
                queries.push(pair);
            }
        };
        if let Some(pair) = edited {
            push(&mut plain_queries, pair);
        }
        push(&mut plain_queries, (artist.clone(), title.clone()));
        let mut synced_queries = plain_queries.clone();
        push(&mut synced_queries, (raw_artist.clone(), raw_title.clone()));

        // Вкладка «Текст» страницы трека; нет её — у YouTube Music текста нет.
        let browse_id = match self.music.next(&track.video_id, None).await {
            Ok(page) => page.lyrics_browse_id,
            Err(error) => {
                if error.kind == crate::ErrorKind::Offline {
                    failed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                None
            }
        };
        let mut lyrics = current.cloned().unwrap_or_default();

        if lyrics.plain.is_none() {
            let youtube = match &browse_id {
                Some(id) => self.music.lyrics(id).await.unwrap_or_else(|e| {
                    if e.kind == crate::ErrorKind::Offline {
                        failed.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    None
                }),
                None => None,
            };
            if let (Some((text, _)), Some(id)) = (youtube, &browse_id) {
                lyrics.plain = Some(text);
                lyrics.plain_source = Some(sources::YOUTUBE_MUSIC.into());
                lyrics.plain_ref = Some(id.clone());
            } else if let Some((text, id)) = self.lrclib_best(&plain_queries, duration_ms, false, &note).await {
                lyrics.plain = Some(text);
                lyrics.plain_source = Some(sources::LRCLIB.into());
                lyrics.plain_ref = Some(id);
            }
        }

        if lyrics.synced.is_none() {
            let mut found: Option<(String, &'static str, String)> = None;
            for step in if is_song { ["ytm", "lrclib"] } else { ["lrclib", "ytm"] } {
                if found.is_some() {
                    break;
                }
                match step {
                    "ytm" => {
                        if let Some(id) = &browse_id {
                            match self.music.timed_lyrics(id).await {
                                Ok(Some(text)) => found = Some((text, sources::YOUTUBE_MUSIC, id.clone())),
                                Ok(None) => {}
                                Err(error) => {
                                    if error.kind == crate::ErrorKind::Offline {
                                        failed.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        if let Some((text, id)) = self.lrclib_best(&synced_queries, duration_ms, true, &note).await {
                            found = Some((text, sources::LRCLIB, id));
                        }
                    }
                }
            }
            if found.is_none() {
                match self.kugou.lyrics(&artist, &title, duration_ms / 1000).await {
                    Ok(Some((text, reference))) => found = Some((text, sources::KUGOU, reference)),
                    Ok(None) => {}
                    Err(error) => note(&error),
                }
            }
            if let Some((text, source, reference)) = found {
                lyrics.synced = Some(text);
                lyrics.synced_source = Some(source.into());
                lyrics.synced_ref = Some(reference);
            }
        }

        let mut mine = false;
        if lyrics.synced.is_none() {
            if let Some(community) = &self.community {
                match community(track.video_id.clone()).await {
                    Ok(Some((payload, own))) if payload.synced.is_some() => {
                        // Своя версия остаётся своей, общая помечается «сообщество Melogold» и своей не становится.
                        lyrics.synced = payload.synced.clone();
                        lyrics.synced_source = if own { payload.synced_source.clone() } else { Some(sources::MELOGOLD.into()) };
                        lyrics.synced_ref = None;
                        if lyrics.plain.is_none() {
                            if let Some(plain) = payload.plain.clone() {
                                lyrics.plain = Some(plain);
                                lyrics.plain_source = if own { payload.plain_source.clone() } else { Some(sources::MELOGOLD.into()) };
                            }
                        }
                        lyrics.offset_ms = -payload.start_time_ms.unwrap_or(0);
                        lyrics.language = payload.language.clone();
                        lyrics.chosen = own;
                        mine = own;
                    }
                    Ok(_) => {}
                    Err(error) => note(&error),
                }
            }
        }
        FetchResult { lyrics, any_failure: failed.load(std::sync::atomic::Ordering::Relaxed), mine }
    }

    /// Лучший текст LrcLib по первому запросу, который что-то нашёл.
    async fn lrclib_best(
        &self,
        queries: &[(String, String)],
        duration_ms: i64,
        synced: bool,
        note: &impl Fn(&ProviderError),
    ) -> Option<(String, String)> {
        for (artist, title) in queries {
            match self.lrclib.best(artist, title, duration_ms, synced).await {
                Ok(Some(found)) => return Some(found),
                Ok(None) => {}
                Err(error) => note(&error),
            }
        }
        None
    }

    /// Закреплённый текст по ссылке у поставщика (задание 0006); `None` — поставщик молчит или текста там нет.
    pub async fn pinned(&self, source: &str, reference: &str) -> Result<Option<StoredLyrics>, ProviderError> {
        let lyrics = match source {
            sources::LRCLIB => {
                let Ok(id) = reference.parse::<i64>() else { return Ok(None) };
                self.lrclib.get(id).await?.map(|record| StoredLyrics {
                    synced: record.synced_lyrics.filter(|t| !t.trim().is_empty()),
                    plain: record.plain_lyrics.filter(|t| !t.trim().is_empty()),
                    synced_source: Some(sources::LRCLIB.into()),
                    plain_source: Some(sources::LRCLIB.into()),
                    synced_ref: Some(reference.to_owned()),
                    plain_ref: Some(reference.to_owned()),
                    ..Default::default()
                })
            }
            sources::YOUTUBE_MUSIC => {
                let offline = |e: crate::YouTubeError| {
                    if e.kind == crate::ErrorKind::Offline {
                        ProviderError::Network(e.message)
                    } else {
                        ProviderError::Other(e.message)
                    }
                };
                let synced = self.music.timed_lyrics(reference).await.map_err(offline)?;
                let plain = self.music.lyrics(reference).await.map_err(offline)?.map(|(t, _)| t);
                (synced.is_some() || plain.is_some()).then(|| StoredLyrics {
                    synced,
                    plain,
                    synced_source: Some(sources::YOUTUBE_MUSIC.into()),
                    plain_source: Some(sources::YOUTUBE_MUSIC.into()),
                    synced_ref: Some(reference.to_owned()),
                    plain_ref: Some(reference.to_owned()),
                    ..Default::default()
                })
            }
            sources::KUGOU => self.kugou.by_ref(reference).await?.map(|text| StoredLyrics {
                synced: Some(text),
                synced_source: Some(sources::KUGOU.into()),
                synced_ref: Some(reference.to_owned()),
                ..Default::default()
            }),
            _ => None,
        };
        Ok(lyrics.filter(|l| l.synced.is_some() || l.plain.is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(duration: f64, name: &str) -> LrcLibTrack {
        LrcLibTrack {
            id: duration as i64,
            track_name: name.into(),
            duration,
            synced_lyrics: Some("[00:01.00]x".into()),
            ..Default::default()
        }
    }

    #[test]
    fn closest_duration_within_tolerance() {
        let tracks = [track(200.0, "Song"), track(240.0, "Song (Live)")];
        assert_eq!(best_matching(&tracks, "Song", 203_000).map(|t| t.id), Some(200));
        assert_eq!(best_matching(&tracks, "Song", 300_000), None, "живая запись другой длины не подходит");
        assert_eq!(best_matching(&tracks, "Song", 0).map(|t| t.id), Some(200));
    }

    #[test]
    fn kugou_keyword_and_credits() {
        assert_eq!(keyword("A & B", "Song (feat. C)"), "A、B、C - Song");
        let raw = "[ti:Song]\r\n[ar:Artist]\r\n[00:00.00]Artist - Song\r\n[00:01.00]作词 : Someone\r\n[00:05.00]First line\r\n[00:07.00]Isn&apos;t it";
        assert_eq!(normalize_kugou(raw), "[00:05.00]First line\n[00:07.00]Isn't it");
    }

    #[tokio::test]
    async fn live_chain_finds_synced_lyrics() {
        if std::env::var("MELOGOLD_LIVE").as_deref() != Ok("1") {
            return;
        }
        let music = YouTubeMusic::new(crate::InnerTube::new("en", "US"));
        let fetcher =
            LyricsFetcher { music, lrclib: LrcLib::new(&melogold_core::app_info::tool_user_agent()), kugou: KuGou::new(), community: None };
        let track = Track {
            video_id: "fJ9rUzIMcZQ".into(),
            title: "Queen – Bohemian Rhapsody (Official Video Remastered)".into(),
            artists_text: Some("Queen Official".into()),
            ..Default::default()
        };
        let result = fetcher.fetch(&track, None, 355_000, None).await;
        let synced = result.lyrics.synced.expect("синхронный текст");
        assert!(melogold_core::lyrics::parse_synced(&synced).is_some());
        let (source, reference) = (result.lyrics.synced_source.unwrap(), result.lyrics.synced_ref.unwrap());
        println!("источник {source}, ссылка {reference}");
        let pinned = fetcher.pinned(&source, &reference).await.unwrap().expect("текст по ссылке");
        assert_eq!(pinned.synced.as_deref(), Some(synced.as_str()));

        // Песня с альбомом: первым — timed-текст YouTube Music этой самой записи.
        let found = fetcher.music.search("Queen Bohemian Rhapsody", crate::music::MusicFilter::Songs).await.unwrap();
        let song = found
            .items
            .into_iter()
            .find_map(|item| match item {
                melogold_core::music::MusicItem::Track(t) if t.album_id.is_some() => Some(t),
                _ => None,
            })
            .expect("песня с альбомом");
        let result = fetcher.fetch(&song, None, song.duration_ms.unwrap_or(355_000), None).await;
        println!("песня {}: синхронный из {:?}, обычный из {:?}", song.video_id, result.lyrics.synced_source, result.lyrics.plain_source);
        assert!(result.lyrics.synced.is_some() || result.lyrics.plain.is_some());
        if result.lyrics.synced_source.as_deref() == Some(sources::YOUTUBE_MUSIC) {
            let reference = result.lyrics.synced_ref.clone().unwrap();
            assert!(reference.starts_with("MPLYt"), "{reference}");
            let pinned = fetcher.pinned(sources::YOUTUBE_MUSIC, &reference).await.unwrap().expect("текст YouTube Music по ссылке");
            assert_eq!(pinned.synced, result.lyrics.synced);
        }
    }
}
