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
use melogold_core::youtube_links::{self, LinkTarget};
use melogold_playback::engine::{Command, Event, QueueView, State};

use crate::account_view::AccountView;
use crate::app::AppContext;
use crate::library_view::LibraryView;
use crate::localization::{tr, trf};
use crate::lyrics_service::LyricsService;
use crate::now_playing::NowPlaying;
use crate::pages;
use crate::player_bar::{Hidden, PlayerBar};
use crate::queue_panel::QueuePanel;
use crate::selection::{Selection, SelectionBar};
use crate::services::playback_settings;
use crate::updates::UpdateService;

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
    /// Строки подсказок по порядку списка.
    suggestion_items: RefCell<Vec<Suggestion>>,
    current: Cell<Tab>,
    player_bar: OnceCell<PlayerBar>,
    pub(crate) now_playing: OnceCell<NowPlaying>,
    queue_panel: OnceCell<QueuePanel>,
    state: RefCell<State>,
    /// Последняя очередь: после правки названия трека (задание 0005) она перерисовывается.
    last_queue: RefCell<Option<QueueView>>,
    /// Узкое окно (порог 720): шапки коллекций ставят обложку сверху (§5.2).
    compact: Cell<bool>,
    headers: RefCell<Vec<glib::WeakRef<gtk::Box>>>,
    pub library_view: LibraryView,
    /// Выделение, чья панель сейчас на экране (задание 0004).
    pub selection: RefCell<Option<Rc<Selection>>>,
    pub selection_bar: SelectionBar,
    pub account_view: AccountView,
    /// Текст играющего трека (срез 6).
    pub lyrics: LyricsService,
    /// Обновления из GitHub Releases (срез 8).
    pub updates: UpdateService,
    /// Точка у «Настроек»: вышла новая версия.
    settings_badge: gtk::Box,
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
        let brand = gtk::Label::builder().label("Melogold").build();
        brand.add_css_class("sidebar-brand");
        sidebar_header.set_title_widget(Some(&brand));
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
        search.add_css_class("main-search");
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
        // Панель выделения — поверх списка над плеером: список под ней не сжимается.
        let selection_bar = SelectionBar::new();
        let stack_overlay = gtk::Overlay::builder().child(&stack).build();
        stack_overlay.add_overlay(&selection_bar.root);
        let content = adw::ToolbarView::new();
        content.add_top_bar(&header);
        content.set_content(Some(&stack_overlay));

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

        let lyrics = LyricsService::new(Rc::clone(&ctx));
        let updates = UpdateService::new(Rc::clone(&ctx));
        let settings_badge = gtk::Box::builder().width_request(8).height_request(8).valign(gtk::Align::Center).visible(false).build();
        settings_badge.add_css_class("update-badge");
        if let Some(settings_row) = sections.iter().find(|s| s.tab == Tab::Settings) {
            if let Some(content) = settings_row.row.child().and_downcast::<gtk::Box>() {
                if let Some(label) = content.last_child() {
                    label.set_hexpand(true);
                }
                content.append(&settings_badge);
            }
        }
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
            suggestion_items: RefCell::default(),
            current: Cell::new(Tab::Trends),
            player_bar: OnceCell::new(),
            now_playing: OnceCell::new(),
            queue_panel: OnceCell::new(),
            state: RefCell::default(),
            last_queue: RefCell::default(),
            compact: Cell::new(false),
            headers: RefCell::default(),
            library_view: LibraryView::default(),
            selection: RefCell::default(),
            selection_bar,
            account_view: AccountView::default(),
            lyrics,
            updates,
            settings_badge,
        }));
        this.build_player();
        this.install_breakpoints();
        this.populate_sections();
        this.connect_sidebar(&top_list);
        this.connect_sidebar(&bottom_list);
        this.install_actions();
        crate::remote_sheet::install(&this);
        this.install_track_actions();
        this.install_selection_actions();
        this.start_library();
        this.start_account();
        this.install_keys();
        this.install_search();
        this.install_drop();
        this.connect_close();
        this.listen_player();
        this.show_tab(this.ctx.settings.get(&keys::LAST_TAB));
        this.start_updates();

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

    pub fn add_toast(&self, toast: adw::Toast) {
        self.toasts.add_toast(toast);
    }

    /// Играющий (или выбранный) трек — каким его дал YouTube (для действий).
    pub fn state_track(&self) -> Option<Track> {
        self.state.borrow().track.clone()
    }

    /// Трек, каким его показывать: со своими названием, исполнителем и альбомом (задание 0005).
    /// В действия (♡, плейлисты, загрузки) идёт исходный трек: в базе остаётся то, что дал YouTube.
    pub fn display(&self, track: &Track) -> Track {
        self.ctx.services.library.display(track)
    }

    /// Правки названий изменились: строки, панель плеера, «Сейчас играет» и очередь — заново.
    pub fn refresh_display(&self) {
        for row in self.library_view.live_rows() {
            row.rebind();
        }
        let state = self.state.borrow().clone();
        self.show_state(&state);
        if let Some(view) = self.last_queue.borrow().clone() {
            self.apply_queue(&view);
        }
    }

    fn show_state(&self, state: &State) {
        let mut shown = state.clone();
        shown.track = shown.track.map(|t| self.display(&t));
        self.lyrics.set_track(state.track.as_ref(), shown.track.as_ref());
        self.player_bar().apply(self, &shown);
        if let Some(now_playing) = self.now_playing.get() {
            now_playing.apply(self, &shown);
        }
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
        let nav = self.nav(self.current.get());
        // Та же страница уже открыта (свой плейлист по тегу) — вернуться к ней, а не открыть вторую.
        if let Some(tag) = page.tag() {
            let stack = nav.navigation_stack();
            let open = (0..stack.n_items())
                .filter_map(|i| stack.item(i).and_downcast::<adw::NavigationPage>())
                .find(|p| p.tag() == Some(tag.clone()));
            if let Some(open) = open {
                nav.pop_to_page(&open);
                return;
            }
        }
        nav.push(page);
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

    pub fn now_playing_open(&self) -> bool {
        self.root_nav.visible_page().is_some_and(|p| p.tag().as_deref() == Some("now-playing"))
    }

    /// Проверка обновлений: нашлась версия — уведомление GNOME и точка у «Настроек».
    fn start_updates(&self) {
        let weak = self.downgrade();
        let refresh: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(window) = weak.upgrade() {
                window.settings_badge.set_visible(window.updates.available().is_some());
            }
        });
        self.updates.listen(&refresh);
        // Слушатель живёт столько же, сколько окно.
        let keep = RefCell::new(Some(refresh));
        self.window.connect_destroy(move |_| {
            keep.take();
        });
        if crate::app::snapshot_mode() {
            return;
        }
        let weak = self.downgrade();
        self.updates.start(move |manifest| {
            if let Some(window) = weak.upgrade() {
                window.updates.announce(manifest);
            }
        });
    }

    /// Трек, который сейчас в плеере (для столбиков «играет» в строках).
    pub fn current_video_id(&self) -> Option<String> {
        self.state.borrow().track.as_ref().map(|t| t.video_id.clone())
    }

    pub fn is_playing(&self) -> bool {
        self.state.borrow().playing
    }

    /// Состояние плеера (таймер сна для меню «…»).
    pub fn player_state(&self) -> State {
        self.state.borrow().clone()
    }

    /// Текст на экране: открыта «Сейчас играет», и текст виден.
    pub fn lyrics_visible(&self) -> bool {
        self.now_playing.get().is_some_and(|n| n.lyrics_visible(self))
    }

    /// «Текст» (Ctrl+L): показать текст; уже на экране — закрыть «Сейчас играет».
    pub fn toggle_lyrics(&self) {
        if self.lyrics_visible() {
            self.close_now_playing();
            return;
        }
        self.show_now_playing();
        if let Some(now_playing) = self.now_playing.get() {
            now_playing.show_lyrics();
        }
    }

    /// Кнопка «Текст» панели плеера нажата, пока текст на экране.
    pub fn update_lyrics_button(&self) {
        if let Some(bar) = self.player_bar.get() {
            bar.set_lyrics_active(self.lyrics_visible());
        }
    }

    fn close_now_playing(&self) -> bool {
        let open = self.root_nav.visible_page().is_some_and(|p| p.tag().as_deref() == Some("now-playing"));
        if open {
            self.root_nav.pop_to_page(&self.main_page);
            // «Во весь экран» — только у «Сейчас играет».
            if self.window.is_fullscreen() {
                self.window.unfullscreen();
            }
        }
        open
    }

    /// F11: «Сейчас играет» во весь экран и обратно.
    pub fn toggle_fullscreen(&self) {
        if self.window.is_fullscreen() {
            self.window.unfullscreen();
            return;
        }
        self.show_now_playing();
        if self.now_playing_open() {
            self.window.fullscreen();
        }
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
                // Экран «Сервер» с заполненным адресом и подтверждением: подключается только кнопкой (API §7.2).
                MelogoldLink::Server { url, server_id } => {
                    self.show_tab(Tab::Settings);
                    self.nav(Tab::Settings).push(&pages::account::server_page_for(self, Some(&url), server_id));
                }
                // Вход по приглашению другого устройства (QR, API §4.6) — в 0.2.
                MelogoldLink::Invite { .. } => self.toast(tr("LinuxInviteLater")),
                MelogoldLink::Request { .. } => self.toast(tr("LinuxLinkRequest")),
                MelogoldLink::Share { url, id } => self.open_share_link(&url, &id),
                MelogoldLink::Unsupported => self.toast(tr("LinkUnsupported")),
            }
            return;
        }
        // Снимок плейлиста по адресу `/s/<код>` и ссылки Spotify, Apple, Яндекса, Deezer, Tidal, SoundCloud (задание 0010).
        if self.open_incoming(text) {
            return;
        }
        self.open_youtube(youtube_links::parse(text));
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

    /// Строка выдачи или карточка: трек играет одиночным треком с радио (REWRITE §2.3), остальное открывается.
    pub fn activate_item(&self, item: &MusicItem) {
        use pages::catalog;
        match item {
            MusicItem::Track(track) => self.ctx.services.player.send(Command::PlaySingle { track: track.clone(), start: Duration::ZERO }),
            MusicItem::Album(album) => self.push(&catalog::album_page(self, &album.browse_id)),
            MusicItem::Artist(artist) => self.push(&catalog::artist_page(self, &artist.browse_id)),
            MusicItem::Playlist(playlist) => self.push(&catalog::playlist_page(self, &playlist.playlist_id)),
            MusicItem::Mood(mood) => self.push(&catalog::browse_page(self, &mood.title, &mood.browse_id, mood.params.as_deref())),
        }
    }

    /// Ссылка YouTube (REWRITE §2.3 «Ссылки и интенты»): видео играет (с `list=` — очередь плейлиста с
    /// этого видео, `t=` — позиция), плейлист, альбом и канал открываются, `@handle` — через `resolve_url`.
    pub(crate) fn open_youtube(&self, target: LinkTarget) {
        use pages::catalog;
        tracing::info!(вход = ?std::mem::discriminant(&target), "ссылка YouTube");
        match target {
            LinkTarget::Video { video_id, playlist_id, start_ms, .. } => self.play_video_link(video_id, playlist_id, start_ms),
            // Миксы RD… — бесконечные очереди, а не плейлисты (кроме подборок RDCLAK).
            LinkTarget::Playlist(id) if id.starts_with("RD") && !id.starts_with("RDCLAK") => self.toast(tr("LinkUnsupported")),
            LinkTarget::Playlist(id) => self.push(&catalog::playlist_page(self, &id)),
            LinkTarget::Album(id) => self.push(&catalog::album_page(self, &id)),
            LinkTarget::Channel(id) => self.push(&catalog::artist_page(self, &id)),
            LinkTarget::Handle(name) => self.resolve_channel(format!("https://www.youtube.com/@{name}")),
            LinkTarget::LegacyChannel(url) => self.resolve_channel(url),
            LinkTarget::Search(query) => self.submit_search(&query),
            LinkTarget::External { .. } => self.toast(tr("LinkImportLater")),
            LinkTarget::Unsupported(youtube_links::EMPTY) => {}
            LinkTarget::Unsupported(_) => self.toast(tr("LinkUnsupported")),
        }
    }

    fn play_video_link(&self, video_id: String, playlist_id: Option<String>, start_ms: Option<i64>) {
        let music = self.ctx.services.music.clone();
        let player = self.ctx.services.player.clone();
        let start = Duration::from_millis(start_ms.unwrap_or(0).max(0) as u64);
        let task = self.ctx.services.run(async move {
            if let Some(list) = playlist_id.filter(|l| !l.starts_with("RD")) {
                if let Ok(tracks) = music.playlist_tracks(&list, 1000).await {
                    if let Some(index) = tracks.iter().position(|t| t.video_id == video_id) {
                        return Ok::<_, ()>((tracks, index));
                    }
                }
            }
            // Сведения о треке — из «Далее»: название, исполнитель, обложка.
            let track = match music.next(&video_id, None).await {
                Ok(page) => page.tracks.into_iter().find(|t| t.video_id == video_id),
                Err(_) => None,
            }
            .unwrap_or_else(|| Track { video_id: video_id.clone(), title: video_id.clone(), ..Default::default() });
            Ok((vec![track], usize::MAX))
        });
        glib::spawn_future_local(async move {
            match task.await {
                Some(Ok((tracks, index))) if index != usize::MAX => player.send(Command::PlayList { tracks, start: index, shuffle: false }),
                Some(Ok((mut tracks, _))) if !tracks.is_empty() => player.send(Command::PlaySingle { track: tracks.remove(0), start }),
                _ => {}
            }
        });
    }

    fn resolve_channel(&self, url: String) {
        let music = self.ctx.services.music.clone();
        let task = self.ctx.services.run(async move { music.resolve_url(&url).await });
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Some(window) = weak.upgrade() else { return };
            match task.await {
                Some(Ok(Some(id))) => window.push(&pages::catalog::artist_page(&window, &id)),
                Some(Ok(None)) => window.toast(tr("LinkUnsupported")),
                _ => window.toast(tr("ErrorOffline")),
            }
        });
    }

    // ── сборка ──

    fn build_player(&self) {
        let bar = PlayerBar::new(self);
        // Над панелью — пульт другого устройства (задание 0011); пока он включён, своя панель скрыта.
        self.outer.add_bottom_bar(&crate::remote_bar::bottom(self, &bar.root));
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
                    window.selection_bar.set_phone(hidden.queue);
                }
            });
            let weak = self.downgrade();
            b.connect_unapply(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.player_bar().set_hidden(Hidden::default());
                    window.selection_bar.set_phone(false);
                }
            });
        };
        // Шапки коллекций: уже 720 — обложка сверху. Порог действует один, поэтому сигналы всех трёх.
        let headers_follow = |b: &adw::Breakpoint, compact: bool| {
            let weak = self.downgrade();
            b.connect_apply(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.set_compact(compact);
                }
            });
            let weak = self.downgrade();
            b.connect_unapply(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.set_compact(false);
                }
            });
        };
        // Панель выделенного: уже 1100 (с боковой панелью это ~840 для содержимого) — только значки.
        let selection_follows = |b: &adw::Breakpoint| {
            let weak = self.downgrade();
            b.connect_apply(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.selection_bar.set_compact(true);
                }
            });
            let weak = self.downgrade();
            b.connect_unapply(move |_| {
                if let Some(window) = weak.upgrade() {
                    window.selection_bar.set_compact(false);
                }
            });
        };
        let medium = breakpoint("max-width: 1100sp");
        volume_as_button(&medium);
        headers_follow(&medium, false);
        selection_follows(&medium);
        self.window.add_breakpoint(medium);
        let narrow = breakpoint("max-width: 720sp");
        volume_as_button(&narrow);
        two_rows(&narrow);
        menu_follows(&narrow, Hidden { modes: true, queue: false, heart: false });
        headers_follow(&narrow, true);
        selection_follows(&narrow);
        self.window.add_breakpoint(narrow);
        let phone = breakpoint("max-width: 480sp");
        volume_as_button(&phone);
        two_rows(&phone);
        phone.add_setter(&bar.queue, "visible", Some(&false.to_value()));
        phone.add_setter(&bar.lyrics, "visible", Some(&false.to_value()));
        phone.add_setter(&bar.heart, "visible", Some(&false.to_value()));
        menu_follows(&phone, Hidden { modes: true, queue: true, heart: true });
        headers_follow(&phone, true);
        selection_follows(&phone);
        self.window.add_breakpoint(phone);
    }

    fn set_compact(&self, compact: bool) {
        self.compact.set(compact);
        // Узкое окно — скорее всего сенсорный экран: действия строк видны и без наведения.
        if compact {
            self.window.add_css_class("compact-window");
        } else {
            self.window.remove_css_class("compact-window");
        }
        let orientation = if compact { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal };
        self.headers.borrow_mut().retain(|header| match header.upgrade() {
            Some(header) => {
                header.set_orientation(orientation);
                true
            }
            None => false,
        });
    }

    /// Шапка коллекции следует за порогом окна.
    pub fn register_header(&self, header: &gtk::Box) {
        header.set_orientation(if self.compact.get() { gtk::Orientation::Vertical } else { gtk::Orientation::Horizontal });
        self.headers.borrow_mut().push(header.downgrade());
    }

    fn populate_sections(&self) {
        for section in &self.sections {
            let page = match section.tab {
                Tab::Trends => pages::catalog::trends(self),
                Tab::WhatsNew => pages::catalog::new_page(self),
                Tab::Library => pages::library::root(self),
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
            simple("fullscreen", Box::new(|w| w.toggle_fullscreen())),
            simple("update-install", Box::new(|w| w.updates.install(Some(w.window.upcast_ref())))),
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
                    if let Some(track) = w.state_track() {
                        w.other_versions(&track);
                    }
                }),
            ),
            simple("lyrics", Box::new(|w| w.toggle_lyrics())),
            simple("lyrics-find", Box::new(|w| w.find_lyrics())),
            simple("lyrics-edit", Box::new(|w| w.edit_lyrics())),
            simple("lyrics-retry", Box::new(|w| w.lyrics.retry())),
            simple("lyrics-toggle", Box::new(|w| w.lyrics.toggle_synced())),
            simple("lyrics-reset-shift", Box::new(|w| w.lyrics.reset_shift())),
            simple(
                "current-like",
                Box::new(|w| {
                    if let Some(track) = w.state_track() {
                        let liked = !w.library_view.is_liked(&track.video_id);
                        w.set_liked(vec![track], liked);
                    }
                }),
            ),
        ];
        self.window.add_action_entries(entries);
        let weak = self.downgrade();
        let shift = gio::ActionEntry::builder("lyrics-shift")
            .parameter_type(Some(glib::VariantTy::INT32))
            .activate(move |_: &adw::ApplicationWindow, _, parameter| {
                if let (Some(window), Some(delta)) = (weak.upgrade(), parameter.and_then(|p| p.get::<i32>())) {
                    window.lyrics.shift(i64::from(delta));
                }
            })
            .build();
        self.window.add_action_entries([shift]);
        // Таймер сна: минуты, 0 — «До конца трека».
        let weak = self.downgrade();
        let sleep = gio::ActionEntry::builder("sleep")
            .parameter_type(Some(glib::VariantTy::INT32))
            .activate(move |_: &adw::ApplicationWindow, _, parameter| {
                let (Some(window), Some(minutes)) = (weak.upgrade(), parameter.and_then(|p| p.get::<i32>())) else { return };
                let command =
                    if minutes <= 0 { Command::SleepAtTrackEnd } else { Command::SetSleepTimer(Duration::from_secs(minutes as u64 * 60)) };
                window.ctx.services.player.send(command);
            })
            .build();
        let weak = self.downgrade();
        let sleep_off = gio::ActionEntry::builder("sleep-off")
            .activate(move |_: &adw::ApplicationWindow, _, _| {
                if let Some(window) = weak.upgrade() {
                    window.ctx.services.player.send(Command::CancelSleepTimer);
                }
            })
            .build();
        self.window.add_action_entries([sleep, sleep_off]);

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
            ("win.lyrics", &["<Control>l"]),
            ("win.current-like", &["<Control>d"]),
        ] {
            app.set_accels_for_action(action, accels);
        }
    }

    pub fn copy_link(&self, track: &Track) {
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
            // Открыт диалог (редактор текста, поиск текста, сведения): клавиши — ему, Esc закрывает его.
            if window.window.visible_dialog().is_some() {
                return glib::Propagation::Proceed;
            }
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
                    // Сначала снимается выделение, потом — «Назад».
                    return if window.clear_selection() || window.go_back() { glib::Propagation::Stop } else { glib::Propagation::Proceed };
                }
                (_, _, _, true) => return glib::Propagation::Proceed,
                (gdk::Key::F11, false, false, _) => "win.fullscreen",
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
            let chosen = window.suggestions.is_visible().then(|| window.suggestion_list.selected_row()).flatten();
            match chosen {
                Some(row) => window.choose_suggestion(row.index()),
                None => {
                    window.suggestions.popdown();
                    window.open_text(&entry.text());
                }
            }
        });
        let weak = self.downgrade();
        self.search.connect_search_changed(move |entry| {
            if let Some(window) = weak.upgrade() {
                window.update_suggestions(entry.text().to_string());
            }
        });
        let weak = self.downgrade();
        self.suggestion_list.connect_row_activated(move |_, row| {
            if let Some(window) = weak.upgrade() {
                window.choose_suggestion(row.index());
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
        focus.connect_enter(move |_| {
            // До ввода — недавние запросы (§5.4 «Поиск»).
            if let Some(window) = weak.upgrade() {
                if window.search.text().trim().is_empty() {
                    window.show_recent_searches();
                }
            }
        });
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

    fn choose_suggestion(&self, index: i32) {
        let item = self.suggestion_items.borrow().get(index.max(0) as usize).cloned();
        self.suggestions.popdown();
        match item {
            Some(Suggestion::Query(query)) | Some(Suggestion::Recent(query)) => {
                self.search.set_text(&query);
                self.submit_search(&query);
            }
            Some(Suggestion::Link(_)) => self.open_text(&self.search.text()),
            Some(Suggestion::Track(track)) => self.ctx.services.player.send(Command::PlaySingle { track: *track, start: Duration::ZERO }),
            None => {}
        }
    }

    /// Запрос из поля: в историю поиска (если она не на паузе) и выдача.
    fn submit_search(&self, query: &str) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        if !self.ctx.settings.get(&keys::SEARCH_HISTORY_PAUSED) {
            let text = query.clone();
            let task = self.ctx.services.db(move |library| library.add_search(&text));
            glib::spawn_future_local(async move {
                if let Some(Err(error)) = task.await {
                    tracing::warn!(%error, "запрос не записался в историю поиска");
                }
            });
        }
        self.search_for(&query);
    }

    fn show_suggestions(&self, items: Vec<Suggestion>) {
        while let Some(child) = self.suggestion_list.first_child() {
            self.suggestion_list.remove(&child);
        }
        for item in &items {
            self.suggestion_list.append(&suggestion_row(item));
        }
        let empty = items.is_empty();
        self.suggestion_items.replace(items);
        if empty {
            self.suggestions.popdown();
        } else {
            self.suggestions.set_width_request(self.search.width());
            self.suggestions.popup();
        }
    }

    /// Недавние запросы — пока поле пустое; история поиска на паузе — их нет.
    fn show_recent_searches(&self) {
        let token = self.suggestion_token.get() + 1;
        self.suggestion_token.set(token);
        if self.ctx.settings.get(&keys::SEARCH_HISTORY_PAUSED) {
            return;
        }
        let task = self.ctx.services.db(|library| library.recent_searches(8).unwrap_or_default());
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let (Some(window), Some(recent)) = (weak.upgrade(), task.await) else { return };
            if window.suggestion_token.get() == token && window.search.has_focus() && window.search.text().trim().is_empty() {
                window.show_suggestions(recent.into_iter().map(Suggestion::Recent).collect());
            }
        });
    }

    /// При вводе (§5.4): ссылка — «Открыть ссылку»; иначе через 150 мс после последней буквы
    /// «В библиотеке» (до трёх своих треков) и подсказки YouTube Music.
    fn update_suggestions(&self, text: String) {
        if text.trim().is_empty() {
            if self.search.has_focus() {
                self.show_recent_searches();
            } else {
                self.suggestions.popdown();
            }
            return;
        }
        let token = self.suggestion_token.get() + 1;
        self.suggestion_token.set(token);
        if links::is_melogold_link(&text) {
            self.suggestions.popdown();
            return;
        }
        let weak = self.downgrade();
        glib::timeout_add_local_once(Duration::from_millis(150), move || {
            let Some(window) = weak.upgrade() else { return };
            if window.suggestion_token.get() != token {
                return;
            }
            // Ссылка YouTube в поле — первой строкой «Открыть ссылку: видео YouTube» (§5.4 «Поиск»).
            if let Some(kind) = crate::share::suggestion_kind(&text) {
                window.show_suggestions(vec![Suggestion::Link(trf("OpenLinkFormat", &[&tr(kind)]))]);
                return;
            }
            let link_kind = match youtube_links::parse(&text) {
                LinkTarget::Video { .. } => Some("LinkKindVideo"),
                LinkTarget::Playlist(_) => Some("LinkKindPlaylist"),
                LinkTarget::Album(_) => Some("LinkKindAlbum"),
                LinkTarget::Channel(_) | LinkTarget::Handle(_) | LinkTarget::LegacyChannel(_) => Some("LinkKindChannel"),
                _ => None,
            };
            if let Some(kind) = link_kind {
                window.show_suggestions(vec![Suggestion::Link(trf("OpenLinkFormat", &[&tr(kind)]))]);
                return;
            }
            let query = text.trim().to_owned();
            let local_query = query.clone();
            let local = window.ctx.services.db(move |library| library.search_library(&local_query, 3).unwrap_or_default());
            let music = window.ctx.services.music.clone();
            let remote = window.ctx.services.run(async move { music.suggestions(&query).await });
            glib::spawn_future_local(async move {
                let mut items: Vec<Suggestion> =
                    local.await.unwrap_or_default().into_iter().map(|t| Suggestion::Track(Box::new(t))).collect();
                if window.suggestion_token.get() == token && window.search.has_focus() && !items.is_empty() {
                    window.show_suggestions(items.clone());
                }
                // Без сети подсказок нет — остаётся «В библиотеке».
                if let Some(Ok(remote)) = remote.await {
                    items.extend(remote.into_iter().take(8).map(Suggestion::Query));
                }
                if window.suggestion_token.get() == token && window.search.has_focus() {
                    window.show_suggestions(items);
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
        let weak = self.downgrade();
        self.window.connect_close_request(move |window| {
            if let Some(main) = weak.upgrade() {
                main.flush_pending();
            }
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
        let (player, weak) = (self.ctx.services.player.clone(), self.downgrade());
        crate::playing_bars::set_source(move || (player.levels(), weak.upgrade().is_some_and(|w| w.is_playing())));
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
                self.show_state(&state);
                let previous = self.current_video_id();
                let current = state.track.as_ref().map(|t| t.video_id.clone());
                if previous != current {
                    for row in self.library_view.live_rows() {
                        row.refresh_playing(current.as_deref());
                    }
                }
                if let Some(action) = self.window.lookup_action("shuffle").and_downcast::<gio::SimpleAction>() {
                    action.set_state(&state.shuffle.to_variant());
                }
                if state.track.is_none() {
                    self.close_now_playing();
                }
                self.state.replace(*state);
            }
            Event::Queue(view) => {
                self.apply_queue(&view);
                self.last_queue.replace(Some(view));
            }
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
            Event::SleepTimerFired => self.toast(tr("SleepTimerEnded")),
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

/// Строка под полем поиска.
#[derive(Clone)]
enum Suggestion {
    /// Недавний запрос (поле пустое).
    Recent(String),
    /// Подсказка YouTube Music.
    Query(String),
    /// «Открыть ссылку: видео YouTube».
    Link(String),
    /// «В библиотеке»: свой трек — играет сразу.
    Track(Box<Track>),
}

fn suggestion_row(item: &Suggestion) -> gtk::ListBoxRow {
    let (icon, title, subtitle) = match item {
        Suggestion::Recent(query) => ("document-open-recent-symbolic", query.clone(), None),
        Suggestion::Query(query) => ("system-search-symbolic", query.clone(), None),
        Suggestion::Link(text) => ("adw-external-link-symbolic", text.clone(), None),
        Suggestion::Track(track) => {
            let artist = track.artists_text.as_deref().unwrap_or_default();
            let subtitle = if artist.is_empty() { tr("InLibrary").to_owned() } else { format!("{} · {artist}", tr("InLibrary")) };
            ("library-symbolic", track.title.clone(), Some(subtitle))
        }
    };
    let content = gtk::Box::builder().spacing(10).build();
    content.append(&gtk::Image::from_icon_name(icon));
    let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).hexpand(true).build();
    let title_label = gtk::Label::builder().label(&title).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    texts.append(&title_label);
    if let Some(subtitle) = subtitle {
        let label = gtk::Label::builder().label(&subtitle).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        texts.append(&label);
    }
    content.append(&texts);
    let row = gtk::ListBoxRow::builder().child(&content).build();
    row.update_property(&[gtk::accessible::Property::Label(&title)]);
    row
}

fn sidebar_list() -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    list.add_css_class("main-nav");
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
    section.append(Some(tr("LinuxRemoteDevice")), Some("win.remote-devices"));
    section.append(Some(tr("NavSettings")), Some("win.preferences"));
    section.append(Some(tr("MenuShortcuts")), Some("app.shortcuts"));
    section.append(Some(tr("LinuxAbout")), Some("app.about"));
    menu.append_section(None, &section);
    menu
}
