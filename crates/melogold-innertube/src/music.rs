//! Каталог YouTube Music и обычного YouTube (Windows `YouTubeMusic.cs`). Ошибки — [`YouTubeError`]
//! с классом для экрана.

use melogold_core::music::{distinct, ArtistRef, ItemsPage, MusicItem, NextPage, SearchSummary, Track};
use serde_json::{json, Value};

use crate::client::{ClientProfile, InnerTube};
use crate::json::Json;
use crate::{at, music_parsers as parsers, web_parsers, YouTubeError};

/// Фильтр выдачи YouTube Music (параметры чипов поиска).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicFilter {
    Songs,
    Videos,
    Albums,
    Artists,
    CommunityPlaylists,
    FeaturedPlaylists,
}

impl MusicFilter {
    fn params(self) -> &'static str {
        match self {
            MusicFilter::Songs => "EgWKAQIIAWoSEAMQCRAEEAUQChAQEBUQDhAR",
            MusicFilter::Videos => "EgWKAQIQAWoSEAMQCRAEEAUQChAQEBUQDhAR",
            MusicFilter::Albums => "EgWKAQIYAWoSEAMQCRAEEAUQChAQEBUQDhAR",
            MusicFilter::Artists => "EgWKAQIgAWoSEAMQCRAEEAUQChAQEBUQDhAR",
            MusicFilter::CommunityPlaylists => "EgeKAQQoAEABahIQAxAJEAQQBRAKEBAQFRAOEBE=",
            MusicFilter::FeaturedPlaylists => "EgeKAQQoADgBahIQAxAJEAQQBRAKEBAQFRAOEBE=",
        }
    }
}

/// Фильтр выдачи обычного YouTube (REWRITE §4.8.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebFilter {
    Videos,
    Channels,
    Playlists,
    Live,
}

impl WebFilter {
    fn params(self) -> &'static str {
        match self {
            WebFilter::Videos => "EgIQAQ==",
            WebFilter::Channels => "EgIQAg==",
            WebFilter::Playlists => "EgIQAw==",
            WebFilter::Live => "EgJAAQ==",
        }
    }
}

#[derive(Clone)]
pub struct YouTubeMusic {
    client: InnerTube,
}

impl YouTubeMusic {
    pub fn new(client: InnerTube) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &InnerTube {
        &self.client
    }

    async fn music(&self, endpoint: &str, body: Value) -> Result<Value, YouTubeError> {
        self.client.post(&ClientProfile::web_remix(), endpoint, body).await
    }

    async fn web(&self, endpoint: &str, body: Value) -> Result<Value, YouTubeError> {
        self.client.post(&ClientProfile::web(), endpoint, body).await
    }

    // ── поиск ──

    /// YTM без фильтра: лучший результат и смешанная выдача (сегмент «Всё», REWRITE §4.8.5).
    pub async fn search_summary(&self, query: &str) -> Result<SearchSummary, YouTubeError> {
        let response = self.music("search", json!({"query": query})).await?;
        Ok(parse_search_summary(&response))
    }

    /// YTM с фильтром; продолжение — [`Self::search_continuation`].
    pub async fn search(&self, query: &str, filter: MusicFilter) -> Result<ItemsPage, YouTubeError> {
        let response = self.music("search", json!({"query": query, "params": filter.params()})).await?;
        Ok(parse_search(&response))
    }

    pub async fn search_continuation(&self, continuation: &str) -> Result<ItemsPage, YouTubeError> {
        let response = self.music("search", json!({"continuation": continuation})).await?;
        if let Some(shelf) = at!(&response, "continuationContents", "musicShelfContinuation") {
            return Ok(ItemsPage { items: parsers::items_of(at!(shelf, "contents")), continuation: parsers::continuation(Some(shelf)) });
        }
        let appended = at!(&response, "onResponseReceivedCommands", 0, "appendContinuationItemsAction", "continuationItems")
            .or_else(|| at!(&response, "onResponseReceivedActions", 0, "appendContinuationItemsAction", "continuationItems"));
        Ok(ItemsPage { items: parsers::items_of(appended), continuation: parsers::continuation(appended) })
    }

    pub async fn suggestions(&self, input: &str) -> Result<Vec<String>, YouTubeError> {
        let response = self.music("music/get_search_suggestions", json!({"input": input})).await?;
        Ok(parse_suggestions(&response))
    }

    /// Обычный YouTube (клиент WEB): видео, каналы, плейлисты, трансляции.
    pub async fn search_web(&self, query: &str, filter: WebFilter) -> Result<ItemsPage, YouTubeError> {
        let response = self.web("search", json!({"query": query, "params": filter.params()})).await?;
        Ok(web_parsers::search_page(at!(
            &response,
            "contents",
            "twoColumnSearchResultsRenderer",
            "primaryContents",
            "sectionListRenderer",
            "contents"
        )))
    }

    pub async fn search_web_continuation(&self, continuation: &str) -> Result<ItemsPage, YouTubeError> {
        let response = self.web("search", json!({"continuation": continuation})).await?;
        Ok(web_parsers::search_page(at!(&response, "onResponseReceivedCommands", 0, "appendContinuationItemsAction", "continuationItems")))
    }

    // ── «Далее» ──

    /// Очередь «Далее»: радио по треку (`RDAMVM<videoId>`, REWRITE §4.10.5) или плейлист с этого трека.
    pub async fn next(&self, video_id: &str, playlist_id: Option<&str>) -> Result<NextPage, YouTubeError> {
        let mut body = json!({
            "videoId": video_id,
            "isAudioOnly": true,
            "enablePersistentPlaylistPanel": true,
            "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
        });
        if let Some(playlist_id) = playlist_id {
            body["playlistId"] = playlist_id.into();
        }
        let response = self.music("next", body).await?;
        Ok(parse_next(&response))
    }

    pub async fn next_continuation(&self, continuation: &str, playlist_id: Option<&str>) -> Result<NextPage, YouTubeError> {
        let mut body = json!({"continuation": continuation, "isAudioOnly": true, "enablePersistentPlaylistPanel": true});
        if let Some(playlist_id) = playlist_id {
            body["playlistId"] = playlist_id.into();
        }
        let response = self.music("next", body).await?;
        let panel = at!(&response, "continuationContents", "playlistPanelContinuation");
        Ok(NextPage {
            tracks: panel_tracks(panel),
            continuation: parsers::continuation(panel),
            playlist_id: playlist_id.map(str::to_owned),
            ..Default::default()
        })
    }
}

pub fn parse_search_summary(response: &Value) -> SearchSummary {
    let sections =
        at!(response, "contents", "tabbedSearchResultsRenderer", "tabs", 0, "tabRenderer", "content", "sectionListRenderer", "contents");
    let mut top = None;
    let mut items = Vec::new();
    for section in sections.items() {
        if let Some(card) = at!(section, "musicCardShelfRenderer") {
            let card_top = parsers::card_top(card);
            let mut card_items = parsers::items_of(at!(card, "contents"));
            // В карточке исполнителя его песни идут без исполнителя в подписи: «Song • 94M plays».
            // Без правки число прослушиваний стало бы исполнителем.
            if let Some(MusicItem::Artist(artist)) = &card_top {
                for item in &mut card_items {
                    if let MusicItem::Track(track) = item {
                        if track.artists.iter().all(|a| a.id.is_none()) {
                            if track.artists_text.as_deref().is_some_and(|t| t.chars().any(|c| c.is_ascii_digit())) {
                                track.views_text = track.artists_text.take();
                            }
                            track.artists = vec![ArtistRef { id: Some(artist.browse_id.clone()), name: artist.name.clone() }];
                            track.artists_text = Some(artist.name.clone());
                        }
                    }
                }
            }
            if top.is_none() {
                top = card_top;
            }
            items.extend(card_items);
        } else if let Some(shelf) = at!(section, "itemSectionRenderer").or_else(|| at!(section, "musicShelfRenderer")) {
            items.extend(parsers::items_of(at!(shelf, "contents")));
        }
    }
    SearchSummary { top, items: distinct(items) }
}

pub fn parse_search(response: &Value) -> ItemsPage {
    let sections =
        at!(response, "contents", "tabbedSearchResultsRenderer", "tabs", 0, "tabRenderer", "content", "sectionListRenderer", "contents");
    let mut items = Vec::new();
    let mut continuation = None;
    for section in sections.items() {
        let Some(shelf) = at!(section, "musicShelfRenderer").or_else(|| at!(section, "itemSectionRenderer")) else { continue };
        items.extend(parsers::items_of(at!(shelf, "contents")));
        if continuation.is_none() {
            continuation = parsers::continuation(Some(shelf));
        }
    }
    ItemsPage { items: distinct(items), continuation }
}

pub fn parse_web_search(response: &Value) -> ItemsPage {
    web_parsers::search_page(at!(
        response,
        "contents",
        "twoColumnSearchResultsRenderer",
        "primaryContents",
        "sectionListRenderer",
        "contents"
    ))
}

pub fn parse_suggestions(response: &Value) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    Some(response)
        .find_all("searchSuggestionRenderer")
        .into_iter()
        .filter_map(|s| at!(s, "suggestion").text())
        .filter(|s| !s.trim().is_empty() && seen.insert(s.clone()))
        .take(10)
        .collect()
}

pub fn parse_next(response: &Value) -> NextPage {
    let tabs =
        at!(response, "contents", "singleColumnMusicWatchNextResultsRenderer", "tabbedRenderer", "watchNextTabbedResultsRenderer", "tabs");
    let panel = at!(tabs, 0, "tabRenderer", "content", "musicQueueRenderer", "content", "playlistPanelRenderer");
    let mut page = NextPage {
        tracks: panel_tracks(panel),
        continuation: parsers::continuation(panel),
        playlist_id: at!(panel, "playlistId").string(),
        ..Default::default()
    };
    for tab in tabs.items() {
        let browse = at!(tab, "tabRenderer", "endpoint", "browseEndpoint");
        let page_type = at!(browse, "browseEndpointContextSupportedConfigs", "browseEndpointContextMusicConfig", "pageType").str();
        match page_type {
            Some("MUSIC_PAGE_TYPE_TRACK_LYRICS") if !at!(tab, "tabRenderer", "unselectable").flag() => {
                page.lyrics_browse_id = at!(browse, "browseId").string();
            }
            Some("MUSIC_PAGE_TYPE_TRACK_RELATED") => page.related_browse_id = at!(browse, "browseId").string(),
            _ => {}
        }
    }
    page
}

fn panel_tracks(panel: Option<&Value>) -> Vec<Track> {
    at!(panel, "contents")
        .items()
        .iter()
        .filter_map(|item| {
            let renderer = at!(item, "playlistPanelVideoRenderer")
                .or_else(|| at!(item, "playlistPanelVideoWrapperRenderer", "primaryRenderer", "playlistPanelVideoRenderer"));
            parsers::panel_video(renderer)
        })
        .collect()
}

/// Треки из элементов выдачи (для «Играть» из полки).
pub fn tracks_of(items: &[MusicItem]) -> Vec<Track> {
    items.iter().filter_map(MusicItem::as_track).cloned().collect()
}
