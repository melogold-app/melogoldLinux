//! Виджеты каталога (§5.4, Windows `ShelfView.cs`, `CollectionHeader.cs`, `MediaCard`): полки,
//! карточки, плитки настроений, списки треков и шапка детального экрана.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use melogold_core::music::{MusicItem, Shelf, Track};
use melogold_core::thumbnails;
use melogold_playback::engine::Command;

use crate::localization::tr;
use crate::widgets::{track_row, Cover};
use crate::window::MainWindow;

/// Что играть по нажатию на трек (REWRITE §2.3).
#[derive(Clone)]
pub enum TrackContext {
    /// Выдача поиска, «Недавние», ссылка: трек и дальше похожие.
    Single,
    /// Альбом, плейлист, «В тренде», популярное исполнителя: весь список с выбранного трека.
    List,
}

/// Список треков (§5.3): двойной щелчок или Enter играет, одиночный щелчок выделяет. Треки списка
/// общие со строками: догрузка дописывает их, и нажатие играет весь известный список.
#[derive(Clone)]
pub struct TrackList {
    pub list: gtk::ListBox,
    pub tracks: Rc<RefCell<Vec<Track>>>,
}

impl TrackList {
    pub fn new(window: &MainWindow, tracks: &[Track], shown: usize, context: TrackContext) -> TrackList {
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).activate_on_single_click(false).build();
        list.add_css_class("boxed-list");
        for track in tracks.iter().take(shown) {
            list.append(&track_row(&window.ctx.services.images, track, Some(window.track_menu(track))));
        }
        let all = Rc::new(RefCell::new(tracks.to_vec()));
        let (weak, shared) = (window.downgrade(), Rc::clone(&all));
        list.connect_row_activated(move |_, row| {
            let Some(window) = weak.upgrade() else { return };
            let index = row.index().max(0) as usize;
            let tracks = shared.borrow().clone();
            let Some(track) = tracks.get(index).cloned() else { return };
            let player = &window.ctx.services.player;
            match context {
                TrackContext::Single => player.send(Command::PlaySingle { track, start: Duration::ZERO }),
                // Весь список играет с выбранного трека, даже если видны не все строки.
                TrackContext::List => player.send(Command::PlayList { tracks, start: index, shuffle: false }),
            }
        });
        TrackList { list, tracks: all }
    }

    /// Дописать строки (продолжения плейлиста и канала).
    pub fn append(&self, window: &MainWindow, tracks: &[Track]) {
        for track in tracks {
            self.list.append(&track_row(&window.ctx.services.images, track, Some(window.track_menu(track))));
        }
        self.tracks.borrow_mut().extend(tracks.iter().cloned());
    }
}

pub fn track_list(window: &MainWindow, tracks: &[Track], shown: usize, context: TrackContext) -> gtk::ListBox {
    TrackList::new(window, tracks, shown, context).list
}

/// Карточка альбома, плейлиста, исполнителя или клипа: обложка и две строки подписи.
pub fn card(window: &MainWindow, item: &MusicItem) -> gtk::Button {
    let (title, subtitle, thumbnail, round, wide) = match item {
        MusicItem::Album(a) => (a.title.clone(), a.subtitle(), a.thumbnail_url.clone(), false, false),
        MusicItem::Playlist(p) => (p.title.clone(), p.subtitle.clone().unwrap_or_default(), p.thumbnail_url.clone(), false, false),
        MusicItem::Artist(a) => (a.name.clone(), a.subtitle.clone().unwrap_or_default(), a.thumbnail_url.clone(), true, false),
        MusicItem::Track(t) => {
            let wide = thumbnails::is_wide(t.thumbnail_url.as_deref());
            (t.title.clone(), t.artists_text.clone().unwrap_or_default(), t.thumbnail_url.clone(), false, wide)
        }
        MusicItem::Mood(m) => (m.title.clone(), String::new(), None, false, false),
    };
    let cover = Cover::new(160);
    if wide {
        // Клипы — карточкой 16:9: кадр видео целиком (§5.3 «Видео и песни»).
        cover.root.set_size_request(240, 135);
    }
    if round {
        cover.root.add_css_class("round");
    }
    cover.set(&window.ctx.services.images, thumbnail.as_deref(), 320);
    let width = if wide { 240 } else { 160 };
    let title_label = gtk::Label::builder()
        .label(&title)
        .xalign(0.0)
        .wrap(true)
        .lines(2)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .width_request(width)
        .build();
    let subtitle_label = gtk::Label::builder()
        .label(&subtitle)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .width_request(width)
        .build();
    subtitle_label.add_css_class("dim-label");
    subtitle_label.add_css_class("caption");
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    content.append(&cover.root);
    content.append(&title_label);
    if !subtitle.is_empty() {
        content.append(&subtitle_label);
    }
    let button = gtk::Button::builder().child(&content).valign(gtk::Align::Start).tooltip_text(&title).build();
    button.add_css_class("flat");
    button.add_css_class("card-button");
    button.update_property(&[gtk::accessible::Property::Label(&format!("{title}, {subtitle}"))]);
    let (weak, item) = (window.downgrade(), item.clone());
    button.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.activate_item(&item);
        }
    });
    button
}

/// Горизонтальный ряд карточек.
pub fn card_row(window: &MainWindow, items: &[MusicItem]) -> gtk::ScrolledWindow {
    let row = gtk::Box::builder().spacing(8).margin_bottom(6).build();
    for item in items {
        row.append(&card(window, item));
    }
    gtk::ScrolledWindow::builder()
        .child(&row)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .build()
}

/// Сетка карточек по ширине окна — без пустоты справа (страницы «Все», библиотека).
pub fn card_grid(window: &MainWindow, items: &[MusicItem]) -> gtk::FlowBox {
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(12)
        .column_spacing(4)
        .row_spacing(12)
        .build();
    for item in items {
        grid.append(&card(window, item));
    }
    grid
}

/// Плитки «Настроения и жанры» с цветной полоской.
pub fn mood_grid(window: &MainWindow, items: &[MusicItem]) -> gtk::FlowBox {
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(8)
        .column_spacing(8)
        .row_spacing(8)
        .build();
    for item in items {
        let MusicItem::Mood(mood) = item else { continue };
        let stripe = gtk::Box::builder().width_request(6).build();
        stripe.add_css_class("mood-stripe");
        if let Some(color) = mood.color {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(&format!(".mood-stripe {{ background-color: #{:06x}; }}", color & 0xFF_FFFF));
            #[allow(deprecated)]
            stripe.style_context().add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        let label = gtk::Label::builder().label(&mood.title).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).hexpand(true).build();
        let content = gtk::Box::builder().spacing(10).build();
        content.append(&stripe);
        content.append(&label);
        let button = gtk::Button::builder().child(&content).build();
        button.add_css_class("mood-tile");
        let (weak, item) = (window.downgrade(), item.clone());
        button.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.activate_item(&item);
            }
        });
        grid.append(&button);
    }
    grid
}

/// Клипы и видео YTM приходят карточками 16:9 без длительности — их показываем рядом, а не строками.
fn is_video_carousel(shelf: &Shelf) -> bool {
    !shelf.items.is_empty() && shelf.items.iter().all(|i| matches!(i, MusicItem::Track(t) if t.is_video() && t.duration_text.is_none()))
}

/// Полка страницы: заголовок с «Все ›», внутри строки треков (до `max_rows`), ряд карточек или плитки.
pub fn shelf_view(window: &MainWindow, shelf: &Shelf, max_rows: usize, context: TrackContext, more: Option<Rc<dyn Fn()>>) -> gtk::Box {
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(18).build();
    let header = gtk::Box::builder().spacing(6).build();
    if let Some(title) = shelf.title.as_deref().filter(|t| !t.is_empty()) {
        let label = gtk::Label::builder().label(title).xalign(0.0).hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build();
        label.add_css_class("title-4");
        label.set_accessible_role(gtk::AccessibleRole::Heading);
        header.append(&label);
    }
    let tracks: Vec<Track> = shelf.tracks().cloned().collect();
    let all_tracks = !shelf.items.is_empty() && tracks.len() == shelf.items.len() && !is_video_carousel(shelf);
    let more = more.or_else(|| more_action(window, shelf)).or_else(|| {
        // Длинный список треков без своей страницы — «Все ›» открывает его целиком.
        (all_tracks && tracks.len() > max_rows).then(|| {
            let (weak, title, tracks) = (window.downgrade(), shelf.title.clone().unwrap_or_default(), tracks.clone());
            Rc::new(move || {
                if let Some(window) = weak.upgrade() {
                    window.push(&crate::pages::catalog::track_list_page(&window, &title, &tracks));
                }
            }) as Rc<dyn Fn()>
        })
    });
    if let Some(more) = more {
        let button = gtk::Button::builder().label(format!("{} ›", tr("SeeAll"))).valign(gtk::Align::Center).build();
        button.add_css_class("flat");
        button.connect_clicked(move |_| more());
        header.append(&button);
    }
    if header.first_child().is_some() {
        root.append(&header);
    }
    if all_tracks {
        root.append(&track_list(window, &tracks, max_rows, context));
    } else if shelf.items.iter().all(|i| matches!(i, MusicItem::Mood(_))) {
        root.append(&mood_grid(window, &shelf.items));
    } else {
        root.append(&card_row(window, &shelf.items));
    }
    root
}

/// «Все ›» полки YouTube: плейлист чарта, все настроения, все новые релизы, страница исполнителя.
fn more_action(window: &MainWindow, shelf: &Shelf) -> Option<Rc<dyn Fn()>> {
    let browse_id = shelf.more_browse_id.clone()?;
    let (weak, title, params) = (window.downgrade(), shelf.title.clone().unwrap_or_default(), shelf.more_params.clone());
    Some(Rc::new(move || {
        let Some(window) = weak.upgrade() else { return };
        let page = if let Some(playlist) = browse_id.strip_prefix("VL") {
            crate::pages::catalog::playlist_page(&window, playlist)
        } else if browse_id.starts_with("UC") && params.is_none() {
            crate::pages::catalog::artist_page(&window, &browse_id)
        } else {
            crate::pages::catalog::browse_page(&window, &title, &browse_id, params.as_deref())
        };
        window.push(&page);
    }))
}

/// Шапка детального экрана (§5.4): обложка ~200 px (у исполнителя — круг), название, подзаголовок и
/// кнопки «Слушать · Перемешать · …». В узком окне (порог 720) обложка сверху, текст и кнопки под ней:
/// шапка подписана на пороги окна, а не на свой размер — `AdwBreakpointBin` не отдаёт естественную
/// высоту, и шапка обрезалась.
pub struct CollectionHeader {
    pub root: gtk::Box,
    pub buttons: gtk::Box,
}

impl CollectionHeader {
    pub fn new(
        window: &MainWindow,
        title: &str,
        subtitle: &str,
        detail: Option<&str>,
        thumbnail: Option<&str>,
        round: bool,
    ) -> CollectionHeader {
        let cover = Cover::new(200);
        if round {
            cover.root.add_css_class("round");
        }
        cover.root.add_css_class("large");
        cover.set(&window.ctx.services.images, thumbnail, 544);
        let title_label = gtk::Label::builder().label(title).xalign(0.0).wrap(true).selectable(true).build();
        title_label.add_css_class("title-1");
        let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).valign(gtk::Align::Center).hexpand(true).build();
        texts.append(&title_label);
        for (text, class) in [(Some(subtitle), "heading"), (detail, "dim-label")] {
            if let Some(text) = text.filter(|t| !t.is_empty()) {
                let label = gtk::Label::builder().label(text).xalign(0.0).wrap(true).build();
                label.add_css_class(class);
                texts.append(&label);
            }
        }
        let buttons = gtk::Box::builder().spacing(8).margin_top(12).build();
        let scroller =
            gtk::ScrolledWindow::builder().child(&buttons).vscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).build();
        texts.append(&scroller);
        let root = gtk::Box::builder().spacing(24).build();
        root.append(&cover.root);
        root.append(&texts);
        window.register_header(&root);
        CollectionHeader { root, buttons }
    }

    pub fn add_button(&self, label: &str, icon: &str, accent: bool, action: impl Fn() + 'static) -> gtk::Button {
        let content = adw::ButtonContent::builder().label(label).icon_name(icon).build();
        let button = gtk::Button::builder().child(&content).build();
        button.add_css_class("pill");
        if accent {
            button.add_css_class("suggested-action");
        }
        button.connect_clicked(move |_| action());
        self.buttons.append(&button);
        button
    }
}

/// Описание: три строки, «Ещё» раскрывает целиком (§5.4). Свёрнутое — одним абзацем: Pango
/// ограничивает строки в каждом абзаце отдельно, и описание с абзацами не сворачивалось.
pub fn description(text: &str) -> gtk::Box {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = gtk::Label::builder().label(&collapsed).xalign(0.0).wrap(true).lines(3).ellipsize(gtk::pango::EllipsizeMode::End).build();
    label.add_css_class("dim-label");
    let more = gtk::Button::builder().label(tr("ResultsMore")).halign(gtk::Align::Start).build();
    more.add_css_class("flat");
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).margin_top(12).build();
    root.append(&label);
    root.append(&more);
    let full = text.to_owned();
    more.connect_clicked(move |button| {
        label.set_label(&full);
        label.set_lines(-1);
        label.set_ellipsize(gtk::pango::EllipsizeMode::None);
        button.set_visible(false);
    });
    root
}
