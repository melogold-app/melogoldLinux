//! Melogold для Linux: клиент YouTube Music и YouTube с общей библиотекой на всех устройствах.

mod app;
mod localization;
mod logging;
mod pages;
mod settings_store;
mod shortcuts;
#[cfg(debug_assertions)]
mod snapshot;
mod strings_generated;
mod window;

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
