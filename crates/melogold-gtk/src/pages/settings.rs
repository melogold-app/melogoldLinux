//! Настройки (docs/PROMPT.md §5.4, REWRITE §3.5). Только то, что есть на Android, плюс
//! платформенное; группы появляются вместе со срезом, который их оживляет.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::app_info::{ISSUES_URL, REPOSITORY_URL, VERSION};
use melogold_core::settings::{keys, ThemeMode};
use melogold_server::account::AccountState;
use melogold_server::sync::SyncStatus;

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

/// Аккаунт и сервер (REWRITE §3.5.12–§3.5.13): «Работает без аккаунта» с «Войти» и «Создать
/// аккаунт», «Вы вошли как …» со статусом синхронизации, «Нужно войти снова»; сервер — адресом.
fn account_group(window: &MainWindow) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let account = adw::ActionRow::new();
    let icon = gtk::Image::from_icon_name("avatar-default-symbolic");
    account.add_prefix(&icon);
    let arrow = gtk::Image::from_icon_name("go-next-symbolic");
    account.add_suffix(&arrow);
    group.add(&account);
    // Без аккаунта — строки-кнопки под ним: в узком окне кнопки справа сжали бы текст.
    let sign_in = adw::ButtonRow::builder().title(tr("AccountSignIn")).build();
    sign_in.add_css_class("suggested-action");
    let register = adw::ButtonRow::builder().title(tr("AccountRegister")).build();
    group.add(&sign_in);
    group.add(&register);
    let server = adw::ActionRow::builder().title(tr("SettingsServer.Header")).activatable(true).build();
    server.add_prefix(&gtk::Image::from_icon_name("network-server-symbolic"));
    server.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    group.add(&server);

    let refresh: Rc<dyn Fn()> = {
        let (weak, account, icon, sign_in, register, arrow, server) =
            (window.downgrade(), account.clone(), icon.clone(), sign_in.clone(), register.clone(), arrow.clone(), server.clone());
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let services = &window.ctx.services;
            let signed_in = matches!(services.account.state(), AccountState::SignedIn { .. });
            match services.account.state() {
                AccountState::SignedIn { login, .. } => {
                    account.set_title(&glib::markup_escape_text(&trf("AccountSignedInAsFormat", &[&login])));
                    let status = services.sync.status();
                    account.set_subtitle(&super::account::status_text(&status));
                    icon.set_icon_name(Some(if matches!(status, SyncStatus::Failed { .. }) {
                        "network-offline-symbolic"
                    } else {
                        "avatar-default-symbolic"
                    }));
                }
                AccountState::AuthRequired { .. } => {
                    account.set_title(tr("AccountAuthRequiredTitle"));
                    account.set_subtitle(tr("AccountAuthRequiredText"));
                    icon.set_icon_name(Some("dialog-warning-symbolic"));
                }
                AccountState::SignedOut => {
                    account.set_title(tr("NoAccountTitle"));
                    account.set_subtitle(tr("NoAccountText"));
                    icon.set_icon_name(Some("avatar-default-symbolic"));
                }
            }
            account.set_activatable(signed_in);
            arrow.set_visible(signed_in);
            sign_in.set_visible(!signed_in);
            register.set_visible(!signed_in);
            server.set_subtitle(&super::account::host(&services.account.server_url()));
        })
    };
    refresh();
    window.account_view.listen(&refresh);
    // Обработчик живёт, пока жива группа.
    let keep = RefCell::new(Some(refresh));
    group.connect_destroy(move |_| {
        keep.take();
    });
    let weak = window.downgrade();
    account.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::account::account_page(&window));
        }
    });
    let weak = window.downgrade();
    sign_in.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::account::sign_in_page(&window));
        }
    });
    let weak = window.downgrade();
    register.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::account::register_page(&window));
        }
    });
    let weak = window.downgrade();
    server.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&super::account::server_page(&window));
        }
    });
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
