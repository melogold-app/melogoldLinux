//! Ссылки «Поделиться» (задание 0010, API §4.11, §7.2): что отправить другу и как разобрать ссылку
//! на снимок плейлиста, пришедшую обратно. Без сети.

use crate::music::Track;

/// Первые 50 треков, которые открывает запасная ссылка `watch_videos`.
pub const WATCH_VIDEOS_LIMIT: usize = 50;

/// Трек из каталога — на music.youtube.com, обычное видео — на www.youtube.com.
pub fn track_url(track: &Track) -> String {
    if track.is_video() {
        format!("https://www.youtube.com/watch?v={}", track.video_id)
    } else {
        format!("https://music.youtube.com/watch?v={}", track.video_id)
    }
}

pub fn album_url(browse_id: &str) -> String {
    format!("https://music.youtube.com/browse/{browse_id}")
}

/// Исполнитель — `music.youtube.com/channel/…`, канал обычного YouTube — `www.youtube.com/channel/…`.
pub fn artist_url(browse_id: &str, is_channel: bool) -> String {
    if is_channel {
        format!("https://www.youtube.com/channel/{browse_id}")
    } else {
        format!("https://music.youtube.com/channel/{browse_id}")
    }
}

pub fn playlist_url(playlist_id: &str) -> String {
    format!("https://music.youtube.com/playlist?list={}", playlist_id.strip_prefix("VL").unwrap_or(playlist_id))
}

/// Запасная ссылка на свой плейлист: без входа, без сервера или без `features.share`. Открывает первые
/// [`WATCH_VIDEOS_LIMIT`] треков на YouTube; второе значение — «ссылка урезана».
pub fn watch_videos_url<'a>(video_ids: impl IntoIterator<Item = &'a str>) -> (String, bool) {
    let all: Vec<&str> = video_ids.into_iter().collect();
    let truncated = all.len() > WATCH_VIDEOS_LIMIT;
    let ids: Vec<&str> = all.into_iter().take(WATCH_VIDEOS_LIMIT).collect();
    (format!("https://www.youtube.com/watch_videos?video_ids={}", ids.join(",")), truncated)
}

/// Текст сообщения: «<Название> — <исполнитель>» и ссылка.
pub fn share_message(title: &str, artists: Option<&str>, url: &str) -> String {
    match artists.map(str::trim).filter(|a| !a.is_empty()) {
        Some(artists) => format!("{title} — {artists}\n{url}"),
        None => format!("{title}\n{url}"),
    }
}

/// Снимок плейлиста по ссылке: сервер и код.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareRef {
    /// Адрес сервера без `/` в конце (с путём, если сервер стоит не в корне).
    pub base: String,
    pub id: String,
}

/// `ShareId` — 10 знаков base62 (API §4.11).
pub fn is_share_id(id: &str) -> bool {
    id.len() == 10 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `https://<сервер>/s/<код>` в тексте (вставлено в поиск, прислано в приложение). Хосты YouTube и других
/// музыкальных сервисов сюда не относятся: их пути разбирают свои правила.
pub fn parse_share_url(text: &str) -> Option<ShareRef> {
    let text = text.trim();
    let url = url::Url::parse(text.split_whitespace().next()?).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.query().is_some() && url.query_pairs().any(|(k, _)| k == "v" || k == "list") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if host.contains("youtube.") || host.contains("youtu.be") || crate::music_services::is_service_host(&host) {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let [prefix @ .., "s", id] = segments.as_slice() else { return None };
    if !is_share_id(id) {
        return None;
    }
    let mut base = format!("{}://{}", url.scheme(), url.host_str()?);
    if let Some(port) = url.port() {
        base.push_str(&format!(":{port}"));
    }
    for part in prefix {
        base.push('/');
        base.push_str(part);
    }
    Some(ShareRef { base, id: (*id).to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, kind: &str) -> Track {
        Track { video_id: id.into(), title: "T".into(), video_type: Some(kind.into()), ..Default::default() }
    }

    #[test]
    fn what_is_sent() {
        assert_eq!(track_url(&track("dQw4w9WgXcQ", "song")), "https://music.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(track_url(&track("dQw4w9WgXcQ", "video")), "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(track_url(&Track { video_id: "x".into(), ..Default::default() }), "https://music.youtube.com/watch?v=x");
        assert_eq!(album_url("MPREb_abc"), "https://music.youtube.com/browse/MPREb_abc");
        assert_eq!(artist_url("UCabc", false), "https://music.youtube.com/channel/UCabc");
        assert_eq!(artist_url("UCabc", true), "https://www.youtube.com/channel/UCabc");
        assert_eq!(playlist_url("PLxyz"), "https://music.youtube.com/playlist?list=PLxyz");
        assert_eq!(playlist_url("VLPLxyz"), "https://music.youtube.com/playlist?list=PLxyz", "browseId плейлиста без VL");
    }

    #[test]
    fn own_playlist_without_server_gives_the_first_fifty() {
        let ids: Vec<String> = (0..80).map(|i| format!("id{i:09}")).collect();
        let (url, truncated) = watch_videos_url(ids.iter().map(String::as_str));
        assert!(truncated);
        assert_eq!(url.matches(',').count(), 49);
        assert!(url.starts_with("https://www.youtube.com/watch_videos?video_ids=id000000000,id000000001"));
        assert!(!url.contains("id000000050"));
        let (url, truncated) = watch_videos_url(["aaaaaaaaaaa", "bbbbbbbbbbb"]);
        assert!(!truncated);
        assert_eq!(url, "https://www.youtube.com/watch_videos?video_ids=aaaaaaaaaaa,bbbbbbbbbbb");
    }

    #[test]
    fn message_is_title_artist_and_link() {
        assert_eq!(share_message("Кино", Some("Группа крови"), "https://x"), "Кино — Группа крови\nhttps://x");
        assert_eq!(share_message("Кино", Some("  "), "https://x"), "Кино\nhttps://x");
        assert_eq!(share_message("Кино", None, "https://x"), "Кино\nhttps://x");
    }

    #[test]
    fn share_urls() {
        let expected = |base: &str| Some(ShareRef { base: base.into(), id: "a1B2c3D4e5".into() });
        assert_eq!(parse_share_url("https://music.example.com/s/a1B2c3D4e5"), expected("https://music.example.com"));
        assert_eq!(parse_share_url("  http://192.168.1.50:8080/s/a1B2c3D4e5 привет"), expected("http://192.168.1.50:8080"));
        assert_eq!(parse_share_url("https://example.com/melogold/s/a1B2c3D4e5/"), expected("https://example.com/melogold"));
        // Не снимки: другой код, другая форма, сервисы.
        for text in [
            "https://example.com/s/short",
            "https://example.com/s/a1B2c3D4e5x",
            "https://example.com/x/a1B2c3D4e5",
            "https://example.com/",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://open.spotify.com/s/a1B2c3D4e5",
            "melogold://share?v=1",
            "не ссылка",
        ] {
            assert_eq!(parse_share_url(text), None, "{text}");
        }
    }
}
