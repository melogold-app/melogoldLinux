//! Настройки в главном потоке: чтение сразу, запись — отложенно и не в главном потоке.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use gtk::glib;
use melogold_core::settings::{Key, Settings};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// Записи из разных потоков по очереди: иначе две записи делили бы один временный файл.
static SAVE_LOCK: Mutex<()> = Mutex::new(());

pub struct SettingsStore {
    path: PathBuf,
    values: RefCell<Settings>,
    save_scheduled: Cell<bool>,
    /// Снимки окна пишут в свою папку, но и её лучше не трогать вовсе.
    read_only: bool,
}

impl SettingsStore {
    pub fn load(path: PathBuf, read_only: bool) -> Rc<Self> {
        let values = Settings::load(&path);
        Rc::new(Self { path, values: RefCell::new(values), save_scheduled: Cell::new(false), read_only })
    }

    pub fn get<T: DeserializeOwned>(&self, key: &Key<T>) -> T {
        self.values.borrow().get(key)
    }

    pub fn set<T: Serialize>(self: &Rc<Self>, key: &Key<T>, value: T) {
        if self.values.borrow_mut().set(key, value) {
            self.schedule_save();
        }
    }

    /// Правки идут пачками (размер окна, ползунок громкости): пишем раз в 300 мс.
    fn schedule_save(self: &Rc<Self>) {
        if self.read_only || self.save_scheduled.replace(true) {
            return;
        }
        let this = Rc::clone(self);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            this.save_scheduled.set(false);
            let snapshot = this.values.borrow().clone();
            let path = this.path.clone();
            std::thread::spawn(move || write(&snapshot, &path));
        });
    }

    /// Запись сразу — при выходе, когда отложенной уже не дождаться.
    pub fn flush(&self) {
        if self.read_only {
            return;
        }
        write(&self.values.borrow(), &self.path);
    }
}

fn write(settings: &Settings, path: &std::path::Path) {
    let _guard = SAVE_LOCK.lock();
    if let Err(error) = settings.save(path) {
        tracing::warn!(%error, "настройки не сохранились");
    }
}
