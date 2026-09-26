//! Разбор ответов обычного YouTube (клиент WEB, REWRITE §4.8.1–§4.8.2), порт Windows
//! `WebParsers.cs`: `videoRenderer`, `channelRenderer`, `lockupViewModel` (видео и плейлисты в
//! новой разметке). Shorts, полки Shorts и промо отбрасываются.

use melogold_core::music::{ArtistItem, ArtistRef, ItemsPage, MusicItem, PlaylistItem, Track};
use melogold_core::text::parse_duration;
use serde_json::Value;

use crate::at;
use crate::json::Json;

fn is_duration(text: &str) -> bool {
    parse_duration(Some(text)).is_some()
}

pub fn search_page(sections: Option<&Value>) -> ItemsPage {
    let mut page = ItemsPage::default();
    for section in sections.items() {
        if let Some(item_section) = at!(section, "itemSectionRenderer") {
            page.items.extend(at!(item_section, "contents").items().iter().filter_map(|i| item(Some(i))));
        } else if let Some(more) = at!(section, "continuationItemRenderer") {
            page.continuation = at!(more, "continuationEndpoint", "continuationCommand", "token").string();
        } else if let Some(parsed) = item(Some(section)) {
            page.items.push(parsed);
        }
    }
    page
}

#[allow(dead_code)] // канал — срез 3
pub fn grid_page(contents: Option<&Value>) -> ItemsPage {
    let mut page = ItemsPage::default();
    for entry in contents.items() {
        if let Some(more) = at!(entry, "continuationItemRenderer") {
            page.continuation = at!(more, "continuationEndpoint", "continuationCommand", "token").string();
        } else if let Some(parsed) = item(at!(entry, "richItemRenderer", "content").or(Some(entry))) {
            page.items.push(parsed);
        }
    }
    page
}

fn item(node: Option<&Value>) -> Option<MusicItem> {
    let node = node?;
    if let Some(video_renderer) = at!(node, "videoRenderer") {
        return video(video_renderer).map(MusicItem::Track);
    }
    if let Some(channel_renderer) = at!(node, "channelRenderer") {
        return channel(channel_renderer);
    }
    if let Some(lockup_view) = at!(node, "lockupViewModel") {
        return lockup(lockup_view);
    }
    let playlist = at!(node, "playlistRenderer")?;
    let id = at!(playlist, "playlistId").string()?;
    let title = at!(playlist, "title").text().filter(|t| !t.is_empty())?;
    Some(MusicItem::Playlist(PlaylistItem {
        playlist_id: id,
        title,
        subtitle: at!(playlist, "longBylineText").text(),
        thumbnail_url: at!(playlist, "thumbnails", 0, "thumbnails").best_thumbnail(),
    }))
}

pub fn video(r: &Value) -> Option<Track> {
    let video_id = at!(r, "videoId").string()?;
    let title = at!(r, "title").text().filter(|t| !t.is_empty())?;
    let overlays = at!(r, "thumbnailOverlays").items();
    let overlay_style = |style: &str| overlays.iter().any(|o| at!(o, "thumbnailOverlayTimeStatusRenderer", "style").str() == Some(style));
    if at!(r, "navigationEndpoint", "reelWatchEndpoint").is_some() || overlay_style("SHORTS") {
        return None;
    }
    let is_live =
        at!(r, "badges").items().iter().any(|b| at!(b, "metadataBadgeRenderer", "style").str() == Some("BADGE_STYLE_TYPE_LIVE_NOW"))
            || overlay_style("LIVE");
    let owner = at!(r, "ownerText").runs().into_iter().next().or_else(|| at!(r, "longBylineText").runs().into_iter().next());
    let channel_name = owner.as_ref().map(|o| o.text.trim().to_owned());
    let duration = if is_live { None } else { at!(r, "lengthText").text() };
    Some(Track {
        video_id,
        title,
        artists: owner.map(|o| vec![ArtistRef { id: o.browse_id, name: o.text.trim().to_owned() }]).unwrap_or_default(),
        artists_text: channel_name,
        duration_ms: parse_duration(duration.as_deref()),
        duration_text: duration,
        thumbnail_url: at!(r, "thumbnail", "thumbnails").best_thumbnail(),
        video_type: Some(if is_live { "live" } else { "ugc" }.into()),
        views_text: at!(r, "shortViewCountText").text().or_else(|| at!(r, "viewCountText").text()),
        ..Default::default()
    })
}

fn channel(r: &Value) -> Option<MusicItem> {
    let id = at!(r, "channelId").string()?;
    let title = at!(r, "title").text().filter(|t| !t.is_empty())?;
    let thumbnail = at!(r, "thumbnail", "thumbnails").best_thumbnail().map(|t| if t.starts_with("//") { format!("https:{t}") } else { t });
    Some(MusicItem::Artist(ArtistItem {
        browse_id: id,
        name: title,
        subtitle: at!(r, "videoCountText").text().or_else(|| at!(r, "subscriberCountText").text()),
        thumbnail_url: thumbnail,
        is_channel: true,
    }))
}

/// `lockupViewModel`: видео или плейлист новой разметки YouTube.
fn lockup(r: &Value) -> Option<MusicItem> {
    let id = at!(r, "contentId").string()?;
    let meta = at!(r, "metadata", "lockupMetadataViewModel");
    let title = at!(meta, "title", "content").string().filter(|t| !t.is_empty())?;
    let rows: Vec<Vec<String>> = at!(meta, "metadata", "contentMetadataViewModel", "metadataRows")
        .items()
        .iter()
        .map(|row| {
            at!(row, "metadataParts").items().iter().filter_map(|p| at!(p, "text", "content").string()).filter(|t| !t.is_empty()).collect()
        })
        .filter(|parts: &Vec<String>| !parts.is_empty())
        .collect();
    let image = at!(r, "contentImage", "thumbnailViewModel", "image", "sources")
        .or_else(|| at!(r, "contentImage", "collectionThumbnailViewModel", "primaryThumbnail", "thumbnailViewModel", "image", "sources"));
    let thumbnail = image.best_thumbnail();
    let badges = Some(r).find_all("thumbnailBadgeViewModel");
    let badge = badges.iter().filter_map(|b| at!(*b, "text").string()).find(|t| is_duration(t));
    match at!(r, "contentType").str()? {
        "LOCKUP_CONTENT_TYPE_VIDEO" => {
            // В строках метаданных видео на канале — «просмотры · дата», в поиске — «канал», затем «просмотры · дата».
            let channel_name = if rows.len() > 1 { rows[0].first().cloned() } else { None };
            let channel_id =
                meta.find_all("browseEndpoint").into_iter().filter_map(|b| at!(b, "browseId").string()).find(|b| b.starts_with("UC"));
            let live = badge.is_none() && badges.iter().any(|b| at!(*b, "badgeStyle").str() == Some("THUMBNAIL_OVERLAY_BADGE_STYLE_LIVE"));
            Some(MusicItem::Track(Track {
                video_id: id,
                title,
                artists: channel_name.clone().map(|name| vec![ArtistRef { id: channel_id, name }]).unwrap_or_default(),
                artists_text: channel_name,
                duration_ms: parse_duration(badge.as_deref()),
                duration_text: badge,
                thumbnail_url: thumbnail,
                video_type: Some(if live { "live" } else { "ugc" }.into()),
                views_text: rows.last().and_then(|row| row.first().cloned()),
                ..Default::default()
            }))
        }
        "LOCKUP_CONTENT_TYPE_PLAYLIST" | "LOCKUP_CONTENT_TYPE_ALBUM" => {
            // Миксы RD… — бесконечные очереди, а не плейлисты.
            if id.starts_with("RD") && !id.starts_with("RDCLAK") {
                return None;
            }
            Some(MusicItem::Playlist(PlaylistItem {
                playlist_id: id,
                title,
                subtitle: rows.first().map(|row| row.join(" · ")),
                thumbnail_url: thumbnail,
            }))
        }
        _ => None,
    }
}
