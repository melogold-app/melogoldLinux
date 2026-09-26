//! Настройки устройства: JSON-объект с ключами реестра Android (REWRITE §4.11.5).
//!
//! Файл — плоский объект `{"theme.mode": "system", "shell.lastTab": "Trends", …}`. Ключи,
//! которых эта версия не знает, сохраняются как были: файл переживает откат версии.
//! Настройки множатся на все клиенты — сюда попадает только то, что есть на Android,
//! плюс платформенное (docs/PROMPT.md §2).

use std::collections::BTreeMap;
use std::io::Write;
use std::marker::PhantomData;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Ключ настройки с типом и значением по умолчанию.
pub struct Key<T> {
    pub name: &'static str,
    default: fn() -> T,
    _type: PhantomData<fn() -> T>,
}

impl<T> Key<T> {
    pub const fn new(name: &'static str, default: fn() -> T) -> Self {
        Self { name, default, _type: PhantomData }
    }

    pub fn default_value(&self) -> T {
        (self.default)()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    values: BTreeMap<String, Value>,
}

impl Settings {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        let values: BTreeMap<String, Value> = serde_json::from_str(text)?;
        Ok(Self { values })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.values).unwrap_or_else(|_| "{}".into())
    }

    /// Нет файла — умолчания; испорченный файл — тоже умолчания и строка в журнале.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text).unwrap_or_else(|error| {
                tracing::warn!(%error, "настройки не читаются, взяты умолчания");
                Self::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!(%error, "настройки не читаются, взяты умолчания");
                Self::default()
            }
        }
    }

    /// Атомарная запись: временный файл рядом и переименование поверх.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.tmp");
        {
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(self.to_json().as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&temp, path)
    }

    /// Значение ключа; нет его или тип не тот — умолчание.
    pub fn get<T: DeserializeOwned>(&self, key: &Key<T>) -> T {
        self.values.get(key.name).and_then(|value| serde_json::from_value(value.clone()).ok()).unwrap_or_else(|| key.default_value())
    }

    /// Возвращает `true`, если значение поменялось и файл надо записать.
    pub fn set<T: Serialize>(&mut self, key: &Key<T>, value: T) -> bool {
        self.set_raw(key.name, serde_json::to_value(value).unwrap_or(Value::Null))
    }

    pub fn get_raw(&self, name: &str) -> Option<&Value> {
        self.values.get(name)
    }

    pub fn set_raw(&mut self, name: &str, value: Value) -> bool {
        if self.values.get(name) == Some(&value) {
            return false;
        }
        self.values.insert(name.to_owned(), value);
        true
    }

    /// Сортировка списка (`sort.favorites` и т. д.): «Поле:asc» или «Поле:desc».
    pub fn sort(&self, screen: &str) -> Option<String> {
        self.values.get(&format!("sort.{screen}")).and_then(Value::as_str).map(str::to_owned)
    }

    pub fn set_sort(&mut self, screen: &str, value: &str) -> bool {
        self.set_raw(&format!("sort.{screen}"), Value::String(value.to_owned()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// Раздел верхнего уровня: значения — как у Android (`TopLevelDestination`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    #[default]
    Trends,
    WhatsNew,
    Library,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Trends, Tab::WhatsNew, Tab::Library, Tab::Settings];

    /// Имя страницы в стеке окна.
    pub fn id(self) -> &'static str {
        match self {
            Tab::Trends => "trends",
            Tab::WhatsNew => "new",
            Tab::Library => "library",
            Tab::Settings => "settings",
        }
    }

    pub fn from_id(id: &str) -> Option<Tab> {
        Tab::ALL.into_iter().find(|tab| tab.id() == id)
    }
}

/// Реестр ключей. Имена — Android (REWRITE §4.11.5); платформенные — с пометкой.
pub mod keys {
    use super::{Key, Tab, ThemeMode};

    pub const THEME_MODE: Key<ThemeMode> = Key::new("theme.mode", ThemeMode::default);
    /// `system`, `ru` или `en` (REWRITE §3.5.2). У Android — только API 24–32, на новых
    /// системный экран; в GNOME своего выбора языка у приложения нет, поэтому он здесь.
    pub const APP_LOCALE: Key<String> = Key::new("app.locale", || "system".into());
    /// Раздел при выходе: приложение открывается в нём (docs/PROMPT.md §5.1).
    pub const LAST_TAB: Key<Tab> = Key::new("shell.lastTab", Tab::default);
    pub const NORMALIZATION: Key<bool> = Key::new("playback.normalization", || true);
    pub const AUTOPLAY: Key<bool> = Key::new("playback.autoplay", || true);
    /// Скорость 0,5–2× — одна глобальная настройка (docs/PROMPT.md §4).
    pub const SPEED: Key<f64> = Key::new("playback.speed", || 1.0);
    /// `synced` или `plain` (REWRITE §3.10.3).
    pub const LYRICS_VIEW: Key<String> = Key::new("lyrics.view", || "synced".into());
    pub const HISTORY_PAUSED: Key<bool> = Key::new("history.paused", || false);
    pub const HIDE_EXPLICIT: Key<bool> = Key::new("filter.hideExplicit", || false);
    /// Лимит кэша музыки в МБ; 0 — без ограничений (задание Windows 0003: 4 ГБ).
    pub const STREAM_CACHE_MB: Key<i64> = Key::new("cache.streamLimit", || 4096);
    pub const UPDATES_AUTO: Key<bool> = Key::new("updates.auto", || true);
    /// Адрес сервера; пусто — рабочий сервер по умолчанию.
    pub const SERVER_URL: Key<Option<String>> = Key::new("server.url", || None);

    // ── платформенные ──
    /// Не сохранять поисковые запросы (Android `pause_search_history`, Windows PauseSearchHistory).
    pub const SEARCH_HISTORY_PAUSED: Key<bool> = Key::new("search.historyPaused", || false);
    /// Громкость 0…1 и «без звука» — у телефона это системная громкость.
    pub const VOLUME: Key<f64> = Key::new("playback.volume", || 0.8);
    pub const MUTED: Key<bool> = Key::new("playback.muted", || false);
    /// Лимит кэша обложек, МБ (Android `coilDiskCacheMaxSize`).
    pub const IMAGE_CACHE_MB: Key<i64> = Key::new("cache.imageLimit", || 128);
    /// Размер окна при закрытии.
    pub const WINDOW_WIDTH: Key<i32> = Key::new("window.width", || 1100);
    pub const WINDOW_HEIGHT: Key<i32> = Key::new("window.height", || 720);
    pub const WINDOW_MAXIMIZED: Key<bool> = Key::new("window.maximized", || false);
    /// Когда последний раз проверяли обновления, мс Unix.
    pub const UPDATES_LAST_CHECK: Key<i64> = Key::new("updates.lastCheck", || 0);
    /// Версия, о которой уже сказали окном: о каждой — один раз.
    pub const UPDATES_ANNOUNCED: Key<Option<String>> = Key::new("updates.announcedVersion", || None);
    /// Версия прошлого запуска: первая после обновления переписывает `.desktop` и иконки (грабли §9 п. 13).
    pub const LAST_RUN_VERSION: Key<Option<String>> = Key::new("app.lastRunVersion", || None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty_or_wrong_type() {
        let settings = Settings::from_json(r#"{"playback.speed": "fast", "theme.mode": "dark"}"#).unwrap();
        assert_eq!(settings.get(&keys::SPEED), 1.0);
        assert_eq!(settings.get(&keys::THEME_MODE), ThemeMode::Dark);
        assert_eq!(settings.get(&keys::LAST_TAB), Tab::Trends);
        assert!(settings.get(&keys::NORMALIZATION));
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let mut settings = Settings::from_json(r#"{"future.key": [1, 2], "shell.lastTab": "Library"}"#).unwrap();
        assert_eq!(settings.get(&keys::LAST_TAB), Tab::Library);
        assert!(settings.set(&keys::LAST_TAB, Tab::WhatsNew));
        assert!(!settings.set(&keys::LAST_TAB, Tab::WhatsNew));
        let again = Settings::from_json(&settings.to_json()).unwrap();
        assert_eq!(again.get_raw("future.key"), Some(&serde_json::json!([1, 2])));
        assert_eq!(again.get(&keys::LAST_TAB), Tab::WhatsNew);
        assert!(settings.to_json().contains("\"shell.lastTab\": \"WhatsNew\""));
    }

    #[test]
    fn save_is_atomic_and_loadable() {
        let dir = std::env::temp_dir().join(format!("melogold-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut settings = Settings::default();
        settings.set(&keys::VOLUME, 0.5);
        settings.set_sort("favorites", "added:desc");
        settings.save(&path).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.get(&keys::VOLUME), 0.5);
        assert_eq!(loaded.sort("favorites").as_deref(), Some("added:desc"));
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, "не json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tab_ids_round_trip() {
        for tab in Tab::ALL {
            assert_eq!(Tab::from_id(tab.id()), Some(tab));
        }
    }
}
