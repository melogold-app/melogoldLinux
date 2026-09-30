//! Виджеты каталога (§5.4, Windows `ShelfView.cs`, `CollectionHeader.cs`, `MediaCard`): полки,
//! карточки, плитки настроений, списки треков и шапка детального экрана.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_core::music::{MusicItem, Shelf, Track};
use melogold_core::thumbnails;
use melogold_playback::engine::Command;

use crate::library_view::RowContext;
use crate::localization::tr;
use crate::selection::Selection;
use crate::track_row::TrackRow;
use crate::widgets::Cover;
use crate::window::MainWindow;

/// Что играть по нажатию на трек (REWRITE §2.3).
#[derive(Clone, Copy)]
pub enum TrackContext {
    /// Выдача поиска, «Недавние», ссылка: трек и дальше похожие.
    Single,
    /// Альбом, плейлист, «В тренде», популярное исполнителя: весь список с выбранного трека.
    List,
}

/// Список треков (§5.3): двойной щелчок или Enter играет, одиночный щелчок выделяет; Ctrl и Shift
/// выделяют несколько (задание 0004). Треки списка общие со строками: догрузка дописывает их, и
/// нажатие играет весь известный список.
#[derive(Clone)]
pub struct TrackList {
    pub list: gtk::ListBox,
    pub tracks: Rc<RefCell<Vec<Track>>>,
    row: RowContext,
    selection: Rc<Selection>,
}

impl TrackList {
    pub fn new(window: &MainWindow, tracks: &[Track], shown: usize, context: TrackContext) -> TrackList {
        TrackList::with_rows(window, tracks, shown, context, RowContext::Plain)
    }

    /// Список, строки которого знают своё место: из плейлиста и истории меню предлагает «Убрать из…».
    pub fn with_rows(window: &MainWindow, tracks: &[Track], shown: usize, context: TrackContext, row: RowContext) -> TrackList {
        let list = gtk::ListBox::builder().activate_on_single_click(false).build();
        list.add_css_class("track-rows");
        let selection = Selection::for_list_box(window, &list, row);
        let all = Rc::new(RefCell::new(tracks.to_vec()));
        let (weak, shared) = (window.downgrade(), Rc::clone(&all));
        list.connect_row_activated(move |_, row| {
            let Some(window) = weak.upgrade() else { return };
            play_from(&window, &shared.borrow(), row.index().max(0) as usize, context);
        });
        let this = TrackList { list, tracks: all, row, selection };
        for track in tracks.iter().take(shown) {
            this.list.append(&this.make_row(window, track));
        }
        this
    }

    fn make_row(&self, window: &MainWindow, track: &Track) -> gtk::ListBoxRow {
        let row = TrackRow::new(window);
        row.bind(track, self.row);
        row.set_selection(Some(&self.selection));
        gtk::ListBoxRow::builder().child(&row).build()
    }

    /// Дописать строки (продолжения плейлиста и канала).
    pub fn append(&self, window: &MainWindow, tracks: &[Track]) {
        for track in tracks {
            self.list.append(&self.make_row(window, track));
        }
        self.tracks.borrow_mut().extend(tracks.iter().cloned());
    }
}

/// Нажатие на трек: одиночный — трек и дальше похожие, в списке — весь список с него (REWRITE §2.3).
pub fn play_from(window: &MainWindow, tracks: &[Track], index: usize, context: TrackContext) {
    let Some(track) = tracks.get(index).cloned() else { return };
    let player = &window.ctx.services.player;
    match context {
        TrackContext::Single => player.send(Command::PlaySingle { track, start: Duration::ZERO }),
        // Весь список играет с выбранного трека, даже если видны не все строки.
        TrackContext::List => player.send(Command::PlayList { tracks: tracks.to_vec(), start: index, shuffle: false }),
    }
}

/// Длинный список треков Библиотеки на `GtkListView`: строки создаются только для видимого, и
/// тысячи треков не тормозят ни прокрутку, ни изменение размера окна. Выделение — `GtkMultiSelection`.
#[derive(Clone)]
pub struct TrackListView {
    /// Прокрутка со списком — его место на странице.
    pub root: gtk::ScrolledWindow,
    store: gio::ListStore,
    model: gtk::MultiSelection,
    tracks: Rc<RefCell<Vec<Track>>>,
}

impl TrackListView {
    /// `place` — откуда строки (для «Убрать из…»); `context` — что играть по нажатию (у Истории меняется).
    pub fn new(window: &MainWindow, place: RowContext, context: Rc<Cell<TrackContext>>) -> TrackListView {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let model = gtk::MultiSelection::new(Some(store.clone()));
        let tracks: Rc<RefCell<Vec<Track>>> = Rc::default();
        let factory = gtk::SignalListItemFactory::new();
        let view = gtk::ListView::builder().model(&model).factory(&factory).single_click_activate(false).show_separators(false).build();
        view.add_css_class("track-list");
        let selection = Selection::for_list_view(window, &view, &model, Rc::clone(&tracks), place);
        let weak = window.downgrade();
        factory.connect_setup(move |_, item| {
            let (Some(window), Some(item)) = (weak.upgrade(), item.downcast_ref::<gtk::ListItem>()) else { return };
            item.set_child(Some(&TrackRow::new(&window)));
        });
        let selection_for_bind = Rc::downgrade(&selection);
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else { return };
            let (Some(row), Some(object)) = (item.child().and_downcast::<TrackRow>(), item.item().and_downcast::<glib::BoxedAnyObject>())
            else {
                return;
            };
            row.bind(&object.borrow::<Track>(), place);
            row.set_position(item.position());
            row.set_selection(selection_for_bind.upgrade().as_ref());
        });
        let (weak, shared) = (window.downgrade(), Rc::clone(&tracks));
        view.connect_activate(move |_, position| {
            if let Some(window) = weak.upgrade() {
                play_from(&window, &shared.borrow(), position as usize, context.get());
            }
        });
        let clamp = adw::ClampScrollable::builder().maximum_size(1100).child(&view).build();
        let root = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
        // Выделение живёт, пока жив список: держим его в замыкании прокрутки.
        let keep = RefCell::new(Some(selection));
        root.connect_destroy(move |_| {
            keep.take();
        });
        TrackListView { root, store, model, tracks }
    }

    /// Показать треки (после фильтра и сортировки). Тот же список (библиотека сообщила о своём
    /// изменении) не перестраивается: не сбиваются ни прокрутка, ни выделение. Новый — выделение
    /// переносится на те же треки.
    pub fn set_tracks(&self, tracks: Vec<Track>) {
        let same = {
            let old = self.tracks.borrow();
            old.len() == tracks.len() && old.iter().zip(&tracks).all(|(a, b)| a.video_id == b.video_id)
        };
        if same {
            // Данные строк (название, длительность) могли уточниться — в строках они обновятся при прокрутке.
            self.tracks.replace(tracks);
            return;
        }
        let selected: std::collections::HashSet<String> = {
            let bitset = self.model.selection();
            let old = self.tracks.borrow();
            (0..bitset.size()).filter_map(|i| old.get(bitset.nth(i as u32) as usize)).map(|t| t.video_id.clone()).collect()
        };
        let objects: Vec<glib::BoxedAnyObject> = tracks.iter().cloned().map(glib::BoxedAnyObject::new).collect();
        let reselect: Vec<u32> = tracks.iter().enumerate().filter(|(_, t)| selected.contains(&t.video_id)).map(|(i, _)| i as u32).collect();
        self.tracks.replace(tracks);
        self.store.splice(0, self.store.n_items(), &objects);
        for position in reselect {
            self.model.select_item(position, false);
        }
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
    let button = card_view(window, &title, &subtitle, thumbnail.as_deref(), round, wide);
    let (weak, item) = (window.downgrade(), item.clone());
    button.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.activate_item(&item);
        }
    });
    button
}

/// Карточка без действия: обложка и две строки подписи (свои плейлисты Библиотеки — тоже ею).
pub fn card_view(window: &MainWindow, title: &str, subtitle: &str, thumbnail: Option<&str>, round: bool, wide: bool) -> gtk::Button {
    let cover = Cover::new(160);
    cover.root.add_css_class("card-cover");
    if wide {
        // Клипы — карточкой 16:9: кадр видео целиком (§5.3 «Видео и песни»).
        cover.root.set_size_request(240, 135);
    }
    if round {
        cover.root.add_css_class("round");
    }
    cover.set(&window.ctx.services.images, thumbnail, 320);
    let width = if wide { 240 } else { 160 };
    let title_label = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .wrap(true)
        .lines(2)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .width_request(width)
        .build();
    title_label.add_css_class("card-title");
    let subtitle_label = gtk::Label::builder()
        .label(subtitle)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(1)
        .width_request(width)
        .build();
    subtitle_label.add_css_class("dim-label");
    subtitle_label.add_css_class("caption");
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
    cover.root.set_margin_bottom(6);
    content.append(&cover.root);
    content.append(&title_label);
    if !subtitle.is_empty() {
        content.append(&subtitle_label);
    }
    let button = gtk::Button::builder().child(&content).valign(gtk::Align::Start).tooltip_text(title).build();
    button.add_css_class("flat");
    button.add_css_class("card-button");
    button.update_property(&[gtk::accessible::Property::Label(&format!("{title}, {subtitle}"))]);
    button
}

/// Горизонтальный ряд карточек.
pub fn card_row(window: &MainWindow, items: &[MusicItem]) -> gtk::ScrolledWindow {
    let row = gtk::Box::builder().spacing(6).margin_bottom(6).build();
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
        .min_children_per_line(1)
        .max_children_per_line(12)
        .column_spacing(4)
        .row_spacing(12)
        .build();
    for item in items {
        grid.append(&card(window, item));
    }
    grid
}

/// Плитки «Настроения и жанры» — залитые цветом настроения, как у YouTube Music и Android: цвет
/// сверху слева переходит в тон темнее. Подпись — белая, на светлом цвете — тёмная (контраст).
pub fn mood_grid(window: &MainWindow, items: &[MusicItem]) -> gtk::FlowBox {
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(8)
        .column_spacing(10)
        .row_spacing(10)
        .build();
    for item in items {
        let MusicItem::Mood(mood) = item else { continue };
        let label = gtk::Label::builder()
            .label(&mood.title)
            .xalign(0.0)
            .yalign(1.0)
            .wrap(true)
            // По словам, а длинное слово — по буквам: иначе «Концентрация» задавала бы ширину плитки,
            // и две плитки в ряд не давали окну сжаться до 360.
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(2)
            .width_chars(8)
            .max_width_chars(14)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .hexpand(true)
            .build();
        let button = gtk::Button::builder().child(&label).build();
        button.add_css_class("mood-tile");
        if let Some(color) = mood.color {
            crate::widgets::widget_css(&button, &mood_css(color & 0xFF_FFFF));
        }
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

/// Заливка плитки настроения: цвет → тот же на 30 % темнее; подпись по яркости цвета.
fn mood_css(color: u32) -> String {
    let channel = |shift: u32| f64::from((color >> shift) & 0xFF) / 255.0;
    let linear = |c: f64| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let luminance = 0.2126 * linear(channel(16)) + 0.7152 * linear(channel(8)) + 0.0722 * linear(channel(0));
    let text = if luminance > 0.45 { "rgba(0, 0, 0, 0.85)" } else { "white" };
    format!("button.mood-tile {{ background-image: linear-gradient(135deg, #{color:06x}, shade(#{color:06x}, 0.7)); color: {text}; }}")
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
        label.add_css_class("shelf-title");
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
        button.add_css_class("see-all");
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
        let cover = Cover::new(220);
        if round {
            cover.root.add_css_class("round");
        }
        cover.root.add_css_class("large");
        cover.set(&window.ctx.services.images, thumbnail, 544);
        let title_label = gtk::Label::builder().label(title).xalign(0.0).wrap(true).selectable(true).build();
        title_label.add_css_class("title-1");
        title_label.add_css_class("collection-title");
        let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).valign(gtk::Align::Center).hexpand(true).build();
        texts.append(&title_label);
        for (text, class) in [(Some(subtitle), "heading"), (detail, "dim-label")] {
            if let Some(text) = text.filter(|t| !t.is_empty()) {
                let label = gtk::Label::builder().label(text).xalign(0.0).wrap(true).build();
                label.add_css_class(class);
                texts.append(&label);
            }
        }
        let buttons = gtk::Box::builder().spacing(8).margin_top(14).build();
        let scroller =
            gtk::ScrolledWindow::builder().child(&buttons).vscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).build();
        texts.append(&scroller);
        let root = gtk::Box::builder().spacing(28).build();
        root.add_css_class("collection-header");
        root.append(&cover.root);
        root.append(&texts);
        window.register_header(&root);
        // Отсвет шапки — цветом обложки (как фон «Сейчас играет»): сверху слева гуще, к низу сходит
        // на нет. Серая обложка — отсвет нейтральный, из стилей приложения.
        let weak = root.downgrade();
        cover.on_loaded(move |texture| {
            let (weak, texture) = (weak.clone(), texture.clone());
            glib::spawn_future_local(async move {
                let seed = crate::widgets::artwork_seed(&texture).await;
                let (Some(root), Some(seed)) = (weak.upgrade(), seed) else { return };
                let manager = adw::StyleManager::default();
                if manager.is_high_contrast() {
                    return;
                }
                let palette = melogold_core::artwork_colors::palette(seed, manager.is_dark());
                let glow = format!("#{:06x}", palette.glow);
                crate::widgets::widget_css(
                    &root,
                    &format!(
                        ".collection-header {{ background-image: linear-gradient(160deg, alpha({glow}, 0.95), alpha({glow}, 0.35) 55%, alpha({glow}, 0.0)); }}"
                    ),
                );
            });
        });
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

impl CollectionHeader {
    /// Переключатель в шапке: «Сохранить» ↔ «В библиотеке», «Подписаться» ↔ «Вы подписаны».
    /// `changed` зовётся только от нажатия; [`Toggle::set_quietly`] ставит состояние молча.
    pub fn add_toggle(&self, labels: [&'static str; 2], icons: [&'static str; 2], changed: impl Fn(bool) + 'static) -> Toggle {
        let content = adw::ButtonContent::builder().label(labels[0]).icon_name(icons[0]).build();
        let button = gtk::ToggleButton::builder().child(&content).build();
        button.add_css_class("pill");
        let quiet = Rc::new(std::cell::Cell::new(false));
        let flag = Rc::clone(&quiet);
        button.connect_toggled(move |button| {
            let on = button.is_active();
            content.set_label(labels[usize::from(on)]);
            content.set_icon_name(icons[usize::from(on)]);
            if !flag.get() {
                changed(on);
            }
        });
        self.buttons.append(&button);
        Toggle { button, quiet }
    }
}

#[derive(Clone)]
pub struct Toggle {
    pub button: gtk::ToggleButton,
    quiet: Rc<std::cell::Cell<bool>>,
}

impl Toggle {
    /// Состояние без вызова действия (прочитано из базы).
    pub fn set_quietly(&self, on: bool) {
        self.quiet.set(true);
        self.button.set_active(on);
        self.quiet.set(false);
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
