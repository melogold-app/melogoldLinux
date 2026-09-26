//! Куда ведёт ссылка или текст из другого приложения (REWRITE §4.9, Windows `YouTubeLinkParser.cs`).
//! Правила и случаи — `spec/youtube-links.vectors.json`. Без сети: `resolve_url` и альбом плейлиста
//! `OLAK5uy_` — дело того, кто ссылку открывает.

use std::collections::HashMap;

use percent_encoding::percent_decode_str;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Video {
        video_id: String,
        playlist_id: Option<String>,
        index: Option<i64>,
        start_ms: Option<i64>,
    },
    /// PL…, RDCLAK5uy_…, RD…, UU…, OLAK5uy_…
    Playlist(String),
    Album(String),
    Channel(String),
    /// `/@name`: нужен `resolve_url`.
    Handle(String),
    /// `/c/…`, `/user/…`: нужен `resolve_url`.
    LegacyChannel(String),
    Search(String),
    /// Apple Music, Яндекс Музыка или Spotify: импорт появится позже.
    External {
        service: String,
        url: String,
    },
    Unsupported(&'static str),
}

pub const EMPTY: &str = "empty";
pub const INVALID_VIDEO_ID: &str = "invalid_video_id";
pub const MISSING_PARAMETER: &str = "missing_parameter";
pub const PRIVATE_PLAYLIST: &str = "private_playlist";
pub const CLIP: &str = "clip";
pub const POST: &str = "post";
pub const UNKNOWN_PATH: &str = "unknown_path";
pub const UNKNOWN_HOST: &str = "unknown_host";
pub const UNSUPPORTED_EXTERNAL: &str = "unsupported_external";

const MAX_QUERY: usize = 200;
const MAX_UNWRAP: usize = 3;
const TRAILING: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '»', '"', '\'', '…'];
const YOUTUBE_HOSTS: &[&str] = &[
    "youtube.com",
    "www.youtube.com",
    "m.youtube.com",
    "music.youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
    "www.youtube-nocookie.com",
];
/// Списки самого аккаунта: вне аккаунта смысла не имеют.
const PRIVATE_PLAYLISTS: &[&str] = &["LL", "WL", "LM"];
const VIDEO_PATHS: &[&str] = &["shorts", "live", "embed", "v", "e"];

pub fn parse(input: &str) -> LinkTarget {
    let text = input.trim();
    if text.is_empty() {
        return LinkTarget::Unsupported(EMPTY);
    }
    if text.get(..12).is_some_and(|p| p.eq_ignore_ascii_case("vnd.youtube:")) {
        let rest = &text[12..];
        let id = rest.split(['?', '&', '#']).next().unwrap_or_default();
        return video(id, &HashMap::new());
    }
    match find_url(text) {
        Some(url) => parse_url(url.trim_end_matches(TRAILING), 0),
        None => LinkTarget::Search(search_text(text)),
    }
}

/// Первая ссылка `https?://…` до пробела, без учёта регистра схемы.
fn find_url(text: &str) -> Option<&str> {
    let lower = text.to_ascii_lowercase();
    let start = ["https://", "http://"].iter().filter_map(|p| lower.find(p)).min()?;
    let rest = &text[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Пробельные серии — в один пробел; не длиннее 200 единиц UTF-16 и без разрыва суррогатной пары.
fn search_text(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut units = 0;
    let mut out = String::new();
    for c in collapsed.chars() {
        units += c.len_utf16();
        if units > MAX_QUERY {
            break;
        }
        out.push(c);
    }
    out
}

fn parse_url(url: &str, depth: usize) -> LinkTarget {
    let Some((_, after_scheme)) = url.split_once("://") else { return LinkTarget::Unsupported(UNKNOWN_PATH) };
    let without_fragment = after_scheme.split('#').next().unwrap_or_default();
    let (before_query, raw_query) = match without_fragment.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (without_fragment, None),
    };
    let (authority, raw_path) = match before_query.find('/') {
        Some(i) => (&before_query[..i], &before_query[i..]),
        None => (before_query, ""),
    };
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = match authority.rfind(':') {
        Some(i) if !authority.ends_with(']') => &authority[..i],
        _ => authority,
    }
    .to_ascii_lowercase();
    if host.is_empty() {
        return LinkTarget::Unsupported(UNKNOWN_HOST);
    }
    let query = query_parameters(raw_query);
    let raw_segments: Vec<&str> = raw_path.split('/').filter(|s| !s.is_empty()).collect();
    let segments: Vec<String> = raw_segments.iter().map(|s| decode_path(s)).collect();
    let unwrap = |target: Option<&String>| -> LinkTarget {
        match target.filter(|t| !t.trim().is_empty()) {
            None => LinkTarget::Unsupported(MISSING_PARAMETER),
            Some(_) if depth >= MAX_UNWRAP => LinkTarget::Unsupported(UNKNOWN_PATH),
            Some(target) => parse_url(target, depth + 1),
        }
    };

    if host == "consent.youtube.com" {
        return unwrap(query.get("continue"));
    }
    if (host == "google.com" || host == "www.google.com") && segments.first().map(String::as_str) == Some("url") {
        return unwrap(query.get("q").or_else(|| query.get("url")));
    }
    if let Some(external) = external(&host, &segments, url) {
        return external;
    }
    if !YOUTUBE_HOSTS.contains(&host.as_str()) {
        return LinkTarget::Unsupported(UNKNOWN_HOST);
    }
    if host == "youtu.be" {
        return match segments.first() {
            Some(id) => video(id, &query),
            None => LinkTarget::Unsupported(MISSING_PARAMETER),
        };
    }
    let Some(first) = segments.first().map(String::as_str) else { return LinkTarget::Unsupported(UNKNOWN_PATH) };
    let second = segments.get(1).map(String::as_str);
    let missing = || LinkTarget::Unsupported(MISSING_PARAMETER);
    let playlist = |list: Option<&String>| match list.filter(|l| !l.is_empty()) {
        None => missing(),
        Some(list) if PRIVATE_PLAYLISTS.contains(&list.as_str()) => LinkTarget::Unsupported(PRIVATE_PLAYLIST),
        Some(list) => LinkTarget::Playlist(list.clone()),
    };
    match first {
        "attribution_link" => {
            let target = query.get("u").map(|u| if u.starts_with('/') { format!("https://www.youtube.com{u}") } else { u.clone() });
            unwrap(target.as_ref())
        }
        "watch" => match query.get("v") {
            Some(id) => video(id, &query),
            None => playlist(query.get("list")),
        },
        _ if VIDEO_PATHS.contains(&first) => match second {
            Some(id) => video(id, &query),
            None => missing(),
        },
        "playlist" => playlist(query.get("list")),
        "browse" => match second {
            None => missing(),
            Some(id) if id.starts_with("VL") => LinkTarget::Playlist(id[2..].to_owned()),
            Some(id) if id.starts_with("MPREb_") => LinkTarget::Album(id.to_owned()),
            Some(id) if id.starts_with("UC") => LinkTarget::Channel(id.to_owned()),
            Some(_) => LinkTarget::Unsupported(UNKNOWN_PATH),
        },
        "channel" => match second {
            None => missing(),
            Some(id) if id.starts_with("UC") => LinkTarget::Channel(id.to_owned()),
            Some(_) => LinkTarget::Unsupported(UNKNOWN_PATH),
        },
        _ if first.starts_with('@') && first.len() > 1 => LinkTarget::Handle(first[1..].to_owned()),
        "c" | "user" => match raw_segments.get(1) {
            Some(name) => LinkTarget::LegacyChannel(format!("https://www.youtube.com/{first}/{name}")),
            None => missing(),
        },
        "search" => search(query.get("q")),
        "results" => search(query.get("search_query")),
        "hashtag" => match second {
            Some(tag) => LinkTarget::Search(format!("#{tag}")),
            None => missing(),
        },
        "clip" => LinkTarget::Unsupported(CLIP),
        "post" => LinkTarget::Unsupported(POST),
        _ => LinkTarget::Unsupported(UNKNOWN_PATH),
    }
}

fn search(query: Option<&String>) -> LinkTarget {
    match query.filter(|q| !q.trim().is_empty()) {
        Some(q) => LinkTarget::Search(q.clone()),
        None => LinkTarget::Unsupported(MISSING_PARAMETER),
    }
}

fn external(host: &str, segments: &[String], url: &str) -> Option<LinkTarget> {
    let has = |name: &str| segments.iter().any(|s| s == name);
    let first = segments.first().map(String::as_str);
    let service = match host {
        "music.apple.com" => has("playlist").then_some("apple"),
        "music.yandex.ru" | "music.yandex.com" => (has("playlists") || first == Some("album")).then_some("yandex"),
        "open.spotify.com" => (first == Some("playlist")).then_some("spotify"),
        _ => return None,
    };
    Some(match service {
        Some(service) => LinkTarget::External { service: service.to_owned(), url: url.to_owned() },
        None => LinkTarget::Unsupported(UNSUPPORTED_EXTERNAL),
    })
}

fn is_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn video(id: &str, query: &HashMap<String, String>) -> LinkTarget {
    if !is_video_id(id) {
        return LinkTarget::Unsupported(INVALID_VIDEO_ID);
    }
    let playlist_id = query.get("list").filter(|l| !l.is_empty() && !PRIVATE_PLAYLISTS.contains(&l.as_str())).cloned();
    let index = query.get("index").and_then(|i| i.parse::<i64>().ok());
    let start_ms = query.get("t").or_else(|| query.get("start")).and_then(|t| time_ms(t));
    LinkTarget::Video { video_id: id.to_owned(), playlist_id, index, start_ms }
}

/// «90», «90s», «1m30s», «1h2m3s» → миллисекунды; иначе `None`.
fn time_ms(value: &str) -> Option<i64> {
    if value.is_empty() {
        return None;
    }
    let (mut total, mut number, mut seen_unit, mut any) = (0i64, String::new(), 0u8, false);
    let order = |unit: char| match unit {
        'h' => 1,
        'm' => 2,
        _ => 3,
    };
    for c in value.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let unit = match c {
            'h' | 'm' | 's' => c,
            _ => return None,
        };
        if number.is_empty() || order(unit) <= seen_unit {
            return None;
        }
        let n: i64 = number.parse().ok()?;
        total += n * match unit {
            'h' => 3600,
            'm' => 60,
            _ => 1,
        };
        seen_unit = order(unit);
        number.clear();
        any = true;
    }
    if !number.is_empty() {
        // Хвост без единицы — секунды, и только последним.
        if seen_unit >= 3 {
            return None;
        }
        total += number.parse::<i64>().ok()?;
        any = true;
    }
    any.then_some(total * 1000)
}

fn query_parameters(raw: Option<&str>) -> HashMap<String, String> {
    let mut parameters = HashMap::new();
    for pair in raw.unwrap_or_default().split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        parameters.entry(decode_query(key)).or_insert_with(|| decode_query(value));
    }
    parameters
}

/// Путь: только percent-escapes, `+` остаётся (как `URI.getPath` в Java).
fn decode_path(value: &str) -> String {
    percent_decode_str(value).decode_utf8().map(|s| s.into_owned()).unwrap_or_else(|_| value.to_owned())
}

/// Как `URLDecoder` в Java: `+` — пробел, неверная последовательность — строка как есть.
fn decode_query(value: &str) -> String {
    let spaced = value.replace('+', " ");
    percent_decode_str(&spaced).decode_utf8().map(|s| s.into_owned()).unwrap_or(spaced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn expected(value: &Value) -> LinkTarget {
        let s = |key: &str| value[key].as_str().map(str::to_owned);
        match value["type"].as_str().unwrap() {
            "Video" => LinkTarget::Video {
                video_id: s("videoId").unwrap(),
                playlist_id: s("playlistId"),
                index: value["index"].as_i64(),
                start_ms: value["startMs"].as_i64(),
            },
            "Playlist" => LinkTarget::Playlist(s("playlistId").unwrap()),
            "Album" => LinkTarget::Album(s("browseId").unwrap()),
            "Channel" => LinkTarget::Channel(s("channelId").unwrap()),
            "Handle" => LinkTarget::Handle(s("handle").unwrap()),
            "LegacyChannel" => LinkTarget::LegacyChannel(s("url").unwrap()),
            "Search" => LinkTarget::Search(s("query").unwrap()),
            "External" => LinkTarget::External { service: s("service").unwrap(), url: s("url").unwrap() },
            "Unsupported" => {
                let reason = s("reason").unwrap();
                let known = [
                    EMPTY,
                    INVALID_VIDEO_ID,
                    MISSING_PARAMETER,
                    PRIVATE_PLAYLIST,
                    CLIP,
                    POST,
                    UNKNOWN_PATH,
                    UNKNOWN_HOST,
                    UNSUPPORTED_EXTERNAL,
                ];
                LinkTarget::Unsupported(known.into_iter().find(|k| *k == reason).expect("известная причина"))
            }
            other => panic!("неизвестный тип {other}"),
        }
    }

    #[test]
    fn shared_vectors() {
        let text = include_str!("../../../spec/youtube-links.vectors.json");
        let vectors: Value = serde_json::from_str(text).unwrap();
        let cases = vectors["cases"].as_array().unwrap();
        let mut failed = Vec::new();
        for case in cases {
            let input = case["input"].as_str().unwrap();
            let got = parse(input);
            let want = expected(&case["expected"]);
            if got != want {
                failed.push(format!("{}: {input:?}\n  ждал {want:?}\n  вышло {got:?}", case["id"]));
            }
        }
        assert!(failed.is_empty(), "{} из {}:\n{}", failed.len(), cases.len(), failed.join("\n"));
    }
}
