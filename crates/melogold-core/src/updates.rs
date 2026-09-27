//! Обновления из GitHub Releases (docs/PROMPT.md §3 «Обновления», Windows `UpdateService.cs`, Android
//! `AppUpdater.kt`): `releases/latest/download/update.json` без лимитов API — формат Android
//! (`version`, `notes` с ключами `ru` и `en`, `publishedAt`) плюс файлы по архитектурам в `assets`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub const MANIFEST_URL: &str = "https://github.com/melogold-app/melogoldLinux/releases/latest/download/update.json";
pub const DOWNLOAD_BASE: &str = "https://github.com/melogold-app/melogoldLinux/releases/download";
/// Страница последнего релиза: deb, rpm и Flatpak сами себя не обновляют — «Скачать» ведёт сюда.
pub const RELEASES_URL: &str = "https://github.com/melogold-app/melogoldLinux/releases/latest";
/// Проверка без нажатия — не чаще.
pub const CHECK_INTERVAL_MS: i64 = 6 * 3600 * 1000;

/// Файл одной архитектуры (AppImage).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateAsset {
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateManifest {
    pub version: String,
    pub notes: HashMap<String, String>,
    pub published_at: Option<String>,
    pub assets: HashMap<String, UpdateAsset>,
}

impl UpdateManifest {
    /// «Что нового» на языке приложения, иначе любое.
    pub fn notes_for(&self, russian: bool) -> Option<&str> {
        self.notes
            .get(if russian { "ru" } else { "en" })
            .or_else(|| self.notes.values().next())
            .map(String::as_str)
            .filter(|n| !n.trim().is_empty())
    }

    /// Файл этой архитектуры (`x86_64`, `aarch64`).
    pub fn asset(&self) -> Option<&UpdateAsset> {
        self.assets.get(std::env::consts::ARCH)
    }

    pub fn download_url(&self, asset: &UpdateAsset) -> String {
        format!("{DOWNLOAD_BASE}/v{}/{}", self.version, asset.file_name)
    }
}

fn parts(version: &str) -> Option<Vec<u64>> {
    version.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect()
}

/// `candidate` новее `current` (числа через точку; неразборчивое — не новее).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parts(candidate), parts(current)) {
        (Some(mut a), Some(mut b)) => {
            let length = a.len().max(b.len());
            a.resize(length, 0);
            b.resize(length, 0);
            a > b
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_numbers() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.9.12"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1", "0.1.0"));
        assert!(!is_newer("next", "0.1.0"));
    }

    #[test]
    fn manifest_of_the_release() {
        let manifest: UpdateManifest = serde_json::from_str(
            r#"{"version":"0.1.1","notes":{"ru":"Исправления","en":"Fixes"},"publishedAt":"2026-09-27T10:00:00Z",
                "assets":{"x86_64":{"fileName":"Melogold-x86_64.AppImage","sizeBytes":123,"sha256":"ab"}}}"#,
        )
        .unwrap();
        assert_eq!(manifest.notes_for(true), Some("Исправления"));
        if std::env::consts::ARCH == "x86_64" {
            let asset = manifest.asset().unwrap();
            assert_eq!(manifest.download_url(asset), format!("{DOWNLOAD_BASE}/v0.1.1/Melogold-x86_64.AppImage"));
        }
    }
}
