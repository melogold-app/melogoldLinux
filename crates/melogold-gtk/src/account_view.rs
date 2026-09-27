//! Аккаунт в окне: экраны подписаны на состояние входа, синхронизации и список устройств и
//! перерисовываются сами; сервер закончил сессию — плашка «Нужно войти снова» с «Войти».

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_server::account::AccountState;

use crate::localization::tr;
use crate::window::MainWindow;

#[derive(Default)]
pub struct AccountView {
    listeners: RefCell<Vec<Weak<dyn Fn()>>>,
    devices: RefCell<Vec<Weak<dyn Fn()>>>,
}

impl AccountView {
    /// Экран, который обновляется при смене входа и статуса синхронизации; живёт, пока жив `refresh`.
    pub fn listen(&self, refresh: &Rc<dyn Fn()>) {
        self.listeners.borrow_mut().push(Rc::downgrade(refresh));
    }

    /// Экран со списком устройств: обновляется по `devices.updated` и после своих действий.
    pub fn listen_devices(&self, refresh: &Rc<dyn Fn()>) {
        self.devices.borrow_mut().push(Rc::downgrade(refresh));
    }

    pub fn notify(&self) {
        call(&self.listeners);
    }

    pub fn notify_devices(&self) {
        call(&self.devices);
    }
}

fn call(listeners: &RefCell<Vec<Weak<dyn Fn()>>>) {
    let alive: Vec<Rc<dyn Fn()>> = {
        let mut list = listeners.borrow_mut();
        list.retain(|l| l.strong_count() > 0);
        list.iter().filter_map(Weak::upgrade).collect()
    };
    for refresh in alive {
        refresh();
    }
}

impl MainWindow {
    pub fn start_account(&self) {
        let services = &self.ctx.services;
        let (changes, weak) = (services.account_changes.clone(), self.downgrade());
        glib::spawn_future_local(async move {
            while let Ok(state) = changes.recv().await {
                let Some(window) = weak.upgrade() else { break };
                window.account_view.notify();
                if let AccountState::AuthRequired { .. } = state {
                    window.auth_required();
                }
            }
        });
        let (status, weak) = (services.sync_status.clone(), self.downgrade());
        glib::spawn_future_local(async move {
            while status.recv().await.is_ok() {
                let Some(window) = weak.upgrade() else { break };
                window.account_view.notify();
            }
        });
        let (devices, weak) = (services.devices_changes.clone(), self.downgrade());
        glib::spawn_future_local(async move {
            while devices.recv().await.is_ok() {
                let Some(window) = weak.upgrade() else { break };
                window.account_view.notify_devices();
            }
        });
        // Сеть вернулась — синхронизация начинается заново; пропала — «Нет связи с сервером».
        let monitor = gio::NetworkMonitor::default();
        let sync = Arc::clone(&services.sync);
        let handle = services.handle();
        monitor.connect_network_changed(move |_, available| {
            let sync = Arc::clone(&sync);
            let _guard = handle.enter();
            sync.network_changed(available);
        });
    }

    /// Сервер закончил сессию (отзыв устройства, смена пароля): войти снова, библиотека на месте.
    fn auth_required(&self) {
        let toast = adw::Toast::builder().title(tr("AccountAuthRequiredTitle")).button_label(tr("AccountSignIn")).timeout(0).build();
        let weak = self.downgrade();
        toast.connect_button_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.open_sign_in();
            }
        });
        self.add_toast(toast);
    }

    /// «Войти» из любого места: раздел Настроек и страница входа в нём.
    pub fn open_sign_in(&self) {
        self.show_tab(melogold_core::settings::Tab::Settings);
        let nav = self.nav(melogold_core::settings::Tab::Settings);
        nav.push(&crate::pages::account::sign_in_page(self));
    }
}
