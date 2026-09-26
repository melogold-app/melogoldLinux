//! Настройки (docs/PROMPT.md §5.4, REWRITE §3.5). Только то, что есть на Android, плюс
//! платформенное; группы появляются вместе со срезом, который их оживляет.

use adw::prelude::*;
use melogold_core::app_info::{DEFAULT_SERVER_URL, ISSUES_URL, REPOSITORY_URL, VERSION};
use melogold_core::settings::{keys, ThemeMode};

use crate::app::{apply_theme, present_about};
use crate::localization::{tr, trf};
use crate::window::MainWindow;

const NEW_ISSUE_URL: &str = "https://github.com/melogold-app/melogoldLinux/issues/new";

pub fn root(window: &MainWindow) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.add(&account_group(window));
    page.add(&appearance_group(window));
    page.add(&about_group(window));
    adw::NavigationPage::builder().title(tr("NavSettings")).tag("root").child(&page).build()
}

fn account_group(window: &MainWindow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let account =
        adw::ActionRow::builder().title(tr("SettingsWithoutAccount.Header")).subtitle(tr("SettingsWithoutAccount.Description")).build();
    account.add_prefix(&gtk::Image::from_icon_name("avatar-default-symbolic"));
    group.add(&account);
    let server_url = window.ctx.settings.get(&keys::SERVER_URL).unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned());
    let host = url::Url::parse(&server_url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or(server_url);
    let server = adw::ActionRow::builder().title(tr("SettingsServer.Header")).subtitle(host).build();
    server.add_prefix(&gtk::Image::from_icon_name("network-server-symbolic"));
    group.add(&server);
    group
}

fn appearance_group(window: &MainWindow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("SettingsAppearance.Text")).build();
    let modes = [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark];
    let names = gtk::StringList::new(&[tr("ThemeSystem.Content"), tr("ThemeLight.Content"), tr("ThemeDark.Content")]);
    let theme = adw::ComboRow::builder().title(tr("SettingsTheme.Header")).model(&names).build();
    let current = window.ctx.settings.get(&keys::THEME_MODE);
    theme.set_selected(modes.iter().position(|mode| *mode == current).unwrap_or(0) as u32);
    let settings = window.ctx.settings.clone();
    theme.connect_selected_notify(move |row| {
        let mode = modes.get(row.selected() as usize).copied().unwrap_or_default();
        settings.set(&keys::THEME_MODE, mode);
        apply_theme(mode);
    });
    group.add(&theme);

    // Названия языков — на своём языке (GLOSSARY: «Как в системе · Русский · English»).
    let locales = ["system", "ru", "en"];
    let names = gtk::StringList::new(&[tr("ThemeSystem.Content"), "Русский", "English"]);
    let language = adw::ComboRow::builder().title(tr("LinuxAppLanguage")).model(&names).build();
    let current = window.ctx.settings.get(&keys::APP_LOCALE);
    language.set_selected(locales.iter().position(|l| *l == current).unwrap_or(0) as u32);
    let settings = window.ctx.settings.clone();
    let weak = window.downgrade();
    language.connect_selected_notify(move |row| {
        let locale = locales.get(row.selected() as usize).copied().unwrap_or("system");
        settings.set(&keys::APP_LOCALE, locale.to_owned());
        if let Some(window) = weak.upgrade() {
            window.toast(tr("LinuxLanguageRestart"));
        }
    });
    group.add(&language);
    group
}

fn about_group(window: &MainWindow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(tr("SettingsAbout.Text")).build();

    let version =
        adw::ActionRow::builder().title(tr("SettingsVersion.Header")).subtitle(trf("VersionFormat", &[&VERSION])).activatable(true).build();
    version.connect_activated(|row| present_about(row.root().and_downcast::<gtk::Window>().as_ref()));
    group.add(&version);

    let shortcuts = adw::ActionRow::builder()
        .title(tr("SettingsShortcuts.Header"))
        .subtitle(tr("LinuxShortcutsDescription"))
        .activatable(true)
        .action_name("app.shortcuts")
        .build();
    group.add(&shortcuts);

    group.add(&super::link_row(tr("SettingsSource.Header"), tr("SettingsSource.Description"), REPOSITORY_URL));
    group.add(&super::link_row(tr("SettingsReportBug.Header"), tr("SettingsReportBug.Description"), ISSUES_URL));
    group.add(&super::link_row(tr("SettingsRequestFeature.Header"), tr("SettingsRequestFeature.Description"), NEW_ISSUE_URL));

    let diagnostics = super::next_row(tr("SettingsDiagnostics.Header"), tr("SettingsDiagnostics.Description"));
    let weak = window.downgrade();
    diagnostics.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::diagnostics::page(&window));
        }
    });
    group.add(&diagnostics);
    group
}
