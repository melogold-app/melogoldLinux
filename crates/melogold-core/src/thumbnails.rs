//! Обложка нужного размера (REWRITE §4.8.2, Windows `Thumbnails.cs`, грабли §9 п. 6).
//!
//! В базе лежит исходный адрес, размер подбирается по месту: у `lh3`/`yt3.googleusercontent`
//! хвост после `=` заменяется на `=w{px}-h{px}-l90-rj`; у `i.ytimg.com/vi/<id>` до 320 px —
//! `mqdefault.jpg`, крупнее — `hq720.jpg`.

/// `videoId` из адреса кадра `i.ytimg.com/vi/<id>/…` (и `vi_webp`).
fn ytimg_video_id(url: &str) -> Option<(&str, usize)> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("i.ytimg.com/vi/").or_else(|| rest.strip_prefix("i.ytimg.com/vi_webp/"))?;
    let id = rest.get(..11)?;
    let valid = id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    (valid && rest.as_bytes().get(11) == Some(&b'/')).then(|| (id, url.len() - rest.len() + 12))
}

pub fn sized(url: Option<&str>, px: u32) -> Option<String> {
    let url = url.filter(|u| !u.is_empty())?;
    let url = if url.starts_with("//") { format!("https:{url}") } else { url.to_owned() };
    if url.contains("googleusercontent.com") || url.contains("ggpht.com") {
        let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
        let base = match url.rfind('=') {
            Some(eq) if eq > scheme_end => &url[..eq],
            _ => url.as_str(),
        };
        return Some(format!("{base}=w{px}-h{px}-l90-rj"));
    }
    if let Some((id, _)) = ytimg_video_id(&url) {
        let file = if px <= 320 { "mqdefault.jpg" } else { "hq720.jpg" };
        return Some(format!("https://i.ytimg.com/vi/{id}/{file}"));
    }
    Some(url)
}

/// Кадр видео по его id, когда своей обложки нет.
pub fn for_video(video_id: &str, px: u32) -> String {
    let file = if px <= 320 { "mqdefault.jpg" } else { "hqdefault.jpg" };
    format!("https://i.ytimg.com/vi/{video_id}/{file}")
}

/// Запасной кадр: у старых видео крупных размеров нет, есть только `hqdefault.jpg` 480×360.
pub fn fallback(url: &str) -> Option<String> {
    let (id, prefix) = ytimg_video_id(url)?;
    let file = url.get(prefix..)?.split('?').next()?;
    matches!(file, "hq720.jpg" | "sddefault.jpg" | "maxresdefault.jpg" | "hq720.webp" | "maxresdefault.webp")
        .then(|| format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"))
}

/// Кадр видео 16:9 — при показе его обрезают до квадрата по середине (§5.5).
pub fn is_wide(url: Option<&str>) -> bool {
    url.is_some_and(|u| ytimg_video_id(u).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn googleusercontent_is_resized() {
        assert_eq!(
            sized(Some("https://lh3.googleusercontent.com/abc=w60-h60-l90-rj"), 544).as_deref(),
            Some("https://lh3.googleusercontent.com/abc=w544-h544-l90-rj")
        );
        assert_eq!(sized(Some("//yt3.ggpht.com/x=s88"), 120).as_deref(), Some("https://yt3.ggpht.com/x=w120-h120-l90-rj"));
    }

    #[test]
    fn video_frames_pick_small_or_large() {
        let url = "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg?sqp=1";
        assert_eq!(sized(Some(url), 120).as_deref(), Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/mqdefault.jpg"));
        assert_eq!(sized(Some(url), 544).as_deref(), Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hq720.jpg"));
        assert!(is_wide(Some(url)));
        assert!(!is_wide(Some("https://lh3.googleusercontent.com/a")));
    }

    #[test]
    fn fallback_only_for_large_frames() {
        assert_eq!(
            fallback("https://i.ytimg.com/vi/dQw4w9WgXcQ/hq720.jpg").as_deref(),
            Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg")
        );
        assert_eq!(fallback("https://i.ytimg.com/vi/dQw4w9WgXcQ/mqdefault.jpg"), None);
    }
}
