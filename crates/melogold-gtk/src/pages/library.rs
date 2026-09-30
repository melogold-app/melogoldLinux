//! Библиотека (§5.4, REWRITE §3.2): плитки коллекций по ширине окна, свои плейлисты, Избранное,
//! «Все треки», История, «Скачанное», сохранённые альбомы и исполнители, свой плейлист.
//!
//! Каждая страница подписана на свои изменения библиотеки ([`Change`]) и перерисовывается сама.
//! Убранное с «Отменить» страницы не показывают, пока идёт отсрочка.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::music::{MusicItem, Track};
use melogold_core::text::now_ms;
use melogold_data::library::LocalPlaylist;
use melogold_data::{Change, DeviceFilter, Library};
use melogold_playback::engine::Command;

use crate::catalog_widgets::{card_grid, card_view, TrackContext, TrackList, TrackListView};
use crate::library_view::{removal_key_history, removal_key_playlist, removal_key_track, RowContext};
use crate::localization::{plural, tr, trf};
use crate::widgets::{size_text, StateView};
use crate::window::MainWindow;

/// Прокручиваемая страница: над содержимым — заголовок и строка фильтра, содержимое — с состояниями.
struct Page {
    page: adw::NavigationPage,
    body: gtk::Box,
    content: gtk::Box,
    state: StateView,
}

fn page(title: &str, tag: Option<&str>) -> Page {
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    let state = StateView::new(&content);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    body.append(&state.root);
    let clamp = adw::Clamp::builder().maximum_size(1100).child(&body).build();
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
    let mut builder = adw::NavigationPage::builder().title(title).child(&scroller);
    if let Some(tag) = tag {
        builder = builder.tag(tag);
    }
    Page { page: builder.build(), body, content, state }
}

impl Page {
    /// Поставить виджет над содержимым (заголовок, фильтр).
    fn above(&self, widget: &impl IsA<gtk::Widget>) {
        widget.insert_before(&self.body, Some(&self.state.root));
    }
}

fn title_label(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).wrap(true).build();
    label.add_css_class("title-1");
    label
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// Перерисовка: сразу и при изменениях `mask`. Обработчик живёт, пока жива страница.
fn live(window: &MainWindow, page: &adw::NavigationPage, mask: Change, refresh: impl Fn() + 'static) -> Rc<dyn Fn()> {
    let refresh: Rc<dyn Fn()> = Rc::new(refresh);
    window.library_view.listen(mask, &refresh);
    refresh();
    let keep = RefCell::new(Some(Rc::clone(&refresh)));
    page.connect_destroy(move |_| {
        keep.take();
    });
    refresh
}

/// Заголовок коллекции: название, под ним «N треков · 3 ч 20 мин», кнопки «Слушать» и «Перемешать»
/// (играют то, что видно после фильтра и сортировки).
struct Header {
    root: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    buttons: gtk::Box,
}

fn header(window: &MainWindow, title: &str, visible: &Rc<RefCell<Vec<Track>>>) -> Header {
    let title = title_label(title);
    let subtitle = gtk::Label::builder().xalign(0.0).build();
    subtitle.add_css_class("dim-label");
    let buttons = gtk::Box::builder().spacing(8).margin_top(8).build();
    for (label, icon, shuffle) in
        [(tr("PlayAll"), "media-playback-start-symbolic", false), (tr("Shuffle"), "media-playlist-shuffle-symbolic", true)]
    {
        let button = gtk::Button::builder().child(&adw::ButtonContent::builder().label(label).icon_name(icon).build()).build();
        button.add_css_class("pill");
        if !shuffle {
            button.add_css_class("suggested-action");
        }
        let (player, tracks) = (window.ctx.services.player.clone(), Rc::clone(visible));
        button.connect_clicked(move |_| {
            let tracks = tracks.borrow().clone();
            if !tracks.is_empty() {
                player.send(Command::PlayList { tracks, start: 0, shuffle });
            }
        });
        buttons.append(&button);
    }
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
    root.append(&title);
    root.append(&subtitle);
    root.append(&buttons);
    Header { root, title, subtitle, buttons }
}

/// «35 ч 54 мин», «12 мин» — как длительность очереди (Windows `ListeningTime`).
fn listening_time(ms: i64) -> String {
    let minutes = ms / 60_000;
    if minutes >= 60 {
        trf("DurationHoursMinutesFormat", &[&(minutes / 60), &(minutes % 60)])
    } else {
        trf("DurationMinutesFormat", &[&minutes])
    }
}

fn summary(tracks: &[Track]) -> String {
    let total: i64 = tracks.iter().filter_map(|t| t.duration_ms).sum();
    format!("{} · {}", plural("Tracks", tracks.len() as i64), listening_time(total))
}

// ── фильтр и сортировка (§5.3, Windows `ListToolbar`, Android `SortPreferences`) ──

/// Поле сортировки — имена как у Android (`TrackSort`): сохранённое «Title:asc» читают оба клиента.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    DateAdded,
    Recent,
    PlayTime,
    Title,
    Artist,
    Duration,
}

impl Sort {
    fn name(self) -> &'static str {
        match self {
            Sort::DateAdded => "DateAdded",
            Sort::Recent => "Recent",
            Sort::PlayTime => "PlayTime",
            Sort::Title => "Title",
            Sort::Artist => "Artist",
            Sort::Duration => "Duration",
        }
    }

    fn label(self) -> &'static str {
        tr(match self {
            Sort::DateAdded => "SortRecent",
            Sort::Recent => "SortRecentlyPlayed",
            Sort::PlayTime => "SortListeningTime",
            Sort::Title => "SortTitle",
            Sort::Artist => "SortArtist",
            Sort::Duration => "SortDuration",
        })
    }

    /// Направление по умолчанию: новое, долгое и много слушанное — сверху, названия — по алфавиту.
    fn descending(self) -> bool {
        !matches!(self, Sort::Title | Sort::Artist)
    }
}

/// Строка над списком: фильтр по названию, исполнителю и альбому и, если есть, сортировка.
#[derive(Clone)]
struct Toolbar {
    root: gtk::Box,
    filter: gtk::SearchEntry,
    sort: Option<(gtk::DropDown, Vec<Sort>)>,
    /// Направление из сохранённого значения: Android умеет его менять, здесь — только читается.
    descending: Rc<RefCell<Option<bool>>>,
}

impl Toolbar {
    fn new(window: &MainWindow, screen: &'static str, sorts: &[Sort], changed: Rc<dyn Fn()>) -> Toolbar {
        let filter = gtk::SearchEntry::builder().placeholder_text(tr("Filter")).max_width_chars(40).build();
        filter.update_property(&[gtk::accessible::Property::Label(tr("Filter"))]);
        // Фильтр — своей естественной ширины (до 40 знаков) у левого края и сжимается в узком окне;
        // сортировка справа видна всегда.
        filter.set_hexpand(false);
        let root = gtk::Box::builder().spacing(8).build();
        root.append(&filter);
        root.append(&gtk::Box::builder().hexpand(true).build());
        let on_filter = Rc::clone(&changed);
        filter.connect_search_changed(move |_| on_filter());
        let descending: Rc<RefCell<Option<bool>>> = Rc::default();
        let sort = (!sorts.is_empty()).then(|| {
            let labels: Vec<&str> = sorts.iter().map(|s| s.label()).collect();
            let dropdown = gtk::DropDown::from_strings(&labels);
            dropdown.set_valign(gtk::Align::Center);
            dropdown.set_tooltip_text(Some(tr("Sort")));
            dropdown.update_property(&[gtk::accessible::Property::Label(tr("Sort"))]);
            let saved = window.ctx.settings.sort(screen).unwrap_or_default();
            let (field, direction) = saved.split_once(':').unwrap_or((saved.as_str(), ""));
            if let Some(index) = sorts.iter().position(|s| s.name() == field) {
                dropdown.set_selected(index as u32);
                descending.replace(match direction {
                    "desc" => Some(true),
                    "asc" => Some(false),
                    _ => None,
                });
            }
            let (settings, list, direction) = (Rc::clone(&window.ctx.settings), sorts.to_vec(), Rc::clone(&descending));
            dropdown.connect_selected_notify(move |dropdown| {
                let sort = list.get(dropdown.selected() as usize).copied().unwrap_or(list[0]);
                direction.replace(None);
                settings.set_sort(screen, &format!("{}:{}", sort.name(), if sort.descending() { "desc" } else { "asc" }));
                changed();
            });
            root.append(&dropdown);
            (dropdown, sorts.to_vec())
        });
        Toolbar { root, filter, sort, descending }
    }

    fn matches(&self, track: &Track, filter: &str) -> bool {
        filter.is_empty()
            || [Some(track.title.as_str()), track.artists_text.as_deref(), track.album_title.as_deref()]
                .iter()
                .flatten()
                .any(|text| text.to_lowercase().contains(filter))
    }

    fn current(&self) -> Option<(Sort, bool)> {
        let (dropdown, sorts) = self.sort.as_ref()?;
        let sort = sorts.get(dropdown.selected() as usize).copied()?;
        Some((sort, self.descending.borrow().unwrap_or(sort.descending())))
    }

    /// Отфильтровать и упорядочить. `entries` — в порядке базы (новое сверху) со временем прослушивания;
    /// фильтр и сортировка — по тому, что видно (со своими названиями), а в список идут исходные треки.
    fn apply(&self, entries: &[(Track, i64)], display: impl Fn(&Track) -> Track) -> Vec<Track> {
        let filter = self.filter.text().trim().to_lowercase();
        let shown: Vec<(Track, i64, &Track)> = entries.iter().map(|(t, time)| (display(t), *time, t)).collect();
        let mut list: Vec<&(Track, i64, &Track)> = shown.iter().filter(|(t, ..)| self.matches(t, &filter)).collect();
        if let Some((sort, descending)) = self.current() {
            // Сначала по возрастанию (Android `sorted`), затем разворот, если по убыванию.
            match sort {
                Sort::DateAdded | Sort::Recent => list.reverse(),
                Sort::PlayTime => list.sort_by_key(|(_, time, _)| *time),
                Sort::Title => list.sort_by_cached_key(|(t, ..)| t.title.to_lowercase()),
                Sort::Artist => list
                    .sort_by_cached_key(|(t, ..)| (t.artists_text.as_deref().unwrap_or_default().to_lowercase(), t.title.to_lowercase())),
                Sort::Duration => list.sort_by_key(|(t, ..)| t.duration_ms.unwrap_or(0)),
            }
            if descending {
                list.reverse();
            }
        }
        list.into_iter().map(|(_, _, original)| (*original).clone()).collect()
    }
}

/// Список треков с меню по месту: нажатие играет весь список с выбранного трека.
fn track_list(window: &MainWindow, tracks: &[Track], context: RowContext, plays: TrackContext) -> gtk::ListBox {
    TrackList::with_rows(window, tracks, usize::MAX, plays, context).list
}

fn heading(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).margin_top(12).build();
    label.add_css_class("title-4");
    label
}

// ── главная Библиотеки ──

pub fn root(window: &MainWindow) -> adw::NavigationPage {
    let p = page(tr("LibraryHeader"), Some("root"));
    p.above(&title_label(tr("LibraryHeader")));
    let (weak, content, state) = (window.downgrade(), p.content.clone(), p.state.clone());
    let mask = Change(Change::LIKES.0 | Change::PLAYLISTS.0 | Change::BOOKMARKS.0 | Change::HISTORY.0 | Change::DOWNLOADS.0);
    live(window, &p.page, mask, move || {
        let Some(window) = weak.upgrade() else { return };
        let songs = window.ctx.services.songs.clone();
        let task = window.ctx.services.db(move |library| {
            let counts = library.counts().unwrap_or_default();
            let all = library.all_tracks_count().unwrap_or(0);
            let downloads = library.download_ids().unwrap_or_default();
            let cached = songs.complete_tracks().iter().filter(|(id, _)| !downloads.contains(id)).count();
            (counts, all, downloads.len(), cached)
        });
        let (weak, content, state) = (window.downgrade(), content.clone(), state.clone());
        glib::spawn_future_local(async move {
            let (Some(window), Some((counts, all, downloaded, cached))) = (weak.upgrade(), task.await) else { return };
            clear(&content);
            // Плитки коллекций по ширине окна, без пустоты справа (§5.4).
            let tiles = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .homogeneous(true)
                .min_children_per_line(1)
                .max_children_per_line(4)
                .column_spacing(8)
                .row_spacing(8)
                .build();
            let downloads_text = if downloaded > 0 {
                trf("DownloadsCountFormat", &[&downloaded, &cached])
            } else {
                trf("DownloadsCachedCountFormat", &[&cached])
            };
            type Open = fn(&MainWindow) -> adw::NavigationPage;
            let entries: [(&str, &str, String, Open); 6] = [
                ("view-list-bullet-symbolic", tr("AllTracks"), plural("Tracks", all), all_tracks),
                ("offline-filled-symbolic", tr("Downloads"), downloads_text, downloads_page),
                ("heart-filled-symbolic", tr("Favorites"), plural("Tracks", counts.likes), favorites),
                ("document-open-recent-symbolic", tr("History"), tr("HistoryHint").to_owned(), history),
                ("media-optical-cd-audio-symbolic", tr("ResultsAlbums"), plural("Albums", counts.albums), saved_albums),
                ("avatar-default-symbolic", tr("ArtistsAndChannels"), plural("Artists", counts.artists), saved_artists),
            ];
            for (icon, title, subtitle, open) in entries {
                tiles.append(&tile(&window, icon, title, &subtitle, open));
            }
            content.append(&tiles);

            let row = gtk::Box::builder().spacing(8).margin_top(12).build();
            let label = heading(tr("ResultsPlaylists"));
            label.set_margin_top(0);
            label.set_hexpand(true);
            label.set_valign(gtk::Align::Center);
            row.append(&label);
            let create = gtk::Button::builder()
                .child(&adw::ButtonContent::builder().label(tr("NewPlaylist")).icon_name("list-add-symbolic").build())
                .build();
            create.add_css_class("flat");
            let weak = window.downgrade();
            create.connect_clicked(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.new_playlist(Vec::new());
                }
            });
            row.append(&create);
            content.append(&row);
            let view = &window.library_view;
            let playlists: Vec<LocalPlaylist> =
                view.playlists().into_iter().filter(|p| !view.is_removing(&removal_key_playlist(p.id))).collect();
            if playlists.is_empty() {
                let status = adw::StatusPage::builder()
                    .icon_name("view-list-bullet-symbolic")
                    .title(tr("NoPlaylistsYet"))
                    .description(tr("NoPlaylistsHint"))
                    .build();
                status.add_css_class("compact");
                content.append(&status);
            } else {
                let grid = gtk::FlowBox::builder()
                    .selection_mode(gtk::SelectionMode::None)
                    .homogeneous(true)
                    .min_children_per_line(1)
                    .max_children_per_line(12)
                    .column_spacing(4)
                    .row_spacing(12)
                    .build();
                for playlist in playlists {
                    let cover = playlist.mosaic.first().cloned().or(playlist.thumbnail_url.clone());
                    let card = card_view(&window, &playlist.name, &plural("Tracks", playlist.track_count), cover.as_deref(), false, false);
                    let (weak, id) = (window.downgrade(), playlist.id);
                    card.connect_clicked(move |_| {
                        if let Some(window) = weak.upgrade() {
                            window.push(&local_playlist(&window, id));
                        }
                    });
                    grid.append(&card);
                }
                content.append(&grid);
            }
            // Последняя строка — перенос библиотеки из ViTune или ViMusic (§5.4, задание Windows 0004).
            let import = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).margin_top(24).build();
            import.add_css_class("boxed-list");
            let row =
                adw::ActionRow::builder().title(tr("LibraryImport")).subtitle(tr("LibraryImportDescription")).activatable(true).build();
            row.add_prefix(&gtk::Image::from_icon_name("document-open-symbolic"));
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = window.downgrade();
            row.connect_activated(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.import_backup();
                }
            });
            import.append(&row);
            content.append(&import);
            state.content();
        });
    });
    p.page
}

fn tile(window: &MainWindow, icon: &str, title: &str, subtitle: &str, open: fn(&MainWindow) -> adw::NavigationPage) -> gtk::Button {
    let image = gtk::Image::builder().icon_name(icon).pixel_size(20).valign(gtk::Align::Center).build();
    image.add_css_class("accent");
    let title_label = gtk::Label::builder().label(title).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    title_label.add_css_class("heading");
    let subtitle_label = gtk::Label::builder().label(subtitle).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    subtitle_label.add_css_class("dim-label");
    subtitle_label.add_css_class("caption");
    let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).valign(gtk::Align::Center).hexpand(true).build();
    texts.append(&title_label);
    texts.append(&subtitle_label);
    let content = gtk::Box::builder().spacing(16).margin_top(8).margin_bottom(8).margin_start(8).margin_end(8).build();
    content.append(&image);
    content.append(&texts);
    let button = gtk::Button::builder().child(&content).build();
    button.add_css_class("library-tile");
    button.update_property(&[gtk::accessible::Property::Label(&format!("{title}, {subtitle}"))]);
    let weak = window.downgrade();
    button.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&open(&window));
        }
    });
    button
}

// ── списки треков ──

type Entries = Rc<RefCell<Option<Vec<(Track, i64)>>>>;

/// Страница длинного списка (§5.3): сверху заголовок и фильтр, под ними прокручивается список
/// ([`TrackListView`] — строки только для видимого).
struct ListPage {
    page: adw::NavigationPage,
    top: gtk::Box,
    list: TrackListView,
    state: StateView,
}

fn list_page(window: &MainWindow, title: &str, tag: Option<&str>, place: RowContext, plays: Rc<Cell<TrackContext>>) -> ListPage {
    let list = TrackListView::new(window, place, plays);
    let state = StateView::new(&list.root);
    let top = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(24)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let clamp = adw::Clamp::builder().maximum_size(1100).child(&top).build();
    let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
    root.append(&clamp);
    root.append(&state.root);
    let mut builder = adw::NavigationPage::builder().title(title).child(&root);
    if let Some(tag) = tag {
        builder = builder.tag(tag);
    }
    ListPage { page: builder.build(), top, list, state }
}

/// Страница списка треков: заголовок со счётом, фильтр и сортировка, список; `load` — из базы.
struct TrackPage {
    page: adw::NavigationPage,
    header: Header,
    entries: Entries,
    show: Rc<dyn Fn()>,
}

fn track_page(window: &MainWindow, title: &str, tag: Option<&str>, screen: &'static str, sorts: &[Sort], place: RowContext) -> TrackPage {
    let p = list_page(window, title, tag, place, Rc::new(Cell::new(TrackContext::List)));
    let visible: Rc<RefCell<Vec<Track>>> = Rc::default();
    let head = header(window, title, &visible);
    p.top.append(&head.root);
    let entries: Entries = Rc::default();
    let toolbar_cell: Rc<RefCell<Option<Toolbar>>> = Rc::default();
    let (weak, list, state, subtitle) = (window.downgrade(), p.list.clone(), p.state.clone(), head.subtitle.clone());
    let (show_entries, show_toolbar) = (Rc::clone(&entries), Rc::clone(&toolbar_cell));
    let show: Rc<dyn Fn()> = Rc::new(move || {
        let (Some(window), Some(toolbar)) = (weak.upgrade(), show_toolbar.borrow().clone()) else { return };
        let Some(all) = show_entries.borrow().clone() else { return };
        let all: Vec<(Track, i64)> = match place {
            RowContext::Playlist(id) => {
                all.into_iter().filter(|(t, _)| !window.library_view.is_removing(&removal_key_track(id, &t.video_id))).collect()
            }
            _ => all,
        };
        let tracks: Vec<Track> = all.iter().map(|(t, _)| t.clone()).collect();
        subtitle.set_label(&summary(&tracks));
        toolbar.root.set_visible(!all.is_empty());
        let shown = toolbar.apply(&all, |t| window.display(t));
        list.set_tracks(shown.clone());
        if all.is_empty() {
            let (icon, title, hint) = match screen {
                "favorites" => ("heart-outline-symbolic", tr("FavoritesEmpty"), tr("FavoritesEmptyHint")),
                "allTracks" => ("view-list-bullet-symbolic", tr("AllTracks"), tr("AllTracksEmpty")),
                _ => ("view-list-bullet-symbolic", tr("PlaylistEmpty"), tr("PlaylistEmptyHint")),
            };
            state.empty(icon, title, hint);
        } else if shown.is_empty() {
            state.empty("edit-find-symbolic", tr("NothingFound"), "");
        } else {
            state.content();
        }
        visible.replace(shown);
    });
    let toolbar = Toolbar::new(window, screen, sorts, Rc::clone(&show));
    p.top.append(&toolbar.root);
    toolbar_cell.replace(Some(toolbar));
    TrackPage { page: p.page, header: head, entries, show }
}

impl TrackPage {
    fn load(&self, window: &MainWindow, mask: Change, load: impl Fn(&Library) -> Vec<(Track, i64)> + Send + Sync + Copy + 'static) {
        let (weak, entries, show) = (window.downgrade(), Rc::clone(&self.entries), Rc::clone(&self.show));
        // Свои названия меняют порядок сортировки и то, что находит фильтр.
        let mask = Change(mask.0 | Change::OVERRIDES.0);
        live(window, &self.page, mask, move || {
            let Some(window) = weak.upgrade() else { return };
            let task = window.ctx.services.db(load);
            let (entries, show) = (Rc::clone(&entries), Rc::clone(&show));
            glib::spawn_future_local(async move {
                if let Some(list) = task.await {
                    entries.replace(Some(list));
                    show();
                }
            });
        });
    }
}

/// Избранное (REWRITE §3.2.2): новое сверху; сортировка и фильтр.
pub fn favorites(window: &MainWindow) -> adw::NavigationPage {
    let sorts = [Sort::DateAdded, Sort::Title, Sort::Artist, Sort::Duration];
    let page = track_page(window, tr("Favorites"), None, "favorites", &sorts, RowContext::Plain);
    page.load(window, Change(Change::LIKES.0 | Change::BLOCKS.0), |library| {
        library.favorites().unwrap_or_default().into_iter().map(|t| (t, 0)).collect()
    });
    page.page
}

/// «Все треки» (задание Windows 0005): прослушанное, лайкнутое, лежащее в своих плейлистах и скачанное.
pub fn all_tracks(window: &MainWindow) -> adw::NavigationPage {
    let sorts = [Sort::Recent, Sort::PlayTime, Sort::Title, Sort::Artist, Sort::Duration];
    let page = track_page(window, tr("AllTracks"), None, "allTracks", &sorts, RowContext::Plain);
    let mask = Change(Change::LIKES.0 | Change::PLAYLISTS.0 | Change::HISTORY.0 | Change::DOWNLOADS.0 | Change::BLOCKS.0);
    page.load(window, mask, |library| library.all_tracks().unwrap_or_default().into_iter().map(|e| (e.track, e.play_time_ms)).collect());
    page.page
}

/// Свой плейлист (REWRITE §3.8.1): «Убрать из плейлиста» и «Удалить плейлист» — с «Отменить»;
/// «Переименовать»; фильтр. Тег страницы — ключ плейлиста: удаление закрывает её.
pub fn local_playlist(window: &MainWindow, id: i64) -> adw::NavigationPage {
    let name = window.library_view.playlists().into_iter().find(|p| p.id == id).map(|p| p.name).unwrap_or_default();
    let page = track_page(window, &name, Some(&removal_key_playlist(id)), "playlistItems", &[], RowContext::Playlist(id));
    let menu = gtk::gio::Menu::new();
    menu.append(Some(tr("Rename")), Some(&format!("win.playlist-rename({id})")));
    menu.append(Some(tr("LinuxCopyLink")), Some(&format!("win.playlist-copy-link({id})")));
    menu.append(Some(tr("DeletePlaylist")), Some(&format!("win.playlist-delete({id})")));
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .valign(gtk::Align::Center)
        .tooltip_text(tr("RowMenu"))
        .build();
    more.add_css_class("circular");
    page.header.buttons.append(&more);
    // Название меняется на месте после «Переименовать».
    let (weak, nav_page, title) = (window.downgrade(), page.page.clone(), page.header.title.clone());
    let rename: Rc<dyn Fn()> = Rc::new(move || {
        let Some(window) = weak.upgrade() else { return };
        if let Some(playlist) = window.library_view.playlists().into_iter().find(|p| p.id == id) {
            nav_page.set_title(&playlist.name);
            title.set_label(&playlist.name);
        }
    });
    window.library_view.listen(Change::PLAYLISTS, &rename);
    let keep = RefCell::new(Some(rename));
    page.page.connect_destroy(move |_| {
        keep.take();
    });
    page.load(window, Change::PLAYLISTS, move |library| {
        library.playlist_tracks(id).unwrap_or_default().into_iter().map(|t| (t, 0)).collect()
    });
    page.page
}

/// История (REWRITE §3.2.4): «Недавние» — трек и дальше похожие, «Чаще всего» за период — весь список.
pub fn history(window: &MainWindow) -> adw::NavigationPage {
    let plays = Rc::new(Cell::new(TrackContext::Single));
    let p = list_page(window, tr("History"), None, RowContext::History, Rc::clone(&plays));
    let clear_button = gtk::Button::builder().label(tr("ClearHistory")).valign(gtk::Align::Center).build();
    clear_button.add_css_class("flat");
    let top = gtk::Box::builder().spacing(8).build();
    let title = title_label(tr("History"));
    title.set_hexpand(true);
    top.append(&title);
    top.append(&clear_button);
    p.top.append(&top);
    let modes = adw::ToggleGroup::builder().halign(gtk::Align::Start).build();
    modes.add(adw::Toggle::builder().name("recent").label(tr("HistoryRecent")).build());
    modes.add(adw::Toggle::builder().name("top").label(tr("HistoryMostPlayed")).build());
    modes.set_active_name(Some("recent"));
    let periods = adw::ToggleGroup::builder().halign(gtk::Align::Start).visible(false).build();
    periods.add_css_class("flat");
    for (name, key) in [("7", "Days7"), ("30", "Days30"), ("365", "Year"), ("all", "AllTime")] {
        periods.add(adw::Toggle::builder().name(name).label(tr(key)).build());
    }
    periods.set_active_name(Some("30"));
    // Чьи прослушивания (задание Windows 0002 §5): виден с аккаунтом, когда есть чужие.
    let device_names = gtk::StringList::new(&[tr("HistoryDeviceAll"), tr("HistoryDeviceThis")]);
    let devices = gtk::DropDown::builder()
        .model(&device_names)
        .valign(gtk::Align::Center)
        .visible(false)
        .tooltip_text(tr("HistoryDeviceChoose"))
        .build();
    devices.update_property(&[gtk::accessible::Property::Label(tr("HistoryDeviceChoose"))]);
    let device_filters: Rc<RefCell<Vec<DeviceFilter>>> = Rc::new(RefCell::new(vec![DeviceFilter::All, DeviceFilter::This(None)]));
    let controls = gtk::Box::builder().spacing(12).build();
    controls.append(&modes);
    controls.append(&periods);
    controls.append(&devices);
    let controls_scroller =
        gtk::ScrolledWindow::builder().child(&controls).vscrollbar_policy(gtk::PolicyType::Never).propagate_natural_height(true).build();
    p.top.append(&controls_scroller);

    let (weak, list, state, modes_ref, periods_ref) = (window.downgrade(), p.list.clone(), p.state.clone(), modes.clone(), periods.clone());
    let (devices_ref, filters_ref) = (devices.clone(), Rc::clone(&device_filters));
    let refresh = live(window, &p.page, Change(Change::HISTORY.0 | Change::BLOCKS.0), move || {
        let Some(window) = weak.upgrade() else { return };
        let filter = filters_ref.borrow().get(devices_ref.selected() as usize).cloned().unwrap_or_default();
        let top = modes_ref.active_name().as_deref() == Some("top");
        periods_ref.set_visible(top);
        let day = 86_400_000;
        let since = match periods_ref.active_name().as_deref() {
            Some("7") => Some(now_ms() - 7 * day),
            Some("30") => Some(now_ms() - 30 * day),
            Some("365") => Some(now_ms() - 365 * day),
            _ => None,
        };
        let task = window.ctx.services.db(move |library| {
            if top {
                library.most_played_for(since, 500, &filter).unwrap_or_default().into_iter().map(|e| e.track).collect::<Vec<_>>()
            } else {
                library.recent_history_for(500, &filter).unwrap_or_default().into_iter().map(|e| e.track).collect()
            }
        });
        let (weak, list, state, plays) = (window.downgrade(), list.clone(), state.clone(), Rc::clone(&plays));
        glib::spawn_future_local(async move {
            let (Some(window), Some(tracks)) = (weak.upgrade(), task.await) else { return };
            let view = &window.library_view;
            let tracks: Vec<Track> = tracks.into_iter().filter(|t| !view.is_removing(&removal_key_history(&t.video_id))).collect();
            plays.set(if top { TrackContext::List } else { TrackContext::Single });
            let empty = tracks.is_empty();
            list.set_tracks(tracks);
            if empty {
                state.empty("document-open-recent-symbolic", tr("HistoryEmpty"), tr("HistoryEmptyHint"));
            } else {
                state.content();
            }
        });
    });
    let (r1, r2, r3) = (Rc::clone(&refresh), Rc::clone(&refresh), Rc::clone(&refresh));
    modes.connect_active_name_notify(move |_| r1());
    periods.connect_active_name_notify(move |_| r2());
    devices.connect_selected_notify(move |_| r3());
    load_history_devices(window, &devices, &device_names, &device_filters);

    let weak = window.downgrade();
    clear_button.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            confirm_clear_history(&window);
        }
    });
    p.page
}

/// Устройства для фильтра Истории: «Это устройство» — с его id на сервере, остальные — по именам
/// из списка устройств аккаунта; ушедшее из списка — «Другое устройство».
fn load_history_devices(window: &MainWindow, dropdown: &gtk::DropDown, names: &gtk::StringList, filters: &Rc<RefCell<Vec<DeviceFilter>>>) {
    let services = &window.ctx.services;
    let Some(session) = services.account.session() else { return };
    let own = session.device_id.clone();
    let seen = services.db(|library| library.play_devices().unwrap_or_default());
    let account = std::sync::Arc::clone(&services.account);
    let named = services.run(async move { account.device_names().await.unwrap_or_default() });
    let (dropdown, names, filters) = (dropdown.clone(), names.clone(), Rc::clone(filters));
    glib::spawn_future_local(async move {
        let (Some(seen), Some(named)) = (seen.await, named.await) else { return };
        let mut others: Vec<(String, String)> = seen
            .into_iter()
            .filter(|id| *id != own)
            .map(|id| {
                let name = named.get(&id).map(|(name, _)| name.clone()).unwrap_or_else(|| tr("HistoryDeviceOther").to_owned());
                (id, name)
            })
            .collect();
        if others.is_empty() {
            return;
        }
        others.sort_by(|a, b| a.1.cmp(&b.1));
        let mut list = vec![DeviceFilter::All, DeviceFilter::This(Some(own))];
        for (id, name) in others {
            names.append(&name);
            list.push(DeviceFilter::Device(id));
        }
        filters.replace(list);
        dropdown.set_visible(true);
    });
}

/// «Очистить историю…»: с числом прослушиваний, кнопка — разрушающая (§5.6).
fn confirm_clear_history(window: &MainWindow) {
    let task = window.ctx.services.db(|library| library.play_count().unwrap_or(0));
    let weak = window.downgrade();
    glib::spawn_future_local(async move {
        let (Some(window), Some(count)) = (weak.upgrade(), task.await) else { return };
        // С аккаунтом история очищается на всех устройствах — так и сказать.
        let key =
            if window.ctx.services.account.session().is_some() { "ClearHistoryEverywherePromptFormat" } else { "ClearHistoryPromptFormat" };
        let dialog = adw::AlertDialog::new(Some(tr("ClearHistoryTitle")), Some(&trf(key, &[&count])));
        dialog.add_response("cancel", tr("Cancel"));
        dialog.add_response("clear", tr("ClearHistory").trim_end_matches('…'));
        dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = window.downgrade();
        dialog.connect_response(Some("clear"), move |_, _| {
            let Some(window) = weak.upgrade() else { return };
            let task = window.ctx.services.db(|library| library.clear_history());
            let weak = window.downgrade();
            glib::spawn_future_local(async move {
                if let (Some(window), Some(Ok(()))) = (weak.upgrade(), task.await) {
                    window.toast(tr("HistoryCleared"));
                }
            });
        });
        dialog.present(Some(&window.window));
    });
}

/// «Скачанное» (задание Windows 0003): сверху скачанное — насовсем; ниже «В кэше» — прослушанное
/// целиком, играет без сети, пока его не сменят новые треки.
pub fn downloads_page(window: &MainWindow) -> adw::NavigationPage {
    let p = page(tr("Downloads"), None);
    p.above(&title_label(tr("Downloads")));
    let note = gtk::Label::builder().label(tr("DownloadsCachedNote")).xalign(0.0).wrap(true).build();
    note.add_css_class("dim-label");
    p.above(&note);
    let (weak, content, state) = (window.downgrade(), p.content.clone(), p.state.clone());
    live(window, &p.page, Change::DOWNLOADS, move || {
        let Some(window) = weak.upgrade() else { return };
        let songs = window.ctx.services.songs.clone();
        let task = window.ctx.services.db(move |library| {
            let downloads = library.downloads().unwrap_or_default();
            let known: std::collections::HashSet<&str> = downloads.iter().map(|t| t.video_id.as_str()).collect();
            let (mut cached, mut cached_bytes) = (Vec::new(), 0);
            for (id, bytes) in songs.complete_tracks() {
                if known.contains(id.as_str()) {
                    continue;
                }
                if let Ok(Some(track)) = library.track(&id) {
                    cached_bytes += bytes;
                    cached.push(track);
                }
            }
            (downloads, cached, cached_bytes)
        });
        let (weak, content, state) = (window.downgrade(), content.clone(), state.clone());
        glib::spawn_future_local(async move {
            let (Some(window), Some((downloads, cached, cached_bytes))) = (weak.upgrade(), task.await) else { return };
            clear(&content);
            if downloads.is_empty() && cached.is_empty() {
                state.empty("offline-filled-symbolic", tr("Downloads"), tr("DownloadsEmpty"));
                return;
            }
            if !downloads.is_empty() {
                let size = size_text(window.ctx.services.downloads.size());
                content.append(&heading(&trf("DownloadsDownloadedFormat", &[&downloads.len(), &size])));
                content.append(&track_list(&window, &downloads, RowContext::Plain, TrackContext::List));
            }
            if !cached.is_empty() {
                content.append(&heading(&trf("DownloadsCachedFormat", &[&cached.len(), &size_text(cached_bytes)])));
                content.append(&track_list(&window, &cached, RowContext::Plain, TrackContext::List));
            }
            state.content();
        });
    });
    p.page
}

// ── сохранённое ──

pub fn saved_albums(window: &MainWindow) -> adw::NavigationPage {
    let empty = ("media-optical-cd-audio-symbolic", "AlbumsEmpty", "AlbumsEmptyHint");
    saved_page(window, tr("ResultsAlbums"), empty, |library| {
        library.saved_albums().unwrap_or_default().into_iter().map(MusicItem::Album).collect()
    })
}

pub fn saved_artists(window: &MainWindow) -> adw::NavigationPage {
    let empty = ("avatar-default-symbolic", "ArtistsEmpty", "ArtistsEmptyHint");
    saved_page(window, tr("ArtistsAndChannels"), empty, |library| {
        library.saved_artists().unwrap_or_default().into_iter().map(MusicItem::Artist).collect()
    })
}

fn saved_page(
    window: &MainWindow,
    title: &str,
    empty: (&'static str, &'static str, &'static str),
    load: fn(&Library) -> Vec<MusicItem>,
) -> adw::NavigationPage {
    let p = page(title, None);
    p.above(&title_label(title));
    let (weak, content, state) = (window.downgrade(), p.content.clone(), p.state.clone());
    live(window, &p.page, Change::BOOKMARKS, move || {
        let Some(window) = weak.upgrade() else { return };
        let task = window.ctx.services.db(load);
        let (weak, content, state) = (window.downgrade(), content.clone(), state.clone());
        glib::spawn_future_local(async move {
            let (Some(window), Some(items)) = (weak.upgrade(), task.await) else { return };
            clear(&content);
            if items.is_empty() {
                state.empty(empty.0, tr(empty.1), tr(empty.2));
                return;
            }
            content.append(&card_grid(&window, &items));
            state.content();
        });
    });
    p.page
}
