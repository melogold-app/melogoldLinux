//! Главное окно (docs/PROMPT.md §5.2).
//!
//! ```text
//! AdwApplicationWindow
//! └ AdwToolbarView ─ низ: панель воспроизведения во всю ширину (срез 2)
//!   └ AdwToastOverlay — «Отменить» всплывает поверх и не сжимает окно
//!     └ AdwOverlaySplitView
//!       ├ боковая панель: Тренды · Новое · Библиотека … Настройки
//!       └ AdwToolbarView: заголовок («Назад», поиск, главное меню) + стек разделов,
//!         у каждого раздела свой AdwNavigationView
//! ```
//!
//! Боковая панель — `AdwOverlaySplitView`, а не `AdwNavigationSplitView`: в узком окне она
//! сворачивается и выезжает поверх по кнопке, как у Windows. У `AdwNavigationSplitView`
//! свёрнутая панель — отдельная страница со своей «Назад», и она спорила бы с «Назад»
//! внутри раздела.

use std::cell::Cell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use melogold_core::links::{self, MelogoldLink};
use melogold_core::settings::{keys, Tab};

use crate::app::AppContext;
use crate::localization::{tr, trf};
use crate::pages;

/// Порог узкого окна: боковая панель сворачивается (docs/PROMPT.md §5.2, порог Windows 720).
const NARROW: &str = "max-width: 720sp";

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
    toasts: adw::ToastOverlay,
    split: adw::OverlaySplitView,
    stack: gtk::Stack,
    sections: Vec<Section>,
    back: gtk::Button,
    search: gtk::SearchEntry,
    current: Cell<Tab>,
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
            let nav = adw::NavigationView::new();
            sections.push(Section { tab, nav, row, list: list.clone() });
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

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        let outer = adw::ToolbarView::new();
        outer.set_content(Some(&toasts));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Melogold")
            .content(&outer)
            .default_width(settings.get(&keys::WINDOW_WIDTH).max(360))
            .default_height(settings.get(&keys::WINDOW_HEIGHT).max(294))
            .width_request(360)
            .height_request(294)
            .build();
        if settings.get(&keys::WINDOW_MAXIMIZED) {
            window.maximize();
        }
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::parse(NARROW).expect("условие порога"));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        narrow.add_setter(&split, "show-sidebar", Some(&false.to_value()));
        window.add_breakpoint(narrow);

        let this =
            MainWindow(Rc::new(Inner { window, ctx, toasts, split, stack, sections, back, search, current: Cell::new(Tab::Trends) }));
        this.populate_sections();
        this.connect_sidebar(&top_list);
        this.connect_sidebar(&bottom_list);
        this.install_actions();
        this.install_keys();
        this.install_drop();
        this.connect_close();
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

    fn section(&self, tab: Tab) -> &Section {
        self.sections.iter().find(|s| s.tab == tab).expect("раздел есть")
    }

    pub fn nav(&self, tab: Tab) -> &adw::NavigationView {
        &self.section(tab).nav
    }

    /// Открыть страницу в стеке текущего раздела (REWRITE §2.3: детальные экраны — в стеке раздела).
    pub fn push(&self, page: &adw::NavigationPage) {
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

    /// Назад: сначала стек раздела. В корне ничего не делаем — выход остаётся за Ctrl+Q и Ctrl+W.
    pub fn go_back(&self) -> bool {
        let nav = self.nav(self.current.get());
        if nav.navigation_stack().n_items() > 1 {
            nav.pop();
            true
        } else {
            false
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
        // Ссылки YouTube и текст — в поиск (разбор ссылок — срез 3).
        self.search.set_text(text);
        self.search.grab_focus();
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

    fn install_actions(&self) {
        let weak = self.downgrade();
        let back = gio::ActionEntry::builder("back")
            .activate(move |_: &adw::ApplicationWindow, _, _| {
                if let Some(window) = weak.upgrade() {
                    window.go_back();
                }
            })
            .build();
        let weak = self.downgrade();
        let search = gio::ActionEntry::builder("search")
            .activate(move |_: &adw::ApplicationWindow, _, _| {
                if let Some(window) = weak.upgrade() {
                    window.focus_search();
                }
            })
            .build();
        let weak = self.downgrade();
        let section = gio::ActionEntry::builder("section")
            .parameter_type(Some(glib::VariantTy::INT32))
            .activate(move |_: &adw::ApplicationWindow, _, parameter| {
                let index = parameter.and_then(|p| p.get::<i32>()).unwrap_or(0);
                if let (Some(window), Some(tab)) = (weak.upgrade(), Tab::ALL.get(index.max(0) as usize)) {
                    window.show_tab(*tab);
                }
            })
            .build();
        let weak = self.downgrade();
        let preferences = gio::ActionEntry::builder("preferences")
            .activate(move |_: &adw::ApplicationWindow, _, _| {
                if let Some(window) = weak.upgrade() {
                    window.show_tab(Tab::Settings);
                }
            })
            .build();
        self.window.add_action_entries([back, search, section, preferences]);
    }

    pub fn focus_search(&self) {
        self.search.grab_focus();
        self.search.select_region(0, -1);
    }

    /// Клавиши без модификаторов — в фазе перехвата и только когда фокус не в поле ввода:
    /// ускорители приложения перехватили бы «/» и в поле поиска (docs/PROMPT.md §5.3).
    fn install_keys(&self) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.downgrade();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(window) = weak.upgrade() else { return glib::Propagation::Proceed };
            let typing = gtk::prelude::GtkWindowExt::focus(&window.window).is_some_and(|w| w.is::<gtk::Text>() || w.is::<gtk::TextView>());
            if typing || !modifiers.difference(gdk::ModifierType::SHIFT_MASK).is_empty() {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::slash => {
                    window.focus_search();
                    glib::Propagation::Stop
                }
                gdk::Key::Escape if window.go_back() => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            }
        });
        self.window.add_controller(keys);

        let weak = self.downgrade();
        self.search.connect_activate(move |entry| {
            if let Some(window) = weak.upgrade() {
                let text = entry.text();
                if links::is_melogold_link(&text) {
                    entry.set_text("");
                    window.open_text(&text);
                }
            }
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
        self.window.connect_close_request(move |window| {
            let (width, height) = window.default_size();
            settings.set(&keys::WINDOW_WIDTH, width);
            settings.set(&keys::WINDOW_HEIGHT, height);
            settings.set(&keys::WINDOW_MAXIMIZED, window.is_maximized());
            settings.flush();
            glib::Propagation::Proceed
        });
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
