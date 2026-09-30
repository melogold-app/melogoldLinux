//! Вход по коду (задание 0008, API §4.6, Android `LinkCodeScreens.kt`).
//!
//! * [`sign_in_by_code_page`] — «Вход по коду» на новом устройстве: показывает свой код или принимает код
//!   другого устройства, затем крупно число, которое нужно выбрать там. Логика — `melogold_server::linking`.
//! * [`show_invite`] — «Показать код для нового устройства» в окне «Добавить устройство» (вошедшее устройство).

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::devices::{countdown_text, normalize_user_code, spoken_code};
use melogold_core::text::now_ms;
use melogold_server::linking::{
    AccountInvitePort, AccountLinkPort, InviteLinker, InviteState, LinkFailure, NewDeviceLinkState, NewDeviceLinker,
};

use super::account::{back_to_root, boxed_list, device_icon, form, show_error, show_link_device, ActionButton, StepDialog};
use crate::localization::{tr, trf};
use crate::window::{MainWindow, WeakWindow};

// ── общие куски ──

/// Ошибка словами (таблица §2.3 задания). `claiming`: код набран здесь, а не показан этим устройством.
pub fn failure_text(failure: LinkFailure, claiming: bool) -> &'static str {
    tr(match failure {
        LinkFailure::Denied => "LinuxLinkErrorDenied",
        LinkFailure::Expired if claiming => "LinuxLinkErrorClaimExpired",
        LinkFailure::Expired => "LinuxLinkErrorExpired",
        LinkFailure::Cancelled => "LinuxLinkErrorCancelled",
        LinkFailure::DeviceLimit => "AccountErrorDeviceLimit",
        LinkFailure::NotFound => "LinkNotFound",
        LinkFailure::AlreadyClaimed => "LinkAlreadyClaimed",
        LinkFailure::WrongMode => "LinuxLinkErrorWrongMode",
        LinkFailure::Throttled => "AccountErrorThrottled",
        LinkFailure::Network => "AccountErrorNetwork",
        LinkFailure::Unknown => "AccountErrorUnknown",
    })
}

/// Тональный контейнер (скругление 18 px) вокруг крупного текста.
fn tonal_card(child: &gtk::Label) -> gtk::Box {
    let card = gtk::Box::builder().hexpand(true).build();
    card.add_css_class("link-code-card");
    child.set_hexpand(true);
    card.append(child);
    card
}

/// Код `K7QX-M2PD` крупно: моноширинный, перенос только после дефиса; для Orca — по знакам.
fn big_code(user_code: &str) -> gtk::Box {
    // Невидимый пробел нулевой ширины после дефиса — единственное место переноса.
    let shown = user_code.replacen('-', "-\u{200b}", 1);
    let label =
        gtk::Label::builder().label(shown).wrap(true).wrap_mode(gtk::pango::WrapMode::Word).justify(gtk::Justification::Center).build();
    label.add_css_class("link-code-text");
    label.update_property(&[gtk::accessible::Property::Label(&trf("LinuxLinkCodeSpokenFormat", &[&spoken_code(user_code)]))]);
    tonal_card(&label)
}

/// Число огромным жирным; читается «Число 47».
fn big_number(number: &str) -> gtk::Box {
    let label = gtk::Label::builder().label(number).justify(gtk::Justification::Center).build();
    label.add_css_class("link-number-text");
    label.update_property(&[gtk::accessible::Property::Label(&trf("LinuxLinkNumberFormat", &[&number]))]);
    tonal_card(&label)
}

fn dim(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).wrap(true).xalign(0.0).build();
    label.add_css_class("dim-label");
    label
}

fn centered(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).wrap(true).justify(gtk::Justification::Center).build();
    label.add_css_class("dim-label");
    label
}

fn error_label(text: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(text).wrap(true).xalign(0.0).build();
    label.add_css_class("error");
    label
}

fn spinner() -> adw::Spinner {
    adw::Spinner::builder().halign(gtk::Align::Center).width_request(24).height_request(24).build()
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// «Действует 4:32»; счётчик не объявляется каждую секунду: это обычная подпись, а не живая область.
fn countdown_label(expires_at: i64) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class("dim-label");
    label.set_visible(true);
    set_countdown(&label, expires_at);
    label
}

fn set_countdown(label: &gtk::Label, expires_at: i64) {
    label.set_label(&trf("LinuxLinkValidForFormat", &[&countdown_text(expires_at - now_ms())]));
}

fn pill(text: &str) -> gtk::Button {
    let button = gtk::Button::builder().label(text).halign(gtk::Align::Center).width_request(200).build();
    button.add_css_class("pill");
    button
}

fn flat(text: &str) -> gtk::Button {
    let button = gtk::Button::builder().label(text).halign(gtk::Align::Center).build();
    button.add_css_class("flat");
    button
}

/// «Выберите это число на «MacBook Air»» со значком одобряющего устройства.
fn approver_header(name: &str, platform: &str) -> gtk::Box {
    let (icon, kind) = device_icon(Some(platform));
    let row = gtk::Box::builder().spacing(12).build();
    let image = gtk::Image::builder().icon_name(icon).pixel_size(32).build();
    image.update_property(&[gtk::accessible::Property::Label(tr(kind))]);
    row.append(&image);
    let text = gtk::Label::builder().label(trf("LinuxLinkVerifyPickFormat", &[&name])).wrap(true).xalign(0.0).hexpand(true).build();
    text.add_css_class("title-3");
    row.append(&text);
    row
}

// ── новое устройство: «Вход по коду» ──

type Linker = NewDeviceLinker<AccountLinkPort>;

struct Controller {
    window: WeakWindow,
    content: gtk::Box,
    /// Без него страница только рисует состояние (снимки окна).
    linker: Option<Linker>,
    /// Набирается код другого устройства (режим `invite`), а не показывается свой.
    claiming: Cell<bool>,
    input: RefCell<String>,
    /// Подпись «Действует …» на экране и конец срока кода.
    countdown: RefCell<Option<(gtk::Label, i64)>>,
}

/// «Вход по коду»: сразу просит у сервера код этого устройства (режим `request`).
pub fn sign_in_by_code_page(window: &MainWindow) -> adw::NavigationPage {
    let linker = NewDeviceLinker::new(AccountLinkPort(Arc::clone(&window.ctx.services.account)), window.ctx.services.handle());
    let (page, controller) = build(window, Some(linker.clone()), false);
    let (sender, receiver) = async_channel::unbounded();
    linker.subscribe(move |state| {
        let _ = sender.try_send(state.clone());
    });
    let weak = Rc::downgrade(&controller);
    glib::spawn_future_local(async move {
        while let Ok(state) = receiver.recv().await {
            let Some(controller) = weak.upgrade() else { break };
            controller.render(&state);
        }
    });
    linker.show_code();
    page
}

/// Страница в заданном состоянии, без сети: для снимков окна.
pub fn preview_page(window: &MainWindow, state: &NewDeviceLinkState, claiming: bool) -> adw::NavigationPage {
    let (page, controller) = build(window, None, claiming);
    controller.render(state);
    page
}

fn build(window: &MainWindow, linker: Option<Linker>, claiming: bool) -> (adw::NavigationPage, Rc<Controller>) {
    let f = form(tr("LinuxLinkCodeTitle"), None);
    let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(16).build();
    f.body.append(&content);
    let controller = Rc::new(Controller {
        window: window.downgrade(),
        content,
        linker,
        claiming: Cell::new(claiming),
        input: RefCell::default(),
        countdown: RefCell::default(),
    });
    // Обратный отсчёт: раз в секунду, пока жива страница.
    let weak = Rc::downgrade(&controller);
    glib::timeout_add_seconds_local(1, move || {
        let Some(controller) = weak.upgrade() else { return glib::ControlFlow::Break };
        if let Some((label, expires_at)) = controller.countdown.borrow().as_ref() {
            set_countdown(label, *expires_at);
        }
        glib::ControlFlow::Continue
    });
    // Уход со страницы (назад, закрытие окна) отдаёт привязку серверу; страница держит контроллер живым.
    let keep = Rc::clone(&controller);
    f.page.connect_hidden(move |_| {
        if let Some(linker) = &keep.linker {
            linker.cancel();
        }
    });
    (f.page, controller)
}

impl Controller {
    fn weak(self: &Rc<Self>) -> Weak<Controller> {
        Rc::downgrade(self)
    }

    fn show_code(&self) {
        self.claiming.set(false);
        if let Some(linker) = &self.linker {
            linker.show_code();
        }
    }

    fn type_code(self: &Rc<Self>) {
        self.claiming.set(true);
        if let Some(linker) = &self.linker {
            linker.cancel();
            self.render(&linker.state());
        }
    }

    fn cancel(&self) {
        if let Some(window) = self.window.upgrade() {
            window.go_back();
        }
    }

    fn render(self: &Rc<Self>, state: &NewDeviceLinkState) {
        clear(&self.content);
        self.countdown.replace(None);
        let claiming = self.claiming.get();
        let field_step = claiming
            && matches!(state, NewDeviceLinkState::Idle | NewDeviceLinkState::Starting | NewDeviceLinkState::Failed { started: false, .. });
        match state {
            NewDeviceLinkState::Verify { verify_code, login, approver_name, approver_platform, expires_at, reconnecting } => {
                self.verify_panel(verify_code, login, approver_name, approver_platform, *expires_at, *reconnecting)
            }
            NewDeviceLinkState::SignedIn => self.working(tr("LinuxLinkSigningIn")),
            _ if field_step => self.claim_step(state),
            NewDeviceLinkState::ShowingCode { user_code, expires_at, reconnecting } => {
                self.code_panel(user_code, *expires_at, *reconnecting)
            }
            NewDeviceLinkState::Failed { failure, started } => self.notice(*failure, *started, claiming),
            _ => self.working(tr("LinuxLinkStarting")),
        }
        if *state == NewDeviceLinkState::SignedIn {
            if let Some(window) = self.window.upgrade() {
                // Сессия взята, как после входа по паролю: страница кода и экран входа под ней закрываются.
                back_to_root(&window);
            }
        }
    }

    /// Ожидание индикатором: «Нет связи…» красным, пока сервер не отвечает.
    fn waiting(&self, reconnecting: bool) {
        if reconnecting {
            let label = gtk::Label::builder().label(tr("LinuxLinkReconnecting")).wrap(true).justify(gtk::Justification::Center).build();
            label.add_css_class("error");
            self.content.append(&label);
        } else {
            self.content.append(&spinner());
        }
    }

    fn working(&self, text: &str) {
        let spin = adw::Spinner::builder().halign(gtk::Align::Center).width_request(32).height_request(32).margin_top(24).build();
        self.content.append(&spin);
        self.content.append(&centered(text));
    }

    fn code_panel(self: &Rc<Self>, user_code: &str, expires_at: i64, reconnecting: bool) {
        self.content.append(&dim(tr("LinuxLinkCodeText")));
        self.content.append(&big_code(user_code));
        self.add_countdown(expires_at);
        self.waiting(reconnecting);
        let cancel = pill(tr("Cancel"));
        let weak = self.weak();
        cancel.connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.cancel();
            }
        });
        self.content.append(&cancel);
        let have = flat(tr("LinuxLinkHaveCode"));
        let weak = self.weak();
        have.connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.type_code();
            }
        });
        self.content.append(&have);
    }

    fn add_countdown(&self, expires_at: i64) {
        let label = countdown_label(expires_at);
        label.set_halign(gtk::Align::Center);
        self.content.append(&label);
        self.countdown.replace(Some((label, expires_at)));
    }

    fn verify_panel(
        self: &Rc<Self>,
        number: &str,
        login: &str,
        approver_name: &str,
        approver_platform: &str,
        expires_at: i64,
        reconnecting: bool,
    ) {
        self.content.append(&approver_header(approver_name, approver_platform));
        self.content.append(&big_number(number));
        self.content.append(&centered(&trf("LinuxLinkVerifyAccountFormat", &[&login])));
        self.add_countdown(expires_at);
        self.waiting(reconnecting);
        let cancel = pill(tr("Cancel"));
        let weak = self.weak();
        cancel.connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.cancel();
            }
        });
        self.content.append(&cancel);
    }

    /// Поле кода другого устройства (режим `invite`); отказ сервера — под полем, ввод не стирается.
    fn claim_step(self: &Rc<Self>, state: &NewDeviceLinkState) {
        self.content.append(&dim(tr("LinuxLinkClaimText")));
        let entry = adw::EntryRow::builder().title(tr("LinkCodeHeader")).text(self.input.borrow().as_str()).build();
        entry.set_input_purpose(gtk::InputPurpose::FreeForm);
        entry.set_enable_emoji_completion(false);
        self.content.append(&boxed_list(&[entry.upcast_ref()]));
        let error = gtk::Label::builder().xalign(0.0).wrap(true).visible(false).build();
        error.add_css_class("error");
        self.content.append(&error);
        if let NewDeviceLinkState::Failed { failure, .. } = state {
            show_error(&error, Some(failure_text(*failure, true)));
        }
        let go = ActionButton::new(tr("LinkContinue"), true);
        go.set_busy(matches!(state, NewDeviceLinkState::Starting));
        go.button.set_sensitive(!go.busy() && normalize_user_code(&entry.text()).is_some());
        self.content.append(&go.button);
        let mine = flat(tr("LinuxLinkShowMyCode"));
        self.content.append(&mine);

        let weak = self.weak();
        let (entry_ref, go_ref) = (entry.clone(), go.clone());
        entry.connect_changed(move |entry| {
            if let Some(controller) = weak.upgrade() {
                controller.input.replace(entry.text().to_string());
            }
            go_ref.button.set_sensitive(!go_ref.busy() && normalize_user_code(&entry.text()).is_some());
        });
        let submit: Rc<dyn Fn()> = {
            let (weak, entry, go, error) = (self.weak(), entry_ref, go.clone(), error.clone());
            Rc::new(move || {
                let Some(controller) = weak.upgrade() else { return };
                if go.busy() {
                    return;
                }
                let Some(code) = normalize_user_code(&entry.text()) else {
                    show_error(&error, Some(tr("LinkInvalidCode")));
                    return;
                };
                entry.set_text(&code);
                controller.input.replace(code.clone());
                if let Some(linker) = &controller.linker {
                    linker.claim(&code);
                }
            })
        };
        let s = Rc::clone(&submit);
        go.button.connect_clicked(move |_| s());
        let s = Rc::clone(&submit);
        entry.connect_entry_activated(move |_| s());
        let weak = self.weak();
        mine.connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.show_code();
            }
        });
        entry.grab_focus();
    }

    /// Привязка кончилась: причина красным, что делать дальше и «Войти по паролю».
    fn notice(self: &Rc<Self>, failure: LinkFailure, started: bool, claiming: bool) {
        self.content.append(&error_label(failure_text(failure, claiming)));
        let primary = ActionButton::new(
            tr(if claiming {
                "LinuxLinkEnterOtherCode"
            } else if started {
                "LinuxLinkNewCode"
            } else {
                "Retry"
            }),
            true,
        );
        self.content.append(&primary.button);
        let weak = self.weak();
        primary.button.connect_clicked(move |_| {
            let Some(controller) = weak.upgrade() else { return };
            if controller.claiming.get() {
                controller.type_code();
            } else {
                controller.show_code();
            }
        });
        let password = flat(tr("LinuxLinkBackToPassword"));
        let weak = self.weak();
        password.connect_clicked(move |_| {
            if let Some(controller) = weak.upgrade() {
                controller.cancel();
            }
        });
        self.content.append(&password);
    }
}

// ── вошедшее устройство: «Показать код для нового устройства» ──

/// Вместо поля кода в окне «Добавить устройство» — код этого устройства (режим `invite`). Когда новое
/// устройство его введёт, окно показывает ту же карточку одобрения, что и для введённого здесь кода.
pub fn show_invite(window: &MainWindow, step: &Rc<StepDialog>) {
    let services = &window.ctx.services;
    let linker = InviteLinker::new(AccountInvitePort(Arc::clone(&services.account)), services.handle());

    // Событие `link.updated` читает приглашение сразу; опрос раз в 3 с идёт и без него.
    let mut updates = services.link_updates.subscribe();
    let watcher = {
        let linker = linker.clone();
        services.handle().spawn(async move {
            while let Ok(id) = updates.recv().await {
                linker.nudge(&id);
            }
        })
    };

    let (sender, receiver) = async_channel::unbounded();
    linker.subscribe(move |state| {
        let _ = sender.try_send(state.clone());
    });
    let weak_window = window.downgrade();
    let step_ref = Rc::clone(step);
    let render_linker = linker.clone();
    glib::spawn_future_local(async move {
        while let Ok(state) = receiver.recv().await {
            let Some(window) = weak_window.upgrade() else { break };
            render_invite(&window, &step_ref, &render_linker, &state);
        }
    });

    // Закрыли окно: без решения приглашение отменяется (их не больше трёх), после решения — нет.
    let (closing, step_for_close) = (linker.clone(), Rc::clone(step));
    step.dialog.connect_closed(move |_| {
        watcher.abort();
        if step_for_close.decided.get() {
            closing.release();
        } else {
            closing.cancel();
        }
    });
    step.error.set_visible(false);
    linker.start();
}

fn render_invite(window: &MainWindow, step: &Rc<StepDialog>, linker: &InviteLinker<AccountInvitePort>, state: &InviteState) {
    clear(&step.body);
    show_error(&step.error, None);
    // В окне всегда одна кнопка «Отмена»; карточка одобрения меняет её на свою пару.
    while let Some(child) = step.buttons.first_child() {
        step.buttons.remove(&child);
    }
    step.buttons.append(&step.cancel);
    step.cancel.set_label(tr("Cancel"));
    step.text.set_visible(false);
    match state {
        InviteState::Idle | InviteState::Starting => {
            step.body.append(&spinner());
            step.body.append(&centered(tr("LinuxLinkStarting")));
        }
        InviteState::Waiting { user_code, expires_at } => {
            step.text.set_label(tr("LinuxLinkInviteText"));
            step.text.set_visible(true);
            step.body.append(&big_code(user_code));
            let countdown = countdown_label(*expires_at);
            countdown.set_halign(gtk::Align::Center);
            step.body.append(&countdown);
            // Отсчёт идёт, пока подпись на экране.
            let (label, expires_at) = (countdown.downgrade(), *expires_at);
            glib::timeout_add_seconds_local(1, move || match label.upgrade().filter(|l| l.root().is_some()) {
                Some(label) => {
                    set_countdown(&label, expires_at);
                    glib::ControlFlow::Continue
                }
                None => glib::ControlFlow::Break,
            });
            step.body.append(&spinner());
            step.body.append(&centered(tr("LinuxLinkWaitingNew")));
        }
        InviteState::Claimed(details) => {
            // Та же карточка, что и для кода, введённого здесь: число выбирает человек, глядя на новое устройство.
            if step.confirm.button.parent().is_none() {
                step.buttons.append(&step.confirm.button);
            }
            step.body.set_sensitive(true);
            show_link_device(window, step, details);
        }
        InviteState::Failed { failure, started } => {
            step.body.append(&error_label(invite_error_text(*failure, *started)));
            let again = ActionButton::new(tr(if *started { "LinuxLinkShowCode" } else { "Retry" }), true);
            let linker = linker.clone();
            again.button.connect_clicked(move |_| linker.start());
            step.body.append(&again.button);
        }
    }
}

/// «Показать код»: `link_expired` → «Код устарел. Покажите новый», отмена → «Приглашение отменено».
fn invite_error_text(failure: LinkFailure, started: bool) -> &'static str {
    tr(match failure {
        LinkFailure::Expired | LinkFailure::NotFound => "LinuxLinkInviteExpired",
        LinkFailure::Cancelled | LinkFailure::Denied if started => "LinuxLinkInviteCancelled",
        LinkFailure::DeviceLimit => "AccountErrorDeviceLimit",
        LinkFailure::Throttled => "AccountErrorThrottled",
        LinkFailure::Network => "AccountErrorNetwork",
        _ => "AccountErrorUnknown",
    })
}

/// Окно «Добавить устройство» с кодом для нового устройства (снимки окна, без сети).
pub fn invite_preview(window: &MainWindow, user_code: &str, expires_at: i64) {
    let step = Rc::new(StepDialog::new(tr("AccountAddDevice"), tr("LinuxLinkInviteText"), tr("LinkContinue"), false));
    let linker = InviteLinker::new(AccountInvitePort(Arc::clone(&window.ctx.services.account)), window.ctx.services.handle());
    render_invite(window, &step, &linker, &InviteState::Waiting { user_code: user_code.to_owned(), expires_at });
    step.dialog.present(Some(&window.window));
}

/// Карточка одобрения для снимков: как приходит от `GET /auth/me/links/{id}`.
pub fn invite_claimed_preview(window: &MainWindow, details: &melogold_server::dto::LinkDetails) {
    let step = Rc::new(StepDialog::new(tr("AccountAddDevice"), tr("LinuxLinkInviteText"), tr("LinkContinue"), false));
    let linker = InviteLinker::new(AccountInvitePort(Arc::clone(&window.ctx.services.account)), window.ctx.services.handle());
    render_invite(window, &step, &linker, &InviteState::Claimed(Box::new(details.clone())));
    step.dialog.present(Some(&window.window));
}
