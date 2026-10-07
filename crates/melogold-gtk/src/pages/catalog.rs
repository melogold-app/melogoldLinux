//! Страницы каталога (§5.4, REWRITE §3.3–§3.9): «Тренды», «Новое», альбом, исполнитель и канал,
//! плейлист YouTube с продолжениями, настроение и страницы «Все».

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::music::{ArtistItem, MusicItem, Shelf, Track};
use melogold_innertube::YouTubeError;
use melogold_playback::engine::Command;

use crate::catalog_widgets::{
    card, card_grid, description, mood_grid, shelf_view, track_list, CollectionHeader, DescriptionOwner, Toggle, TrackContext, TrackList,
};
use crate::localization::tr;
use crate::widgets::{catalog_error, StateView};
use crate::window::MainWindow;

/// Прокручиваемая страница: содержимое по ширине до 1100, состояния загрузки и ошибки.
struct Scaffold {
    page: adw::NavigationPage,
    content: gtk::Box,
    state: StateView,
    scroller: gtk::ScrolledWindow,
    /// Над колонкой страницы, во всю ширину: шапка исполнителя (задание 0019). У остальных страниц пусто.
    hero: gtk::Box,
    /// Поверх верха страницы: полоса с именем исполнителя при прокрутке.
    sticky: gtk::Box,
}

fn scaffold(title: &str, tag: Option<&str>, heading: bool) -> Scaffold {
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    let state = StateView::new(&content);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    if heading {
        let label = gtk::Label::builder().label(title).xalign(0.0).wrap(true).margin_bottom(6).build();
        label.add_css_class("title-1");
        label.add_css_class("page-title");
        body.append(&label);
    }
    body.append(&state.root);
    let clamp = adw::Clamp::builder().maximum_size(1100).child(&body).build();
    let hero = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    column.append(&hero);
    column.append(&clamp);
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&column).vexpand(true).build();
    let sticky = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Start).build();
    let overlay = gtk::Overlay::builder().child(&scroller).build();
    overlay.add_overlay(&sticky);
    let mut builder = adw::NavigationPage::builder().title(title).child(&overlay);
    if let Some(tag) = tag {
        builder = builder.tag(tag);
    }
    Scaffold { page: builder.build(), content, state, scroller, hero, sticky }
}

/// Загрузить в рантайме tokio и показать; ошибка — текст по классу и «Повторить».
fn load<T, F, Fut>(window: &MainWindow, scaffold: &Scaffold, fetch: F, show: impl Fn(&MainWindow, &gtk::Box, T) + 'static)
where
    T: Send + 'static,
    F: Fn() -> Fut + 'static,
    Fut: Future<Output = Result<T, YouTubeError>> + Send + 'static,
{
    let (weak, content, state) = (window.downgrade(), scaffold.content.clone(), scaffold.state.clone());
    let fetch = Rc::new(fetch);
    let show = Rc::new(show);
    let run: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::default();
    let again = Rc::clone(&run);
    let runner: Rc<dyn Fn()> = Rc::new(move || {
        let Some(window) = weak.upgrade() else { return };
        state.loading();
        let task = window.ctx.services.run(fetch());
        let (weak, content, state, show, again) = (window.downgrade(), content.clone(), state.clone(), Rc::clone(&show), Rc::clone(&again));
        glib::spawn_future_local(async move {
            let Some(window) = weak.upgrade() else { return };
            match task.await {
                Some(Ok(value)) => {
                    while let Some(child) = content.first_child() {
                        content.remove(&child);
                    }
                    show(&window, &content, value);
                    state.content();
                }
                result => {
                    let kind = match result {
                        Some(Err(error)) => {
                            tracing::warn!(%error, "каталог не загрузился");
                            error.kind
                        }
                        _ => melogold_innertube::ErrorKind::Unknown,
                    };
                    let again = again.borrow().clone();
                    state.error(catalog_error(kind), move || {
                        if let Some(again) = &again {
                            again();
                        }
                    });
                }
            }
        });
    });
    run.replace(Some(Rc::clone(&runner)));
    runner();
}

// ── разделы ──

/// «Тренды» (REWRITE §3.3): «В тренде» — чарт обзора YTM, «Все ›» — плейлист чарта; «Настроения и
/// жанры» — плитки, «Все ›» — все настроения. Стартовый раздел при первом запуске.
pub fn trends(window: &MainWindow) -> adw::NavigationPage {
    let page = scaffold(tr("TrendsHeader"), Some("root"), true);
    let music = window.ctx.services.music.clone();
    load(
        window,
        &page,
        move || {
            let music = music.clone();
            async move { music.explore().await }
        },
        |window, content, shelves: Vec<Shelf>| {
            // Треки обзора — «В тренде»: играют списком.
            for shelf in shelves.iter().filter(|s| !s.items.is_empty() && s.items.iter().all(|i| matches!(i, MusicItem::Track(_)))) {
                if shelf.tracks().any(|t| !t.is_video() || t.duration_text.is_some()) {
                    content.append(&shelf_view(window, shelf, 10, TrackContext::List, None));
                }
            }
            for shelf in shelves.iter().filter(|s| !s.items.is_empty() && s.items.iter().all(|i| matches!(i, MusicItem::Mood(_)))) {
                content.append(&shelf_view(window, shelf, 10, TrackContext::List, None));
            }
        },
    );
    page.page
}

/// «Новое» (REWRITE §3.4): новые альбомы и синглы, рекомендации, новые клипы. «Для вас» из истории
/// появится вместе с библиотекой (срез 4); пока истории нет — рекомендации главной YouTube Music.
pub fn new_page(window: &MainWindow) -> adw::NavigationPage {
    let page = scaffold(tr("NewHeader"), Some("root"), true);
    let music = window.ctx.services.music.clone();
    load(
        window,
        &page,
        move || {
            let music = music.clone();
            async move {
                let explore = music.explore().await?;
                let home = music.home().await.unwrap_or_default();
                Ok((explore, home))
            }
        },
        |window, content, (explore, home): (Vec<Shelf>, Vec<Shelf>)| {
            let releases = explore
                .iter()
                .find(|s| s.more_browse_id.as_deref() == Some("FEmusic_new_releases_albums"))
                .or_else(|| explore.iter().find(|s| !s.items.is_empty() && s.items.iter().all(|i| matches!(i, MusicItem::Album(_)))));
            if let Some(releases) = releases {
                let shelf = Shelf { title: Some(tr("NewReleases").to_owned()), ..releases.clone() };
                content.append(&shelf_view(window, &shelf, 10, TrackContext::Single, None));
            }
            for shelf in home.iter().take(4) {
                content.append(&shelf_view(window, shelf, 10, TrackContext::List, None));
            }
            if let Some(videos) = explore.iter().find(|s| s.more_browse_id.as_deref() == Some("FEmusic_new_releases_videos")) {
                content.append(&shelf_view(window, videos, 10, TrackContext::Single, None));
            }
        },
    );
    page.page
}

// ── детальные экраны ──

pub fn album_page(window: &MainWindow, browse_id: &str) -> adw::NavigationPage {
    let page = scaffold("Melogold", None, false);
    let (music, id) = (window.ctx.services.music.clone(), browse_id.to_owned());
    let title_page = page.page.clone();
    load(
        window,
        &page,
        move || {
            let (music, id) = (music.clone(), id.clone());
            async move { music.album(&id).await }
        },
        move |window, content, album| {
            title_page.set_title(&album.album.title);
            let subtitle = [album.album.type_text.as_deref(), album.album.artists_text.as_deref(), album.album.year.as_deref()]
                .iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>()
                .join(" · ");
            let header = CollectionHeader::new(
                window,
                &album.album.title,
                &subtitle,
                album.count_text.as_deref(),
                album.album.thumbnail_url.as_deref(),
                false,
            );
            add_play_buttons(window, &header, album.tracks.clone());
            let (weak, item) = (window.downgrade(), album.album.clone());
            let save =
                header.add_toggle([tr("SaveToLibrary"), tr("InLibrary")], ["list-add-symbolic", "object-select-symbolic"], move |on| {
                    let Some(window) = weak.upgrade() else { return };
                    let item = item.clone();
                    let task = window.ctx.services.db(move |library| library.set_album_saved(&item, on));
                    glib::spawn_future_local(async move {
                        if let Some(Err(error)) = task.await {
                            tracing::warn!(%error, "альбом не сохранился");
                        }
                    });
                });
            bookmark_state(window, &save, {
                let id = album.album.browse_id.clone();
                move |library| library.is_album_saved(&id).unwrap_or(false)
            });
            if let Some(artist) = album.album.artists.iter().find_map(|a| a.id.clone()) {
                let weak = window.downgrade();
                header.add_button(tr("MenuGoToArtist"), "avatar-default-symbolic", false, move || {
                    if let Some(window) = weak.upgrade() {
                        window.push(&artist_page(&window, &artist));
                    }
                });
            }
            crate::share::add_copy_link(window, &header, melogold_core::share_links::album_url(&album.album.browse_id));
            add_collection_menu(window, &header, &MusicItem::Album(album.album.clone()));
            content.append(&header.root);
            if let Some(text) = album.description.as_deref().filter(|d| !d.trim().is_empty()) {
                let owner = DescriptionOwner {
                    title: album.album.title.clone(),
                    line: subtitle.clone(),
                    cover: album.album.thumbnail_url.clone(),
                    round: false,
                };
                content.append(&description(window, text, owner));
            }
            content.append(&spacer());
            content.append(&track_list(window, &album.tracks, usize::MAX, TrackContext::List));
            for shelf in &album.shelves {
                content.append(&shelf_view(window, shelf, 10, TrackContext::Single, None));
            }
        },
    );
    page.page
}

/// Плейлист YouTube: плейлисты длиннее 100 треков догружаются продолжениями (грабли §9 п. 8).
pub fn playlist_page(window: &MainWindow, playlist_id: &str) -> adw::NavigationPage {
    let page = scaffold("Melogold", None, false);
    let (music, id) = (window.ctx.services.music.clone(), playlist_id.to_owned());
    let (title_page, scroller) = (page.page.clone(), page.scroller.clone());
    load(
        window,
        &page,
        move || {
            let (music, id) = (music.clone(), id.clone());
            async move { music.playlist(&id).await }
        },
        move |window, content, playlist| {
            title_page.set_title(&playlist.playlist.title);
            let header = CollectionHeader::new(
                window,
                &playlist.playlist.title,
                playlist.author_text.as_deref().unwrap_or_default(),
                playlist.count_text.as_deref(),
                playlist.playlist.thumbnail_url.as_deref(),
                false,
            );
            // «Слушать» и «Перемешать» — весь плейлист, со всеми продолжениями.
            let (weak, music, id) = (window.downgrade(), window.ctx.services.music.clone(), playlist.playlist.playlist_id.clone());
            let whole = move |shuffle: bool| {
                let Some(window) = weak.upgrade() else { return };
                let (music, id) = (music.clone(), id.clone());
                let task = window.ctx.services.run(async move { music.playlist_tracks(&id, 5000).await });
                let player = window.ctx.services.player.clone();
                glib::spawn_future_local(async move {
                    if let Some(Ok(tracks)) = task.await {
                        if !tracks.is_empty() {
                            player.send(Command::PlayList { tracks, start: 0, shuffle });
                        }
                    }
                });
            };
            let whole = Rc::new(whole);
            let play = Rc::clone(&whole);
            header.add_button(tr("PlayAll"), "media-playback-start-symbolic", true, move || play(false));
            header.add_button(tr("Shuffle"), "media-playlist-shuffle-symbolic", false, move || whole(true));
            crate::share::add_copy_link(window, &header, melogold_core::share_links::playlist_url(&playlist.playlist.playlist_id));
            add_collection_menu(window, &header, &MusicItem::Playlist(playlist.playlist.clone()));
            content.append(&header.root);
            content.append(&spacer());
            let list = TrackList::new(window, &playlist.tracks, usize::MAX, TrackContext::List);
            content.append(&list.list);
            follow_continuation(window, &scroller, list, playlist.continuation, Source::Playlist);
        },
    );
    page.page
}

/// Исполнитель YTM; без музыкального профиля — канал YouTube с бесконечной лентой видео.
pub fn artist_page(window: &MainWindow, browse_id: &str) -> adw::NavigationPage {
    artist_page_with(window, browse_id, None)
}

/// Страница исполнителя из готового ответа — снимки окна без сети.
pub fn artist_page_from(window: &MainWindow, details: melogold_core::music::ArtistDetails) -> adw::NavigationPage {
    let id = details.browse_id.clone();
    artist_page_with(window, &id, Some(details))
}

fn artist_page_with(window: &MainWindow, browse_id: &str, ready: Option<melogold_core::music::ArtistDetails>) -> adw::NavigationPage {
    let page = scaffold("Melogold", None, false);
    let (music, id) = (window.ctx.services.music.clone(), browse_id.to_owned());
    let (title_page, scroller) = (page.page.clone(), page.scroller.clone());
    let (hero_slot, sticky_slot) = (page.hero.clone(), page.sticky.clone());
    load(
        window,
        &page,
        move || {
            let (music, id, ready) = (music.clone(), id.clone(), ready.clone());
            async move {
                let artist = match ready {
                    Some(artist) => artist,
                    None => music.artist(&id).await?,
                };
                // У канала — продолжения ленты видео.
                let channel = if artist.is_channel { music.channel(&id).await.ok() } else { None };
                Ok((artist, channel))
            }
        },
        move |window, content, (artist, channel)| {
            title_page.set_title(&artist.name);
            for slot in [&hero_slot, &sticky_slot] {
                while let Some(child) = slot.first_child() {
                    slot.remove(&child);
                }
            }
            let kind = if artist.is_channel { tr("YouTubeChannel") } else { "" };
            let item = ArtistItem {
                browse_id: artist.browse_id.clone(),
                name: artist.name.clone(),
                subtitle: None,
                thumbnail_url: artist.thumbnail_url.clone(),
                is_channel: artist.is_channel,
            };
            let top: Vec<Track> = artist.shelves.first().map(|s| s.tracks().cloned().collect()).unwrap_or_default();
            // «Слушать» — все песни исполнителя (плейлист «Песни»), не вышло — популярные.
            let all_songs: Option<Rc<dyn Fn(bool)>> = (!top.is_empty()).then(|| {
                let (weak, music, songs, top_tracks) =
                    (window.downgrade(), window.ctx.services.music.clone(), artist.songs_playlist_id.clone(), top.clone());
                Rc::new(move |shuffle: bool| {
                    let Some(window) = weak.upgrade() else { return };
                    let player = window.ctx.services.player.clone();
                    let Some(songs) = songs.clone() else {
                        player.send(Command::PlayList { tracks: top_tracks.clone(), start: 0, shuffle });
                        return;
                    };
                    let (music, fallback) = (music.clone(), top_tracks.clone());
                    let task = window.ctx.services.run(async move { music.playlist_tracks(&songs, 500).await });
                    glib::spawn_future_local(async move {
                        let tracks = match task.await {
                            Some(Ok(tracks)) if !tracks.is_empty() => tracks,
                            _ => fallback,
                        };
                        player.send(Command::PlayList { tracks, start: 0, shuffle });
                    });
                }) as Rc<dyn Fn(bool)>
            });
            let save = {
                let weak = window.downgrade();
                move |on: bool| {
                    let Some(window) = weak.upgrade() else { return };
                    let item = item.clone();
                    let task = window.ctx.services.db(move |library| library.set_artist_saved(&item, on));
                    glib::spawn_future_local(async move {
                        if let Some(Err(error)) = task.await {
                            tracing::warn!(%error, "подписка не сохранилась");
                        }
                    });
                }
            };
            let id = artist.browse_id.clone();
            if artist.is_channel {
                let link = melogold_core::share_links::artist_url(&artist.browse_id, artist.is_channel);
                // Канал обычного YouTube — с прежней шапкой: широкого фото и слушателей у него нет.
                let header = CollectionHeader::new(
                    window,
                    &artist.name,
                    kind,
                    artist.subscribers_text.as_deref(),
                    artist.thumbnail_url.as_deref(),
                    true,
                );
                if let Some(play) = &all_songs {
                    let (listen, shuffle) = (Rc::clone(play), Rc::clone(play));
                    header.add_button(tr("PlayAll"), "media-playback-start-symbolic", true, move || listen(false));
                    header.add_button(tr("Shuffle"), "media-playlist-shuffle-symbolic", false, move || shuffle(true));
                }
                let subscribe =
                    header.add_toggle([tr("Subscribe"), tr("Subscribed")], ["list-add-symbolic", "object-select-symbolic"], save);
                bookmark_state(window, &subscribe, move |library| library.is_artist_saved(&id).unwrap_or(false));
                crate::share::add_copy_link(window, &header, link);
                content.append(&header.root);
            } else {
                // Шапка как в Apple Music — над колонкой страницы, во всю ширину (задание 0019).
                let play = all_songs.clone().unwrap_or_else(|| Rc::new(|_| {}));
                let hero = Rc::new(crate::artist_hero::build(window, &artist, play, save));
                bookmark_state(window, &hero.subscribe, move |library| library.is_artist_saved(&id).unwrap_or(false));
                hero_slot.append(&hero.root);
                sticky_slot.append(&hero.sticky);
                let follower = Rc::clone(&hero);
                scroller.vadjustment().connect_value_changed(move |adjustment| follower.follow_scroll(adjustment.value()));
            }
            if let Some(text) = artist.description.as_deref().filter(|d| !d.trim().is_empty()) {
                let owner = DescriptionOwner {
                    title: artist.name.clone(),
                    line: [kind, artist.subscribers_text.as_deref().unwrap_or_default()]
                        .iter()
                        .filter(|part| !part.is_empty())
                        .copied()
                        .collect::<Vec<_>>()
                        .join(" · "),
                    cover: artist.thumbnail_url.clone(),
                    round: true,
                };
                content.append(&description(window, text, owner));
            }
            match channel {
                Some(channel) if artist.is_channel => {
                    content.append(&spacer());
                    let list = TrackList::new(window, &channel.videos, usize::MAX, TrackContext::List);
                    content.append(&list.list);
                    follow_continuation(window, &scroller, list, channel.continuation, Source::Channel);
                }
                _ => {
                    for (index, shelf) in artist.shelves.iter().enumerate() {
                        // «Популярное» — «Все ›» ведёт в плейлист всех песен исполнителя.
                        let more = (index == 0 && shelf.items.iter().all(|i| matches!(i, MusicItem::Track(_))))
                            .then(|| artist.songs_playlist_id.clone())
                            .flatten()
                            .map(|songs| {
                                let weak = window.downgrade();
                                Rc::new(move || {
                                    if let Some(window) = weak.upgrade() {
                                        window.push(&playlist_page(&window, &songs));
                                    }
                                }) as Rc<dyn Fn()>
                            });
                        // «Все ›» альбомов, синглов и прочих полок с собственной сеткой — отдельная страница.
                        let more = more.or_else(|| artist_items_more(window, shelf, &artist.name));
                        // Популярное исполнителя — сеткой на широком окне (задание 0019).
                        let grid = index == 0 && !artist.is_channel;
                        content.append(&crate::catalog_widgets::shelf_view_with(window, shelf, 5, TrackContext::List, more, grid));
                    }
                }
            }
        },
    );
    page.page
}

/// «Все ›» полки исполнителя, у которой YouTube Music отдаёт полную сетку (`MPAD…` или `UC…` + `params`).
/// Полка карточек без такой ссылки (все альбомы уже на полке) тоже открывается отдельной страницей —
/// сеткой тех же карточек, без запроса: листать ленту вбок неудобно (пользователь, 2026-09-30).
fn artist_items_more(window: &MainWindow, shelf: &Shelf, artist: &str) -> Option<Rc<dyn Fn()>> {
    let title = match shelf.title.as_deref().filter(|t| !t.is_empty()) {
        Some(name) => format!("{name} — {artist}"),
        None => artist.to_owned(),
    };
    let grid_link =
        shelf.more_browse_id.clone().filter(|id| id.starts_with("MPAD") || (id.starts_with("UC") && shelf.more_params.is_some()));
    let Some(browse_id) = grid_link else {
        let cards = shelf.items.len() >= 2 && shelf.items.iter().all(|item| !matches!(item, MusicItem::Mood(_)));
        let all_tracks = shelf.items.iter().all(|item| matches!(item, MusicItem::Track(t) if !t.is_video()));
        if !cards || all_tracks || shelf.more_browse_id.is_some() {
            return None;
        }
        let (weak, items) = (window.downgrade(), shelf.items.clone());
        return Some(Rc::new(move || {
            if let Some(window) = weak.upgrade() {
                window.push(&shelf_items_page(&window, &title, &items));
            }
        }));
    };
    let (weak, params, fallback) = (window.downgrade(), shelf.more_params.clone(), shelf.items.clone());
    Some(Rc::new(move || {
        if let Some(window) = weak.upgrade() {
            window.push(&artist_items_page(&window, &title, &browse_id, params.as_deref(), fallback.clone()));
        }
    }))
}

/// Карточки полки сеткой, без сети: все они уже пришли со страницей исполнителя.
fn shelf_items_page(window: &MainWindow, title: &str, items: &[MusicItem]) -> adw::NavigationPage {
    let page = scaffold(title, None, true);
    page.content.append(&card_grid(window, items));
    page.state.content();
    page.page
}

/// Все альбомы, синглы, плейлисты исполнителя сеткой карточек; продолжения подгружаются при прокрутке.
/// Если YouTube не отдал страницу — те карточки, что были на полке.
pub fn artist_items_page(
    window: &MainWindow,
    title: &str,
    browse_id: &str,
    params: Option<&str>,
    fallback: Vec<MusicItem>,
) -> adw::NavigationPage {
    let page = scaffold(title, None, true);
    let scroller = page.scroller.clone();
    let (music, id, params) = (window.ctx.services.music.clone(), browse_id.to_owned(), params.map(str::to_owned));
    load(
        window,
        &page,
        move || {
            let (music, id, params, fallback) = (music.clone(), id.clone(), params.clone(), fallback.clone());
            async move {
                match music.browse_grid(&id, params.as_deref()).await {
                    Ok(page) => Ok(page),
                    Err(_) if !fallback.is_empty() => Ok(melogold_core::music::ItemsPage { items: fallback, continuation: None }),
                    Err(error) => Err(error),
                }
            }
        },
        move |window, content, first| {
            let grid = card_grid(window, &first.items);
            content.append(&grid);
            let seen: Rc<RefCell<std::collections::HashSet<String>>> = Rc::default();
            seen.borrow_mut().extend(first.items.iter().filter_map(item_key));
            let token_cell = Rc::new(RefCell::new(first.continuation));
            let loading = Rc::new(Cell::new(false));
            let weak = window.downgrade();
            scroller.vadjustment().connect_value_changed(move |adjustment| {
                if adjustment.value() < adjustment.upper() - adjustment.page_size() - 800.0 {
                    return;
                }
                let Some(token) = token_cell.borrow().clone() else { return };
                if loading.replace(true) {
                    return;
                }
                let Some(window) = weak.upgrade() else { return };
                let music = window.ctx.services.music.clone();
                let fetch_token = token.clone();
                let task = window.ctx.services.run(async move { music.playlist_continuation(&fetch_token).await });
                let (weak, grid, token_cell, loading, seen) =
                    (window.downgrade(), grid.clone(), Rc::clone(&token_cell), Rc::clone(&loading), Rc::clone(&seen));
                glib::spawn_future_local(async move {
                    let result = task.await;
                    loading.set(false);
                    let (Some(window), Some(Ok(page))) = (weak.upgrade(), result) else {
                        // Ошибка сети — больше не дёргаем, что есть, то показано.
                        return;
                    };
                    for item in &page.items {
                        if item_key(item).is_none_or(|key| seen.borrow_mut().insert(key)) {
                            grid.append(&card(&window, item));
                        }
                    }
                    token_cell.replace(if page.continuation.as_deref() == Some(token.as_str()) { None } else { page.continuation });
                });
            });
        },
    );
    page.page
}

/// Ключ карточки для отсева повторов между страницами продолжения.
fn item_key(item: &MusicItem) -> Option<String> {
    match item {
        MusicItem::Album(a) => Some(a.browse_id.clone()),
        MusicItem::Playlist(p) => Some(p.browse_id()),
        MusicItem::Artist(a) => Some(a.browse_id.clone()),
        MusicItem::Track(t) => Some(t.video_id.clone()),
        _ => None,
    }
}

/// Настроение, «Все настроения», «Все новые релизы»: полки страницы, карточки — сеткой.
pub fn browse_page(window: &MainWindow, title: &str, browse_id: &str, params: Option<&str>) -> adw::NavigationPage {
    let page = scaffold(title, None, true);
    let (music, id, params) = (window.ctx.services.music.clone(), browse_id.to_owned(), params.map(str::to_owned));
    load(
        window,
        &page,
        move || {
            let (music, id, params) = (music.clone(), id.clone(), params.clone());
            async move { music.browse_shelves(&id, params.as_deref()).await }
        },
        |window, content, shelves: Vec<Shelf>| {
            for shelf in &shelves {
                if let Some(title) = shelf.title.as_deref().filter(|t| !t.is_empty()) {
                    let label = gtk::Label::builder().label(title).xalign(0.0).margin_top(18).margin_bottom(6).build();
                    label.add_css_class("title-4");
                    label.add_css_class("shelf-title");
                    content.append(&label);
                }
                if shelf.items.iter().all(|i| matches!(i, MusicItem::Mood(_))) {
                    content.append(&mood_grid(window, &shelf.items));
                } else if shelf.items.iter().all(|i| matches!(i, MusicItem::Track(_))) {
                    let tracks: Vec<Track> = shelf.tracks().cloned().collect();
                    content.append(&track_list(window, &tracks, usize::MAX, TrackContext::List));
                } else {
                    content.append(&card_grid(window, &shelf.items));
                }
            }
        },
    );
    page.page
}

/// Весь список треков полки («Все ›» у длинной полки без своей страницы).
pub fn track_list_page(window: &MainWindow, title: &str, tracks: &[Track]) -> adw::NavigationPage {
    let page = scaffold(title, None, true);
    page.content.append(&track_list(window, tracks, usize::MAX, TrackContext::List));
    page.state.content();
    page.page
}

/// Состояние «Сохранить»/«Подписаться» — из базы, после того как шапка уже показана.
fn bookmark_state(window: &MainWindow, toggle: &Toggle, read: impl FnOnce(&melogold_data::Library) -> bool + Send + 'static) {
    let task = window.ctx.services.db(read);
    let toggle = toggle.clone();
    glib::spawn_future_local(async move {
        if let Some(on) = task.await {
            toggle.set_quietly(on);
        }
    });
}

fn spacer() -> gtk::Box {
    gtk::Box::builder().height_request(12).build()
}

/// «…» в шапке коллекции; флажок «Сохранено» — из базы, до первого открытия.
fn add_collection_menu(window: &MainWindow, header: &CollectionHeader, item: &MusicItem) {
    let Some(menu) = crate::collection_menu::build(window, item) else { return };
    header.add_menu(&menu);
    let menu = Rc::new(menu);
    crate::collection_menu::read_saved(window, item, move |on| menu.set_saved(on));
}

fn add_play_buttons(window: &MainWindow, header: &CollectionHeader, tracks: Vec<Track>) {
    if tracks.is_empty() {
        return;
    }
    let (player, all) = (window.ctx.services.player.clone(), Rc::new(tracks));
    let (play_player, play_tracks) = (player.clone(), Rc::clone(&all));
    header.add_button(tr("PlayAll"), "media-playback-start-symbolic", true, move || {
        play_player.send(Command::PlayList { tracks: play_tracks.as_ref().clone(), start: 0, shuffle: false });
    });
    header.add_button(tr("Shuffle"), "media-playlist-shuffle-symbolic", false, move || {
        player.send(Command::PlayList { tracks: all.as_ref().clone(), start: 0, shuffle: true });
    });
}

#[derive(Clone, Copy)]
enum Source {
    Playlist,
    Channel,
}

/// Продолжения при прокрутке к концу списка: плейлист — страницами по 100, канал — лентой.
fn follow_continuation(window: &MainWindow, scroller: &gtk::ScrolledWindow, list: TrackList, continuation: Option<String>, source: Source) {
    let token_cell = Rc::new(RefCell::new(continuation));
    let loading = Rc::new(Cell::new(false));
    let weak = window.downgrade();
    scroller.vadjustment().connect_value_changed(move |adjustment| {
        if adjustment.value() < adjustment.upper() - adjustment.page_size() - 800.0 {
            return;
        }
        let Some(token) = token_cell.borrow().clone() else { return };
        if loading.replace(true) {
            return;
        }
        let Some(window) = weak.upgrade() else { return };
        let music = window.ctx.services.music.clone();
        let fetch_token = token.clone();
        let task = window.ctx.services.run(async move {
            match source {
                Source::Playlist => music.playlist_continuation(&fetch_token).await,
                Source::Channel => music.channel_continuation(&fetch_token).await,
            }
        });
        let (weak, list, token_cell, loading) = (window.downgrade(), list.clone(), Rc::clone(&token_cell), Rc::clone(&loading));
        glib::spawn_future_local(async move {
            let result = task.await;
            loading.set(false);
            let (Some(window), Some(Ok(page))) = (weak.upgrade(), result) else { return };
            let seen: std::collections::HashSet<String> = list.tracks.borrow().iter().map(|t| t.video_id.clone()).collect();
            let fresh: Vec<Track> = page
                .items
                .into_iter()
                .filter_map(|i| match i {
                    MusicItem::Track(t) if !seen.contains(&t.video_id) => Some(t),
                    _ => None,
                })
                .collect();
            list.append(&window, &fresh);
            token_cell.replace(if page.continuation.as_deref() == Some(token.as_str()) { None } else { page.continuation });
        });
    });
}
