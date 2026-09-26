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
    current_list: RefCell<Option<(gtk::ListBox, Rc<RefCell<Vec<MusicItem>>>)>>,
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
                let mut ytm: Vec<MusicItem> = Vec::new();
                if let Some(summary) = &summary {
                    if let Some(top) = &summary.top {
                        ytm.push(top.clone());
                    }
                    let top_key = summary.top.as_ref().map(MusicItem::key);
                    ytm.extend(summary.items.iter().filter(|i| Some(i.key()) != top_key).cloned());
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
                if ytm.is_empty() && videos.is_empty() {
                    inner.state.empty("system-search-symbolic", tr("ResultsNothing"), "");
                    return;
                }
                if !ytm.is_empty() {
                    inner.list.append(&section_title("YouTube Music"));
                    append(&inner, ytm, true);
                } else {
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
    let (list, items_of_list) = match existing {
        Some(pair) => pair,
        None => {
            let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).activate_on_single_click(false).build();
            list.add_css_class("boxed-list");
            let items_of_list: Rc<RefCell<Vec<MusicItem>>> = Rc::default();
            let (weak, lookup) = (window.downgrade(), Rc::clone(&items_of_list));
            // Двойной щелчок или Enter играет, одиночный выделяет (§5.3).
            list.connect_row_activated(move |_, row| {
                if let (Some(window), Some(item)) = (weak.upgrade(), lookup.borrow().get(row.index() as usize).cloned()) {
                    window.activate_item(&item);
                }
            });
            inner.list.append(&list);
            inner.current_list.replace(Some((list.clone(), Rc::clone(&items_of_list))));
            (list, items_of_list)
        }
    };
    for item in items {
        if !inner.shown.borrow_mut().insert(item.key()) {
            continue;
        }
        let row = match &item {
            MusicItem::Track(track) => crate::widgets::track_row(&window.ctx.services.images, track, Some(window.track_menu(track))),
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
                    row
                }
                None => continue,
            },
        };
        list.append(&row);
        items_of_list.borrow_mut().push(item);
    }
}
