//! Где приложение хранит своё (docs/PROMPT.md §3 «Где что лежит», XDG Base Directory).
//!
//! База, логи, кэш музыки и загрузки — в `$XDG_DATA_HOME/melogold`, а не в `~/.cache`:
//! чистильщики и `systemd-tmpfiles` стирают `~/.cache` без спроса, а гигабайты музыки
//! человек терять не хочет. Обложки — в `$XDG_CACHE_HOME/melogold`: их не жалко.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    data: PathBuf,
    cache: PathBuf,
    config: PathBuf,
}

impl AppPaths {
    /// Папки по переменным XDG. Относительный путь в переменной спецификация велит не
    /// учитывать — тогда берётся умолчание от `$HOME`.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let xdg = |name: &str, fallback: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
                .join("melogold")
        };
        Self {
            data: xdg("XDG_DATA_HOME", ".local/share"),
            cache: xdg("XDG_CACHE_HOME", ".cache"),
            config: xdg("XDG_CONFIG_HOME", ".config"),
        }
    }

    pub fn with_roots(data: impl Into<PathBuf>, cache: impl Into<PathBuf>, config: impl Into<PathBuf>) -> Self {
        Self { data: data.into(), cache: cache.into(), config: config.into() }
    }

    pub fn data(&self) -> &Path {
        &self.data
    }

    pub fn cache(&self) -> &Path {
        &self.cache
    }

    pub fn config(&self) -> &Path {
        &self.config
    }

    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }

    pub fn database(&self) -> PathBuf {
        self.data.join("library.db")
    }

    /// Настройки устройства с ключами реестра Android (REWRITE §4.11.5).
    pub fn settings(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    /// Соль установки для `hwid` (API §1.6): случайная, создаётся один раз.
    pub fn install_salt(&self) -> PathBuf {
        self.data.join("install-salt")
    }

    /// Запасной `platformId`, если `/etc/machine-id` нет (контейнер).
    pub fn fallback_machine_id(&self) -> PathBuf {
        self.data.join("machine-id")
    }

    /// Токены сессии, когда Secret Service не запущен: файл `0600`.
    pub fn session_fallback(&self) -> PathBuf {
        self.data.join("session.json")
    }

    /// Кэш музыки (задание Windows 0003): вытесняется сам, лимит — в настройках.
    pub fn song_cache(&self) -> PathBuf {
        self.data.join("cache")
    }

    /// Скачанные треки: не кэш — ни лимит, ни «Очистить кэш» их не трогают.
    pub fn downloads(&self) -> PathBuf {
        self.data.join("downloads")
    }

    pub fn backups(&self) -> PathBuf {
        self.data.join("backups")
    }

    pub fn updates(&self) -> PathBuf {
        self.data.join("updates")
    }

    /// Обложки: кэш, который можно потерять.
    pub fn images(&self) -> PathBuf {
        self.cache.join("images")
    }

    /// Сохранённая копия `config/stream-clients.json` (docs/PROMPT.md §4).
    pub fn stream_clients(&self) -> PathBuf {
        self.data.join("stream-clients.json")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for directory in [self.data.clone(), self.cache.clone(), self.config.clone(), self.logs()] {
            std::fs::create_dir_all(directory)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_cache_is_not_under_xdg_cache() {
        let paths = AppPaths::with_roots("/d/melogold", "/c/melogold", "/cfg/melogold");
        assert!(paths.song_cache().starts_with("/d/melogold"));
        assert!(paths.downloads().starts_with("/d/melogold"));
        assert!(paths.images().starts_with("/c/melogold"));
        assert_eq!(paths.settings(), PathBuf::from("/cfg/melogold/settings.json"));
    }
}
