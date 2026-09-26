//! Главное окно (docs/PROMPT.md §5.2).
//!
//! ```text
//! AdwApplicationWindow
//! └ AdwNavigationView: «окно» и поверх него «Сейчас играет»
//!   └ AdwToolbarView ─ низ: панель воспроизведения во всю ширину
//!     └ AdwToastOverlay — «Отменить» всплывает поверх и не сжимает окно
//!       └ AdwOverlaySplitView (справа — очередь)
//!         └ AdwOverlaySplitView (слева — разделы)
//!           ├ боковая панель: Тренды · Новое · Библиотека … Настройки
//!           └ AdwToolbarView: заголовок («Назад», поиск, главное меню) + стек разделов,
//!             у каждого раздела свой AdwNavigationView
//! ```
//!
//! Боковая панель — `AdwOverlaySplitView`, а не `AdwNavigationSplitView`: в узком окне она
//! сворачивается и выезжает поверх по кнопке, как у Windows. У `AdwNavigationSplitView` свёрнутая
//! панель — отдельная страница со своей «Назад», и она спорила бы с «Назад» внутри раздела.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use melogold_core::links::{self, MelogoldLink};
use melogold_core::music::{MusicItem, Track};
use melogold_core::settings::{keys, Tab};
use melogold_playback::engine::{Command, Event, QueueView, State};

use crate::app::AppContext;
use crate::localization::{tr, trf};
use crate::now_playing::NowPlaying;
use crate::pages;
use crate::player_bar::{Hidden, PlayerBar};
use crate::queue_panel::QueuePanel;
use crate::services::playback_settings;

#[derive(Clone)]
pub struct MainWindow(Rc<Inner>);

#[derive(Clone)]
pub struct WeakWindow(Weak<Inner>);

impl WeakWindow {
    pub fn upgrade(&self) -> Option<MainWindow> {
        self.0.upgrade().map(MainWindow)
    }
}

pub struct Inner {
    pub window: adw::ApplicationWindow,
    pub ctx: Rc<AppContext>,
    root_nav: adw::NavigationView,
    main_page: adw::NavigationPage,
    outer: adw::ToolbarView,
    toasts: adw::ToastOverlay,
    split: adw::OverlaySplitView,
    queue_split: adw::OverlaySplitView,
    stack: gtk::Stack,
    sections: Vec<Section>,
    back: gtk::Button,
    search: gtk::SearchEntry,
    suggestions: gtk::Popover,
    suggestion_list: gtk::ListBox,
    suggestion_token: Cell<u64>,
    current: Cell<Tab>,
    player_bar: OnceCell<PlayerBar>,
    now_playing: OnceCell<NowPlaying>,
    queue_panel: OnceCell<QueuePanel>,
    state: RefCell<State>,
}

struct Section {
    tab: Tab,
    nav: adw::NavigationView,
    row: gtk::ListBoxRow,
    list: gtk::ListBox,
}

impl std::ops::Deref for MainWindow {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl MainWindow {
    pub fn new(app: &adw::Application, ctx: Rc<AppContext>) -> Self {
        let settings = &ctx.settings;

        // ── боковая панель ──
        let top_list = sidebar_list();
        top_list.set_vexpand(true);
        let bottom_list = sidebar_list();
        let mut sections = Vec::new();
        for tab in Tab::ALL {
            let (icon, label) = match tab {
                Tab::Trends => ("trending-up-symbolic", tr("NavTrends.Content")),
                Tab::WhatsNew => ("new-releases-symbolic", tr("NavNew.Content")),
                Tab::Library => ("library-symbolic", tr("NavLibrary.Content")),
                Tab::Settings => ("preferences-system-symbolic", tr("NavSettings")),
            };
            let row = sidebar_row(icon, label);
            row.set_widget_name(tab.id());
            let list = if tab == Tab::Settings { &bottom_list } else { &top_list };
            list.append(&row);
            sections.push(Section { tab, nav: adw::NavigationView::new(), row, list: list.clone() });
        }
        let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&top_list).vexpand(true).build();
        sidebar_box.append(&scroller);
        sidebar_box.append(&bottom_list);
        let sidebar = adw::ToolbarView::new();
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.set_title_widget(Some(&adw::WindowTitle::new("Melogold", "")));
        sidebar.add_top_bar(&sidebar_header);
        sidebar.set_content(Some(&sidebar_box));

        // ── содержимое ──
        let stack = gtk::Stack::new();
        for section in &sections {
            stack.add_named(&section.nav, Some(section.tab.id()));
        }
        let header = adw::HeaderBar::new();
        let sidebar_toggle =
            gtk::ToggleButton::builder().icon_name("sidebar-show-symbolic").tooltip_text(tr("LinuxShowSidebar")).visible(false).build();
        let back = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(format!("{} (Alt+←)", tr("Back")))
            .action_name("win.back")
            .visible(false)
            .build();
        header.pack_start(&sidebar_toggle);
        header.pack_start(&back);
        let search = gtk::SearchEntry::builder().placeholder_text(tr("SearchBox.PlaceholderText")).hexpand(true).build();
        search.update_property(&[gtk::accessible::Property::Label(tr("SearchBox.PlaceholderText"))]);
        let clamp = adw::Clamp::builder().maximum_size(520).tightening_threshold(360).child(&search).hexpand(true).build();
        header.set_title_widget(Some(&clamp));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&main_menu())
            .primary(true)
            .tooltip_text(format!("{} (F10)", tr("LinuxMainMenu")))
            .build();
        header.pack_end(&menu_button);
        let content = adw::ToolbarView::new();
        content.add_top_bar(&header);
        content.set_content(Some(&stack));

        let split =
            adw::OverlaySplitView::builder().sidebar(&sidebar).content(&content).min_sidebar_width(200.0).max_sidebar_width(260.0).build();
        split.bind_property("collapsed", &sidebar_toggle, "visible").sync_create().build();
        split.bind_property("show-sidebar", &sidebar_toggle, "active").sync_create().bidirectional().build();
        let queue_split = adw::OverlaySplitView::builder()
            .content(&split)
            .sidebar_position(gtk::PackType::End)
            .show_sidebar(false)
            .min_sidebar_width(280.0)
            .max_sidebar_width(360.0)
            .build();

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&queue_split));
        let outer = adw::ToolbarView::new();
        outer.set_content(Some(&toasts));
        let main_page = adw::NavigationPage::builder().title("Melogold").tag("main").child(&outer).build();
        let root_nav = adw::NavigationView::new();
        root_nav.add(&main_page);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Melogold")
            .content(&root_nav)
            .default_width(settings.get(&keys::WINDOW_WIDTH).max(360))
            .default_height(settings.get(&keys::WINDOW_HEIGHT).max(294))
            .width_request(360)
            .height_request(294)
            .build();
        if settings.get(&keys::WINDOW_MAXIMIZED) {
            window.maximize();
        }

        let suggestion_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::Single).build();
        suggestion_list.add_css_class("navigation-sidebar");
        let suggestions = gtk::Popover::builder()
            .child(&suggestion_list)
            .autohide(false)
            .has_arrow(false)
            .can_focus(false)
            .position(gtk::PositionType::Bottom)
            .halign(gtk::Align::Start)
            .build();
        suggestions.set_parent(&search);

        let this = MainWindow(Rc::new(Inner {
            window,
            ctx,
            root_nav,
            main_page,
            outer,
            toasts,
            split,
            queue_split,
            stack,
            sections,
            back,
            search,
            suggestions,
            suggestion_list,
            suggestion_token: Cell::new(0),
            current: Cell::new(Tab::Trends),
            player_bar: OnceCell::new(),
            now_playing: OnceCell::new(),
            queue_panel: OnceCell::new(),
            state: RefCell::default(),
        }));
        this.build_player();
        this.install_breakpoints();
        this.populate_sections();
        this.connect_sidebar(&top_list);
        this.connect_sidebar(&bottom_list);
        this.install_actions();
        this.install_keys();
        this.install_search();
        this.install_drop();
        this.connect_close();
        this.listen_player();
        this.show_tab(this.ctx.settings.get(&keys::LAST_TAB));

        #[cfg(debug_assertions)]
        crate::snapshot::maybe_start(&this);
        this
    }

    pub fn downgrade(&self) -> WeakWindow {
        WeakWindow(Rc::downgrade(&self.0))
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    pub fn player_bar(&self) -> &PlayerBar {
        self.player_bar.get().expect("панель собрана в new")
    }

    fn section(&self, tab: Tab) -> &Section {
        self.sections.iter().find(|s| s.tab == tab).expect("раздел есть")
    }

    pub fn nav(&self, tab: Tab) -> &adw::NavigationView {
        &self.section(tab).nav
    }

    /// Открыть страницу в стеке текущего раздела (REWRITE §2.3: детальные экраны — в стеке раздела).
    pub fn push(&self, page: &adw::NavigationPage) {
        self.close_now_playing();
        self.nav(self.current.get()).push(page);
    }

    pub fn show_tab(&self, tab: Tab) {
        let section = self.section(tab);
        self.stack.set_visible_child_name(tab.id());
        for other in &self.sections {
            if other.list != section.list {
                other.list.unselect_all();
            }
        }
        section.list.select_row(Some(&section.row));
        self.current.set(tab);
        self.ctx.settings.set(&keys::LAST_TAB, tab);
        self.update_back();
        if self.split.is_collapsed() {
            self.split.set_show_sidebar(false);
        }
    }

    /// Назад: сначала «Сейчас играет», затем стек раздела. В корне ничего не делаем.
    pub fn go_back(&self) -> bool {
        if self.close_now_playing() {
            return true;
        }
        let nav = self.nav(self.current.get());
        if nav.navigation_stack().n_items() > 1 {
            nav.pop();
            true
        } else {
            false
        }
    }

    pub fn show_now_playing(&self) {
        let Some(now_playing) = self.now_playing.get() else { return };
        if self.state.borrow().track.is_none() {
            return;
        }
        if self.root_nav.visible_page().as_ref() != Some(&now_playing.page) {
            self.root_nav.push(&now_playing.page);
        }
    }

    fn close_now_playing(&self) -> bool {
        let open = self.root_nav.visible_page().is_some_and(|p| p.tag().as_deref() == Some("now-playing"));
        if open {
            self.root_nav.pop_to_page(&self.main_page);
        }
        open
    }

    /// Ссылка или текст: из командной строки, второго экземпляра, перетаскивания, поля поиска.
    pub fn open_text(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if let Some(link) = links::parse(text) {
            tracing::info!("вход: ссылка melogold://");
            match link {
                // Экран «Сервер» с подтверждением и вход по приглашению — в срезе 5 (аккаунт).
                MelogoldLink::Server { url, .. } | MelogoldLink::Invite { server: url, .. } => {
                    self.show_tab(Tab::Settings);
                    let host = url::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or(url);
                    self.toast(&trf("LinuxServerLinkOpened", &[&host]));
                }
                MelogoldLink::Request { .. } => self.toast(tr("LinuxLinkRequest")),
                MelogoldLink::Unsupported => self.toast(tr("LinkUnsupported")),
            }
            return;
        }
        self.search_for(text);
    }

    /// Выдача поиска в стеке текущего раздела.
    pub fn search_for(&self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.suggestions.popdown();
        if self.search.text() != query {
            self.search.set_text(query);
        }
        if self.current.get() == Tab::Settings {
            self.show_tab(Tab::Trends);
        }
        tracing::debug!("поиск");
        self.push(&pages::search::page(self, query));
    }

    /// Строка выдачи: трек играет одиночным треком с радио (REWRITE §2.3), остальное открывается.
    pub fn activate_item(&self, item: &MusicItem) {
        match item {
            MusicItem::Track(track) => self.ctx.services.player.send(Command::PlaySingle { track: track.clone(), start: Duration::ZERO }),
            other => self.push(&pages::placeholder_for(other)),
        }
    }

    /// Меню «…» строки трека: действия получают трек параметром действия окна.
    pub fn track_menu(&self, track: &Track) -> gio::MenuModel {
        let target = serde_json::to_string(track).unwrap_or_default().to_variant();
        let item = |label: &str, action: &str| {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some(action), Some(&target));
            item
        };
        let menu = gio::Menu::new();
        let queue = gio::Menu::new();
        queue.append_item(&item(tr("MenuPlayNext"), "win.track-play-next"));
        queue.append_item(&item(tr("MenuAddToQueue"), "win.track-add-to-queue"));
        menu.append_section(None, &queue);
        let other = gio::Menu::new();
        other.append_item(&item(tr("MenuTrackRadio"), "win.track-radio"));
        other.append_item(&item(tr("MenuCopyLink"), "win.track-copy-link"));
        menu.append_section(None, &other);
        menu.upcast()
    }

    // ── сборка ──

    fn build_player(&self) {
        let bar = PlayerBar::new(self);
        self.outer.add_bottom_bar(&bar.root);
        let _ = self.player_bar.set(bar);
        let now_playing = NowPlaying::new(self);
        let _ = self.now_playing.set(now_playing);
        let queue = QueuePanel::new(self);
        self.queue_split.set_sidebar(Some(&queue.root));
        let _ = self.queue_panel.set(queue);
    }

    /// Пороги ширины (§5.2): 1100 — громкость кнопкой; 720 — разделы и панель в две строки; 480 —
    /// «Очередь» уходит в «…». Действует последний подошедший порог, поэтому каждый несёт всё своё.
    fn install_breakpoints(&self) {
        let bar = self.player_bar();
        let breakpoint = |condition: &str| adw::Breakpoint::new(adw::BreakpointCondition::parse(condition).expect("условие порога"));
        let volume_as_button = |b: &adw::Breakpoint| {
            b.add_setter(&bar.volume_inline, "visible", Some(&false.to_value()));
            b.add_setter(&bar.volume_button, "visible", Some(&true.to_value()));
        };
        let two_rows = |b: &adw::Breakpoint| {
            b.add_setter(&self.split, "collapsed", Some(&true.to_value()));
            b.add_setter(&self.split, "show-sidebar", Some(&false.to_value()));
            b.add_setter(&self.queue_split, "collapsed", Some(&true.to_value()));
            b.add_setter(&bar.seek_center, "visible", Some(&false.to_value()));
            b.add_setter(&bar.seek_top, "visible", Some(&true.to_value()));
            b.add_setter(&bar.shuffle, "visible", Some(&false.to_value()));
            b.add_setter(&bar.repeat, "visible", Some(&false.to_value()));
        };
        // Меню «…» следует за порогом: спрятанное с панели появляется в нём и уходит обратно.
        let menu_follows = |b: &adw::Breakpoint, hidden: Hidden| {
            let weak = self.downgrade();
            b.connect_apply(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.player_bar().set_hidden(hidden);
                }
            });
            let weak = self.downgrade();
            b.connect_unapply(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.player_bar().set_hidden(Hidden::default());
                }
            });
        };
        let medium = breakpoint("max-width: 1100sp");
        volume_as_button(&medium);
        self.window.add_breakpoint(medium);
        let narrow = breakpoint("max-width: 720sp");
        volume_as_button(&narrow);
        two_rows(&narrow);
        menu_follows(&narrow, Hidden { modes: true, queue: false });
        self.window.add_breakpoint(narrow);
        let phone = breakpoint("max-width: 480sp");
        volume_as_button(&phone);
        two_rows(&phone);
        phone.add_setter(&bar.queue, "visible", Some(&false.to_value()));
        menu_follows(&phone, Hidden { modes: true, queue: true });
        self.window.add_breakpoint(phone);
    }

    fn populate_sections(&self) {
        for section in &self.sections {
            let page = match section.tab {
                Tab::Trends => pages::placeholder(tr("TrendsHeader"), "trending-up-symbolic", tr("LinuxSectionSoonTrends")),
                Tab::WhatsNew => pages::placeholder(tr("NewHeader"), "new-releases-symbolic", tr("LinuxSectionSoonNew")),
                Tab::Library => pages::placeholder(tr("LibraryHeader"), "library-symbolic", tr("LinuxSectionSoonLibrary")),
                Tab::Settings => pages::settings::root(self),
            };
            section.nav.add(&page);
            let weak = self.downgrade();
            section.nav.connect_visible_page_notify(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.update_back();
                }
            });
        }
    }

    fn update_back(&self) {
        let depth = self.nav(self.current.get()).navigation_stack().n_items();
        self.back.set_visible(depth > 1);
    }

    fn connect_sidebar(&self, list: &gtk::ListBox) {
        let weak = self.downgrade();
        list.connect_row_activated(move |_, row| {
            let Some(window) = weak.upgrade() else { return };
            let Some(tab) = Tab::from_id(&row.widget_name()) else { return };
            window.close_now_playing();
            if tab == window.current.get() {
                // Повторное нажатие по активному разделу — к его корню (REWRITE §2.3).
                let nav = window.nav(tab);
                if let Some(root) = nav.navigation_stack().item(0).and_downcast::<adw::NavigationPage>() {
                    nav.pop_to_page(&root);
                }
                if window.split.is_collapsed() {
                    window.split.set_show_sidebar(false);
                }
            } else {
                tracing::debug!(раздел = tab.id(), "переход");
                window.show_tab(tab);
            }
        });
    }

    // ── действия ──

    fn install_actions(&self) {
        let player = self.ctx.services.player.clone();
        let simple = |name: &str, run: Box<dyn Fn(&MainWindow)>| {
            let weak = self.downgrade();
            gio::ActionEntry::builder(name)
                .activate(move |_: &adw::ApplicationWindow, _, _| {
                    if let Some(window) = weak.upgrade() {
                        run(&window);
                    }
                })
                .build()
        };
        let with_track = |name: &str, run: Box<dyn Fn(&MainWindow, Track)>| {
            let weak = self.downgrade();
            gio::ActionEntry::builder(name)
                .parameter_type(Some(glib::VariantTy::STRING))
                .activate(move |_: &adw::ApplicationWindow, _, parameter| {
                    let track = parameter.and_then(|p| p.get::<String>()).and_then(|json| serde_json::from_str::<Track>(&json).ok());
                    if let (Some(window), Some(track)) = (weak.upgrade(), track) {
                        run(&window, track);
                    }
                })
                .build()
        };
        let weak = self.downgrade();
        let section = gio::ActionEntry::builder("section")
            .parameter_type(Some(glib::VariantTy::INT32))
            .activate(move |_: &adw::ApplicationWindow, _, parameter| {
                let index = parameter.and_then(|p| p.get::<i32>()).unwrap_or(0);
                if let (Some(window), Some(tab)) = (weak.upgrade(), Tab::ALL.get(index.max(0) as usize)) {
                    window.close_now_playing();
                    window.show_tab(*tab);
                }
            })
            .build();
        let send = |command: fn() -> Command| {
            let player = player.clone();
            Box::new(move |_: &MainWindow| player.send(command())) as Box<dyn Fn(&MainWindow)>
        };
        let entries = [
            simple(
                "back",
                Box::new(|w| {
                    w.go_back();
                }),
            ),
            simple("search", Box::new(|w| w.focus_search())),
            section,
            simple(
                "preferences",
                Box::new(|w| {
                    w.close_now_playing();
                    w.show_tab(Tab::Settings)
                }),
            ),
            simple("now-playing", Box::new(|w| w.show_now_playing())),
            simple("play-pause", send(|| Command::TogglePlay)),
            simple("next", send(|| Command::Next)),
            simple("previous", send(|| Command::Previous)),
            simple("seek-forward", send(|| Command::SeekBy(5000))),
            simple("seek-backward", send(|| Command::SeekBy(-5000))),
            simple("volume-up", Box::new(|w| w.player_bar().set_volume(w.player_bar().volume() + 0.05))),
            simple("volume-down", Box::new(|w| w.player_bar().set_volume(w.player_bar().volume() - 0.05))),
            simple(
                "repeat",
                Box::new(|w| {
                    let next = w.state.borrow().repeat.next();
                    w.ctx.services.player.send(Command::SetRepeat(next));
                }),
            ),
            simple("stream-info", Box::new(|w| w.show_stream_info())),
            simple("retry", send(|| Command::Retry)),
            simple(
                "other-versions",
                Box::new(|w| {
                    // «Другие версии»: тот же трек у других загрузчиков — поиск по названию и исполнителю.
                    if let Some(track) = w.state.borrow().track.clone() {
                        let query = format!("{} {}", track.title, track.artists_text.unwrap_or_default());
                        w.search_for(query.trim());
                    }
                }),
            ),
            simple(
                "current-radio",
                Box::new(|w| {
                    if let Some(track) = w.state.borrow().track.clone() {
                        w.ctx.services.player.send(Command::PlaySingle { track, start: Duration::ZERO });
                    }
                }),
            ),
            simple(
                "current-copy-link",
                Box::new(|w| {
                    if let Some(track) = w.state.borrow().track.clone() {
                        w.copy_link(&track);
                    }
                }),
            ),
            with_track(
                "track-play-next",
                Box::new(|w, track| {
                    w.toast(&trf("PlayingNextFormat", &[&track.title]));
                    w.ctx.services.player.send(Command::PlayNext(vec![track]));
                }),
            ),
            with_track(
                "track-add-to-queue",
                Box::new(|w, track| {
                    w.toast(&trf("QueuedCountFormat", &[&track.title]));
                    w.ctx.services.player.send(Command::AddToEnd(vec![track]));
                }),
            ),
            with_track(
                "track-radio",
                Box::new(|w, track| w.ctx.services.player.send(Command::PlaySingle { track, start: Duration::ZERO })),
            ),
            with_track("track-copy-link", Box::new(|w, track| w.copy_link(&track))),
        ];
        self.window.add_action_entries(entries);

        // Состояния: перемешать, без звука, очередь — у переключателей своё отмеченное состояние.
        let shuffle = gio::SimpleAction::new_stateful("shuffle", None, &false.to_variant());
        let weak = self.downgrade();
        shuffle.connect_activate(move |action, _| {
            let on = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            if let Some(window) = weak.upgrade() {
                window.ctx.services.player.send(Command::SetShuffle(on));
            }
        });
        self.window.add_action(&shuffle);
        let mute = gio::SimpleAction::new_stateful("mute", None, &self.ctx.settings.get(&keys::MUTED).to_variant());
        let weak = self.downgrade();
        mute.connect_activate(move |action, _| {
            let muted = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            action.set_state(&muted.to_variant());
            if let Some(window) = weak.upgrade() {
                window.ctx.settings.set(&keys::MUTED, muted);
                window.ctx.services.player.send(Command::Settings(playback_settings(&window.ctx.settings)));
                window.player_bar().update_volume_icon(&window.ctx.settings);
            }
        });
        self.window.add_action(&mute);
        let queue = gio::SimpleAction::new_stateful("queue", None, &false.to_variant());
        let weak = self.downgrade();
        queue.connect_activate(move |action, _| {
            let show = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            if let Some(window) = weak.upgrade() {
                window.queue_split.set_show_sidebar(show);
            }
        });
        self.window.add_action(&queue);
        let weak = self.downgrade();
        self.queue_split.connect_show_sidebar_notify(move |split| {
            if let Some(action) = weak.upgrade().and_then(|w| w.window.lookup_action("queue")).and_downcast::<gio::SimpleAction>() {
                action.set_state(&split.shows_sidebar().to_variant());
            }
        });

        let app = self.window.application().expect("окно приложения");
        for (action, accels) in [
            ("win.shuffle", &["<Control>h"][..]),
            ("win.repeat", &["<Control>t"]),
            ("win.queue", &["<Control>u"]),
            ("win.mute", &["<Control>m"]),
        ] {
            app.set_accels_for_action(action, accels);
        }
    }

    fn copy_link(&self, track: &Track) {
        let link = if track.is_video() {
            format!("https://www.youtube.com/watch?v={}", track.video_id)
        } else {
            format!("https://music.youtube.com/watch?v={}", track.video_id)
        };
        self.window.clipboard().set_text(&link);
        self.toast(tr("LinkCopied"));
    }

    fn show_stream_info(&self) {
        let state = self.state.borrow().clone();
        let Some(stream) = state.stream else { return };
        let dialog = adw::AlertDialog::new(Some(tr("StreamInfoTitle")), None);
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        let size = stream.content_length.map(|b| format!("{:.1} MB", b as f64 / 1_048_576.0)).unwrap_or_else(|| "—".into());
        let bitrate = stream.bitrate.map(|b| format!("{} kbps", b / 1000)).unwrap_or_else(|| "—".into());
        let loudness = stream.loudness_db.map(|l| format!("{l:+.1} dB")).unwrap_or_else(|| "—".into());
        for (title, value) in [
            (tr("StreamSource"), stream.source.clone()),
            ("itag", stream.itag.to_string()),
            ("Codec", stream.codec().to_owned()),
            (tr("StreamBitrate"), bitrate),
            ("Loudness", loudness),
            ("Size", size),
        ] {
            let row = adw::ActionRow::builder().title(title).subtitle(value).build();
            row.add_css_class("property");
            list.append(&row);
        }
        dialog.set_extra_child(Some(&list));
        dialog.add_response("close", tr("Close"));
        dialog.present(Some(&self.window));
    }

    pub fn focus_search(&self) {
        self.close_now_playing();
        self.search.grab_focus();
        self.search.select_region(0, -1);
    }

    /// Клавиши без модификаторов и стрелки с Ctrl/Shift — в фазе перехвата и только когда фокус не в
    /// поле ввода: стрелки с Ctrl и Shift в поле остаются полю (у Windows Ctrl+← в поиске сначала
    /// переключало трек, §5.3).
    fn install_keys(&self) {
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        controller.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(window) = weak.upgrade() else { return glib::Propagation::Proceed };
            let typing = gtk::prelude::GtkWindowExt::focus(&window.window).is_some_and(|w| w.is::<gtk::Text>() || w.is::<gtk::TextView>());
            let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
            let others = modifiers.difference(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK);
            if !others.is_empty() {
                return glib::Propagation::Proceed;
            }
            let action = match (key, control, shift, typing) {
                (gdk::Key::Escape, false, false, _) if window.suggestions.is_visible() => {
                    window.suggestions.popdown();
                    return glib::Propagation::Stop;
                }
                (gdk::Key::Escape, false, false, false) => {
                    return if window.go_back() { glib::Propagation::Stop } else { glib::Propagation::Proceed };
                }
                (_, _, _, true) => return glib::Propagation::Proceed,
                (gdk::Key::slash, false, _, _) => "win.search",
                (gdk::Key::space, false, false, _) => "win.play-pause",
                (gdk::Key::m | gdk::Key::M, false, _, _) => "win.mute",
                (gdk::Key::Right, true, false, _) => "win.next",
                (gdk::Key::Left, true, false, _) => "win.previous",
                (gdk::Key::Right, false, true, _) => "win.seek-forward",
                (gdk::Key::Left, false, true, _) => "win.seek-backward",
                (gdk::Key::Up, true, false, _) => "win.volume-up",
                (gdk::Key::Down, true, false, _) => "win.volume-down",
                _ => return glib::Propagation::Proceed,
            };
            let _ = WidgetExt::activate_action(&window.window, action, None);
            glib::Propagation::Stop
        });
        self.window.add_controller(controller);
    }

    // ── поиск и подсказки ──

    fn install_search(&self) {
        let weak = self.downgrade();
        self.search.connect_activate(move |entry| {
            let Some(window) = weak.upgrade() else { return };
            // Выбранная стрелками подсказка важнее набранного.
            let chosen = window
                .suggestions
                .is_visible()
                .then(|| window.suggestion_list.selected_row().and_then(|row| row.child()).and_downcast::<gtk::Label>().map(|l| l.label()))
                .flatten();
            let text = chosen.map(|t| t.to_string()).unwrap_or_else(|| entry.text().to_string());
            window.open_text(&text);
        });
        let weak = self.downgrade();
        self.search.connect_search_changed(move |entry| {
            if let Some(window) = weak.upgrade() {
                window.update_suggestions(entry.text().to_string());
            }
        });
        let weak = self.downgrade();
        self.suggestion_list.connect_row_activated(move |_, row| {
            if let (Some(window), Some(label)) = (weak.upgrade(), row.child().and_downcast::<gtk::Label>()) {
                window.search_for(&label.label());
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = self.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(window) = weak.upgrade() else { return glib::Propagation::Proceed };
            if !window.suggestions.is_visible() {
                return glib::Propagation::Proceed;
            }
            let list = &window.suggestion_list;
            let index = list.selected_row().map(|r| r.index()).unwrap_or(-1);
            let target = match key {
                gdk::Key::Down => index + 1,
                gdk::Key::Up => index - 1,
                _ => return glib::Propagation::Proceed,
            };
            match list.row_at_index(target) {
                Some(row) => list.select_row(Some(&row)),
                None => list.unselect_all(),
            }
            glib::Propagation::Stop
        });
        self.search.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let weak = self.downgrade();
        focus.connect_leave(move |_| {
            if let Some(window) = weak.upgrade() {
                // Щелчок по подсказке уводит фокус — даём ему дойти до строки.
                let weak = window.downgrade();
                glib::timeout_add_local_once(Duration::from_millis(200), move || {
                    if let Some(window) = weak.upgrade() {
                        window.suggestions.popdown();
                    }
                });
            }
        });
        self.search.add_controller(focus);
    }

    /// Подсказки YouTube Music при вводе — через 150 мс после последней буквы.
    fn update_suggestions(&self, text: String) {
        let token = self.suggestion_token.get() + 1;
        self.suggestion_token.set(token);
        if text.trim().is_empty() || links::is_melogold_link(&text) {
            self.suggestions.popdown();
            return;
        }
        let weak = self.downgrade();
        glib::timeout_add_local_once(Duration::from_millis(150), move || {
            let Some(window) = weak.upgrade() else { return };
            if window.suggestion_token.get() != token {
                return;
            }
            let music = window.ctx.services.music.clone();
            let query = text.clone();
            let task = window.ctx.services.run(async move { music.suggestions(&query).await });
            glib::spawn_future_local(async move {
                let Some(Ok(suggestions)) = task.await else { return };
                if window.suggestion_token.get() != token || !window.search.has_focus() {
                    return;
                }
                while let Some(child) = window.suggestion_list.first_child() {
                    window.suggestion_list.remove(&child);
                }
                for suggestion in suggestions.iter().take(8) {
                    let label = gtk::Label::builder().label(suggestion).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
                    window.suggestion_list.append(&label);
                }
                if suggestions.is_empty() {
                    window.suggestions.popdown();
                } else {
                    window.suggestions.set_width_request(window.search.width());
                    window.suggestions.popup();
                }
            });
        });
    }

    /// Перетаскивание ссылки YouTube или `melogold://` в окно (docs/PROMPT.md §3 «Ссылки»).
    fn install_drop(&self) {
        let drop = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::COPY);
        let weak = self.downgrade();
        drop.connect_drop(move |_, value, _, _| {
            let (Some(window), Ok(text)) = (weak.upgrade(), value.get::<String>()) else { return false };
            window.open_text(text.lines().next().unwrap_or_default());
            true
        });
        self.window.add_controller(drop);
    }

    fn connect_close(&self) {
        let settings = Rc::clone(&self.ctx.settings);
        let player = self.ctx.services.player.clone();
        self.window.connect_close_request(move |window| {
            let (width, height) = window.default_size();
            settings.set(&keys::WINDOW_WIDTH, width);
            settings.set(&keys::WINDOW_HEIGHT, height);
            settings.set(&keys::WINDOW_MAXIMIZED, window.is_maximized());
            settings.flush();
            player.send(Command::SaveQueue);
            glib::Propagation::Proceed
        });
    }

    // ── плеер ──

    fn listen_player(&self) {
        let events = self.ctx.services.player.subscribe();
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                let Some(window) = weak.upgrade() else { break };
                window.on_player_event(event);
            }
        });
        let weak = self.downgrade();
        glib::timeout_add_local(Duration::from_millis(250), move || {
            let Some(window) = weak.upgrade() else { return glib::ControlFlow::Break };
            let position = window.ctx.services.player.position();
            window.player_bar().tick(position);
            if let Some(now_playing) = window.now_playing.get() {
                now_playing.tick(position);
            }
            glib::ControlFlow::Continue
        });
        self.on_player_event(Event::State(Box::new(self.ctx.services.player.state())));
    }

    fn on_player_event(&self, event: Event) {
        match event {
            Event::State(state) => {
                self.player_bar().apply(self, &state);
                if let Some(now_playing) = self.now_playing.get() {
                    now_playing.apply(self, &state);
                }
                if let Some(action) = self.window.lookup_action("shuffle").and_downcast::<gio::SimpleAction>() {
                    action.set_state(&state.shuffle.to_variant());
                }
                if state.track.is_none() {
                    self.close_now_playing();
                }
                self.state.replace(*state);
            }
            Event::Queue(view) => self.apply_queue(&view),
            Event::Skipped(error) => self.toast(&crate::texts::skipped(&error)),
            Event::QueueReplaced(snapshot) => {
                let toast = adw::Toast::builder().title(tr("QueueReplaced")).button_label(tr("Undo")).timeout(5).build();
                let player = self.ctx.services.player.clone();
                let snapshot = RefCell::new(Some(snapshot));
                toast.connect_button_clicked(move |_| {
                    if let Some(snapshot) = snapshot.take() {
                        player.send(Command::RestoreQueue(snapshot));
                    }
                });
                self.toasts.add_toast(toast);
            }
            Event::Seeked(_) | Event::Listened { .. } => {}
        }
    }

    fn apply_queue(&self, view: &QueueView) {
        if let Some(panel) = self.queue_panel.get() {
            panel.apply(self, view);
        }
    }

    /// Просьбы MPRIS, которые касаются окна и настроек.
    pub fn on_mpris(&self, request: crate::mpris::Request) {
        use crate::mpris::Request;
        match request {
            Request::Raise => self.present(),
            Request::Quit => {
                if let Some(app) = self.window.application() {
                    app.quit();
                }
            }
            Request::Volume(volume) => self.player_bar().set_volume(volume),
            Request::Rate(rate) => {
                self.ctx.settings.set(&keys::SPEED, rate);
                self.ctx.services.player.send(Command::Settings(playback_settings(&self.ctx.settings)));
            }
            Request::Shuffle(on) => self.ctx.services.player.send(Command::SetShuffle(on)),
            Request::Repeat(mode) => self.ctx.services.player.send(Command::SetRepeat(mode)),
        }
    }

    pub fn split_view(&self) -> &adw::OverlaySplitView {
        &self.split
    }
}

fn sidebar_list() -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    list
}

fn sidebar_row(icon: &str, label: &str) -> gtk::ListBoxRow {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.append(&gtk::Image::from_icon_name(icon));
    content.append(&gtk::Label::builder().label(label).xalign(0.0).build());
    gtk::ListBoxRow::builder().child(&content).build()
}

fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some(tr("NavSettings")), Some("win.preferences"));
    section.append(Some(tr("MenuShortcuts")), Some("app.shortcuts"));
    section.append(Some(tr("LinuxAbout")), Some("app.about"));
    menu.append_section(None, &section);
    menu
}
