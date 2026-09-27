//! Аккаунт и сервер (docs/PROMPT.md §5.4 «Аккаунт», REWRITE §3.5.12–§3.5.13, Windows
//! `AccountPages.cs`, `LinkDeviceDialog.cs`): вход, регистрация с кодом восстановления, аккаунт с
//! синхронизацией и устройствами, «Добавить устройство» по коду, адрес сервера.
//!
//! Пароль из поля уходит прямо в запрос и нигде не остаётся: на устройстве хранятся только токены.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::devices::{self, DeviceKind};
use melogold_core::iso;
use melogold_core::server_address;
use melogold_core::text::now_ms;
use melogold_server::account::{AccountState, DEFAULT_SERVER_URL};
use melogold_server::api::ApiError;
use melogold_server::dto::{DeviceDto, LinkDetails};
use melogold_server::sync::SyncStatus;

use crate::localization::{plural, tr, trf};
use crate::window::MainWindow;

// ── тексты ──

/// Ошибка сервера словами (Windows `AccountTexts.Error`).
pub fn error_text(error: &ApiError) -> &'static str {
    tr(match error.code.as_str() {
        "invalid_credentials" => "AccountErrorCredentials",
        "login_throttled" | "rate_limited" | "reauth_throttled" => "AccountErrorThrottled",
        "device_limit_reached" => "AccountErrorDeviceLimit",
        "login_taken" => "AccountErrorLoginTaken",
        "invalid_login_format" => "AccountErrorLoginFormat",
        "password_too_short" => "AccountErrorPasswordShort",
        "password_too_common" | "password_too_weak" | "password_contains_login" | "password_too_long" => "AccountErrorPasswordWeak",
        "registration_closed" => "AccountErrorRegistrationClosed",
        "recent_device_restricted" => "AccountErrorPasswordNeeded",
        "invalid_password" => "AccountErrorInvalidPassword",
        "client_outdated" => "ServerClientOutdated",
        "server_outdated" => "ServerTooOld",
        _ if error.is_network() => "AccountErrorNetwork",
        _ => "AccountErrorUnknown",
    })
}

/// «Синхронизировано · 2 минуты назад», «Синхронизация…», «Нет связи с сервером»…
pub fn status_text(status: &SyncStatus) -> String {
    match status {
        SyncStatus::Syncing => tr("SyncStatusSyncing").to_owned(),
        SyncStatus::Failed { offline: true, .. } => tr("SyncStatusOffline").to_owned(),
        SyncStatus::Failed { .. } => tr("SyncStatusFailed").to_owned(),
        SyncStatus::Idle { last_sync_at: Some(at) } => trf("SyncStatusDoneFormat", &[&relative(*at)]),
        _ => tr("SyncStatusNever").to_owned(),
    }
}

/// «только что», «5 минут назад», «2 часа назад», «3 дня назад», дальше — дата.
pub fn relative(epoch_ms: i64) -> String {
    let age = (now_ms() - epoch_ms).max(0) / 1000;
    match age {
        a if a < 60 => tr("JustNow").to_owned(),
        a if a < 3600 => plural("MinutesAgo", a / 60),
        a if a < 86_400 => plural("HoursAgo", a / 3600),
        a if a < 7 * 86_400 => plural("DaysAgo", a / 86_400),
        _ => glib::DateTime::from_unix_local(epoch_ms / 1000)
            .ok()
            .and_then(|d| d.format("%-d %B %Y").ok())
            .map(|s| s.to_string())
            .unwrap_or_default(),
    }
}

pub fn host(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| url.to_owned())
}

/// Значок и подпись вида устройства (задание Windows 0006 §1).
pub fn device_icon(platform: Option<&str>) -> (&'static str, &'static str) {
    match devices::kind(platform) {
        DeviceKind::Phone => ("phone-symbolic", "DevicePhone"),
        DeviceKind::Tablet => ("tablet-symbolic", "DeviceTablet"),
        DeviceKind::Computer => ("computer-symbolic", "DeviceComputer"),
        DeviceKind::Watch => ("preferences-system-time-symbolic", "DeviceWatch"),
        DeviceKind::Headset => ("audio-headset-symbolic", "DeviceHeadset"),
        DeviceKind::Other => ("computer-symbolic", "DeviceOther"),
    }
}

// ── общая разметка форм ──

/// Страница-форма: узкая колонка, заголовок, пояснение, поля.
struct Form {
    page: adw::NavigationPage,
    body: gtk::Box,
    error: gtk::Label,
}

fn form(title: &str, description: Option<&str>) -> Form {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(24)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    let heading = gtk::Label::builder().label(title).xalign(0.0).wrap(true).build();
    heading.add_css_class("title-1");
    body.append(&heading);
    if let Some(description) = description {
        let text = gtk::Label::builder().label(description).xalign(0.0).wrap(true).build();
        text.add_css_class("dim-label");
        body.append(&text);
    }
    let error = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
    error.add_css_class("error");
    let clamp = adw::Clamp::builder().maximum_size(480).child(&body).build();
    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&clamp).vexpand(true).build();
    let page = adw::NavigationPage::builder().title(title).child(&scroller).build();
    Form { page, body, error }
}

fn show_error(label: &gtk::Label, text: Option<&str>) {
    label.set_label(text.unwrap_or_default());
    label.set_visible(text.is_some());
}

fn boxed_list(rows: &[&gtk::Widget]) -> gtk::ListBox {
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    list.add_css_class("boxed-list");
    for row in rows {
        list.append(*row);
    }
    list
}

/// Кнопка действия формы: на время запроса — крутилка вместо текста.
#[derive(Clone)]
struct ActionButton {
    button: gtk::Button,
    stack: gtk::Stack,
}

impl ActionButton {
    fn new(label: &str, suggested: bool) -> ActionButton {
        let stack = gtk::Stack::new();
        stack.add_named(&gtk::Label::new(Some(label)), Some("label"));
        stack.add_named(&adw::Spinner::new(), Some("busy"));
        let button = gtk::Button::builder().child(&stack).halign(gtk::Align::Center).width_request(200).build();
        button.add_css_class("pill");
        if suggested {
            button.add_css_class("suggested-action");
        }
        button.update_property(&[gtk::accessible::Property::Label(label)]);
        ActionButton { button, stack }
    }

    fn set_busy(&self, busy: bool) {
        self.stack.set_visible_child_name(if busy { "busy" } else { "label" });
    }

    fn busy(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some("busy")
    }
}

fn server_caption(window: &MainWindow) -> gtk::Label {
    let label =
        gtk::Label::builder().label(trf("AccountOnServerFormat", &[&host(&window.ctx.services.account.server_url())])).xalign(0.0).build();
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    label
}

/// К корню Настроек — после входа, выхода, смены сервера.
fn back_to_root(window: &MainWindow) {
    let nav = window.nav(melogold_core::settings::Tab::Settings);
    if let Some(root) = nav.navigation_stack().item(0).and_downcast::<adw::NavigationPage>() {
        nav.pop_to_page(&root);
    }
}

// ── вход ──

/// Вход логином и паролем (API §4.3, Android `SignInScreen`).
pub fn sign_in_page(window: &MainWindow) -> adw::NavigationPage {
    let f = form(tr("AccountSignInTitle"), Some(tr("AccountSignInText")));
    let account = &window.ctx.services.account;
    // Вход после того, как сервер закончил сессию: логин известен.
    let known = match account.state() {
        AccountState::AuthRequired { login, .. } => login,
        _ => String::new(),
    };
    let login = adw::EntryRow::builder().title(tr("AccountLogin")).text(&known).build();
    login.set_input_purpose(gtk::InputPurpose::FreeForm);
    let password = adw::PasswordEntryRow::builder().title(tr("AccountPassword")).activates_default(false).build();
    f.body.append(&boxed_list(&[login.upcast_ref(), password.upcast_ref()]));
    f.body.append(&f.error);
    let submit = ActionButton::new(tr("AccountSignIn"), true);
    f.body.append(&submit.button);
    let register = gtk::Button::builder().label(tr("AccountNoAccount")).halign(gtk::Align::Center).build();
    register.add_css_class("flat");
    f.body.append(&register);
    f.body.append(&server_caption(window));

    let update = {
        let (login, password, submit) = (login.clone(), password.clone(), submit.clone());
        Rc::new(move || submit.button.set_sensitive(!submit.busy() && !login.text().trim().is_empty() && !password.text().is_empty()))
    };
    update();
    let u = Rc::clone(&update);
    login.connect_changed(move |_| u());
    let u = Rc::clone(&update);
    password.connect_changed(move |_| u());

    let submit_action: Rc<dyn Fn()> = {
        let (weak, login, password, submit, error, update) =
            (window.downgrade(), login.clone(), password.clone(), submit.clone(), f.error.clone(), Rc::clone(&update));
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            if submit.busy() || !submit.button.is_sensitive() {
                return;
            }
            submit.set_busy(true);
            update();
            show_error(&error, None);
            let account = Arc::clone(&window.ctx.services.account);
            let (name, secret) = (login.text().trim().to_owned(), password.text().to_string());
            let task = window.ctx.services.run(async move { account.sign_in(&name, &secret).await });
            let (weak, submit, error, update) = (window.downgrade(), submit.clone(), error.clone(), Rc::clone(&update));
            glib::spawn_future_local(async move {
                submit.set_busy(false);
                update();
                match task.await {
                    Some(Ok(())) => {
                        if let Some(window) = weak.upgrade() {
                            back_to_root(&window);
                        }
                    }
                    Some(Err(e)) => show_error(&error, Some(error_text(&e))),
                    None => {}
                }
            });
        })
    };
    let s = Rc::clone(&submit_action);
    submit.button.connect_clicked(move |_| s());
    let s = Rc::clone(&submit_action);
    password.connect_entry_activated(move |_| s());
    let weak = window.downgrade();
    register.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.push(&register_page(&window));
        }
    });
    let (login_focus, password_focus) = (login.clone(), password.clone());
    f.page.connect_shown(move |_| {
        if login_focus.text().is_empty() {
            login_focus.grab_focus();
        } else {
            password_focus.grab_focus();
        }
    });
    f.page
}

// ── регистрация ──

/// Регистрация логином и паролем, затем код восстановления — один раз (Android `RegisterScreen`).
pub fn register_page(window: &MainWindow) -> adw::NavigationPage {
    let f = form(tr("AccountRegisterTitle"), Some(tr("AccountRegisterText")));
    let login = adw::EntryRow::builder().title(tr("AccountLogin")).build();
    let password = adw::PasswordEntryRow::builder().title(tr("AccountPassword")).build();
    let repeat = adw::PasswordEntryRow::builder().title(tr("AccountPasswordRepeat")).build();
    f.body.append(&boxed_list(&[login.upcast_ref(), password.upcast_ref(), repeat.upcast_ref()]));
    let hints =
        gtk::Label::builder().label(format!("{}\n{}", tr("AccountLoginHint"), tr("AccountPasswordHint"))).xalign(0.0).wrap(true).build();
    hints.add_css_class("dim-label");
    hints.add_css_class("caption");
    f.body.append(&hints);
    f.body.append(&f.error);
    let submit = ActionButton::new(tr("AccountRegister"), true);
    f.body.append(&submit.button);
    f.body.append(&server_caption(window));

    let update = {
        let (login, password, repeat, submit) = (login.clone(), password.clone(), repeat.clone(), submit.clone());
        Rc::new(move || {
            submit.button.set_sensitive(
                !submit.busy()
                    && login.text().trim().chars().count() >= 3
                    && password.text().chars().count() >= 8
                    && !repeat.text().is_empty(),
            )
        })
    };
    update();
    for entry in [login.upcast_ref::<gtk::Editable>(), password.upcast_ref(), repeat.upcast_ref()] {
        let u = Rc::clone(&update);
        entry.connect_changed(move |_| u());
    }
    // Доказательство работы прерывается, если со страницы ушли.
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    f.page.connect_hidden(move |_| flag.store(true, Ordering::Relaxed));
    let flag = Arc::clone(&cancel);
    f.page.connect_shown(move |_| flag.store(false, Ordering::Relaxed));

    let submit_action: Rc<dyn Fn()> = {
        let (weak, login, password, repeat, submit, error, update, page_body) = (
            window.downgrade(),
            login.clone(),
            password.clone(),
            repeat.clone(),
            submit.clone(),
            f.error.clone(),
            Rc::clone(&update),
            f.body.clone(),
        );
        let page = f.page.clone();
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            if submit.busy() || !submit.button.is_sensitive() {
                return;
            }
            if password.text() != repeat.text() {
                show_error(&error, Some(tr("AccountErrorPasswordMismatch")));
                return;
            }
            submit.set_busy(true);
            update();
            show_error(&error, None);
            let account = Arc::clone(&window.ctx.services.account);
            let (name, secret, cancel) = (login.text().trim().to_owned(), password.text().to_string(), Arc::clone(&cancel));
            // Доказательство работы считается в фоне (API §4.3): кнопка крутится, окно живое.
            let task = window.ctx.services.run(async move { account.register(&name, &secret, cancel).await });
            let (weak, submit, error, update, body, page) =
                (window.downgrade(), submit.clone(), error.clone(), Rc::clone(&update), page_body.clone(), page.clone());
            glib::spawn_future_local(async move {
                let result = task.await;
                submit.set_busy(false);
                update();
                match result {
                    Some(Ok(code)) => {
                        if let Some(window) = weak.upgrade() {
                            show_recovery_code(&window, &page, &body, &code);
                        }
                    }
                    Some(Err(e)) if e.code == "cancelled" => {}
                    Some(Err(e)) => show_error(&error, Some(error_text(&e))),
                    None => {}
                }
            });
        })
    };
    let s = Rc::clone(&submit_action);
    submit.button.connect_clicked(move |_| s());
    let s = Rc::clone(&submit_action);
    repeat.connect_entry_activated(move |_| s());
    let focus = login.clone();
    f.page.connect_shown(move |_| {
        focus.grab_focus();
    });
    f.page
}

/// Страница с кодом восстановления (снимки окна; при регистрации код встаёт на место формы).
#[cfg(debug_assertions)]
pub fn recovery_code_page(window: &MainWindow, code: &str) -> adw::NavigationPage {
    let f = form(tr("AccountRecoveryTitle"), None);
    show_recovery_code(window, &f.page, &f.body, code);
    f.page
}

/// Код восстановления: крупно, моноширинным, «Копировать» и «Я сохранил код» (REWRITE §3.5.13).
fn show_recovery_code(window: &MainWindow, page: &adw::NavigationPage, body: &gtk::Box, code: &str) {
    while let Some(child) = body.first_child() {
        body.remove(&child);
    }
    page.set_title(tr("AccountRecoveryTitle"));
    page.set_can_pop(false);
    let heading = gtk::Label::builder().label(tr("AccountRecoveryTitle")).xalign(0.0).build();
    heading.add_css_class("title-1");
    body.append(&heading);
    let text = gtk::Label::builder().label(tr("AccountRecoveryText")).xalign(0.0).wrap(true).build();
    text.add_css_class("dim-label");
    body.append(&text);
    let code_label = gtk::Label::builder().label(code).selectable(true).wrap(true).justify(gtk::Justification::Center).build();
    code_label.add_css_class("recovery-code");
    code_label.add_css_class("monospace");
    let card = gtk::Box::builder().build();
    card.add_css_class("card");
    code_label.set_hexpand(true);
    code_label.set_margin_top(24);
    code_label.set_margin_bottom(24);
    code_label.set_margin_start(12);
    code_label.set_margin_end(12);
    card.append(&code_label);
    body.append(&card);
    let copy = gtk::Button::builder()
        .child(&adw::ButtonContent::builder().label(tr("AccountRecoveryCopy")).icon_name("edit-copy-symbolic").build())
        .halign(gtk::Align::Center)
        .build();
    copy.add_css_class("pill");
    let (weak, text_to_copy) = (window.downgrade(), code.to_owned());
    copy.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.window.clipboard().set_text(&text_to_copy);
            window.toast(tr("AccountRecoveryCopied"));
        }
    });
    body.append(&copy);
    let saved = gtk::CheckButton::builder().label(tr("AccountRecoverySaved")).halign(gtk::Align::Center).build();
    body.append(&saved);
    // Фокус — на «Копировать»: иначе выделяемый код выделился бы целиком при показе.
    let (focus, selection) = (copy.clone(), code_label.clone());
    glib::idle_add_local_once(move || {
        focus.grab_focus();
        selection.select_region(0, 0);
    });
    let done = gtk::Button::builder().label(tr("Done")).halign(gtk::Align::Center).width_request(200).sensitive(false).build();
    done.add_css_class("pill");
    done.add_css_class("suggested-action");
    let done_ref = done.clone();
    saved.connect_toggled(move |check| done_ref.set_sensitive(check.is_active()));
    let weak = window.downgrade();
    let page = page.clone();
    done.connect_clicked(move |_| {
        page.set_can_pop(true);
        if let Some(window) = weak.upgrade() {
            back_to_root(&window);
        }
    });
    body.append(&done);
}

// ── аккаунт ──

/// Аккаунт (REWRITE §3.5.13, Android `AccountScreen`): «Синхронизировать сейчас» со статусом,
/// устройства с «Отвязать» и «Выйти на других устройствах», «Добавить устройство», «Выйти».
pub fn account_page(window: &MainWindow) -> adw::NavigationPage {
    let account = &window.ctx.services.account;
    let login = account.session().map(|s| s.login).unwrap_or_default();
    let preferences = adw::PreferencesPage::builder().title(&login).build();

    // Шапка: кто вошёл и куда (в заголовке окна — поиск, а не название страницы).
    let header_group = adw::PreferencesGroup::new();
    let header = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).halign(gtk::Align::Center).build();
    let avatar = adw::Avatar::builder().size(72).text(&login).show_initials(true).build();
    header.append(&avatar);
    let name = gtk::Label::builder().label(&login).wrap(true).build();
    name.add_css_class("title-2");
    header.append(&name);
    let server = gtk::Label::builder().label(trf("AccountOnServerFormat", &[&host(&account.server_url())])).build();
    server.add_css_class("dim-label");
    header.append(&server);
    header_group.add(&header);
    preferences.add(&header_group);

    let sync_group = adw::PreferencesGroup::builder().title(tr("AccountSyncGroup")).description(tr("AccountSyncWhat")).build();
    let sync_row = adw::ActionRow::builder().title(tr("AccountSyncNow")).activatable(true).build();
    let sync_icon = gtk::Image::from_icon_name("emblem-synchronizing-symbolic");
    sync_row.add_prefix(&sync_icon);
    sync_group.add(&sync_row);
    preferences.add(&sync_group);

    let devices_group = adw::PreferencesGroup::builder().title(tr("AccountDevicesGroup")).build();
    let add_device = gtk::Button::builder()
        .child(&adw::ButtonContent::builder().label(tr("AccountAddDevice")).icon_name("list-add-symbolic").build())
        .valign(gtk::Align::Center)
        .build();
    add_device.add_css_class("flat");
    devices_group.set_header_suffix(Some(&add_device));
    let devices_list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    devices_list.add_css_class("boxed-list");
    devices_group.add(&devices_list);
    preferences.add(&devices_group);

    let sign_out_group = adw::PreferencesGroup::new();
    let sign_out = gtk::Button::builder()
        .child(&adw::ButtonContent::builder().label(tr("AccountSignOut")).icon_name("system-log-out-symbolic").build())
        .halign(gtk::Align::Center)
        .build();
    sign_out.add_css_class("pill");
    sign_out_group.add(&sign_out);
    preferences.add(&sign_out_group);

    let page = adw::NavigationPage::builder().title(&login).child(&preferences).tag("account").build();

    // Статус синхронизации — сейчас и при каждом изменении.
    let show_status: Rc<dyn Fn()> = {
        let (weak, row, icon) = (window.downgrade(), sync_row.clone(), sync_icon.clone());
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let status = window.ctx.services.sync.status();
            row.set_subtitle(&status_text(&status));
            row.set_sensitive(status != SyncStatus::Syncing);
            icon.set_icon_name(Some(if matches!(status, SyncStatus::Failed { .. }) {
                "network-offline-symbolic"
            } else {
                "emblem-synchronizing-symbolic"
            }));
        })
    };
    show_status();
    window.account_view.listen(&show_status);
    let keep = RefCell::new(Some(show_status));
    page.connect_destroy(move |_| {
        keep.take();
    });
    let weak = window.downgrade();
    sync_row.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            let sync = Arc::clone(&window.ctx.services.sync);
            let task = window.ctx.services.run(async move { sync.sync(true).await });
            glib::spawn_future_local(async move {
                let _ = task.await;
            });
        }
    });

    // Устройства — сейчас, по devices.updated и после своих действий.
    let load_devices: Rc<dyn Fn()> = {
        let (weak, list) = (window.downgrade(), devices_list.clone());
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let account = Arc::clone(&window.ctx.services.account);
            let task = window.ctx.services.run(async move { account.devices().await });
            let (weak, list) = (window.downgrade(), list.clone());
            glib::spawn_future_local(async move {
                let (Some(window), Some(result)) = (weak.upgrade(), task.await) else { return };
                match result {
                    Ok(response) => fill_devices(&window, &list, response.devices),
                    Err(error) => tracing::warn!(%error, "устройства не загрузились"),
                }
            });
        })
    };
    load_devices();
    window.account_view.listen_devices(&load_devices);
    let keep = RefCell::new(Some(Rc::clone(&load_devices)));
    page.connect_destroy(move |_| {
        keep.take();
    });

    let weak = window.downgrade();
    add_device.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            link_device_dialog(&window);
        }
    });
    let weak = window.downgrade();
    sign_out.connect_clicked(move |_| {
        let Some(window) = weak.upgrade() else { return };
        let dialog = adw::AlertDialog::new(Some(tr("AccountSignOutTitle")), Some(tr("AccountSignOutText")));
        dialog.add_response("cancel", tr("Cancel"));
        dialog.add_response("sign-out", tr("AccountSignOut"));
        dialog.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = window.downgrade();
        dialog.connect_response(Some("sign-out"), move |_, _| {
            let Some(window) = weak.upgrade() else { return };
            let account = Arc::clone(&window.ctx.services.account);
            let task = window.ctx.services.run(async move { account.sign_out().await });
            glib::spawn_future_local(async move {
                let _ = task.await;
            });
            back_to_root(&window);
        });
        dialog.present(Some(&window.window));
    });
    page
}

fn fill_devices(window: &MainWindow, list: &gtk::ListBox, mut devices: Vec<DeviceDto>) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    devices.sort_by_key(|d| (!d.is_current, -iso::parse(&d.last_seen_at).unwrap_or(0)));
    for device in &devices {
        let (icon, kind) = device_icon(Some(&device.platform));
        let subtitle = if device.is_current {
            tr("AccountDeviceCurrent").to_owned()
        } else {
            iso::parse(&device.last_seen_at).map(|seen| trf("AccountDeviceSeenFormat", &[&relative(seen)])).unwrap_or_default()
        };
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&device.name)).subtitle(subtitle).build();
        let image = gtk::Image::from_icon_name(icon);
        image.update_property(&[gtk::accessible::Property::Label(tr(kind))]);
        row.add_prefix(&image);
        if !device.is_current {
            let revoke = gtk::Button::builder().label(tr("AccountDeviceRevoke")).valign(gtk::Align::Center).build();
            revoke.add_css_class("flat");
            let (weak, id, name) = (window.downgrade(), device.id.clone(), device.name.clone());
            revoke.connect_clicked(move |_| {
                let Some(window) = weak.upgrade() else { return };
                let id = id.clone();
                password_confirm(
                    &window,
                    &trf("AccountDeviceRevokeTitleFormat", &[&name]),
                    tr("AccountDeviceRevokeText"),
                    tr("AccountDeviceRevoke"),
                    Rc::new(move |window: &MainWindow, password: Option<String>| {
                        let (account, id) = (Arc::clone(&window.ctx.services.account), id.clone());
                        Box::pin(window.ctx.services.run(async move { account.revoke(&id, password.as_deref()).await.map(|_| None) }))
                    }),
                );
            });
            row.add_suffix(&revoke);
        }
        list.append(&row);
    }
    if devices.len() > 1 {
        let others = adw::ButtonRow::builder().title(tr("AccountRevokeOthers")).start_icon_name("system-log-out-symbolic").build();
        others.add_css_class("destructive-action");
        let weak = window.downgrade();
        others.connect_activated(move |_| {
            let Some(window) = weak.upgrade() else { return };
            password_confirm(
                &window,
                tr("AccountRevokeOthersTitle"),
                tr("AccountRevokeOthersText"),
                tr("AccountRevokeOthers"),
                Rc::new(|window: &MainWindow, password: Option<String>| {
                    let account = Arc::clone(&window.ctx.services.account);
                    Box::pin(window.ctx.services.run(async move {
                        account
                            .revoke_others(password.as_deref())
                            .await
                            .map(|r| Some(trf("AccountRevokedOthersFormat", &[&r.revoked_count])))
                    }))
                }),
            );
        });
        list.append(&others);
    }
}

type Action = Rc<
    dyn Fn(&MainWindow, Option<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Result<Option<String>, ApiError>>>>>,
>;

/// Окно в несколько шагов (`AdwAlertDialog` закрывается при любом ответе): заголовок, пояснение,
/// содержимое, ошибка и кнопки «Отмена» · действие.
struct StepDialog {
    dialog: adw::Dialog,
    text: gtk::Label,
    body: gtk::Box,
    error: gtk::Label,
    buttons: gtk::Box,
    cancel: gtk::Button,
    confirm: ActionButton,
}

impl StepDialog {
    fn new(title: &str, text: &str, confirm: &str, destructive: bool) -> StepDialog {
        let text_label = gtk::Label::builder().label(text).wrap(true).xalign(0.0).build();
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
        let error = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        error.add_css_class("error");
        let cancel = gtk::Button::builder().label(tr("Cancel")).hexpand(true).build();
        cancel.add_css_class("pill");
        let confirm = ActionButton::new(confirm, !destructive);
        confirm.button.set_hexpand(true);
        confirm.button.set_width_request(-1);
        if destructive {
            confirm.button.add_css_class("destructive-action");
        }
        let buttons = gtk::Box::builder().spacing(12).homogeneous(true).margin_top(6).build();
        buttons.append(&cancel);
        buttons.append(&confirm.button);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(6)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        content.append(&text_label);
        content.append(&body);
        content.append(&error);
        content.append(&buttons);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&content));
        let dialog = adw::Dialog::builder().title(title).content_width(420).child(&toolbar).build();
        let weak = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
        });
        StepDialog { dialog, text: text_label, body, error, buttons, cancel, confirm }
    }
}

/// Действие с устройствами: сначала без пароля; если сервер просит его (устройство новое на
/// аккаунте, DESIGN §4.8) — окно спрашивает пароль и пробует снова. Ответ `Some(текст)` — плашка.
fn password_confirm(window: &MainWindow, title: &str, text: &str, confirm: &str, action: Action) {
    let step = StepDialog::new(title, text, confirm, true);
    let password = adw::PasswordEntryRow::builder().title(tr("AccountPassword")).build();
    let list = boxed_list(&[password.upcast_ref()]);
    list.set_visible(false);
    step.body.append(&list);
    let needs_password = Rc::new(Cell::new(false));
    let (confirm, needs) = (step.confirm.clone(), Rc::clone(&needs_password));
    password.connect_changed(move |entry| confirm.button.set_sensitive(!needs.get() || !entry.text().is_empty()));
    let run: Rc<dyn Fn()> = {
        let (weak, weak_dialog, confirm, list, password, error) =
            (window.downgrade(), step.dialog.downgrade(), step.confirm.clone(), list.clone(), password.clone(), step.error.clone());
        Rc::new(move || {
            let (Some(window), Some(dialog)) = (weak.upgrade(), weak_dialog.upgrade()) else { return };
            if confirm.busy() {
                return;
            }
            confirm.set_busy(true);
            show_error(&error, None);
            let future = action(&window, needs_password.get().then(|| password.text().to_string()));
            let (weak, confirm, list, error, needs) =
                (window.downgrade(), confirm.clone(), list.clone(), error.clone(), Rc::clone(&needs_password));
            let dialog = dialog.downgrade();
            glib::spawn_future_local(async move {
                let result = future.await;
                confirm.set_busy(false);
                let (Some(window), Some(dialog)) = (weak.upgrade(), dialog.upgrade()) else { return };
                match result {
                    Some(Ok(message)) => {
                        dialog.close();
                        if let Some(message) = message {
                            window.toast(&message);
                        }
                        window.account_view.notify_devices();
                    }
                    Some(Err(e)) => {
                        if e.code == "recent_device_restricted" && !needs.get() {
                            needs.set(true);
                            list.set_visible(true);
                            confirm.button.set_sensitive(false);
                            if let Some(label) = confirm.stack.child_by_name("label").and_downcast::<gtk::Label>() {
                                label.set_label(tr("AccountPasswordConfirm"));
                            }
                        }
                        show_error(&error, Some(error_text(&e)));
                    }
                    None => {
                        dialog.close();
                    }
                }
            });
        })
    };
    let r = Rc::clone(&run);
    step.confirm.button.connect_clicked(move |_| r());
    let r = Rc::clone(&run);
    password.connect_entry_activated(move |_| r());
    let _ = (&step.cancel, &step.buttons, &step.text);
    step.dialog.present(Some(&window.window));
}

// ── «Добавить устройство» (задание Windows 0006 §2) ──

/// Новое устройство (часы, где неудобно набирать пароль) показывает код `K7QX-M2PD`; здесь его
/// вводят, видят, какое устройство просится, и выбирают число, которое видно на нём.
pub fn link_device_dialog(window: &MainWindow) {
    let step = Rc::new(StepDialog::new(tr("AccountAddDevice"), tr("LinkCodeText"), tr("LinkContinue"), false));
    let code = adw::EntryRow::builder().title(tr("LinkCodeHeader")).build();
    step.body.append(&boxed_list(&[code.upcast_ref()]));
    let resolve: Rc<dyn Fn()> = {
        let (weak, step_ref, code) = (window.downgrade(), Rc::clone(&step), code.clone());
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let step = Rc::clone(&step_ref);
            if step.confirm.busy() {
                return;
            }
            let Some(normalized) = devices::normalize_user_code(&code.text()) else {
                show_error(&step.error, Some(tr("LinkInvalidCode")));
                return;
            };
            code.set_text(&normalized);
            step.confirm.set_busy(true);
            show_error(&step.error, None);
            let account = Arc::clone(&window.ctx.services.account);
            let task = window.ctx.services.run(async move { account.resolve_link(&normalized).await });
            let weak = window.downgrade();
            glib::spawn_future_local(async move {
                let result = task.await;
                step.confirm.set_busy(false);
                let (Some(window), Some(result)) = (weak.upgrade(), result) else { return };
                match result {
                    Ok(details) => show_link_device(&window, &step, &details),
                    Err(e) => show_error(&step.error, Some(link_error(&e))),
                }
            });
        })
    };
    let r = Rc::clone(&resolve);
    step.confirm.button.connect_clicked(move |_| r());
    let r = Rc::clone(&resolve);
    code.connect_entry_activated(move |_| r());
    step.dialog.present(Some(&window.window));
    code.grab_focus();
}

fn link_error(error: &ApiError) -> &'static str {
    match error.code.as_str() {
        "link_not_found" => tr("LinkNotFound"),
        "link_expired" | "link_cancelled" | "link_not_claimed" => tr("LinkExpired"),
        "link_verify_mismatch" => tr("LinkVerifyMismatch"),
        "link_already_claimed" => tr("LinkAlreadyClaimed"),
        "link_wrong_mode" => tr("LinkWrongMode"),
        _ => error_text(error),
    }
}

/// Какое устройство просится: значок, имя, модель и система, та же ли сеть, сколько ещё действует
/// код; три числа и «Отклонить».
fn show_link_device(window: &MainWindow, step: &Rc<StepDialog>, link: &LinkDetails) {
    while let Some(child) = step.body.first_child() {
        step.body.remove(&child);
    }
    step.text.set_visible(false);
    let device = link.device.as_ref();
    let (icon, kind) = device_icon(device.map(|d| d.platform.as_str()));
    let name = device.map(|d| d.name.clone()).unwrap_or_else(|| tr("DeviceOther").to_owned());
    let row = adw::ActionRow::builder().title(glib::markup_escape_text(&name)).build();
    let mut lines = Vec::new();
    let details: Vec<String> = [device.and_then(|d| d.model.clone()), device.and_then(|d| d.os_version.clone())]
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect();
    if !details.is_empty() {
        lines.push(details.join(" · "));
    }
    if let Some(same) = link.same_network {
        lines.push(tr(if same { "LinkSameNetwork" } else { "LinkOtherNetwork" }).to_owned());
    }
    if let Some(expires) = iso::parse(&link.expires_at) {
        let minutes = ((expires - now_ms()) as f64 / 60_000.0).ceil().max(1.0) as i64;
        lines.push(trf("LinkExpiresFormat", &[&minutes]));
    }
    row.set_subtitle(&glib::markup_escape_text(&lines.join("\n")));
    let image = gtk::Image::builder().icon_name(icon).pixel_size(32).build();
    image.update_property(&[gtk::accessible::Property::Label(tr(kind))]);
    row.add_prefix(&image);
    step.body.append(&boxed_list(&[row.upcast_ref()]));
    let choose = gtk::Label::builder().label(tr("LinkChooseNumber")).wrap(true).margin_top(6).build();
    step.body.append(&choose);
    let numbers = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
    for choice in &link.verify_choices {
        let label = gtk::Label::new(Some(choice));
        label.add_css_class("title-1");
        let button = gtk::Button::builder().child(&label).width_request(80).height_request(64).build();
        button.update_property(&[gtk::accessible::Property::Label(choice)]);
        let (weak, step, link, choice) = (window.downgrade(), Rc::clone(step), link.clone(), choice.clone());
        button.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                decide(&window, &step, &link, Some(&choice));
            }
        });
        numbers.append(&button);
    }
    step.body.append(&numbers);
    // Вместо «Продолжить» — «Отклонить».
    step.buttons.remove(&step.confirm.button);
    let deny = gtk::Button::builder().label(tr("LinkDeny")).hexpand(true).build();
    deny.add_css_class("pill");
    deny.add_css_class("destructive-action");
    let (weak, step_ref, link) = (window.downgrade(), Rc::clone(step), link.clone());
    deny.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            decide(&window, &step_ref, &link, None);
        }
    });
    step.buttons.append(&deny);
}

/// Число выбрано (или «Отклонить»): итог — плашкой, окно закрывается.
fn decide(window: &MainWindow, step: &Rc<StepDialog>, link: &LinkDetails, verify: Option<&str>) {
    if !step.body.is_sensitive() {
        return;
    }
    step.body.set_sensitive(false);
    let account = Arc::clone(&window.ctx.services.account);
    let (link_id, verify) = (link.link_id.clone(), verify.map(str::to_owned));
    let approve = verify.is_some();
    let task = window.ctx.services.run(async move {
        match verify {
            Some(code) => account.approve_link(&link_id, &code).await,
            None => account.deny_link(&link_id).await,
        }
    });
    let name = link.device.as_ref().map(|d| d.name.clone()).unwrap_or_else(|| tr("DeviceOther").to_owned());
    let (weak, step) = (window.downgrade(), Rc::clone(step));
    glib::spawn_future_local(async move {
        let result = task.await;
        step.body.set_sensitive(true);
        let (Some(window), Some(result)) = (weak.upgrade(), result) else { return };
        match result {
            Ok(_) if approve => {
                step.dialog.close();
                window.toast(&trf("LinkApprovedFormat", &[&name]));
                window.account_view.notify_devices();
            }
            Ok(_) => {
                step.dialog.close();
                window.toast(tr("LinkDenied"));
            }
            Err(e) if e.code == "device_limit_reached" => {
                step.dialog.close();
                window.toast(tr("AccountErrorDeviceLimit"));
            }
            Err(e) => {
                show_error(&step.error, Some(link_error(&e)));
                if e.code == "link_verify_mismatch" {
                    // После «число не совпало» выбирать больше нечего: остаётся только закрыть.
                    step.body.set_visible(false);
                    while let Some(child) = step.buttons.first_child() {
                        step.buttons.remove(&child);
                    }
                    step.cancel.set_label(tr("Close"));
                    step.buttons.append(&step.cancel);
                }
            }
        }
    });
}

// ── сервер ──

/// Сервер (REWRITE §3.5.12, Android `ServerScreen`): адрес по правилам API §7.1, проверка
/// `/server/info` и «Подключить», который выходит из аккаунта прежнего сервера.
pub fn server_page(window: &MainWindow) -> adw::NavigationPage {
    let f = form(tr("ServerTitle"), Some(tr("ServerText")));
    let account = &window.ctx.services.account;
    let address = adw::EntryRow::builder().title(tr("ServerAddress")).text(account.server_url()).build();
    address.set_input_purpose(gtk::InputPurpose::Url);
    f.body.append(&boxed_list(&[address.upcast_ref()]));
    let hint = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
    hint.add_css_class("caption");
    f.body.append(&hint);
    let info = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
    info.add_css_class("dim-label");
    f.body.append(&info);
    f.body.append(&f.error);
    let check = ActionButton::new(tr("ServerCheck"), false);
    let connect = ActionButton::new(tr("ServerConnect"), true);
    // Две кнопки в ряд: без общей ширины 200 — иначе окно в 360 px не вмещает их.
    let buttons = gtk::Box::builder().spacing(12).homogeneous(true).halign(gtk::Align::Center).build();
    for action in [&check, &connect] {
        action.button.set_width_request(-1);
        action.button.set_hexpand(true);
        buttons.append(&action.button);
    }
    f.body.append(&buttons);
    let switch_note = gtk::Label::builder().label(tr("ServerSwitchSignsOut")).xalign(0.0).wrap(true).build();
    switch_note.add_css_class("dim-label");
    switch_note.add_css_class("caption");
    f.body.append(&switch_note);
    let default = gtk::Button::builder().label(tr("ServerDefault")).halign(gtk::Align::Center).build();
    default.add_css_class("flat");
    f.body.append(&default);

    let checked: Rc<RefCell<Option<(String, melogold_server::dto::ServerInfo)>>> = Rc::default();
    let update: Rc<dyn Fn()> = {
        let (weak, address, hint, info, check, connect, switch_note, default, checked) = (
            window.downgrade(),
            address.clone(),
            hint.clone(),
            info.clone(),
            check.clone(),
            connect.clone(),
            switch_note.clone(),
            default.clone(),
            Rc::clone(&checked),
        );
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let account = &window.ctx.services.account;
            let parsed = server_address::normalize(&address.text());
            let hint_text = match &parsed {
                Err("empty") => None,
                Err("https_required") => Some(tr("ServerHttpsNeeded")),
                Err("credentials_or_params") => Some(tr("ServerErrorParams")),
                Err("unsupported_scheme") => Some(tr("ServerErrorScheme")),
                Err(_) => Some(tr("ServerErrorMalformed")),
                Ok(a) if a.insecure => Some(tr("ServerInsecure")),
                Ok(_) => None,
            };
            hint.set_label(hint_text.unwrap_or_default());
            hint.set_visible(hint_text.is_some());
            if parsed.is_err() {
                hint.add_css_class("error");
            } else {
                hint.remove_css_class("error");
            }
            let normalized = parsed.ok().map(|a| a.url);
            let current = normalized.as_deref() == Some(account.server_url().as_str());
            let busy = check.busy() || connect.busy();
            check.button.set_sensitive(!busy && normalized.is_some());
            connect.button.set_sensitive(!busy && normalized.is_some() && !current);
            if let Some(label) = connect.stack.child_by_name("label").and_downcast::<gtk::Label>() {
                label.set_label(tr(if current { "ServerCurrent" } else { "ServerConnect" }));
            }
            switch_note.set_visible(!current && account.session().is_some());
            default.set_visible(address.text().trim() != DEFAULT_SERVER_URL);
            match checked.borrow().as_ref() {
                Some((url, server)) if Some(url) == normalized.as_ref() => {
                    let registration = tr(match server.registration.as_str() {
                        "open" => "ServerRegistrationOpen",
                        "first" => "ServerRegistrationFirst",
                        _ => "ServerRegistrationClosed",
                    });
                    info.set_label(&format!("{}\n{registration}", trf("ServerInfoFormat", &[&server.instance_name, &server.version])));
                    info.set_visible(true);
                }
                _ => info.set_visible(false),
            }
        })
    };
    update();
    let run_check: Rc<dyn Fn(bool)> = {
        let (weak, address, error, check, connect, checked, update) =
            (window.downgrade(), address.clone(), f.error.clone(), check.clone(), connect.clone(), Rc::clone(&checked), Rc::clone(&update));
        Rc::new(move |do_connect: bool| {
            let Some(window) = weak.upgrade() else { return };
            let Ok(parsed) = server_address::normalize(&address.text()) else { return };
            if check.busy() || connect.busy() {
                return;
            }
            let button = if do_connect { connect.clone() } else { check.clone() };
            button.set_busy(true);
            show_error(&error, None);
            update();
            let account = Arc::clone(&window.ctx.services.account);
            let url = parsed.url.clone();
            let task = window.ctx.services.run(async move {
                let info = account.check(&url).await?;
                if do_connect {
                    account.set_server(&url).await;
                }
                Ok::<_, ApiError>(info)
            });
            let (weak, error, checked, update, url) =
                (window.downgrade(), error.clone(), Rc::clone(&checked), Rc::clone(&update), parsed.url);
            glib::spawn_future_local(async move {
                let result = task.await;
                button.set_busy(false);
                let Some(window) = weak.upgrade() else { return };
                match result {
                    Some(Ok(info)) => {
                        checked.replace(Some((url.clone(), info)));
                        if do_connect {
                            window.ctx.settings.set(&melogold_core::settings::keys::SERVER_URL, (url != DEFAULT_SERVER_URL).then_some(url));
                            window.go_back();
                        }
                    }
                    Some(Err(e)) => {
                        checked.replace(None);
                        let text = match e.code.as_str() {
                            "not_melogold" => tr("ServerNotMelogold"),
                            "client_outdated" | "server_outdated" => error_text(&e),
                            _ => tr("ServerUnreachable"),
                        };
                        show_error(&error, Some(text));
                    }
                    None => {}
                }
                update();
            });
        })
    };
    let u = Rc::clone(&update);
    let (checked_ref, error_ref) = (Rc::clone(&checked), f.error.clone());
    address.connect_changed(move |_| {
        checked_ref.replace(None);
        show_error(&error_ref, None);
        u();
    });
    let r = Rc::clone(&run_check);
    address.connect_entry_activated(move |_| r(false));
    let r = Rc::clone(&run_check);
    check.button.connect_clicked(move |_| r(false));
    let r = Rc::clone(&run_check);
    connect.button.connect_clicked(move |_| r(true));
    let address_ref = address.clone();
    default.connect_clicked(move |_| address_ref.set_text(DEFAULT_SERVER_URL));
    // Проверить сразу: видно, какой сервер и открыта ли регистрация.
    let r = Rc::clone(&run_check);
    f.page.connect_shown(move |_| r(false));
    f.page
}
