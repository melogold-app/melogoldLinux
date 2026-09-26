//! Разбор рендереров YouTube Music (WEB_REMIX), порт Windows `MusicParsers.cs`.
//!
//! Тип элемента определяется по переходу (`pageType`, `musicVideoType`), а не по локализованной
//! подписи; поля трека — по ссылкам внутри подписи (`UC…` — исполнитель, `MPREb_…` — альбом),
//! а не по позиции (REWRITE §4.8.2).

use melogold_core::music::{AlbumItem, ArtistItem, ArtistRef, MoodItem, MusicItem, PlaylistItem, Shelf, Track};
use melogold_core::text::parse_duration;
use serde_json::{json, Value};

use crate::at;
use crate::json::{Json, Run};

/// Подписи типа в смешанной выдаче: первая группа подписи, её выбрасываем.
const TYPE_LABELS: &[&str] = &[
    "song",
    "video",
    "album",
    "single",
    "ep",
    "playlist",
    "artist",
    "episode",
    "podcast",
    "profile",
    "composition",
    "композиция",
    "песня",
    "трек",
    "видео",
    "клип",
    "альбом",
    "сингл",
    "плейлист",
    "исполнитель",
    "выпуск",
    "подкаст",
    "профиль",
];

const PAGE_ALBUM: &str = "MUSIC_PAGE_TYPE_ALBUM";
const PAGE_AUDIOBOOK: &str = "MUSIC_PAGE_TYPE_AUDIOBOOK";
const PAGE_ARTIST: &str = "MUSIC_PAGE_TYPE_ARTIST";
const PAGE_USER_CHANNEL: &str = "MUSIC_PAGE_TYPE_USER_CHANNEL";
const PAGE_PLAYLIST: &str = "MUSIC_PAGE_TYPE_PLAYLIST";

fn is_type_label(text: &str) -> bool {
    let lower = text.to_lowercase();
    TYPE_LABELS.contains(&lower.as_str())
}

fn is_duration(text: &str) -> bool {
    let parts: Vec<&str> = text.split(':').collect();
    (2..=3).contains(&parts.len())
        && parts[0].len() <= 2
        && !parts[0].is_empty()
        && parts[0].bytes().all(|b| b.is_ascii_digit())
        && parts[1..].iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn is_year(text: &str) -> bool {
    text.len() == 4 && text.bytes().all(|b| b.is_ascii_digit())
}

pub fn video_type_of(music_video_type: Option<&str>) -> Option<String> {
    Some(
        match music_video_type? {
            "MUSIC_VIDEO_TYPE_ATV" => "song",
            "MUSIC_VIDEO_TYPE_UGC" => "ugc",
            "MUSIC_VIDEO_TYPE_PODCAST_EPISODE" => "podcast_episode",
            _ => "video",
        }
        .to_owned(),
    )
}

fn music_video_type(watch: Option<&Value>) -> Option<String> {
    at!(watch, "watchEndpointMusicSupportedConfigs", "watchEndpointMusicConfig", "musicVideoType").string()
}

fn is_artist_run(run: &Run) -> bool {
    run.browse_id.as_deref().is_some_and(|id| id.starts_with("UC") || id.starts_with("FEmusic_library_privately_owned_artist"))
        || matches!(run.page_type.as_deref(), Some(PAGE_ARTIST | PAGE_USER_CHANNEL))
}

fn is_album_run(run: &Run) -> bool {
    run.browse_id.as_deref().is_some_and(|id| id.starts_with("MPREb_")) || run.page_type.as_deref() == Some(PAGE_ALBUM)
}

/// Подпись, разбитая по « • » на группы.
fn groups(runs: Vec<Run>) -> Vec<Vec<Run>> {
    let mut groups: Vec<Vec<Run>> = vec![Vec::new()];
    for run in runs {
        if run.is_separator() {
            if !groups.last().is_some_and(Vec::is_empty) {
                groups.push(Vec::new());
            }
            continue;
        }
        groups.last_mut().expect("есть группа").push(run);
    }
    if groups.last().is_some_and(Vec::is_empty) {
        groups.pop();
    }
    groups
}

fn group_text(group: &[Run]) -> String {
    group.iter().map(|r| r.text.as_str()).collect::<String>().trim().to_owned()
}

fn artists_of(group: &[Run]) -> Vec<ArtistRef> {
    let linked: Vec<ArtistRef> =
        group.iter().filter(|r| is_artist_run(r)).map(|r| ArtistRef { id: r.browse_id.clone(), name: r.text.trim().to_owned() }).collect();
    if linked.is_empty() {
        vec![ArtistRef { id: None, name: group_text(group) }]
    } else {
        linked
    }
}

fn is_explicit(node: Option<&Value>, key: &str) -> bool {
    at!(node, key).items().iter().any(|b| at!(b, "musicInlineBadgeRenderer", "icon", "iconType").str() == Some("MUSIC_EXPLICIT_BADGE"))
}

fn thumb(renderer: Option<&Value>) -> Option<String> {
    at!(renderer, "thumbnail", "musicThumbnailRenderer", "thumbnail", "thumbnails")
        .best_thumbnail()
        .or_else(|| at!(renderer, "thumbnailRenderer", "musicThumbnailRenderer", "thumbnail", "thumbnails").best_thumbnail())
        .or_else(|| at!(renderer, "thumbnail", "thumbnails").best_thumbnail())
}

/// Что из подписи трека известно по группам.
#[derive(Default)]
struct Meta {
    artists: Vec<ArtistRef>,
    artists_text: Option<String>,
    album_id: Option<String>,
    album_title: Option<String>,
    duration: Option<String>,
    views: Option<String>,
    year: Option<String>,
}

fn meta_of(groups: &[Vec<Run>]) -> Meta {
    let mut artists: Option<&Vec<Run>> = None;
    let mut meta = Meta::default();
    let mut rest: Vec<&Vec<Run>> = Vec::new();
    let any_links = groups.iter().any(|g| g.iter().any(|r| r.browse_id.is_some()));
    for (index, group) in groups.iter().enumerate() {
        let text = group_text(group);
        if text.is_empty() {
            continue;
        }
        let unlinked = group.iter().all(|r| r.browse_id.is_none());
        if let Some(album) = group.iter().find(|r| is_album_run(r)) {
            meta.album_id = album.browse_id.clone();
            meta.album_title = Some(album.text.clone());
        } else if artists.is_none() && group.iter().any(is_artist_run) {
            artists = Some(group);
        } else if is_duration(&text) {
            meta.duration = Some(text);
        } else if is_year(&text) {
            meta.year = Some(text);
        } else if index == 0 && unlinked && (is_type_label(&text) || (any_links && groups.len() > 2)) {
            continue;
        } else if unlinked && text.chars().any(|c| c.is_ascii_digit()) && artists.is_some() {
            meta.views.get_or_insert(text);
        } else {
            rest.push(group);
        }
    }
    if artists.is_none() && !rest.is_empty() {
        artists = Some(rest.remove(0));
    }
    if meta.views.is_none() {
        if let Some(first) = rest.first() {
            let text = group_text(first);
            if text.chars().any(|c| c.is_ascii_digit()) {
                meta.views = Some(text);
            }
        }
    }
    if let Some(group) = artists {
        meta.artists = artists_of(group);
        meta.artists_text = Some(group_text(group));
    }
    meta
}

fn subtitle_without_type(groups: &[Vec<Run>]) -> Option<String> {
    let mut parts: Vec<String> = groups.iter().map(|g| group_text(g)).filter(|t| !t.is_empty()).collect();
    if parts.len() > 1 && is_type_label(&parts[0]) {
        parts.remove(0);
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn strip_vl(browse_id: &str) -> String {
    browse_id.strip_prefix("VL").unwrap_or(browse_id).to_owned()
}

fn first_type_text(groups: &[Vec<Run>]) -> Option<String> {
    let first = groups.first()?;
    let text = group_text(first);
    (first.iter().all(|r| r.browse_id.is_none()) && !is_year(&text) && !text.is_empty()).then_some(text)
}

// ── строки ──

/// `musicResponsiveListItemRenderer` → трек, альбом, исполнитель или плейлист; прочее — `None`.
pub fn responsive_item(r: Option<&Value>) -> Option<MusicItem> {
    let r = r?;
    let columns: Vec<Option<&Value>> =
        at!(r, "flexColumns").items().iter().map(|c| at!(c, "musicResponsiveListItemFlexColumnRenderer", "text")).collect();
    let title = columns.first().copied().flatten().map(|c| Some(c).text().unwrap_or_default().trim().to_owned())?;
    if title.is_empty() {
        return None;
    }
    let browse = at!(r, "navigationEndpoint", "browseEndpoint");
    let browse_id = at!(browse, "browseId").string();
    let page_type = at!(browse, "browseEndpointContextSupportedConfigs", "browseEndpointContextMusicConfig", "pageType").str();
    let mut subtitle_runs = Vec::new();
    for column in columns.iter().skip(1) {
        subtitle_runs.extend(column.runs());
        subtitle_runs.push(Run::separator());
    }
    let groups = groups(subtitle_runs);
    let thumbnail = thumb(Some(r));
    let overlay = at!(r, "overlay", "musicItemThumbnailOverlayRenderer", "content", "musicPlayButtonRenderer", "playNavigationEndpoint");

    if let Some(browse_id) = &browse_id {
        match page_type {
            Some(PAGE_ALBUM | PAGE_AUDIOBOOK) => {
                let meta = meta_of(&groups);
                return Some(MusicItem::Album(AlbumItem {
                    browse_id: browse_id.clone(),
                    title,
                    artists: meta.artists,
                    artists_text: meta.artists_text,
                    year: meta.year,
                    type_text: first_type_text(&groups),
                    thumbnail_url: thumbnail,
                    playlist_id: at!(overlay, "watchPlaylistEndpoint", "playlistId").string(),
                    explicit: is_explicit(Some(r), "badges"),
                }));
            }
            Some(PAGE_ARTIST | PAGE_USER_CHANNEL) => {
                return Some(MusicItem::Artist(ArtistItem {
                    browse_id: browse_id.clone(),
                    name: title,
                    subtitle: subtitle_without_type(&groups),
                    thumbnail_url: thumbnail,
                    is_channel: page_type == Some(PAGE_USER_CHANNEL),
                }));
            }
            Some(PAGE_PLAYLIST) => {
                return Some(MusicItem::Playlist(PlaylistItem {
                    playlist_id: strip_vl(browse_id),
                    title,
                    subtitle: subtitle_without_type(&groups),
                    thumbnail_url: thumbnail,
                }));
            }
            _ => {}
        }
    }

    let title_watch = columns
        .first()
        .copied()
        .flatten()
        .and_then(|c| at!(c, "runs").items().iter().find_map(|run| at!(run, "navigationEndpoint", "watchEndpoint")));
    let overlay_watch = at!(overlay, "watchEndpoint");
    let video_id = at!(r, "playlistItemData", "videoId")
        .string()
        .or_else(|| at!(title_watch, "videoId").string())
        .or_else(|| at!(overlay_watch, "videoId").string())?;
    let music_video_type = music_video_type(title_watch).or_else(|| music_video_type(overlay_watch));
    if music_video_type.as_deref() == Some("MUSIC_VIDEO_TYPE_PODCAST_EPISODE") {
        return None;
    }
    let fixed = at!(r, "fixedColumns")
        .items()
        .iter()
        .filter_map(|c| at!(c, "musicResponsiveListItemFixedColumnRenderer", "text").text())
        .find(|t| !t.trim().is_empty());
    let meta = meta_of(&groups);
    let duration_text = fixed.map(|t| t.trim().to_owned()).filter(|t| is_duration(t)).or(meta.duration);
    Some(MusicItem::Track(Track {
        video_id,
        title,
        artists: meta.artists,
        artists_text: meta.artists_text,
        album_id: meta.album_id,
        album_title: meta.album_title,
        duration_ms: parse_duration(duration_text.as_deref()),
        duration_text,
        thumbnail_url: thumbnail,
        explicit: is_explicit(Some(r), "badges"),
        video_type: video_type_of(music_video_type.as_deref()),
        views_text: meta.views,
        unavailable: at!(r, "musicItemRendererDisplayPolicy").str() == Some("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT"),
    }))
}

// ── карточки ──

/// `musicTwoRowItemRenderer` → альбом, исполнитель, плейлист или трек.
pub fn two_row_item(r: Option<&Value>) -> Option<MusicItem> {
    let r = r?;
    let title = at!(r, "title").text()?.trim().to_owned();
    if title.is_empty() {
        return None;
    }
    let groups = groups(at!(r, "subtitle").runs());
    let thumbnail = thumb(Some(r));
    let nav = at!(r, "navigationEndpoint");
    if let Some(browse) = at!(nav, "browseEndpoint") {
        let browse_id = at!(browse, "browseId").string()?;
        let page_type = at!(browse, "browseEndpointContextSupportedConfigs", "browseEndpointContextMusicConfig", "pageType").str();
        return match page_type {
            Some(PAGE_ALBUM | PAGE_AUDIOBOOK) => {
                let meta = meta_of(&groups);
                let linked: Vec<ArtistRef> = meta.artists.iter().filter(|a| a.id.is_some()).cloned().collect();
                Some(MusicItem::Album(AlbumItem {
                    browse_id,
                    title,
                    artists_text: (!linked.is_empty()).then_some(meta.artists_text).flatten(),
                    artists: linked,
                    year: meta.year,
                    type_text: first_type_text(&groups),
                    thumbnail_url: thumbnail,
                    playlist_id: at!(
                        r,
                        "thumbnailOverlay",
                        "musicItemThumbnailOverlayRenderer",
                        "content",
                        "musicPlayButtonRenderer",
                        "playNavigationEndpoint",
                        "watchPlaylistEndpoint",
                        "playlistId"
                    )
                    .string(),
                    explicit: is_explicit(Some(r), "subtitleBadges"),
                }))
            }
            Some(PAGE_ARTIST | PAGE_USER_CHANNEL) => Some(MusicItem::Artist(ArtistItem {
                browse_id,
                name: title,
                subtitle: subtitle_without_type(&groups),
                thumbnail_url: thumbnail,
                is_channel: page_type == Some(PAGE_USER_CHANNEL),
            })),
            Some(PAGE_PLAYLIST) => Some(MusicItem::Playlist(PlaylistItem {
                playlist_id: strip_vl(&browse_id),
                title,
                subtitle: subtitle_without_type(&groups),
                thumbnail_url: thumbnail,
            })),
            _ => None,
        };
    }
    let watch = at!(nav, "watchEndpoint")?;
    let video_id = at!(watch, "videoId").string()?;
    let music_video_type = music_video_type(Some(watch));
    if music_video_type.as_deref() == Some("MUSIC_VIDEO_TYPE_PODCAST_EPISODE") {
        return None;
    }
    let meta = meta_of(&groups);
    Some(MusicItem::Track(Track {
        video_id,
        title,
        artists: meta.artists,
        artists_text: meta.artists_text,
        album_id: meta.album_id,
        album_title: meta.album_title,
        thumbnail_url: thumbnail,
        video_type: video_type_of(music_video_type.as_deref()),
        views_text: meta.views,
        duration_ms: parse_duration(meta.duration.as_deref()),
        duration_text: meta.duration,
        ..Default::default()
    }))
}

/// `musicNavigationButtonRenderer` → плитка настроения.
pub fn navigation_button(r: Option<&Value>) -> Option<MusicItem> {
    let title = at!(r, "buttonText").text().filter(|t| !t.is_empty())?;
    let browse = at!(r, "clickCommand", "browseEndpoint");
    let browse_id = at!(browse, "browseId").string()?;
    Some(MusicItem::Mood(MoodItem {
        title,
        browse_id,
        params: at!(browse, "params").string(),
        color: at!(r, "solid", "leftStripeColor").i64().map(|c| c as u32),
    }))
}

/// Строка очереди «Далее» (`playlistPanelVideoRenderer`).
pub fn panel_video(r: Option<&Value>) -> Option<Track> {
    let r = r?;
    let video_id = at!(r, "videoId").string().or_else(|| at!(r, "navigationEndpoint", "watchEndpoint", "videoId").string())?;
    let title = at!(r, "title").text()?.trim().to_owned();
    if title.is_empty() {
        return None;
    }
    let meta = meta_of(&groups(at!(r, "longBylineText").runs()));
    let duration = at!(r, "lengthText").text().map(|t| t.trim().to_owned());
    Some(Track {
        video_id,
        title,
        artists: meta.artists,
        artists_text: meta.artists_text.or_else(|| at!(r, "shortBylineText").text()),
        album_id: meta.album_id,
        album_title: meta.album_title,
        duration_ms: parse_duration(duration.as_deref()),
        duration_text: duration,
        thumbnail_url: at!(r, "thumbnail", "thumbnails").best_thumbnail(),
        explicit: is_explicit(Some(r), "badges"),
        video_type: video_type_of(music_video_type(at!(r, "navigationEndpoint", "watchEndpoint")).as_deref()),
        unavailable: at!(r, "unplayableText").is_some(),
        ..Default::default()
    })
}

// ── полки ──

pub fn items_of(contents: Option<&Value>) -> Vec<MusicItem> {
    contents
        .items()
        .iter()
        .filter_map(|item| {
            if let Some(row) = at!(item, "musicResponsiveListItemRenderer") {
                responsive_item(Some(row))
            } else if let Some(card) = at!(item, "musicTwoRowItemRenderer") {
                two_row_item(Some(card))
            } else if let Some(button) = at!(item, "musicNavigationButtonRenderer") {
                navigation_button(Some(button))
            } else {
                at!(item, "playlistPanelVideoRenderer").and_then(|panel| panel_video(Some(panel))).map(MusicItem::Track)
            }
        })
        .collect()
}

/// Полки `sectionListRenderer.contents`: списки, карусели, сетки.
#[allow(dead_code)] // каталог — срез 3
pub fn shelves(section_contents: Option<&Value>) -> Vec<Shelf> {
    let mut shelves = Vec::new();
    let more_of = |browse: Option<&Value>| (at!(browse, "browseId").string(), at!(browse, "params").string());
    for section in section_contents.items() {
        if let Some(list) = at!(section, "musicShelfRenderer") {
            let more = at!(list, "bottomEndpoint", "browseEndpoint")
                .or_else(|| at!(list, "title", "runs", 0, "navigationEndpoint", "browseEndpoint"));
            let (more_browse_id, more_params) = more_of(more);
            shelves.push(Shelf { title: at!(list, "title").text(), items: items_of(at!(list, "contents")), more_browse_id, more_params });
        } else if let Some(carousel) = at!(section, "musicCarouselShelfRenderer") {
            let header = at!(carousel, "header", "musicCarouselShelfBasicHeaderRenderer");
            let more = at!(header, "moreContentButton", "buttonRenderer", "navigationEndpoint", "browseEndpoint")
                .or_else(|| at!(header, "title", "runs", 0, "navigationEndpoint", "browseEndpoint"));
            let (more_browse_id, more_params) = more_of(more);
            shelves.push(Shelf {
                title: at!(header, "title").text(),
                items: items_of(at!(carousel, "contents")),
                more_browse_id,
                more_params,
            });
        } else if let Some(grid) = at!(section, "gridRenderer") {
            shelves.push(Shelf {
                title: at!(grid, "header", "gridHeaderRenderer", "title").text(),
                items: items_of(at!(grid, "items")),
                ..Default::default()
            });
        } else if let Some(playlist) = at!(section, "musicPlaylistShelfRenderer") {
            shelves.push(Shelf { items: items_of(at!(playlist, "contents")), ..Default::default() });
        } else if let Some(item_section) = at!(section, "itemSectionRenderer") {
            for inner in at!(item_section, "contents").items() {
                if let Some(grid) = at!(inner, "gridRenderer") {
                    shelves.push(Shelf {
                        title: at!(grid, "header", "gridHeaderRenderer", "title").text(),
                        items: items_of(at!(grid, "items")),
                        ..Default::default()
                    });
                }
            }
        }
    }
    shelves.retain(|s| !s.items.is_empty());
    shelves
}

/// Токен продолжения: и старый `continuations[].nextContinuationData`, и новый `continuationItemRenderer`.
pub fn continuation(shelf: Option<&Value>) -> Option<String> {
    let old = at!(shelf, "continuations", 0, "nextContinuationData", "continuation")
        .or_else(|| at!(shelf, "continuations", 0, "nextRadioContinuationData", "continuation"))
        .string();
    if old.is_some() {
        return old;
    }
    let contents = at!(shelf, "contents").or(shelf.filter(|v| v.is_array()));
    contents
        .items()
        .iter()
        .rev()
        .find_map(|c| at!(c, "continuationItemRenderer", "continuationEndpoint", "continuationCommand", "token").string())
}

/// Лучший результат выдачи «Всё» (`musicCardShelfRenderer`) — как строка списка.
pub fn card_top(card: &Value) -> Option<MusicItem> {
    let title_runs = at!(card, "title").runs();
    let first = title_runs.first()?;
    let mut synthetic = json!({
        "flexColumns": [
            {"musicResponsiveListItemFlexColumnRenderer": {"text": at!(card, "title").cloned()}},
            {"musicResponsiveListItemFlexColumnRenderer": {"text": at!(card, "subtitle").cloned()}},
        ],
        "thumbnail": at!(card, "thumbnail").cloned(),
        "navigationEndpoint": at!(card, "title", "runs", 0, "navigationEndpoint").cloned(),
    });
    if let Some(video_id) = &first.watch_video_id {
        synthetic["playlistItemData"] = json!({ "videoId": video_id });
    }
    responsive_item(Some(&synthetic))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_and_years() {
        assert!(is_duration("3:45"));
        assert!(is_duration("1:02:10"));
        assert!(!is_duration("345"));
        assert!(!is_duration("3:4"));
        assert!(is_year("2024"));
        assert!(!is_year("20245"));
    }
}
