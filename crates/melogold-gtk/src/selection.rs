//! Действия с выделенными треками (задание 0004, Windows `SelectionBar.cs`): выделяют, как везде в
//! системе, — Ctrl+щелчок, Shift+щелчок, Ctrl+A, Esc снимает; на сенсоре долгое нажатие включает
//! выделение касаниями. От двух выделенных — панель над плеером поверх списка (не сжимает его):
//! «Выбрано: N» · Слушать · В конец очереди · В Избранное · Добавить в плейлист… · Новый плейлист… ·
//! Скачать | Выбрать все · Снять выделение. В узком окне подписи уходят, а в телефонной ширине
//! часть кнопок — в «…». Правый щелчок по выделенному — те же действия меню.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use melogold_core::music::Track;
use melogold_playback::engine::Command;

use crate::library_view::RowContext;
use crate::localization::{plural, tr, trf};
use crate::track_row::TrackRow;
use crate::window::MainWindow;

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Выделение одного списка: что выделено (в порядке списка) и как выделить всё или снять.
pub struct Selection {
    id: u64,
    count: Box<dyn Fn() -> u32>,
    tracks: Box<dyn Fn() -> Vec<Track>>,
    select_all: Box<dyn Fn()>,
    clear: Box<dyn Fn()>,
    contains: Box<dyn Fn(&TrackRow) -> bool>,
    /// Касания отмечают и снимают отметку (после долгого нажатия на сенсоре).
    touch_mode: Cell<bool>,
    /// Откуда выделяли: у своего плейлиста «Указать альбом…» предлагает его название.
    place: RowContext,
}

impl Selection {
    pub fn count(&self) -> u32 {
        (self.count)()
    }

    pub fn tracks(&self) -> Vec<Track> {
        (self.tracks)()
    }

    pub fn contains(&self, row: &TrackRow) -> bool {
        (self.contains)(row)
    }

    /// Выделение `GtkListBox`: строки треков — `TrackRow` внутри строки списка; остальные строки
    /// (альбомы и исполнители в выдаче) в выделение не входят.
    pub fn for_list_box(window: &MainWindow, list: &gtk::ListBox, place: RowContext) -> Rc<Selection> {
        list.set_selection_mode(gtk::SelectionMode::Multiple);
        let track_rows = |list: &gtk::ListBox| -> Vec<Track> {
            list.selected_rows().iter().filter_map(|row| row.child().and_downcast::<TrackRow>()).filter_map(|row| row.track()).collect()
        };
        let (l1, l2, l3, l4) = (list.downgrade(), list.downgrade(), list.downgrade(), list.downgrade());
        let selection = Rc::new(Selection {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            count: Box::new(move || l1.upgrade().map(|l| track_rows(&l).len() as u32).unwrap_or(0)),
            tracks: Box::new(move || {
                // Порядок списка, а не порядок нажатий.
                let Some(list) = l2.upgrade() else { return Vec::new() };
                let mut rows = list.selected_rows();
                rows.sort_by_key(|r| r.index());
                rows.iter().filter_map(|r| r.child().and_downcast::<TrackRow>()).filter_map(|r| r.track()).collect()
            }),
            select_all: Box::new(move || {
                if let Some(list) = l3.upgrade() {
                    list.select_all();
                }
            }),
            clear: Box::new(move || {
                if let Some(list) = l4.upgrade() {
                    list.unselect_all();
                }
            }),
            contains: Box::new(|row| row.parent().and_downcast::<gtk::ListBoxRow>().is_some_and(|r| r.is_selected())),
            touch_mode: Cell::new(false),
            place,
        });
        let (weak, sel) = (window.downgrade(), Rc::downgrade(&selection));
        list.connect_selected_rows_changed(move |_| {
            if let (Some(window), Some(selection)) = (weak.upgrade(), sel.upgrade()) {
                window.selection_changed(&selection);
            }
        });
        install_list_gestures(window, list.upcast_ref(), &selection, |list, row| {
            if let Some(list_row) = row.parent().and_downcast::<gtk::ListBoxRow>() {
                let list = list.downcast_ref::<gtk::ListBox>().expect("список");
                if list_row.is_selected() {
                    list.unselect_row(&list_row);
                } else {
                    list.select_row(Some(&list_row));
                }
            }
        });
        selection
    }

    /// Выделение `GtkListView` с `GtkMultiSelection`: позиции — места треков в `tracks`.
    pub fn for_list_view(
        window: &MainWindow,
        view: &gtk::ListView,
        model: &gtk::MultiSelection,
        tracks: Rc<RefCell<Vec<Track>>>,
        place: RowContext,
    ) -> Rc<Selection> {
        let positions = |model: &gtk::MultiSelection| -> Vec<u32> {
            let bitset = model.selection();
            (0..bitset.size()).map(|i| bitset.nth(i as u32)).collect()
        };
        let (m1, m2, m3, m4, m5) = (model.downgrade(), model.downgrade(), model.downgrade(), model.downgrade(), model.downgrade());
        let selection = Rc::new(Selection {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            count: Box::new(move || m1.upgrade().map(|m| m.selection().size() as u32).unwrap_or(0)),
            tracks: Box::new(move || {
                let Some(model) = m2.upgrade() else { return Vec::new() };
                let all = tracks.borrow();
                positions(&model).into_iter().filter_map(|p| all.get(p as usize).cloned()).collect()
            }),
            select_all: Box::new(move || {
                if let Some(model) = m3.upgrade() {
                    model.select_all();
                }
            }),
            clear: Box::new(move || {
                if let Some(model) = m4.upgrade() {
                    model.unselect_all();
                }
            }),
            contains: Box::new(move |row| m5.upgrade().is_some_and(|m| m.is_selected(row.position()))),
            touch_mode: Cell::new(false),
            place,
        });
        let (weak, sel) = (window.downgrade(), Rc::downgrade(&selection));
        model.connect_selection_changed(move |_, _, _| {
            if let (Some(window), Some(selection)) = (weak.upgrade(), sel.upgrade()) {
                window.selection_changed(&selection);
            }
        });
        let model_weak = model.downgrade();
        install_list_gestures(window, view.upcast_ref(), &selection, move |_, row| {
            if let Some(model) = model_weak.upgrade() {
                let position = row.position();
                if model.is_selected(position) {
                    model.unselect_item(position);
                } else {
                    model.select_item(position, false);
                }
            }
        });
        selection
    }
}

/// Сенсор: долгое нажатие отмечает строку и включает режим, в котором касания отмечают и снимают
/// отметку. Клавиша меню и Shift+F10 по строке в фокусе — её меню. Список ушёл с экрана — панель
/// его выделения тоже.
fn install_list_gestures(
    window: &MainWindow,
    list: &gtk::Widget,
    selection: &Rc<Selection>,
    toggle: impl Fn(&gtk::Widget, &TrackRow) + 'static,
) {
    let toggle = Rc::new(toggle);
    let long_press = gtk::GestureLongPress::builder().touch_only(true).build();
    let (weak_list, sel, t) = (list.downgrade(), Rc::downgrade(selection), Rc::clone(&toggle));
    long_press.connect_pressed(move |gesture, x, y| {
        let (Some(list), Some(selection)) = (weak_list.upgrade(), sel.upgrade()) else { return };
        if let Some(row) = row_at(&list, x, y) {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            selection.touch_mode.set(true);
            if !selection.contains(&row) {
                t(&list, &row);
            }
        }
    });
    list.add_controller(long_press);
    let tap = gtk::GestureClick::builder().touch_only(true).build();
    tap.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (weak_list, sel) = (list.downgrade(), Rc::downgrade(selection));
    tap.connect_released(move |gesture, _, x, y| {
        let (Some(list), Some(selection)) = (weak_list.upgrade(), sel.upgrade()) else { return };
        if !selection.touch_mode.get() {
            return;
        }
        if let Some(row) = row_at(&list, x, y) {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            toggle(&list, &row);
            if selection.count() == 0 {
                selection.touch_mode.set(false);
            }
        }
    });
    list.add_controller(tap);

    // Клавиши строки в фокусе: меню (клавиша меню, Shift+F10), Delete — убрать, Alt+↑ ↓ — порядок
    // в своём плейлисте (§5.3).
    let keys = gtk::EventControllerKey::new();
    let (weak_list, weak_window) = (list.downgrade(), window.downgrade());
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let Some(row) = weak_list.upgrade().and_then(|l| gtk::prelude::RootExt::focus(&l.root()?)).and_then(|w| find_row(&w)) else {
            return glib::Propagation::Proceed;
        };
        let alt = modifiers.contains(gdk::ModifierType::ALT_MASK);
        let handled = match key {
            gdk::Key::Menu => {
                row.popup_menu(None);
                true
            }
            gdk::Key::F10 if modifiers.contains(gdk::ModifierType::SHIFT_MASK) => {
                row.popup_menu(None);
                true
            }
            gdk::Key::Delete | gdk::Key::KP_Delete if modifiers.is_empty() => row.remove_from_place(),
            gdk::Key::Up | gdk::Key::Down if alt => match (row.context(), row.video_id(), weak_window.upgrade()) {
                (crate::library_view::RowContext::Playlist(id), Some(video_id), Some(window)) => {
                    window.move_in_playlist(id, &video_id, if key == gdk::Key::Up { -1 } else { 1 });
                    true
                }
                _ => false,
            },
            _ => false,
        };
        if handled {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    list.add_controller(keys);

    let (weak, id) = (window.downgrade(), selection.id);
    list.connect_unmap(move |_| {
        if let Some(window) = weak.upgrade() {
            window.forget_selection(id);
        }
    });
    let (weak, sel) = (window.downgrade(), Rc::downgrade(selection));
    list.connect_map(move |_| {
        if let (Some(window), Some(selection)) = (weak.upgrade(), sel.upgrade()) {
            window.selection_changed(&selection);
        }
    });
}

fn row_at(list: &gtk::Widget, x: f64, y: f64) -> Option<TrackRow> {
    let picked = list.pick(x, y, gtk::PickFlags::DEFAULT)?;
    find_row(&picked)
}

/// Строка трека, в которой лежит виджет (или которая лежит в нём — строка списка в фокусе).
fn find_row(widget: &gtk::Widget) -> Option<TrackRow> {
    if let Some(row) = widget.downcast_ref::<TrackRow>() {
        return Some(row.clone());
    }
    if let Some(row) = widget.first_child().and_downcast::<TrackRow>() {
        return Some(row);
    }
    widget.ancestor(TrackRow::static_type()).and_downcast::<TrackRow>()
}

// ── панель ──

pub struct SelectionBar {
    pub root: gtk::Revealer,
    count: gtk::Label,
    labelled: Vec<(adw::ButtonContent, &'static str)>,
    /// Уходят в «…» в телефонной ширине.
    extra: Vec<gtk::Widget>,
    playlists: gtk::MenuButton,
    more: gtk::MenuButton,
}

impl SelectionBar {
    pub fn new() -> SelectionBar {
        let count = gtk::Label::builder().margin_start(8).margin_end(4).build();
        count.add_css_class("heading");
        count.add_css_class("numeric");
        let content = gtk::Box::builder().spacing(2).build();
        content.append(&count);
        let mut labelled: Vec<(adw::ButtonContent, &'static str)> = Vec::new();
        let mut extra: Vec<gtk::Widget> = Vec::new();
        let button = |key: &'static str, icon: &str, action: &str| {
            let label = adw::ButtonContent::builder().icon_name(icon).label(tr(key)).build();
            let button = gtk::Button::builder().child(&label).action_name(action).tooltip_text(tr(key)).build();
            button.add_css_class("flat");
            content.append(&button);
            (button, label)
        };
        for (key, icon, action) in [
            ("SelectionPlay", "media-playback-start-symbolic", "win.selection-play"),
            ("SelectionQueue", "list-add-symbolic", "win.selection-queue"),
            ("SelectionLike", "heart-outline-symbolic", "win.selection-like"),
        ] {
            let (_, label) = button(key, icon, action);
            labelled.push((label, key));
        }
        // «Добавить в плейлист…» — меню со списком плейлистов, как в меню трека.
        // Длинные подписи («Добавить в плейлист…», «Новый плейлист…») — только в подсказках: иначе
        // панель не влезает рядом с боковой панелью.
        let playlists =
            gtk::MenuButton::builder().icon_name("view-list-bullet-symbolic").tooltip_text(tr("SelectionAddToPlaylist")).build();
        playlists.add_css_class("flat");
        playlists.update_property(&[gtk::accessible::Property::Label(tr("SelectionAddToPlaylist"))]);
        content.append(&playlists);
        extra.push(playlists.clone().upcast());
        let (new_playlist, new_label) = button("SelectionNewPlaylist", "document-new-symbolic", "win.selection-new-playlist");
        new_label.set_label("");
        new_playlist.update_property(&[gtk::accessible::Property::Label(tr("SelectionNewPlaylist"))]);
        extra.push(new_playlist.upcast());
        let (download, label) = button("SelectionDownload", "folder-download-symbolic", "win.selection-download");
        labelled.push((label, "SelectionDownload"));
        extra.push(download.upcast());
        let (album, album_label) = button("LinuxSetAlbum", "media-optical-cd-audio-symbolic", "win.selection-set-album");
        album_label.set_label("");
        album.update_property(&[gtk::accessible::Property::Label(tr("LinuxSetAlbum"))]);
        extra.push(album.upcast());
        let separator = gtk::Separator::builder().orientation(gtk::Orientation::Vertical).margin_start(4).margin_end(4).build();
        content.append(&separator);
        extra.push(separator.upcast());
        let (all_button, all) = button("SelectionSelectAll", "edit-select-all-symbolic", "win.selection-select-all");
        all.set_label("");
        all_button.set_tooltip_text(Some(&format!("{} (Ctrl+A)", tr("SelectionSelectAll"))));
        all_button.update_property(&[gtk::accessible::Property::Label(tr("SelectionSelectAll"))]);
        extra.push(all_button.upcast());
        let more = gtk::MenuButton::builder().icon_name("view-more-symbolic").tooltip_text(tr("RowMenu")).visible(false).build();
        more.add_css_class("flat");
        content.append(&more);
        let clear = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .action_name("win.selection-clear")
            .tooltip_text(format!("{} (Esc)", tr("SelectionClear")))
            .build();
        clear.add_css_class("flat");
        clear.update_property(&[gtk::accessible::Property::Label(tr("SelectionClear"))]);
        content.append(&clear);
        content.add_css_class("selection-bar");
        let root = gtk::Revealer::builder()
            .child(&content)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .reveal_child(false)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(12)
            .build();
        SelectionBar { root, count, labelled, extra, playlists, more }
    }

    /// Меню плейлистов собираются при открытии: плейлисты могли появиться.
    fn connect(&self, window: &MainWindow) {
        let weak = window.downgrade();
        self.playlists.set_create_popup_func(move |button| {
            if let Some(window) = weak.upgrade() {
                button.set_menu_model(Some(&playlist_menu(&window)));
            }
        });
        let weak = window.downgrade();
        self.more.set_create_popup_func(move |button| {
            if let Some(window) = weak.upgrade() {
                button.set_menu_model(Some(&overflow_menu(&window)));
            }
        });
    }

    fn show(&self, count: u32) {
        self.count.set_label(&trf("SelectionCountFormat", &[&count]));
        self.root.set_reveal_child(true);
        self.root.set_can_target(true);
    }

    fn hide(&self) {
        self.root.set_reveal_child(false);
        self.root.set_can_target(false);
    }

    fn shown(&self) -> bool {
        self.root.reveals_child()
    }

    /// Уже 1100 — только значки (подписи — в подсказках).
    pub fn set_compact(&self, compact: bool) {
        for (content, key) in &self.labelled {
            content.set_label(if compact { "" } else { tr(key) });
        }
    }

    /// Телефонная ширина: реже нужное — в «…».
    pub fn set_phone(&self, phone: bool) {
        for widget in &self.extra {
            widget.set_visible(!phone);
        }
        self.more.set_visible(phone);
    }
}

fn playlist_menu(window: &MainWindow) -> gio::Menu {
    let menu = gio::Menu::new();
    let new = gio::Menu::new();
    new.append(Some(tr("SelectionNewPlaylist")), Some("win.selection-new-playlist"));
    menu.append_section(None, &new);
    let existing = gio::Menu::new();
    for playlist in window.library_view.playlists().iter().take(30) {
        existing.append(Some(&playlist.name), Some(&format!("win.selection-add-to-playlist({})", playlist.id)));
    }
    menu.append_section(None, &existing);
    menu
}

fn overflow_menu(window: &MainWindow) -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append_submenu(Some(tr("SelectionAddToPlaylist")), &playlist_menu(window));
    menu.append(Some(tr("SelectionNewPlaylist")), Some("win.selection-new-playlist"));
    menu.append(Some(tr("SelectionDownload")), Some("win.selection-download"));
    menu.append(Some(tr("LinuxSetAlbum")), Some("win.selection-set-album"));
    menu.append(Some(tr("SelectionSelectAll")), Some("win.selection-select-all"));
    menu
}

/// Меню правого щелчка по выделенному — те же действия, что на панели.
pub fn menu(window: &MainWindow) -> gio::Menu {
    let menu = gio::Menu::new();
    let play = gio::Menu::new();
    play.append(Some(tr("SelectionPlay")), Some("win.selection-play"));
    play.append(Some(tr("SelectionQueue")), Some("win.selection-queue"));
    menu.append_section(None, &play);
    let collect = gio::Menu::new();
    collect.append(Some(tr("SelectionLike")), Some("win.selection-like"));
    collect.append_submenu(Some(tr("SelectionAddToPlaylist")), &playlist_menu(window));
    collect.append(Some(tr("SelectionDownload")), Some("win.selection-download"));
    collect.append(Some(tr("LinuxSetAlbum")), Some("win.selection-set-album"));
    menu.append_section(None, &collect);
    let select = gio::Menu::new();
    select.append(Some(tr("SelectionSelectAll")), Some("win.selection-select-all"));
    select.append(Some(tr("SelectionClear")), Some("win.selection-clear"));
    menu.append_section(None, &select);
    menu
}

// ── окно ──

impl MainWindow {
    /// Выделение списка изменилось: два трека и больше — панель, иначе — спрятать (если она этого списка).
    pub fn selection_changed(&self, selection: &Rc<Selection>) {
        let count = selection.count();
        if count == 0 {
            selection.touch_mode.set(false);
        }
        if count >= 2 {
            self.show_selection(selection);
        } else {
            self.forget_selection(selection.id);
        }
    }

    pub fn show_selection(&self, selection: &Rc<Selection>) {
        self.selection.replace(Some(Rc::clone(selection)));
        self.selection_bar.show(selection.count());
    }

    pub fn forget_selection(&self, id: u64) {
        let current = self.selection.borrow().as_ref().map(|s| s.id);
        if current == Some(id) {
            self.selection.replace(None);
            self.selection_bar.hide();
        }
    }

    /// Esc: снять выделение, если панель на экране.
    pub fn clear_selection(&self) -> bool {
        let selection = self.selection.borrow().clone();
        match selection {
            Some(selection) if self.selection_bar.shown() => {
                (selection.clear)();
                true
            }
            _ => false,
        }
    }

    fn selected(&self) -> Vec<Track> {
        self.selection.borrow().as_ref().map(|s| s.tracks()).unwrap_or_default()
    }

    pub fn install_selection_actions(&self) {
        self.selection_bar.connect(self);
        self.selection_bar.hide();
        let simple = |name: &str, run: fn(&MainWindow, Vec<Track>)| {
            let weak = self.downgrade();
            gio::ActionEntry::builder(name)
                .activate(move |_: &adw::ApplicationWindow, _, _| {
                    if let Some(window) = weak.upgrade() {
                        let tracks = window.selected();
                        if !tracks.is_empty() {
                            run(&window, tracks);
                        }
                    }
                })
                .build()
        };
        let weak = self.downgrade();
        let add_to_playlist = gio::ActionEntry::builder("selection-add-to-playlist")
            .parameter_type(Some(glib::VariantTy::INT64))
            .activate(move |_: &adw::ApplicationWindow, _, parameter| {
                if let (Some(window), Some(id)) = (weak.upgrade(), parameter.and_then(|p| p.get::<i64>())) {
                    let tracks = window.selected();
                    if !tracks.is_empty() {
                        window.add_to_playlist(id, tracks);
                    }
                }
            })
            .build();
        let weak = self.downgrade();
        let select_all = gio::ActionEntry::builder("selection-select-all")
            .activate(move |_: &adw::ApplicationWindow, _, _| {
                if let Some(selection) = weak.upgrade().and_then(|w| w.selection.borrow().clone()) {
                    (selection.select_all)();
                }
            })
            .build();
        let weak = self.downgrade();
        let clear = gio::ActionEntry::builder("selection-clear")
            .activate(move |_: &adw::ApplicationWindow, _, _| {
                if let Some(window) = weak.upgrade() {
                    window.clear_selection();
                }
            })
            .build();
        self.window.add_action_entries([
            // «Слушать» — в порядке списка, новой очередью.
            simple("selection-play", |w, tracks| w.ctx.services.player.send(Command::PlayList { tracks, start: 0, shuffle: false })),
            simple("selection-queue", |w, tracks| {
                w.toast(&trf("QueuedCountFormat", &[&plural("Tracks", tracks.len() as i64)]));
                w.ctx.services.player.send(Command::AddToEnd(tracks));
            }),
            // ♡ всем сразу — одной записью.
            simple("selection-like", |w, tracks| {
                w.toast(&trf("LikedCountFormat", &[&plural("Tracks", tracks.len() as i64)]));
                w.set_liked(tracks, true);
            }),
            simple("selection-new-playlist", |w, tracks| w.new_playlist(tracks)),
            simple("selection-set-album", |w, tracks| {
                let place = w.selection.borrow().as_ref().map(|s| s.place).unwrap_or_default();
                w.set_album(tracks, place);
            }),
            // Скачанное, скачивающееся и трансляции пропускаются.
            simple("selection-download", |w, tracks| {
                let count = w.ctx.services.downloads.download(&tracks);
                if count == 0 {
                    w.toast(tr("NothingToDownload"));
                } else {
                    w.toast(&trf("DownloadingCountFormat", &[&plural("Tracks", count as i64)]));
                }
            }),
            add_to_playlist,
            select_all,
            clear,
        ]);
    }
}
