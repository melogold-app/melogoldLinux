//! Приложение: один экземпляр, действия уровня приложения, входы из командной строки.
//!
//! `app.melogold.Melogold` — GApplication: второй запуск (в том числе ссылка `melogold://`
//! из браузера) отдаёт аргументы уже открытому окну и выходит (docs/PROMPT.md §3 «Идентичность»).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_core::app_info::{APP_ID, VERSION};
use melogold_core::paths::AppPaths;
use melogold_core::settings::{keys, ThemeMode};

use crate::localization::{self, tr};
use crate::settings_store::SettingsStore;
use crate::window::MainWindow;
use crate::{logging, shortcuts};

/// Общее для окна и страниц: пути, настройки. Живёт столько же, сколько приложение.
pub struct AppContext {
    pub paths: AppPaths,
    pub settings: Rc<SettingsStore>,
}

pub fn snapshot_mode() -> bool {
    cfg!(debug_assertions) && std::env::var_os("MELOGOLD_SCREENSHOT_DIR").is_some()
}

pub fn run(args: Vec<String>) -> glib::ExitCode {
    // Режим снимков — отдельный экземпляр: имя на шине переживает убитый процесс, и
    // осиротевшая регистрация заставляла бы следующий прогон молча выйти (Clementine).
    let mut flags = gio::ApplicationFlags::HANDLES_OPEN;
    if snapshot_mode() {
        flags |= gio::ApplicationFlags::NON_UNIQUE;
    }
    let app = adw::Application::builder().application_id(APP_ID).flags(flags).resource_base_path("/app/melogold/Melogold").build();

    let state: Rc<RefCell<Option<Rc<AppContext>>>> = Rc::default();
    let window: Rc<RefCell<Option<MainWindow>>> = Rc::default();

    app.connect_startup({
        let state = Rc::clone(&state);
        move |app| {
            let paths = AppPaths::from_env();
            if let Err(error) = paths.ensure() {
                eprintln!("папки данных не создались: {error}");
            }
            logging::init(&paths.logs());
            let lang = localization::lang();
            tracing::info!(
                "Melogold {VERSION} · {} · GTK {}.{}.{} · libadwaita {}.{}.{} · язык {lang:?}",
                melogold_core::system::os_pretty_name(),
                gtk::major_version(),
                gtk::minor_version(),
                gtk::micro_version(),
                adw::major_version(),
                adw::minor_version(),
                adw::micro_version(),
            );
            let settings = SettingsStore::load(paths.settings(), snapshot_mode());
            apply_theme(settings.get(&keys::THEME_MODE));
            state.replace(Some(Rc::new(AppContext { paths, settings })));
            install_actions(app);
        }
    });

    let present = {
        let state = Rc::clone(&state);
        let window = Rc::clone(&window);
        move |app: &adw::Application| -> MainWindow {
            if let Some(existing) = window.borrow().as_ref() {
                existing.present();
                return existing.clone();
            }
            let ctx = state.borrow().clone().expect("startup прошёл раньше activate");
            let created = MainWindow::new(app, ctx);
            created.present();
            window.replace(Some(created.clone()));
            created
        }
    };

    app.connect_activate({
        let present = present.clone();
        move |app| {
            present(app);
        }
    });

    // Ссылки: `melogold://…` и адреса YouTube — первым аргументом или от второго экземпляра.
    app.connect_open(move |app, files, _hint| {
        let window = present(app);
        for file in files {
            let uri = file.uri();
            tracing::debug!("вход: ссылка из командной строки");
            window.open_text(&uri);
        }
    });

    app.connect_shutdown(move |_| {
        if let Some(ctx) = state.borrow().as_ref() {
            ctx.settings.flush();
        }
    });

    app.run_with_args(&args)
}

pub fn apply_theme(mode: ThemeMode) {
    let scheme = match mode {
        ThemeMode::System => adw::ColorScheme::Default,
        ThemeMode::Light => adw::ColorScheme::ForceLight,
        ThemeMode::Dark => adw::ColorScheme::ForceDark,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}

fn install_actions(app: &adw::Application) {
    let quit = gio::ActionEntry::builder("quit")
        .activate(|app: &adw::Application, _, _| {
            for window in app.windows() {
                window.close();
            }
            app.quit();
        })
        .build();
    let shortcuts = gio::ActionEntry::builder("shortcuts")
        .activate(|app: &adw::Application, _, _| {
            shortcuts::present(app.active_window().as_ref());
        })
        .build();
    let about =
        gio::ActionEntry::builder("about").activate(|app: &adw::Application, _, _| present_about(app.active_window().as_ref())).build();
    app.add_action_entries([quit, shortcuts, about]);

    // Стандарт GNOME (docs/PROMPT.md §5.3): Ctrl+Q — выйти, Ctrl+? и F1 — сочетания.
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("app.shortcuts", &["<Control>question", "F1"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
    app.set_accels_for_action("win.preferences", &["<Control>comma"]);
    app.set_accels_for_action("win.search", &["<Control>f"]);
    app.set_accels_for_action("win.section(0)", &["<Control>1"]);
    app.set_accels_for_action("win.section(1)", &["<Control>2"]);
    app.set_accels_for_action("win.section(2)", &["<Control>3"]);
    app.set_accels_for_action("win.back", &["<Alt>Left"]);
}

pub fn present_about(parent: Option<&gtk::Window>) {
    let about = adw::AboutDialog::builder()
        .application_name("Melogold")
        .application_icon(APP_ID)
        .version(VERSION)
        .developer_name("Melogold")
        .website(melogold_core::app_info::REPOSITORY_URL)
        .issue_url(melogold_core::app_info::ISSUES_URL)
        .license_type(gtk::License::Gpl30)
        .comments(tr("LinuxAboutComments"))
        .build();
    about.present(parent);
}
