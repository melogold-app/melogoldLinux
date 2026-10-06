//! Каталог YouTube Music и обычного YouTube (Windows `YouTubeMusic.cs`). Ошибки — [`YouTubeError`]
//! с классом для экрана.

use melogold_core::music::{
    distinct, AlbumDetails, AlbumItem, ArtistDetails, ArtistRef, ChannelPage, ItemsPage, MusicItem, NextPage, PlaylistDetails,
    PlaylistItem, SearchSummary, Shelf, Track,
};
use serde_json::{json, Value};

use crate::client::{ClientProfile, InnerTube};
use crate::json::Json;
use crate::{at, music_parsers as parsers, web_parsers, ErrorKind, YouTubeError};

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

    // ── разделы ──

    /// «Обзор» YTM: новые альбомы, настроения, «В тренде», новые клипы.
    pub async fn explore(&self) -> Result<Vec<Shelf>, YouTubeError> {
        self.browse_shelves("FEmusic_explore", None).await
    }

    pub async fn home(&self) -> Result<Vec<Shelf>, YouTubeError> {
        self.browse_shelves("FEmusic_home", None).await
    }

    /// Полки произвольной страницы: настроение, «Все» полки исполнителя, все новые релизы.
    pub async fn browse_shelves(&self, browse_id: &str, params: Option<&str>) -> Result<Vec<Shelf>, YouTubeError> {
        let mut body = json!({"browseId": browse_id});
        if let Some(params) = params {
            body["params"] = params.into();
        }
        let response = self.music("browse", body).await?;
        parse_browse_shelves(&response).ok_or_else(|| YouTubeError::new(ErrorKind::Parser, format!("no sections in {browse_id}")))
    }

    /// Полный список полки исполнителя («Все ›»): сетка альбомов/синглов и токен продолжения.
    /// Дальше — `playlist_continuation` (тот же `browse` с `continuation`).
    pub async fn browse_grid(&self, browse_id: &str, params: Option<&str>) -> Result<ItemsPage, YouTubeError> {
        let mut body = json!({"browseId": browse_id});
        if let Some(params) = params {
            body["params"] = params.into();
        }
        let response = self.music("browse", body).await?;
        parse_browse_grid(&response).ok_or_else(|| YouTubeError::new(ErrorKind::Parser, format!("no grid in {browse_id}")))
    }

    // ── страницы ──

    pub async fn album(&self, browse_id: &str) -> Result<AlbumDetails, YouTubeError> {
        let response = self.music("browse", json!({"browseId": browse_id})).await?;
        parse_album(browse_id, &response).ok_or_else(|| YouTubeError::new(ErrorKind::Parser, format!("album {browse_id} has no header")))
    }

    pub async fn playlist(&self, playlist_id: &str) -> Result<PlaylistDetails, YouTubeError> {
        let browse_id = if playlist_id.starts_with("VL") { playlist_id.to_owned() } else { format!("VL{playlist_id}") };
        let response = self.music("browse", json!({"browseId": browse_id})).await?;
        parse_playlist(&browse_id, &response)
            .ok_or_else(|| YouTubeError::new(ErrorKind::Parser, format!("playlist {playlist_id} has no content")))
    }

    /// Следующая страница плейлиста: ответы «старые» и «новые» (REWRITE §4.8.3).
    pub async fn playlist_continuation(&self, continuation: &str) -> Result<ItemsPage, YouTubeError> {
        let response = self.music("browse", json!({"continuation": continuation})).await?;
        Ok(parse_playlist_continuation(&response))
    }

    /// Весь плейлист со всеми продолжениями («Слушать», «Сохранить» длинного списка).
    pub async fn playlist_tracks(&self, playlist_id: &str, max: usize) -> Result<Vec<Track>, YouTubeError> {
        let page = self.playlist(playlist_id).await?;
        let mut seen: std::collections::HashSet<String> = page.tracks.iter().map(|t| t.video_id.clone()).collect();
        let mut tracks = page.tracks;
        let mut continuation = page.continuation;
        while let Some(token) = continuation.take() {
            if tracks.len() >= max {
                break;
            }
            let next = self.playlist_continuation(&token).await?;
            let empty = next.items.is_empty();
            tracks.extend(next.items.into_iter().filter_map(|i| match i {
                MusicItem::Track(t) if seen.insert(t.video_id.clone()) => Some(t),
                _ => None,
            }));
            if empty || next.continuation.as_deref() == Some(token.as_str()) {
                break;
            }
            continuation = next.continuation;
        }
        Ok(tracks)
    }

    /// Исполнитель YTM; если музыкального профиля нет — канал обычного YouTube (REWRITE §4.8.5).
    pub async fn artist(&self, browse_id: &str) -> Result<ArtistDetails, YouTubeError> {
        let response = self.music("browse", json!({"browseId": browse_id})).await?;
        if let Some(artist) = parse_artist(browse_id, &response) {
            return Ok(artist);
        }
        let channel = self.channel(browse_id).await?;
        Ok(ArtistDetails {
            browse_id: browse_id.to_owned(),
            name: channel.name,
            thumbnail_url: channel.thumbnail_url,
            subscribers_text: channel.subscribers_text,
            description: channel.description,
            is_channel: true,
            shelves: if channel.videos.is_empty() {
                Vec::new()
            } else {
                vec![Shelf { items: channel.videos.into_iter().map(MusicItem::Track).collect(), ..Default::default() }]
            },
            ..Default::default()
        })
    }

    /// Канал обычного YouTube (клиент WEB): шапка и вкладка «Видео».
    pub async fn channel(&self, channel_id: &str) -> Result<ChannelPage, YouTubeError> {
        let response = self.web("browse", json!({"browseId": channel_id, "params": "EgZ2aWRlb3PyBgQKAjoA"})).await?;
        Ok(web_parsers::channel_page(channel_id, &response))
    }

    pub async fn channel_continuation(&self, continuation: &str) -> Result<ItemsPage, YouTubeError> {
        let response = self.web("browse", json!({"continuation": continuation})).await?;
        Ok(web_parsers::grid_page(at!(&response, "onResponseReceivedActions", 0, "appendContinuationItemsAction", "continuationItems")))
    }

    /// `/@handle`, `/c/…`, `/user/…` → browseId канала (`navigation/resolve_url` клиента WEB).
    pub async fn resolve_url(&self, url: &str) -> Result<Option<String>, YouTubeError> {
        let response = self.web("navigation/resolve_url", json!({"url": url})).await?;
        Ok(at!(&response, "endpoint", "browseEndpoint", "browseId").string())
    }

    /// Вкладка «Похожие»: треки, альбомы, исполнители («Для вас», автовоспроизведение).
    pub async fn related(&self, related_browse_id: &str) -> Result<Vec<Shelf>, YouTubeError> {
        let response = self.music("browse", json!({"browseId": related_browse_id})).await?;
        Ok(parsers::shelves(at!(&response, "contents", "sectionListRenderer", "contents")))
    }

    // ── тексты ──

    /// Обычный текст вкладки «Текст» (`MPLYt…`) и подпись источника; `None` — текста нет.
    pub async fn lyrics(&self, browse_id: &str) -> Result<Option<(String, Option<String>)>, YouTubeError> {
        let response = self.music("browse", json!({ "browseId": browse_id })).await?;
        let shelf = Some(&response).find("musicDescriptionShelfRenderer");
        let text = at!(shelf, "description").text().filter(|t| !t.trim().is_empty());
        Ok(text.map(|t| (t, at!(shelf, "footer").text())))
    }

    /// Синхронный текст YouTube Music: ту же вкладку клиент ANDROID_MUSIC отдаёт со временем строк
    /// (`timedLyricsData`). LRC или `None`, если времени у строк нет.
    pub async fn timed_lyrics(&self, browse_id: &str) -> Result<Option<String>, YouTubeError> {
        let response = self.client.post(&ClientProfile::android_music(), "browse", json!({ "browseId": browse_id })).await?;
        let lines: Vec<String> = Some(&response)
            .find("timedLyricsData")
            .items()
            .iter()
            .filter_map(|line| {
                let start = at!(line, "cueRange", "startTimeMilliseconds").i64()?;
                let centis = start / 10;
                Some(format!(
                    "[{:02}:{:02}.{:02}]{}",
                    centis / 6000,
                    centis / 100 % 60,
                    centis % 100,
                    at!(line, "lyricLine").str().unwrap_or_default()
                ))
            })
            .collect();
        Ok((!lines.is_empty()).then(|| lines.join("\n")))
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

pub fn parse_channel(channel_id: &str, response: &Value) -> ChannelPage {
    web_parsers::channel_page(channel_id, response)
}

/// Страница «Все ›» полки исполнителя (`MPAD…` + `params`, сетка альбомов/синглов): карточки всех полок
/// страницы и токен продолжения сетки. `None` — у ответа нет секций.
pub fn parse_browse_grid(response: &Value) -> Option<ItemsPage> {
    let sections = at!(
        response,
        "contents",
        "singleColumnBrowseResultsRenderer",
        "tabs",
        0,
        "tabRenderer",
        "content",
        "sectionListRenderer",
        "contents"
    )?;
    let mut page = ItemsPage::default();
    for section in Some(sections).items() {
        let Some(grid) = at!(section, "gridRenderer").or_else(|| at!(section, "musicCarouselShelfRenderer")) else { continue };
        page.items.extend(parsers::items_of(at!(grid, "items").or_else(|| at!(grid, "contents"))));
        page.continuation = page.continuation.take().or_else(|| parsers::continuation(Some(grid)));
    }
    (!page.items.is_empty()).then_some(page)
}

pub fn parse_browse_shelves(response: &Value) -> Option<Vec<Shelf>> {
    let sections = at!(
        response,
        "contents",
        "singleColumnBrowseResultsRenderer",
        "tabs",
        0,
        "tabRenderer",
        "content",
        "sectionListRenderer",
        "contents"
    )
    .or_else(|| at!(response, "contents", "twoColumnBrowseResultsRenderer", "secondaryContents", "sectionListRenderer", "contents"))?;
    Some(parsers::shelves(Some(sections)))
}

fn year_of(text: &str) -> bool {
    text.len() == 4 && text.bytes().all(|b| b.is_ascii_digit())
}

pub fn parse_album(browse_id: &str, response: &Value) -> Option<AlbumDetails> {
    let header = at!(
        response,
        "contents",
        "twoColumnBrowseResultsRenderer",
        "tabs",
        0,
        "tabRenderer",
        "content",
        "sectionListRenderer",
        "contents",
        0,
        "musicResponsiveHeaderRenderer"
    )?;
    let secondary = at!(response, "contents", "twoColumnBrowseResultsRenderer", "secondaryContents", "sectionListRenderer", "contents")?;
    let title = at!(header, "title").text().unwrap_or_default();
    let subtitle = at!(header, "subtitle").runs();
    let year = subtitle.iter().map(|r| r.text.trim()).find(|t| year_of(t)).map(str::to_owned);
    let type_text = subtitle.iter().find(|r| !r.is_separator() && !r.text.trim().is_empty()).map(|r| r.text.trim().to_owned());
    let strapline = at!(header, "straplineTextOne");
    let artists: Vec<ArtistRef> =
        strapline.runs().into_iter().filter(|r| r.browse_id.is_some()).map(|r| ArtistRef { id: r.browse_id, name: r.text }).collect();
    let artists_text = strapline.text();
    let thumbnail = at!(header, "thumbnail", "musicThumbnailRenderer", "thumbnail", "thumbnails").best_thumbnail();
    let shelf = secondary.as_array()?.iter().find_map(|s| at!(s, "musicShelfRenderer"));
    let playlist_id = at!(shelf, "contents").items().iter().find_map(|c| {
        at!(
            c,
            "musicResponsiveListItemRenderer",
            "flexColumns",
            0,
            "musicResponsiveListItemFlexColumnRenderer",
            "text",
            "runs",
            0,
            "navigationEndpoint",
            "watchEndpoint",
            "playlistId"
        )
        .string()
    });
    let tracks = parsers::items_of(at!(shelf, "contents"))
        .into_iter()
        .filter_map(|i| match i {
            MusicItem::Track(t) => Some(t),
            _ => None,
        })
        .map(|t| {
            let linked = t.artists.iter().any(|a| a.id.is_some());
            Track {
                album_id: Some(browse_id.to_owned()),
                album_title: Some(title.clone()),
                thumbnail_url: t.thumbnail_url.clone().or_else(|| thumbnail.clone()),
                artists: if linked { t.artists.clone() } else { artists.clone() },
                artists_text: if linked { t.artists_text.clone() } else { artists_text.clone() },
                video_type: t.video_type.clone().or_else(|| Some("song".into())),
                ..t
            }
        })
        .collect();
    let album = AlbumItem {
        browse_id: browse_id.to_owned(),
        title,
        artists,
        artists_text,
        year,
        type_text,
        thumbnail_url: thumbnail,
        playlist_id,
        explicit: false,
    };
    Some(AlbumDetails {
        album,
        description: at!(header, "description", "musicDescriptionShelfRenderer", "description").text(),
        count_text: at!(header, "secondSubtitle").text(),
        tracks,
        shelves: parsers::shelves(Some(secondary)).into_iter().filter(|s| !matches!(s.items.first(), Some(MusicItem::Track(_)))).collect(),
    })
}

pub fn parse_playlist(browse_id: &str, response: &Value) -> Option<PlaylistDetails> {
    let tab = at!(
        response,
        "contents",
        "twoColumnBrowseResultsRenderer",
        "tabs",
        0,
        "tabRenderer",
        "content",
        "sectionListRenderer",
        "contents",
        0
    );
    let header = at!(tab, "musicResponsiveHeaderRenderer")
        .or_else(|| at!(tab, "musicEditablePlaylistDetailHeaderRenderer", "header", "musicResponsiveHeaderRenderer"))
        .or_else(|| at!(response, "header", "musicDetailHeaderRenderer"));
    let secondary = at!(response, "contents", "twoColumnBrowseResultsRenderer", "secondaryContents", "sectionListRenderer", "contents")
        .or_else(|| {
            at!(
                response,
                "contents",
                "singleColumnBrowseResultsRenderer",
                "tabs",
                0,
                "tabRenderer",
                "content",
                "sectionListRenderer",
                "contents"
            )
        });
    let shelf = secondary.items().iter().find_map(|s| at!(s, "musicPlaylistShelfRenderer").or_else(|| at!(s, "musicShelfRenderer")));
    if header.is_none() && shelf.is_none() {
        return None;
    }
    let id = browse_id.strip_prefix("VL").unwrap_or(browse_id).to_owned();
    let title = at!(header, "title").text().unwrap_or_default();
    let thumbnail = at!(header, "thumbnail", "musicThumbnailRenderer", "thumbnail", "thumbnails")
        .best_thumbnail()
        .or_else(|| at!(header, "thumbnail", "croppedSquareThumbnailRenderer", "thumbnail", "thumbnails").best_thumbnail());
    let author =
        at!(header, "straplineTextOne").text().or_else(|| at!(header, "facepile", "avatarStackViewModel", "text", "content").string());
    let tracks = parsers::items_of(at!(shelf, "contents"))
        .into_iter()
        .filter_map(|i| match i {
            MusicItem::Track(t) => Some(t),
            _ => None,
        })
        .collect();
    Some(PlaylistDetails {
        playlist: PlaylistItem { playlist_id: id, title, thumbnail_url: thumbnail, subtitle: author.clone() },
        author_text: author,
        description: at!(header, "description", "musicDescriptionShelfRenderer", "description").text(),
        count_text: at!(header, "secondSubtitle").text(),
        tracks,
        continuation: parsers::continuation(shelf),
    })
}

pub fn parse_playlist_continuation(response: &Value) -> ItemsPage {
    if let Some(old) = at!(response, "continuationContents", "musicPlaylistShelfContinuation")
        .or_else(|| at!(response, "continuationContents", "musicShelfContinuation"))
    {
        return ItemsPage { items: parsers::items_of(at!(old, "contents")), continuation: parsers::continuation(Some(old)) };
    }
    if let Some(grid) = at!(response, "continuationContents", "gridContinuation") {
        return ItemsPage { items: parsers::items_of(at!(grid, "items")), continuation: parsers::continuation(Some(grid)) };
    }
    let appended = at!(response, "onResponseReceivedActions", 0, "appendContinuationItemsAction", "continuationItems");
    ItemsPage { items: parsers::items_of(appended), continuation: parsers::continuation(appended) }
}

/// Исполнитель YTM; `None` — музыкального профиля нет, нужен канал обычного YouTube.
///
/// Исполнитель — только если у YTM есть «Популярное» (список песен), альбомы или синглы (REWRITE
/// §3.7.2): одних клипов и плейлистов мало — архивный канал с двумя клипами читается лучше как канал.
pub fn parse_artist(browse_id: &str, response: &Value) -> Option<ArtistDetails> {
    let header = at!(response, "header", "musicImmersiveHeaderRenderer")
        .or_else(|| at!(response, "header", "musicVisualHeaderRenderer"))
        .or_else(|| at!(response, "header", "musicHeaderRenderer"))?;
    let sections = at!(
        response,
        "contents",
        "singleColumnBrowseResultsRenderer",
        "tabs",
        0,
        "tabRenderer",
        "content",
        "sectionListRenderer",
        "contents"
    );
    let shelves = parsers::shelves(sections);
    let songs_shelf = sections.items().iter().find_map(|s| at!(s, "musicShelfRenderer"));
    let has_albums = shelves.iter().any(|s| s.items.iter().any(|i| matches!(i, MusicItem::Album(_))));
    if songs_shelf.is_none() && !has_albums {
        return None;
    }
    let songs_browse = at!(songs_shelf, "title", "runs", 0, "navigationEndpoint", "browseEndpoint", "browseId")
        .or_else(|| at!(songs_shelf, "bottomEndpoint", "browseEndpoint", "browseId"))
        .str();
    Some(ArtistDetails {
        browse_id: browse_id.to_owned(),
        name: at!(header, "title").text().unwrap_or_default(),
        description: at!(header, "description").text(),
        thumbnail_url: at!(header, "thumbnail", "musicThumbnailRenderer", "thumbnail", "thumbnails").best_thumbnail(),
        subscribers_text: at!(header, "subscriptionButton", "subscribeButtonRenderer", "longSubscriberCountText")
            .text()
            .or_else(|| at!(header, "monthlyListenerCount").text()),
        monthly_listeners_text: at!(header, "monthlyListenerCount").text(),
        subscriber_count: at!(header, "subscriptionButton", "subscribeButtonRenderer", "subscriberCountText").text(),
        views_text: sections
            .items()
            .iter()
            .find_map(|s| at!(s, "musicDescriptionShelfRenderer", "subheader").text().filter(|t| !t.trim().is_empty())),
        is_channel: false,
        shelves,
        songs_playlist_id: songs_browse.and_then(|b| b.strip_prefix("VL")).map(str::to_owned),
        radio_playlist_id: at!(header, "startRadioButton", "buttonRenderer", "navigationEndpoint", "watchPlaylistEndpoint", "playlistId")
            .or_else(|| at!(header, "startRadioButton", "buttonRenderer", "navigationEndpoint", "watchEndpoint", "playlistId"))
            .string(),
    })
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
