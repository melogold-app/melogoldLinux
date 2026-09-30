//! Фильтр «Чьи прослушивания показать» для Истории и Итогов (задания Windows 0002 §5, Linux 0012).
//!
//! «Это устройство» — с его id на сервере, остальные — по именам из списка устройств аккаунта.
//! Удалённое из аккаунта устройство не показывается: его прослушивания видны в «Все устройства».
//! Фильтр виден с аккаунтом, когда есть хотя бы одно другое устройство.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::devices::filter_devices;
use melogold_data::DeviceFilter;

use crate::localization::tr;
use crate::window::MainWindow;

#[derive(Clone)]
pub struct DeviceFilterBox {
    pub dropdown: gtk::DropDown,
    names: gtk::StringList,
    filters: Rc<RefCell<Vec<DeviceFilter>>>,
    /// Пока список перестраивается, выбор не считается действием пользователя.
    busy: Rc<Cell<bool>>,
}

impl DeviceFilterBox {
    pub fn new() -> DeviceFilterBox {
        let names = gtk::StringList::new(&[tr("HistoryDeviceAll"), tr("HistoryDeviceThis")]);
        let dropdown = gtk::DropDown::builder()
            .model(&names)
            .valign(gtk::Align::Center)
            .visible(false)
            .tooltip_text(tr("HistoryDeviceChoose"))
            .build();
        dropdown.update_property(&[gtk::accessible::Property::Label(tr("HistoryDeviceChoose"))]);
        let filters = Rc::new(RefCell::new(vec![DeviceFilter::All, DeviceFilter::This(None)]));
        DeviceFilterBox { dropdown, names, filters, busy: Rc::new(Cell::new(false)) }
    }

    pub fn current(&self) -> DeviceFilter {
        self.filters.borrow().get(self.dropdown.selected() as usize).cloned().unwrap_or_default()
    }

    /// Выбор пользователя (не перестройка списка).
    pub fn connect_changed(&self, changed: impl Fn() + 'static) {
        let busy = Rc::clone(&self.busy);
        self.dropdown.connect_selected_notify(move |_| {
            if !busy.get() {
                changed();
            }
        });
    }

    /// Список устройств сейчас, при смене входа и по `devices.updated`; `changed` — когда выбранное
    /// устройство пропало и фильтр вернулся к «Все устройства». Живёт, пока жива `page`.
    pub fn attach(&self, window: &MainWindow, page: &impl IsA<gtk::Widget>, changed: Rc<dyn Fn()>) {
        let (weak, this) = (window.downgrade(), self.clone());
        let reload: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(window) = weak.upgrade() {
                this.reload(&window, Rc::clone(&changed));
            }
        });
        window.account_view.listen(&reload);
        window.account_view.listen_devices(&reload);
        reload();
        let keep = RefCell::new(Some(reload));
        page.connect_destroy(move |_| {
            keep.take();
        });
    }

    fn reload(&self, window: &MainWindow, changed: Rc<dyn Fn()>) {
        let services = &window.ctx.services;
        let Some(session) = services.account.session() else {
            self.apply(None, changed);
            return;
        };
        let own = session.device_id;
        let seen = services.db(|library| library.play_devices().unwrap_or_default());
        let account = std::sync::Arc::clone(&services.account);
        // Без сети — последний полученный список устройств.
        let named = services.run(async move { account.device_names().await });
        let this = self.clone();
        glib::spawn_future_local(async move {
            let (Some(seen), Some(named)) = (seen.await, named.await) else { return };
            let names = named.into_iter().map(|(id, (name, _))| (id, name)).collect();
            let others = filter_devices(&seen, &own, &names);
            this.apply(Some((own, others)), changed);
        });
    }

    fn apply(&self, devices: Option<(String, Vec<(String, String)>)>, changed: Rc<dyn Fn()>) {
        let before = self.current();
        let (list, visible) = match devices {
            Some((own, others)) if !others.is_empty() => {
                let mut list = vec![DeviceFilter::All, DeviceFilter::This(Some(own))];
                list.extend(others.iter().map(|(id, _)| DeviceFilter::Device(id.clone())));
                (list, Some(others))
            }
            _ => (vec![DeviceFilter::All, DeviceFilter::This(None)], None),
        };
        // Выбранное осталось в списке — остаётся выбранным, иначе «Все устройства».
        let same = |a: &DeviceFilter, b: &DeviceFilter| match (a, b) {
            (DeviceFilter::This(_), DeviceFilter::This(_)) => true,
            _ => a == b,
        };
        let index = list.iter().position(|f| same(f, &before)).unwrap_or(0);
        self.busy.set(true);
        let mut labels = vec![tr("HistoryDeviceAll").to_owned(), tr("HistoryDeviceThis").to_owned()];
        labels.extend(visible.iter().flatten().map(|(_, name)| name.clone()));
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.names.splice(0, self.names.n_items(), &labels);
        self.filters.replace(list);
        self.dropdown.set_selected(index as u32);
        self.dropdown.set_visible(visible.is_some());
        self.busy.set(false);
        if !same(&self.current(), &before) {
            changed();
        }
    }
}
