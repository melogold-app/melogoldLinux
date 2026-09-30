//! Ссылки других музыкальных сервисов (задание 0010, Android `providers/songlink`): Spotify, Apple Music,
//! Яндекс Музыка, Deezer, Tidal, SoundCloud. Здесь только разбор — без сети:
//!
//! * [`parse`] — что за сервис и на что ссылка;
//! * [`search_text`] — слова для поиска в YouTube Music из начала страницы ссылки (`<title>`, `og:title`,
//!   `og:description`; у каждого сервиса свои правила). song.link без ключа закрыт
//!   (`401 PUBLIC_API_ACCESS_DEPRECATED`, 2026-09), поэтому основной путь — заголовок страницы;
//! * [`parse_song_link`] — ответ song.link, когда в сборке задан ключ.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicService {
    Spotify,
    AppleMusic,
    YandexMusic,
    Deezer,
    Tidal,
    SoundCloud,
}

impl MusicService {
    pub fn name(self) -> &'static str {
        match self {
            MusicService::Spotify => "Spotify",
            MusicService::AppleMusic => "Apple Music",
            MusicService::YandexMusic => "Yandex Music",
            MusicService::Deezer => "Deezer",
            MusicService::Tidal => "Tidal",
            MusicService::SoundCloud => "SoundCloud",
        }
    }
}

/// На что ссылка; `Unknown` — короткая ссылка, которую надо пройти, чтобы узнать.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicLinkKind {
    Track,
    Album,
    Artist,
    Playlist,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MusicServiceLink {
    pub service: MusicService,
    pub kind: MusicLinkKind,
    pub url: String,
}

/// Хост одного из сервисов (даже если путь ссылки нам не знаком).
pub fn is_service_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    const EXACT: &[&str] = &[
        "open.spotify.com",
        "play.spotify.com",
        "spotify.link",
        "spotify.app.link",
        "music.apple.com",
        "itunes.apple.com",
        "geo.music.apple.com",
        "deezer.com",
        "link.deezer.com",
        "deezer.page.link",
        "dzr.page.link",
        "tidal.com",
        "listen.tidal.com",
        "link.tidal.com",
        "soundcloud.com",
        "m.soundcloud.com",
        "on.soundcloud.com",
    ];
    EXACT.contains(&host) || host.starts_with("music.yandex.")
}

const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '»', '"', '\'', '…'];

/// Ссылка сервиса в тексте (первая `http(s)://…`) и что она такое.
pub fn parse(input: &str) -> Option<MusicServiceLink> {
    let lower = input.to_ascii_lowercase();
    let start = ["https://", "http://"].iter().filter_map(|p| lower.find(p)).min()?;
    let rest = &input[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let link = rest[..end].trim_end_matches(TRAILING);
    let url = url::Url::parse(link).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_owned();
    let segments: Vec<String> = url.path_segments().map(|s| s.filter(|p| !p.is_empty()).map(str::to_owned).collect()).unwrap_or_default();
    let has_query = |name: &str| url.query_pairs().any(|(k, _)| k == name);
    let segment = |i: usize| segments.get(i).map(String::as_str);

    let (service, kind) = if matches!(host.as_str(), "open.spotify.com" | "play.spotify.com" | "spotify.link" | "spotify.app.link") {
        (MusicService::Spotify, spotify(&host, &segments)?)
    } else if matches!(host.as_str(), "music.apple.com" | "itunes.apple.com" | "geo.music.apple.com") {
        (MusicService::AppleMusic, apple(&segments, has_query("i")))
    } else if host.starts_with("music.yandex.") {
        let kind = if segments.iter().any(|s| s == "playlists") {
            MusicLinkKind::Playlist
        } else if segment(0) == Some("album") && segment(2) == Some("track") || segment(0) == Some("track") {
            MusicLinkKind::Track
        } else if segment(0) == Some("album") {
            MusicLinkKind::Album
        } else if segment(0) == Some("artist") {
            MusicLinkKind::Artist
        } else {
            MusicLinkKind::Unknown
        };
        (MusicService::YandexMusic, kind)
    } else if matches!(host.as_str(), "deezer.com" | "link.deezer.com" | "deezer.page.link" | "dzr.page.link") {
        let kind =
            if host != "deezer.com" { MusicLinkKind::Unknown } else { first_of(&segments, &["track", "album", "artist", "playlist"]) };
        (MusicService::Deezer, kind)
    } else if matches!(host.as_str(), "tidal.com" | "listen.tidal.com" | "link.tidal.com") {
        let kind = if host == "link.tidal.com" {
            MusicLinkKind::Unknown
        } else {
            first_of(&segments, &["track", "album", "artist", "playlist", "mix"])
        };
        (MusicService::Tidal, kind)
    } else if matches!(host.as_str(), "soundcloud.com" | "m.soundcloud.com" | "on.soundcloud.com") {
        let kind = if host == "on.soundcloud.com" {
            MusicLinkKind::Unknown
        } else if segments.len() >= 3 && segments[1] == "sets" {
            MusicLinkKind::Playlist
        } else if segments.len() == 2 && segments[1] != "sets" {
            MusicLinkKind::Track
        } else if segments.len() == 1 {
            MusicLinkKind::Artist
        } else {
            MusicLinkKind::Unknown
        };
        (MusicService::SoundCloud, kind)
    } else {
        return None;
    };
    Some(MusicServiceLink { service, kind, url: canonical_url(service, &url, link) })
}

/// Адрес для запроса страницы без следов отслеживания: ссылки из «Поделиться» Spotify несут `?si=…`, а
/// страница с ним уходит в цепочку переходов. У Apple Music остаётся `i` (трек внутри альбома).
fn canonical_url(service: MusicService, url: &url::Url, original: &str) -> String {
    let mut clean = url.clone();
    clean.set_fragment(None);
    if service == MusicService::AppleMusic {
        let keep: Vec<(String, String)> =
            url.query_pairs().filter(|(k, _)| k == "i").map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        if keep.is_empty() {
            clean.set_query(None);
        } else {
            clean.query_pairs_mut().clear().extend_pairs(keep);
        }
    } else if matches!(service, MusicService::Spotify | MusicService::Deezer | MusicService::Tidal | MusicService::SoundCloud) {
        clean.set_query(None);
    } else {
        return original.to_owned();
    }
    clean.to_string()
}

fn kind_of(word: &str) -> MusicLinkKind {
    match word {
        "track" | "song" => MusicLinkKind::Track,
        "album" => MusicLinkKind::Album,
        "artist" => MusicLinkKind::Artist,
        "playlist" | "mix" => MusicLinkKind::Playlist,
        _ => MusicLinkKind::Unknown,
    }
}

fn first_of(segments: &[String], words: &[&str]) -> MusicLinkKind {
    segments.iter().find(|s| words.contains(&s.as_str())).map(|s| kind_of(s)).unwrap_or(MusicLinkKind::Unknown)
}

/// `open.spotify.com/intl-de/track/<id>`: впереди может стоять язык.
fn spotify(host: &str, segments: &[String]) -> Option<MusicLinkKind> {
    if host != "open.spotify.com" && host != "play.spotify.com" {
        return Some(MusicLinkKind::Unknown);
    }
    let path = if segments.first().is_some_and(|s| s.starts_with("intl-")) { &segments[1..] } else { segments };
    match path.first().map(String::as_str) {
        Some("track") => Some(MusicLinkKind::Track),
        Some("album") => Some(MusicLinkKind::Album),
        Some("artist") => Some(MusicLinkKind::Artist),
        Some("playlist") => Some(MusicLinkKind::Playlist),
        _ => None,
    }
}

/// `music.apple.com/<страна>/album/<имя>/<id>?i=<трек>` — трек альбома.
fn apple(segments: &[String], has_track_query: bool) -> MusicLinkKind {
    let Some(word) = segments.iter().find(|s| ["song", "album", "playlist", "artist", "music-video"].contains(&s.as_str())) else {
        return MusicLinkKind::Unknown;
    };
    match word.as_str() {
        "song" => MusicLinkKind::Track,
        "album" if has_track_query => MusicLinkKind::Track,
        "album" => MusicLinkKind::Album,
        "playlist" => MusicLinkKind::Playlist,
        "artist" => MusicLinkKind::Artist,
        _ => MusicLinkKind::Unknown,
    }
}

// ── слова для поиска из начала страницы ──

const MAX_QUERY: usize = 120;

/// Слова для поиска: «Never Gonna Give You Up Rick Astley». У каждого сервиса название и исполнитель лежат
/// по-своему; страница, которая ничего годного не говорит (SoundCloud), даёт `None`.
pub fn search_text(link: &MusicServiceLink, html: &str) -> Option<String> {
    let title = tag_text(html, "title").map(|t| clean(&t));
    let og = meta(html, "og:title");
    let description = meta(html, "og:description");
    let (title, og, description) = (title.as_deref(), og.as_deref(), description.as_deref());
    let query = match link.service {
        MusicService::Spotify => spotify_words(link.kind, title, og, description),
        MusicService::AppleMusic => apple_words(link.kind, title, og),
        MusicService::YandexMusic => yandex_words(link.kind, title, og, description),
        MusicService::Tidal => tidal_words(link.kind, title, og),
        MusicService::Deezer => deezer_words(title, og),
        MusicService::SoundCloud => soundcloud_words(title),
    };
    query.map(|q| tidy(&q)).filter(|q| !q.is_empty())
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("правильное выражение"))
}

fn pair(caps: &regex::Captures) -> String {
    format!("{} {}", &caps[1], &caps[2])
}

/// «Never Gonna Give You Up - song and lyrics by Rick Astley | Spotify», «Rick Astley | Spotify».
fn spotify_words(kind: MusicLinkKind, title: Option<&str>, og: Option<&str>, description: Option<&str>) -> Option<String> {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    let title_re = re(&TITLE, r"(?i)^(.+?) - (?:song and lyrics|song|album|single|EP)(?: and lyrics)? by (.+?) \| Spotify$");
    if let Some(caps) = title.and_then(|t| title_re.captures(t)) {
        return Some(pair(&caps));
    }
    if kind == MusicLinkKind::Artist {
        return og.map(str::to_owned).or_else(|| title.and_then(|t| t.strip_suffix(" | Spotify")).map(str::to_owned));
    }
    // Страница трека: og:title — название, описание начинается с исполнителя («Rick Astley · Album · Song · 1987»).
    let artist = description.filter(|d| d.contains(" · ")).and_then(|d| d.split(" · ").next());
    let name = og.or_else(|| title.and_then(|t| t.strip_suffix(" | Spotify")))?;
    Some([Some(name), artist].into_iter().flatten().collect::<Vec<_>>().join(" "))
}

/// «Never Gonna Give You Up - Song with Lyrics by Rick Astley - Apple Music», «Rick Astley - Apple Music».
fn apple_words(kind: MusicLinkKind, title: Option<&str>, og: Option<&str>) -> Option<String> {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static OG: OnceLock<Regex> = OnceLock::new();
    let title_re = re(&TITLE, r"(?i)^(.+?) - (?:Song|Album|Single|EP)(?: with Lyrics)? by (.+?) - Apple Music$");
    let og_re = re(&OG, r"(?i)^(.+) by (.+?) on Apple Music$");
    if let Some(caps) = title.and_then(|t| title_re.captures(t)) {
        return Some(pair(&caps));
    }
    if let Some(caps) = og.and_then(|t| og_re.captures(t)) {
        return Some(pair(&caps));
    }
    if kind == MusicLinkKind::Artist {
        return og
            .and_then(|t| t.strip_suffix(" on Apple Music"))
            .or_else(|| title.and_then(|t| t.strip_suffix(" - Apple Music")))
            .map(str::to_owned);
    }
    None
}

/// «Never Gonna Give You Up Rick Astley слушать онлайн на Яндекс Музыке», название и «Rick Astley • Трек • 2019».
fn yandex_words(kind: MusicLinkKind, title: Option<&str>, og: Option<&str>, description: Option<&str>) -> Option<String> {
    static TAIL: OnceLock<Regex> = OnceLock::new();
    let tail = re(&TAIL, r"(?i)\s+(?:слушать онлайн|listen online).*$");
    let artist = description.filter(|d| d.contains(" • ")).and_then(|d| d.split(" • ").next());
    let from_title = || title.map(|t| tail.replace(t, "").into_owned());
    if kind == MusicLinkKind::Artist {
        return og.map(str::to_owned).or_else(from_title);
    }
    match og {
        Some(og) => Some([Some(og), artist].into_iter().flatten().collect::<Vec<_>>().join(" ")),
        None => from_title(),
    }
}

/// «Never Gonna Give You Up by Rick Astley on TIDAL», og:title «Rick Astley - Never Gonna Give You Up».
fn tidal_words(kind: MusicLinkKind, title: Option<&str>, og: Option<&str>) -> Option<String> {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    let title_re = re(&TITLE, r"(?i)^(.+) by (.+?) on TIDAL$");
    if let Some(caps) = title.and_then(|t| title_re.captures(t)) {
        return Some(pair(&caps));
    }
    if kind == MusicLinkKind::Artist {
        return og.map(str::to_owned).or_else(|| title.and_then(|t| t.strip_suffix(" on TIDAL")).map(str::to_owned));
    }
    og.map(|o| o.replace(" - ", " "))
}

/// «Daft Punk - Harder, Better, Faster, Stronger | Deezer», «Daft Punk | Deezer».
fn deezer_words(title: Option<&str>, og: Option<&str>) -> Option<String> {
    let head =
        title.map(|t| t.trim_end_matches("| Deezer").trim().trim_end_matches('|').trim().to_owned()).or_else(|| og.map(str::to_owned))?;
    Some(head.replace(" - ", " "))
}

/// Старый заголовок страницы трека: «Stream Never Gonna Give You Up by Rick Astley | Listen online…».
fn soundcloud_words(title: Option<&str>) -> Option<String> {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    let title_re = re(&TITLE, r"(?i)^Stream (.+?) by (.+?) \|");
    title.and_then(|t| title_re.captures(t)).map(|c| pair(&c))
}

fn tag_text(html: &str, tag: &str) -> Option<String> {
    let pattern = format!(r"(?is)<{tag}[^>]*>(.*?)</{tag}>");
    Regex::new(&pattern).ok()?.captures(html).map(|c| c[1].to_owned())
}

/// `content` у `<meta property="…" | name="…">`, атрибуты в любом порядке, кавычки любые.
fn meta(html: &str, name: &str) -> Option<String> {
    static TAG: OnceLock<Regex> = OnceLock::new();
    static CONTENT: OnceLock<Regex> = OnceLock::new();
    let tags = re(&TAG, r"(?is)<meta\b[^>]*>");
    let content = re(&CONTENT, r#"(?is)content\s*=\s*(?:"([^"]*)"|'([^']*)')"#);
    let named = Regex::new(&format!(r#"(?i)(?:property|name)\s*=\s*(?:"{0}"|'{0}')"#, regex::escape(name))).ok()?;
    for tag in tags.find_iter(html) {
        if !named.is_match(tag.as_str()) {
            continue;
        }
        if let Some(caps) = content.captures(tag.as_str()) {
            let value = caps.get(1).or_else(|| caps.get(2)).map(|m| m.as_str()).unwrap_or_default();
            return Some(clean(value));
        }
    }
    None
}

/// Сущности HTML — в символы, невидимые метки, которые сервисы кладут в заголовки, — прочь,
/// неразрывные пробелы — в пробелы.
fn clean(raw: &str) -> String {
    static ENTITY: OnceLock<Regex> = OnceLock::new();
    let entity = re(&ENTITY, r"&(#x[0-9a-fA-F]+|#\d+|[a-zA-Z]+);");
    let decoded = entity.replace_all(raw, |caps: &regex::Captures| {
        let body = &caps[1];
        let decoded = if let Some(hex) = body.strip_prefix("#x") {
            u32::from_str_radix(hex, 16).ok().and_then(char::from_u32).map(String::from)
        } else if let Some(number) = body.strip_prefix('#') {
            number.parse::<u32>().ok().and_then(char::from_u32).map(String::from)
        } else {
            match body.to_ascii_lowercase().as_str() {
                "amp" => Some("&".into()),
                "quot" => Some("\"".into()),
                "apos" => Some("'".into()),
                "lt" => Some("<".into()),
                "gt" => Some(">".into()),
                "nbsp" => Some(" ".into()),
                _ => None,
            }
        };
        decoded.unwrap_or_else(|| caps[0].to_owned())
    });
    decoded
        .chars()
        .filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{200b}' | '\u{feff}' | '\u{202a}' | '\u{202c}'))
        .map(|c| if matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}') { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn tidy(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(MAX_QUERY).collect::<String>().trim().to_owned()
}

// ── ответ song.link (только с ключом в сборке) ──

/// Что song.link знает о ссылке: где то же самое на YouTube Music, иначе на YouTube, и как оно называется.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SongLinkAnswer {
    pub youtube_url: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub is_album: bool,
}

impl SongLinkAnswer {
    /// Слова для поиска, когда ссылки на YouTube нет: «Название Исполнитель».
    pub fn search_text(&self) -> Option<String> {
        let title = self.title.as_ref()?;
        let text = [Some(title.as_str()), self.artist.as_deref()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        Some(text.trim().to_owned()).filter(|t| !t.is_empty())
    }
}

/// `linksByPlatform.youtubeMusic.url`, иначе `youtube`; название и исполнитель — у сущности ссылки
/// (`entitiesByUniqueId[entityUniqueId]`). `None` — тело не ответ (в том числе `401 PUBLIC_API_ACCESS_DEPRECATED`).
pub fn parse_song_link(body: &str) -> Option<SongLinkAnswer> {
    let root: serde_json::Value = serde_json::from_str(body).ok()?;
    let links = root.get("linksByPlatform");
    let youtube_url = ["youtubeMusic", "youtube"]
        .iter()
        .find_map(|platform| links?.get(platform)?.get("url")?.as_str().filter(|u| u.starts_with("http")).map(str::to_owned));
    let entities = root.get("entitiesByUniqueId").and_then(|e| e.as_object());
    let unique_id = root.get("entityUniqueId").and_then(|v| v.as_str());
    let entity = unique_id.and_then(|id| entities?.get(id)).or_else(|| entities?.values().find(|e| e.get("title").is_some()));
    let text = |key: &str| entity?.get(key)?.as_str().filter(|s| !s.trim().is_empty()).map(str::to_owned);
    let (title, artist) = (text("title"), text("artistName"));
    let is_album = text("type").as_deref() == Some("album") || unique_id.is_some_and(|id| id.contains("_ALBUM::"));
    if youtube_url.is_none() && title.is_none() {
        return None;
    }
    Some(SongLinkAnswer { youtube_url, title, artist, is_album })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> &'static str {
        match name {
            "spotify-track" => include_str!("../tests/fixtures/songlink/spotify-track.html"),
            "spotify-album" => include_str!("../tests/fixtures/songlink/spotify-album.html"),
            "spotify-artist" => include_str!("../tests/fixtures/songlink/spotify-artist.html"),
            "apple-track" => include_str!("../tests/fixtures/songlink/apple-track.html"),
            "apple-album" => include_str!("../tests/fixtures/songlink/apple-album.html"),
            "yandex-track" => include_str!("../tests/fixtures/songlink/yandex-track.html"),
            "yandex-album" => include_str!("../tests/fixtures/songlink/yandex-album.html"),
            "tidal-track" => include_str!("../tests/fixtures/songlink/tidal-track.html"),
            "deezer-track" => include_str!("../tests/fixtures/songlink/deezer-track.html"),
            "soundcloud-track" => include_str!("../tests/fixtures/songlink/soundcloud-track.html"),
            "answer-track" => include_str!("../tests/fixtures/songlink/answer-track.json"),
            "answer-album" => include_str!("../tests/fixtures/songlink/answer-album.json"),
            "answer-only-youtube" => include_str!("../tests/fixtures/songlink/answer-only-youtube.json"),
            "answer-no-youtube" => include_str!("../tests/fixtures/songlink/answer-no-youtube.json"),
            other => panic!("нет фикстуры {other}"),
        }
    }

    fn words(service: MusicService, kind: MusicLinkKind, name: &str) -> Option<String> {
        search_text(&MusicServiceLink { service, kind, url: "https://example.test/x".into() }, fixture(name))
    }

    fn link(text: &str) -> (MusicService, MusicLinkKind) {
        let link = parse(text).unwrap_or_else(|| panic!("не ссылка сервиса: {text}"));
        (link.service, link.kind)
    }

    #[test]
    fn links_of_services_and_what_they_are() {
        use MusicLinkKind::*;
        use MusicService::*;
        let cases = [
            ("https://open.spotify.com/track/4PTG3Z6ehGkBFwjybzWkR8?si=abc", (Spotify, Track)),
            ("https://open.spotify.com/intl-de/album/6uEoklrFKdhTeeAUC1VOmW", (Spotify, Album)),
            ("https://open.spotify.com/artist/0gxyHStUsqpMadRV0Di1Qt", (Spotify, Artist)),
            ("https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M", (Spotify, Playlist)),
            ("https://spotify.link/abc123", (Spotify, Unknown)),
            ("https://music.apple.com/us/song/never-gonna-give-you-up/1558533901", (AppleMusic, Track)),
            ("https://music.apple.com/us/album/whenever-you-need-somebody/1558533900?i=1558533901", (AppleMusic, Track)),
            ("https://music.apple.com/us/album/whenever-you-need-somebody/1558533900", (AppleMusic, Album)),
            ("https://music.apple.com/us/playlist/hits/pl.abc", (AppleMusic, Playlist)),
            ("https://music.yandex.ru/album/6009684/track/609676", (YandexMusic, Track)),
            ("https://music.yandex.com/album/6009684", (YandexMusic, Album)),
            ("https://music.yandex.ru/users/x/playlists/1000", (YandexMusic, Playlist)),
            ("https://music.yandex.ru/artist/79215", (YandexMusic, Artist)),
            ("https://www.deezer.com/en/track/3135556", (Deezer, Track)),
            ("https://www.deezer.com/album/302127", (Deezer, Album)),
            ("https://link.deezer.com/s/abc", (Deezer, Unknown)),
            ("https://tidal.com/browse/track/1234567", (Tidal, Track)),
            ("https://tidal.com/album/1234567", (Tidal, Album)),
            ("https://soundcloud.com/rick-astley/never-gonna-give-you-up", (SoundCloud, Track)),
            ("https://soundcloud.com/rick-astley/sets/hits", (SoundCloud, Playlist)),
            ("https://soundcloud.com/rick-astley", (SoundCloud, Artist)),
            ("https://on.soundcloud.com/xyz", (SoundCloud, Unknown)),
        ];
        for (text, expected) in cases {
            assert_eq!(link(text), expected, "{text}");
        }
    }

    #[test]
    fn a_link_inside_a_text_loses_its_punctuation() {
        let found = parse("Послушай: https://open.spotify.com/track/4PTG3Z6ehGkBFwjybzWkR8), класс!").unwrap();
        assert_eq!(found.url, "https://open.spotify.com/track/4PTG3Z6ehGkBFwjybzWkR8");
    }

    #[test]
    fn tracking_parameters_are_dropped_from_the_page_address() {
        let url = |text: &str| parse(text).unwrap().url;
        assert_eq!(
            url("https://open.spotify.com/track/4PTG3Z6ehGkBFwjybzWkR8?si=abc123&utm_source=copy-link"),
            "https://open.spotify.com/track/4PTG3Z6ehGkBFwjybzWkR8"
        );
        assert_eq!(
            url("https://music.apple.com/us/album/x/1558533900?i=1558533901&uo=4&app=music"),
            "https://music.apple.com/us/album/x/1558533900?i=1558533901"
        );
        assert_eq!(url("https://music.apple.com/us/album/x/1558533900?uo=4"), "https://music.apple.com/us/album/x/1558533900");
        assert_eq!(url("https://soundcloud.com/a/b?utm_source=x#t=1"), "https://soundcloud.com/a/b");
    }

    #[test]
    fn what_is_not_a_service_is_nothing() {
        for text in
            ["https://www.youtube.com/watch?v=dQw4w9WgXcQ", "https://example.com/track/1", "просто слова", "https://open.spotify.com/", ""]
        {
            assert!(parse(text).is_none(), "{text}");
        }
    }

    #[test]
    fn spotify_says_the_name_in_og_title_and_the_artist_first_in_the_description() {
        assert_eq!(
            words(MusicService::Spotify, MusicLinkKind::Track, "spotify-track").as_deref(),
            Some("Never Gonna Give You Up Rick Astley")
        );
        assert_eq!(
            words(MusicService::Spotify, MusicLinkKind::Album, "spotify-album").as_deref(),
            Some("Whenever You Need Somebody Rick Astley")
        );
        assert_eq!(words(MusicService::Spotify, MusicLinkKind::Artist, "spotify-artist").as_deref(), Some("Rick Astley"));
    }

    #[test]
    fn apple_music_says_both_in_the_title_with_an_invisible_mark_in_front() {
        assert_eq!(
            words(MusicService::AppleMusic, MusicLinkKind::Track, "apple-track").as_deref(),
            Some("Never Gonna Give You Up Rick Astley")
        );
        assert_eq!(words(MusicService::AppleMusic, MusicLinkKind::Album, "apple-album").as_deref(), Some("3 Originals Rick Astley"));
    }

    #[test]
    fn yandex_has_the_name_and_the_artist_after_a_dot_in_the_description() {
        assert_eq!(
            words(MusicService::YandexMusic, MusicLinkKind::Track, "yandex-track").as_deref(),
            Some("Never Gonna Give You Up Rick Astley")
        );
        assert_eq!(words(MusicService::YandexMusic, MusicLinkKind::Album, "yandex-album").as_deref(), Some("De Verdade Bokaloka"));
    }

    #[test]
    fn yandex_title_in_english_loses_its_tail_too() {
        let link = MusicServiceLink { service: MusicService::YandexMusic, kind: MusicLinkKind::Track, url: "u".into() };
        let html = "<title>Never Gonna Give You Up Rick Astley Listen online on Yandex Music</title>";
        assert_eq!(search_text(&link, html).as_deref(), Some("Never Gonna Give You Up Rick Astley"));
    }

    #[test]
    fn tidal_and_deezer_put_the_artist_and_the_title_in_one_line() {
        assert_eq!(words(MusicService::Tidal, MusicLinkKind::Track, "tidal-track").as_deref(), Some("Never Gonna Give You Up Rick Astley"));
        assert_eq!(
            words(MusicService::Deezer, MusicLinkKind::Track, "deezer-track").as_deref(),
            Some("Daft Punk Harder, Better, Faster, Stronger")
        );
    }

    #[test]
    fn soundcloud_gives_a_title_of_its_own_and_so_nothing_to_search_for() {
        assert_eq!(words(MusicService::SoundCloud, MusicLinkKind::Track, "soundcloud-track"), None);
    }

    #[test]
    fn entities_and_attribute_order_do_not_matter_and_a_long_text_is_cut() {
        let spotify = |kind| MusicServiceLink { service: MusicService::Spotify, kind, url: "u".into() };
        let html = "<html><head><TITLE>Tom &amp; Jerry &#8211; Theme - song and lyrics by A&#x27;B | Spotify</TITLE>\n<meta content=\"x\" property=\"og:title\"></head></html>";
        assert_eq!(search_text(&spotify(MusicLinkKind::Track), html).as_deref(), Some("Tom & Jerry – Theme A'B"));
        let long = format!("<title>{} - song by B | Spotify</title>", "a".repeat(300));
        assert_eq!(search_text(&spotify(MusicLinkKind::Track), &long).map(|t| t.chars().count()), Some(120));
        assert_eq!(search_text(&spotify(MusicLinkKind::Artist), "<html></html>"), None);
    }

    #[test]
    fn song_link_answers() {
        let track = parse_song_link(fixture("answer-track")).unwrap();
        assert_eq!(track.youtube_url.as_deref(), Some("https://music.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert_eq!(track.title.as_deref(), Some("Never Gonna Give You Up"), "сущность ссылки, а не длинное название видео");
        assert_eq!(track.artist.as_deref(), Some("Rick Astley"));
        assert!(!track.is_album);
        assert_eq!(track.search_text().as_deref(), Some("Never Gonna Give You Up Rick Astley"));

        let album = parse_song_link(fixture("answer-album")).unwrap();
        assert_eq!(album.youtube_url.as_deref(), Some("https://music.youtube.com/playlist?list=OLAK5uy_kAbCdEfGhIjKlMnOpQrStUvWxYz"));
        assert!(album.is_album);

        assert_eq!(
            parse_song_link(fixture("answer-only-youtube")).unwrap().youtube_url.as_deref(),
            Some("https://www.youtube.com/watch?v=gAjR4_CbPpQ")
        );
        let none = parse_song_link(fixture("answer-no-youtube")).unwrap();
        assert_eq!(none.youtube_url, None);
        assert_eq!(none.search_text().as_deref(), Some("Группа крови Кино"));

        for body in [
            "",
            "not json",
            r#"{"statusCode":401,"code":"PUBLIC_API_ACCESS_DEPRECATED"}"#,
            r#"{"linksByPlatform":{"spotify":{"url":"https://open.spotify.com/track/x"}}}"#,
        ] {
            assert_eq!(parse_song_link(body), None, "{body}");
        }
    }
}
