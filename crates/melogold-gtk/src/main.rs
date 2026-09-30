//! Melogold для Linux: клиент YouTube Music и YouTube с общей библиотекой на всех устройствах.

// Обработчики GTK живут в `Rc<RefCell<Option<Box<dyn Fn…>>>>` и подобном — это их обычная форма,
// а не запутанность; псевдонимы на каждый такой тип читались бы хуже.
#![allow(clippy::type_complexity)]

mod account_view;
mod app;
mod backup_ui;
mod catalog_widgets;
mod desktop_integration;
mod device_filter;
mod images;
mod library_view;
mod localization;
mod logging;
mod lyrics_dialogs;
mod lyrics_editor;
mod lyrics_service;
mod lyrics_view;
mod mpris;
mod now_playing;
mod pages;
mod player_bar;
mod playing_bars;
mod queue_panel;
mod remote;
mod remote_bar;
mod remote_sheet;
mod save_file;
mod selection;
mod services;
mod settings_store;
mod share;
mod shortcuts;
mod snapshot;
mod snapshot_features;
mod snapshot_stats;
mod stats_chart;
mod stats_text;
mod strings_generated;
mod texts;
mod track_row;
mod updates;
mod widgets;
mod window;
mod wrapped;

use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().skip(1).any(|arg| arg == "--version" || arg == "-V") {
        println!("melogold {}", melogold_core::app_info::VERSION);
        return glib::ExitCode::SUCCESS;
    }
    gio::resources_register_include!("melogold.gresource").expect("ресурсы вшиты в бинарь");
    localization::init(chosen_language());
    app::run(args)
}

/// Язык снимков, `MELOGOLD_LANG` или «Язык приложения» из настроек; `None` — как в системе.
fn chosen_language() -> Option<localization::Lang> {
    use melogold_core::settings::{keys, Settings};
    if app::snapshot_mode() {
        if let Some(lang) = std::env::var("MELOGOLD_SCREENSHOT_LANG").ok().as_deref().and_then(localization::Lang::parse) {
            return Some(lang);
        }
    }
    if let Some(lang) = std::env::var("MELOGOLD_LANG").ok().as_deref().and_then(localization::Lang::parse) {
        return Some(lang);
    }
    let paths = melogold_core::paths::AppPaths::from_env();
    localization::Lang::parse(&Settings::load(&paths.settings()).get(&keys::APP_LOCALE))
}
