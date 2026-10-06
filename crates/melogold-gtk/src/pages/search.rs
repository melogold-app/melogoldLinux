//! Выдача поиска (§5.4, REWRITE §3.1.3, Windows `SearchPage.cs`): «Всё · Музыка · YouTube».
//!
//! «Всё» — два параллельных запроса: YouTube Music (лучший результат и первые строки) и обычный
//! YouTube (видео, которых нет в YTM). «Музыка» — Песни · Альбомы · Исполнители · Клипы ·
//! Плейлисты, «YouTube» — Видео · Каналы · Трансляции · Плейлисты, с продолжениями при прокрутке.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::music::{ItemsPage, MusicItem};
use melogold_innertube::music::{MusicFilter, WebFilter};
use melogold_innertube::YouTubeError;

use crate::localization::tr;
use crate::selection::Selection;
use crate::track_row::TrackRow;
use crate::widgets::{catalog_error, section_title, StateView};
use crate::window::MainWindow;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    Music,
    YouTube,
}

const MUSIC_FILTERS: [(&str, MusicFilter); 5] = [
    ("ResultsSongs", MusicFilter::Songs),
    ("ResultsAlbums", MusicFilter::Albums),
    ("ResultsArtists", MusicFilter::Artists),
    ("ResultsMusicVideos", MusicFilter::Videos),
    ("ResultsPlaylists", MusicFilter::CommunityPlaylists),
];

const WEB_FILTERS: [(&str, WebFilter); 4] = [
    ("ResultsVideos", WebFilter::Videos),
    ("ResultsChannels", WebFilter::Channels),
    ("ResultsLive", WebFilter::Live),
    ("ResultsPlaylists", WebFilter::Playlists),
];

struct Inner {
    window: crate::window::WeakWindow,
    query: String,
    scope: Cell<Scope>,
    filter: Cell<usize>,
    scopes: adw::ToggleGroup,
    filters: adw::ToggleGroup,
    list: gtk::Box,
    state: StateView,
    scroller: gtk::ScrolledWindow,
    /// Растёт при каждой новой загрузке: ответ старой в список не попадает.
    generation: Cell<u64>,
    continuation: RefCell<Option<String>>,
    loading_more: Cell<bool>,
    shown: RefCell<HashSet<String>>,
    /// Последний список выдачи и его элементы по порядку строк.
    current_list: RefCell<Option<(gtk::ListBox, Rc<RefCell<Vec<MusicItem>>>, Rc<Selection>)>>,
}

pub fn page(window: &MainWindow, query: &str) -> adw::NavigationPage {
    let scopes = adw::ToggleGroup::builder().halign(gtk::Align::Center).build();
    for (name, key) in [("all", "ResultsAll"), ("music", "ResultsMusic"), ("youtube", "ResultsYouTube")] {
        scopes.add(adw::Toggle::builder().name(name).label(tr(key)).build());
    }
    scopes.set_active_name(Some("all"));
    let filters = adw::ToggleGroup::builder().halign(gtk::Align::Center).visible(false).build();
    filters.add_css_class("flat");
    let title = gtk::Label::builder().label(query).xalign(0.0).wrap(true).build();
    title.add_css_class("title-1");
    title.add_css_class("page-title");
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    header.append(&title);
    header.append(&scopes);
    header.append(&filters);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    body.append(&header);
    let state = StateView::new(&list);
    body.append(&state.root);
    let clamp = adw::Clamp::builder().maximum_size(900).child(&body).build();
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();

    let inner = Rc::new(Inner {
        window: window.downgrade(),
        query: query.to_owned(),
        scope: Cell::new(Scope::All),
        filter: Cell::new(0),
        scopes,
        filters,
        list,
        state,
        scroller,
        generation: Cell::new(0),
        continuation: RefCell::default(),
        loading_more: Cell::new(false),
        shown: RefCell::default(),
        current_list: RefCell::default(),
    });

    let weak = Rc::downgrade(&inner);
    inner.scopes.connect_active_name_notify(move |group| {
        let Some(inner) = weak.upgrade() else { return };
        let scope = match group.active_name().as_deref() {
            Some("music") => Scope::Music,
            Some("youtube") => Scope::YouTube,
            _ => Scope::All,
        };
        if scope != inner.scope.get() {
            show(&inner, scope, 0);
        }
    });
    let weak = Rc::downgrade(&inner);
    inner.filters.connect_active_notify(move |group| {
        let Some(inner) = weak.upgrade() else { return };
        let index = group.active() as usize;
        if index != inner.filter.get() && index < 5 {
            inner.filter.set(index);
            load(&inner);
        }
    });
    let weak = Rc::downgrade(&inner);
    inner.scroller.vadjustment().connect_value_changed(move |adjustment| {
        if let Some(inner) = weak.upgrade() {
            if adjustment.value() > adjustment.upper() - adjustment.page_size() - 800.0 {
                load_more(&inner);
            }
        }
    });

    show(&inner, Scope::All, 0);
    let page = adw::NavigationPage::builder().title(query).tag(format!("search:{query}")).child(&inner.scroller).build();
    // Состояние живёт, пока жива страница: обработчик освобождается вместе с ней.
    page.connect_hidden(move |_| {
        let _ = &inner;
    });
    page
}

fn show(inner: &Rc<Inner>, scope: Scope, filter: usize) {
    inner.scope.set(scope);
    inner.filter.set(filter);
    let name = match scope {
        Scope::All => "all",
        Scope::Music => "music",
        Scope::YouTube => "youtube",
    };
    if inner.scopes.active_name().as_deref() != Some(name) {
        inner.scopes.set_active_name(Some(name));
    }
    let keys: Vec<&str> = match scope {
        Scope::All => Vec::new(),
        Scope::Music => MUSIC_FILTERS.iter().map(|(k, _)| *k).collect(),
        Scope::YouTube => WEB_FILTERS.iter().map(|(k, _)| *k).collect(),
    };
    inner.filters.remove_all();
    for (index, key) in keys.iter().enumerate() {
        inner.filters.add(adw::Toggle::builder().name(format!("f{index}")).label(tr(key)).build());
    }
    inner.filters.set_visible(!keys.is_empty());
    if !keys.is_empty() {
        inner.filters.set_active(filter as u32);
    }
    load(inner);
}

fn clear(inner: &Inner) {
    while let Some(child) = inner.list.first_child() {
        inner.list.remove(&child);
    }
    inner.continuation.replace(None);
    inner.shown.borrow_mut().clear();
    inner.current_list.replace(None);
}

fn load(inner: &Rc<Inner>) {
    let generation = inner.generation.get() + 1;
    inner.generation.set(generation);
    clear(inner);
    inner.state.loading();
    let Some(window) = inner.window.upgrade() else { return };
    let (music, query) = (window.ctx.services.music.clone(), inner.query.clone());
    let inner = Rc::clone(inner);
    match inner.scope.get() {
        Scope::All => {
            let typed = query.clone();
            let summary = window.ctx.services.run({
                let (music, query) = (music.clone(), query.clone());
                async move { music.search_summary(&query).await }
            });
            let web = window.ctx.services.run(async move { music.search_web(&query, WebFilter::Videos).await });
            glib::spawn_future_local(async move {
                let (summary, web) = futures_util::future::join(summary, web).await;
                if inner.generation.get() != generation {
                    return;
                }
                let (summary, web) = (summary.and_then(Result::ok), web.and_then(Result::ok));
                if summary.is_none() && web.is_none() {
                    fail(&inner, None);
                    return;
                }
                // Лучший результат — крупной карточкой над выдачей, строкой ниже не повторяется
                // (задание 0018, доктрина §4.6).
                let top = summary.as_ref().and_then(|summary| melogold_core::search_top::pick(summary, &typed));
                let mut ytm: Vec<MusicItem> = Vec::new();
                if let Some(summary) = &summary {
                    ytm.extend(summary.items.iter().filter(|i| !melogold_core::search_top::same(i, top.as_ref())).cloned());
                }
                ytm.truncate(8);
                let known: HashSet<String> = ytm.iter().map(MusicItem::key).collect();
                let videos: Vec<MusicItem> = web
                    .map(|page| page.items)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|i| matches!(i, MusicItem::Track(_)) && !known.contains(&i.key()))
                    .take(6)
                    .collect();
                if top.is_none() && ytm.is_empty() && videos.is_empty() {
                    inner.state.empty("system-search-symbolic", tr("ResultsNothing"), "");
                    return;
                }
                if let (Some(top), Some(window)) = (&top, inner.window.upgrade()) {
                    inner.list.append(&section_title(tr("ResultsTopResult")));
                    inner.list.append(&top_result_card(&window, top));
                }
                if !ytm.is_empty() {
                    inner.list.append(&section_title("YouTube Music"));
                    append(&inner, ytm, true);
                } else if top.is_none() {
                    let note = gtk::Label::builder().label(tr("ResultsNothingInCatalog")).wrap(true).xalign(0.0).build();
                    note.add_css_class("dim-label");
                    inner.list.append(&note);
                }
                if !videos.is_empty() {
                    inner.list.append(&section_title(tr("ResultsYouTube")));
                    append(&inner, videos, true);
                }
                inner.state.content();
            });
        }
        scope => {
            let filter = inner.filter.get();
            let task = window.ctx.services.run(async move {
                match scope {
                    Scope::Music => music.search(&query, MUSIC_FILTERS[filter.min(4)].1).await,
                    _ => music.search_web(&query, WEB_FILTERS[filter.min(3)].1).await,
                }
            });
            glib::spawn_future_local(async move {
                let result = task.await;
                if inner.generation.get() != generation {
                    return;
                }
                match result {
                    Some(Ok(page)) if page.items.is_empty() => inner.state.empty("system-search-symbolic", tr("ResultsNothing"), ""),
                    Some(Ok(page)) => {
                        inner.continuation.replace(page.continuation.clone());
                        append(&inner, page.items, true);
                        inner.state.content();
                    }
                    Some(Err(error)) => fail(&inner, Some(error)),
                    None => fail(&inner, None),
                }
            });
        }
    }
}

fn fail(inner: &Rc<Inner>, error: Option<YouTubeError>) {
    let kind = error.map(|e| e.kind).unwrap_or(melogold_innertube::ErrorKind::Offline);
    let weak = Rc::downgrade(inner);
    inner.state.error(catalog_error(kind), move || {
        if let Some(inner) = weak.upgrade() {
            load(&inner);
        }
    });
}

/// Продолжение выдачи, когда до конца списка осталось немного.
fn load_more(inner: &Rc<Inner>) {
    if inner.loading_more.get() || inner.scope.get() == Scope::All {
        return;
    }
    let Some(token) = inner.continuation.borrow().clone() else { return };
    let Some(window) = inner.window.upgrade() else { return };
    inner.loading_more.set(true);
    let (music, scope, generation) = (window.ctx.services.music.clone(), inner.scope.get(), inner.generation.get());
    let task = window.ctx.services.run({
        let token = token.clone();
        async move {
            match scope {
                Scope::Music => music.search_continuation(&token).await,
                _ => music.search_web_continuation(&token).await,
            }
        }
    });
    let inner = Rc::clone(inner);
    glib::spawn_future_local(async move {
        let result = task.await;
        inner.loading_more.set(false);
        if inner.generation.get() != generation {
            return;
        }
        match result {
            Some(Ok(ItemsPage { items, continuation })) => {
                inner.continuation.replace(if continuation.as_deref() == Some(token.as_str()) { None } else { continuation });
                append(&inner, items, false);
            }
            Some(Err(error)) => tracing::warn!(%error, "продолжение выдачи не загрузилось"),
            None => {}
        }
    });
}

/// Строки выдачи; `new_group` — начать новый список (секция «Всё»), иначе дописать в последний.
fn append(inner: &Rc<Inner>, items: Vec<MusicItem>, new_group: bool) {
    let Some(window) = inner.window.upgrade() else { return };
    let existing = if new_group { None } else { inner.current_list.borrow().clone() };
    let (list, items_of_list, selection) = match existing {
        Some(pair) => pair,
        None => {
            let list = gtk::ListBox::builder().activate_on_single_click(false).build();
            list.add_css_class("track-rows");
            // Треки выдачи выделяются, как в любом списке (задание 0004); альбомы и исполнители — нет.
            let selection = Selection::for_list_box(&window, &list, crate::library_view::RowContext::Plain);
            let items_of_list: Rc<RefCell<Vec<MusicItem>>> = Rc::default();
            let (weak, lookup) = (window.downgrade(), Rc::clone(&items_of_list));
            // Двойной щелчок или Enter играет, одиночный выделяет (§5.3).
            list.connect_row_activated(move |_, row| {
                if let (Some(window), Some(item)) = (weak.upgrade(), lookup.borrow().get(row.index() as usize).cloned()) {
                    window.activate_item(&item);
                }
            });
            inner.list.append(&list);
            inner.current_list.replace(Some((list.clone(), Rc::clone(&items_of_list), Rc::clone(&selection))));
            (list, items_of_list, selection)
        }
    };
    for item in items {
        if !inner.shown.borrow_mut().insert(item.key()) {
            continue;
        }
        let row = match &item {
            MusicItem::Track(track) => {
                let row = TrackRow::new(&window);
                row.bind(track, crate::library_view::RowContext::Plain);
                row.set_selection(Some(&selection));
                gtk::ListBoxRow::builder().child(&row).build()
            }
            other => match crate::widgets::item_row(&window.ctx.services.images, other) {
                Some(row) => {
                    // Переход — одним щелчком: это не трек, выделять в нём нечего.
                    let click = gtk::GestureClick::new();
                    let (weak, item) = (window.downgrade(), item.clone());
                    click.connect_released(move |gesture, presses, _, _| {
                        if presses == 1 {
                            if let Some(window) = weak.upgrade() {
                                gesture.set_state(gtk::EventSequenceState::Claimed);
                                window.activate_item(&item);
                            }
                        }
                    });
                    row.add_controller(click);
                    row.set_selectable(false);
                    row
                }
                None => continue,
            },
        };
        list.append(&row);
        items_of_list.borrow_mut().push(item);
    }
}

/// Карточка лучшего результата: обложка (у исполнителя — круглое фото), название, что это, и
/// «Слушать» с «Открыть» (задание 0018; Windows `TopResultCard`). Строка системного списка: нажатие по
/// ней открывает страницу, а у трека — играет; кнопки внутри — отдельные цели фокуса.
pub(crate) fn top_result_card(window: &MainWindow, item: &MusicItem) -> gtk::ListBox {
    let join = |parts: &[Option<&str>]| parts.iter().flatten().filter(|p| !p.trim().is_empty()).copied().collect::<Vec<_>>().join(" · ");
    let (title, kind, cover_url, round) = match item {
        MusicItem::Artist(artist) => (
            artist.name.clone(),
            join(&[Some(tr(if artist.is_channel { "TypeChannel" } else { "TypeArtist" })), artist.subtitle.as_deref()]),
            artist.thumbnail_url.clone(),
            true,
        ),
        MusicItem::Album(album) => (
            album.title.clone(),
            join(&[Some(album.type_text.as_deref().unwrap_or(tr("TypeAlbum"))), album.artists_text.as_deref(), album.year.as_deref()]),
            album.thumbnail_url.clone(),
            false,
        ),
        MusicItem::Track(track) => (
            track.title.clone(),
            join(&[
                Some(tr(if track.is_video() { "TypeVideo" } else { "TypeSong" })),
                track.artists_text.as_deref(),
                track.album_title.as_deref(),
            ]),
            track.thumbnail_url.clone(),
            false,
        ),
        MusicItem::Playlist(playlist) => (
            playlist.title.clone(),
            join(&[Some(tr("ResultsPlaylists")), playlist.subtitle.as_deref()]),
            playlist.thumbnail_url.clone(),
            false,
        ),
        MusicItem::Mood(mood) => (mood.title.clone(), String::new(), None, false),
    };

    let cover = crate::widgets::Cover::new(96);
    cover.root.add_css_class("card-cover");
    if round {
        cover.root.add_css_class("round");
    }
    cover.root.set_valign(gtk::Align::Center);
    // Фото исполнителя в выдаче приходит на 120 px — берём крупнее, как у обложек.
    cover.set(&window.ctx.services.images, cover_url.as_deref(), 240);

    let name = gtk::Label::builder().label(&title).xalign(0.0).wrap(true).build();
    name.add_css_class("title-2");
    let what = gtk::Label::builder().label(&kind).xalign(0.0).wrap(true).build();
    what.add_css_class("dim-label");

    // Кнопки переносятся строкой ниже, когда не помещаются: узкое окно — 360 px (`docs/PROMPT.md` §5.2).
    let buttons = adw::WrapBox::builder().child_spacing(8).line_spacing(8).margin_top(6).build();
    let listen = gtk::Button::builder().label(tr("PlayAll")).build();
    listen.add_css_class("pill");
    listen.add_css_class("suggested-action");
    buttons.append(&listen);
    let opens = !matches!(item, MusicItem::Track(_) | MusicItem::Mood(_));
    if opens {
        let open = gtk::Button::builder().label(tr("ResultsOpen")).build();
        open.add_css_class("pill");
        let (weak, item) = (window.downgrade(), item.clone());
        open.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.activate_item(&item);
            }
        });
        buttons.append(&open);
    }
    {
        let (weak, item) = (window.downgrade(), item.clone());
        listen.connect_clicked(move |button| {
            if let Some(window) = weak.upgrade() {
                listen_to(&window, &item, button);
            }
        });
    }

    let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).valign(gtk::Align::Center).hexpand(true).build();
    texts.append(&name);
    texts.append(&what);
    texts.append(&buttons);
    let content = gtk::Box::builder().spacing(14).margin_top(12).margin_bottom(12).margin_start(12).margin_end(12).build();
    content.append(&cover.root);
    content.append(&texts);

    let row = gtk::ListBoxRow::builder().activatable(true).child(&content).build();
    row.update_property(&[gtk::accessible::Property::Label(&format!("{}: {title}, {kind}", tr("ResultsTopResult")))]);
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).margin_bottom(6).build();
    list.add_css_class("boxed-list");
    list.append(&row);
    let (weak, item) = (window.downgrade(), item.clone());
    list.connect_row_activated(move |_, _| {
        if let Some(window) = weak.upgrade() {
            window.activate_item(&item);
        }
    });
    list
}

/// «Слушать» лучшего результата: у исполнителя — все его песни (плейлист «Песни» со страницы
/// исполнителя), не вышло — популярные треки; у альбома и плейлиста — их треки; трек — сам. Пока
/// грузится — кнопка выключена; ошибка сети — всплывающее сообщение.
fn listen_to(window: &MainWindow, item: &MusicItem, button: &gtk::Button) {
    use melogold_playback::engine::Command;
    if let MusicItem::Track(_) = item {
        window.activate_item(item);
        return;
    }
    let music = window.ctx.services.music.clone();
    let item = item.clone();
    let task = window.ctx.services.run(async move {
        match item {
            MusicItem::Artist(artist) => {
                let details = music.artist(&artist.browse_id).await?;
                if let Some(songs) = &details.songs_playlist_id {
                    if let Ok(tracks) = music.playlist_tracks(songs, 500).await {
                        if !tracks.is_empty() {
                            return Ok(tracks);
                        }
                    }
                }
                Ok(details.shelves.first().map(|shelf| shelf.tracks().cloned().collect()).unwrap_or_default())
            }
            MusicItem::Album(album) => Ok(music.album(&album.browse_id).await?.tracks),
            MusicItem::Playlist(playlist) => music.playlist_tracks(&playlist.playlist_id, 500).await,
            _ => Ok(Vec::new()),
        }
    });
    button.set_sensitive(false);
    let (weak, button) = (window.downgrade(), button.downgrade());
    glib::spawn_future_local(async move {
        let result: Option<Result<Vec<melogold_core::music::Track>, YouTubeError>> = task.await;
        if let Some(button) = button.upgrade() {
            button.set_sensitive(true);
        }
        let Some(window) = weak.upgrade() else { return };
        match result {
            Some(Ok(tracks)) if !tracks.is_empty() => {
                window.ctx.services.player.send(Command::PlayList { tracks, start: 0, shuffle: false })
            }
            Some(Err(error)) if error.kind == melogold_innertube::ErrorKind::Blocked => window.toast(tr("ErrorBlocked")),
            _ => window.toast(tr("ErrorOffline")),
        }
    });
}
