//! Выбор устройства и всё, что окно делает по событиям пульта (задание 0011):
//!
//! * кнопка «Устройство» в панели плеера и пункт главного меню открывают лист: «Это устройство» и
//!   «Другие устройства» (`GET /playback/devices`: в сети или нет, что играет, громкость);
//! * команды другого устройства выполняются здесь (громкость живёт в главном потоке);
//! * ответы пульта («не в сети», «управление выключено») и «Управляет «…»» — плашками.
//!
//! Кнопка и пункт меню есть только у вошедшего пользователя на сервере с `features.remote`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_server::dto::RemoteDevice;
use melogold_server::remote::{Incoming, RemoteNotice};

use crate::localization::{tr, trf};
use crate::pages::account::device_icon;
use crate::window::MainWindow;

pub fn install(window: &MainWindow) {
    let services = &window.ctx.services;

    // Действие меню и кнопка в панели плеера.
    let action = gio::SimpleAction::new("remote-devices", None);
    action.set_enabled(false);
    let weak = window.downgrade();
    action.connect_activate(move |_, _| {
        if let Some(window) = weak.upgrade() {
            present(&window);
        }
    });
    window.window.add_action(&action);

    let button = gtk::Button::builder()
        .icon_name("view-dual-symbolic")
        .tooltip_text(tr("LinuxRemoteDevice"))
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    button.add_css_class("flat");
    button.update_property(&[gtk::accessible::Property::Label(tr("LinuxRemoteDevice"))]);
    button.set_action_name(Some("win.remote-devices"));
    let bar = window.player_bar();
    if let Some(actions) = bar.volume_button.parent().and_downcast::<gtk::Box>() {
        actions.insert_child_after(&button, Some(&bar.volume_button));
    }

    // Доступность: вошли и сервер умеет `remote`. Проверяется при смене входа.
    let available = Rc::new(Cell::new(false));
    let refresh: Rc<dyn Fn()> = {
        let (weak, action, button, available) = (window.downgrade(), action.clone(), button.clone(), Rc::clone(&available));
        Rc::new(move || {
            let Some(window) = weak.upgrade() else { return };
            let account = Arc::clone(&window.ctx.services.account);
            let task = window.ctx.services.run(async move {
                account.session().is_some() && account.ensure_server_info().await.is_some_and(|info| info.features.remote.is_some())
            });
            let (weak, action, button, available) = (window.downgrade(), action.clone(), button.clone(), Rc::clone(&available));
            glib::spawn_future_local(async move {
                let ok = task.await.unwrap_or(false);
                available.set(ok);
                action.set_enabled(ok);
                button.set_visible(ok);
                if !ok {
                    if let Some(window) = weak.upgrade() {
                        window.ctx.services.remote.control.disconnect();
                    }
                }
            });
        })
    };
    refresh();
    window.account_view.listen(&refresh);
    let keep = std::cell::RefCell::new(Some(refresh));
    window.window.connect_destroy(move |_| {
        keep.take();
    });

    // Команды другого устройства этому.
    let (incoming, weak) = (services.remote.incoming.clone(), window.downgrade());
    glib::spawn_future_local(async move {
        while let Ok(command) = incoming.recv().await {
            let Some(window) = weak.upgrade() else { break };
            execute(&window, &command);
        }
    });
    // Что сказать человеку.
    let (notices, weak) = (services.remote.notices.clone(), window.downgrade());
    glib::spawn_future_local(async move {
        while let Ok(notice) = notices.recv().await {
            let Some(window) = weak.upgrade() else { break };
            window.toast(&match &notice {
                RemoteNotice::Offline(name) => trf("LinuxRemoteOfflineFormat", &[name]),
                RemoteNotice::Disabled(name) => trf("LinuxRemoteDisabledFormat", &[name]),
                RemoteNotice::Failed => tr("AccountErrorUnknown").to_owned(),
            });
        }
    });
    let (controlled, weak) = (services.remote.controlled_by.clone(), window.downgrade());
    glib::spawn_future_local(async move {
        while let Ok(name) = controlled.recv().await {
            let Some(window) = weak.upgrade() else { break };
            window.toast(&trf("LinuxRemoteControlledByFormat", &[&name]));
        }
    });
}

/// Выполнить команду другого устройства своим плеером: мимо перехватчика, он — для команд этого окна.
fn execute(window: &MainWindow, incoming: &Incoming) {
    let player = &window.ctx.services.player;
    if let Incoming::Volume(percent) = incoming {
        // Громкость приложения: ползунок, настройка и плеер разом.
        window.player_bar().set_volume(f64::from(*percent) / 100.0);
        return;
    }
    for command in crate::remote::engine_commands(incoming) {
        player.send_local(command);
    }
}

/// Лист «Устройство».
pub fn present(window: &MainWindow) {
    open(window, None);
}

/// Лист с готовым списком устройств, без сети (снимки окна).
pub fn preview(window: &MainWindow, devices: Vec<RemoteDevice>) {
    open(window, Some(devices));
}

fn open(window: &MainWindow, given: Option<Vec<RemoteDevice>>) {
    let control = window.ctx.services.remote.control.clone();
    let view = control.view();

    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(6)
        .margin_bottom(24)
        .margin_start(12)
        .margin_end(12)
        .build();
    let dialog = adw::Dialog::builder().title(tr("LinuxRemoteDevice")).content_width(440).content_height(480).build();

    // «Это устройство».
    let this_group = adw::PreferencesGroup::new();
    let this_row = adw::ActionRow::builder()
        .title(tr("LinuxRemoteThisDevice"))
        .subtitle(melogold_core::system::device_name())
        .activatable(true)
        .build();
    this_row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
    let this_check = gtk::Image::from_icon_name("object-select-symbolic");
    this_check.set_visible(view.target.is_none());
    this_row.add_suffix(&this_check);
    this_group.add(&this_row);
    list.append(&this_group);
    let (weak_dialog, control_this) = (dialog.downgrade(), control.clone());
    this_row.connect_activated(move |_| {
        control_this.disconnect();
        if let Some(dialog) = weak_dialog.upgrade() {
            dialog.close();
        }
    });

    // «Другие устройства»: читаются с сервера.
    let others = adw::PreferencesGroup::builder().title(tr("LinuxRemoteOthers")).build();
    let spinner =
        adw::Spinner::builder().halign(gtk::Align::Center).margin_top(12).margin_bottom(12).width_request(28).height_request(28).build();
    others.add(&spinner);
    list.append(&others);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(520)
        .child(&list)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(&window.window));

    let control_load = control.clone();
    let task = window.ctx.services.run(async move {
        match given {
            Some(devices) => Ok(devices),
            None => control_load.devices().await,
        }
    });
    let (weak, weak_dialog, target) = (window.downgrade(), dialog.downgrade(), view.target.map(|t| t.device_id));
    glib::spawn_future_local(async move {
        let result = task.await;
        others.remove(&spinner);
        let (Some(window), Some(dialog)) = (weak.upgrade(), weak_dialog.upgrade()) else { return };
        match result {
            Some(Ok(devices)) if !devices.is_empty() => {
                for device in devices {
                    others.add(&device_row(&window, &dialog, &device, target.as_deref() == Some(device.device_id.as_str())));
                }
            }
            Some(Ok(_)) => others.add(&dim_row(tr("LinuxRemoteNoDevices"))),
            _ => others.add(&dim_row(tr("AccountErrorNetwork"))),
        }
    });
}

fn dim_row(text: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(text).sensitive(false).build();
    row.add_css_class("dim-label");
    row
}

/// Устройство: значок и имя, «В сети» или «Не в сети», что там играет и громкость; не в сети — неактивно.
fn device_row(window: &MainWindow, dialog: &adw::Dialog, device: &RemoteDevice, current: bool) -> adw::ActionRow {
    let mut parts = vec![tr(if device.online { "LinuxRemoteOnline" } else { "LinuxRemoteOffline" }).to_owned()];
    if device.online && !device.controllable {
        parts.push(tr("LinuxRemoteControlOff").to_owned());
    }
    if device.online {
        if let Some(track) = device.playing.as_ref().and_then(|p| p.track.as_ref()) {
            let artist = track.artists_text.as_deref().filter(|a| !a.is_empty());
            parts.push(match artist {
                Some(artist) => format!("{artist} — {}", track.title),
                None => track.title.clone(),
            });
        }
        if let Some(volume) = device.volume.or_else(|| device.playing.as_ref().and_then(|p| p.volume)) {
            parts.push(format!("{volume} %"));
        }
    }
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&device.name))
        .subtitle(glib::markup_escape_text(&parts.join(" · ")))
        .activatable(device.online && device.controllable)
        .sensitive(device.online && device.controllable)
        .build();
    let (icon, kind) = device_icon(Some(&device.platform));
    let image = gtk::Image::from_icon_name(icon);
    image.update_property(&[gtk::accessible::Property::Label(tr(kind))]);
    row.add_prefix(&image);
    if current {
        row.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
    }
    let (weak, weak_dialog, device) = (window.downgrade(), dialog.downgrade(), device.clone());
    row.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            // Свой плеер не играет вместе с чужим: пока устройство выбрано, здесь пауза.
            window.ctx.services.player.send_local(melogold_playback::engine::Command::Pause);
            window.ctx.services.remote.control.connect(&device);
        }
        if let Some(dialog) = weak_dialog.upgrade() {
            dialog.close();
        }
    });
    row
}
