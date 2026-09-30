//! Обновления из GitHub Releases (docs/PROMPT.md §3 «Обновления», Windows `UpdateService.cs`, Android
//! `AppUpdater.kt`): `releases/latest/download/update.json` без лимитов API — формат Android
//! (`version`, `notes` с ключами `ru` и `en`, `publishedAt`) плюс файлы по архитектурам в `assets`
//! (AppImage — так читают 0.1.3 и старше, поле не менять) и `packages` — deb и rpm.
//!
//! Общий код окна и помощника установки (`melogold-update`, работает под root): разбор манифеста,
//! выбор файла, проверка размера и SHA-256.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MANIFEST_URL: &str = "https://github.com/melogold-app/melogoldLinux/releases/latest/download/update.json";
pub const DOWNLOAD_BASE: &str = "https://github.com/melogold-app/melogoldLinux/releases/download";
/// Страница последнего релиза: deb, rpm и Flatpak сами себя не обновляют — «Скачать» ведёт сюда.
pub const RELEASES_URL: &str = "https://github.com/melogold-app/melogoldLinux/releases/latest";
/// Проверка без нажатия — не чаще.
pub const CHECK_INTERVAL_MS: i64 = 6 * 3600 * 1000;

/// Формат системного пакета, которым Melogold поставлен.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PackageFormat {
    Deb,
    Rpm,
}

impl PackageFormat {
    /// Ключ в `packages` манифеста.
    pub fn key(self) -> &'static str {
        match self {
            PackageFormat::Deb => "deb",
            PackageFormat::Rpm => "rpm",
        }
    }

    pub fn parse(key: &str) -> Option<PackageFormat> {
        match key {
            "deb" => Some(PackageFormat::Deb),
            "rpm" => Some(PackageFormat::Rpm),
            _ => None,
        }
    }
}

/// Файл одной архитектуры (AppImage, deb, rpm).
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
    /// deb и rpm: формат → архитектура → файл. Старые версии этого поля не знают и молча пропускают.
    pub packages: HashMap<String, HashMap<String, UpdateAsset>>,
}

/// Чем файл отличается от записи в манифесте.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetMismatch {
    Size { actual: u64, expected: u64 },
    Sha256,
}

impl std::fmt::Display for AssetMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetMismatch::Size { actual, expected } => write!(f, "размер {actual} вместо {expected}"),
            AssetMismatch::Sha256 => f.write_str("SHA-256 не совпал"),
        }
    }
}

impl UpdateAsset {
    /// Размер и SHA-256 совпадают с манифестом: битый или подменённый файл не ставится.
    pub fn check(&self, data: &[u8]) -> Result<(), AssetMismatch> {
        if data.len() as u64 != self.size_bytes {
            return Err(AssetMismatch::Size { actual: data.len() as u64, expected: self.size_bytes });
        }
        if hex::encode(Sha256::digest(data)) != self.sha256.to_lowercase() {
            return Err(AssetMismatch::Sha256);
        }
        Ok(())
    }

    /// Имя — только имя файла из символов, которые бывают в именах пакетов: без `/`, `..` и пробелов.
    /// Манифест приходит по сети, а имя попадает и в адрес скачивания, и в путь на диске.
    pub fn has_safe_name(&self) -> bool {
        let name = &self.file_name;
        !name.is_empty()
            && name.len() <= 128
            && !name.starts_with('.')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '~'))
    }
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

    /// Файл deb или rpm этой архитектуры (`x86_64`, `aarch64`).
    pub fn package(&self, format: PackageFormat) -> Option<&UpdateAsset> {
        self.packages.get(format.key())?.get(std::env::consts::ARCH)
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
        assert!(manifest.packages.is_empty());
        assert_eq!(manifest.package(PackageFormat::Deb), None);
        if std::env::consts::ARCH == "x86_64" {
            let asset = manifest.asset().unwrap();
            assert_eq!(manifest.download_url(asset), format!("{DOWNLOAD_BASE}/v0.1.1/Melogold-x86_64.AppImage"));
        }
    }

    const NEW_FORMAT: &str = r#"{"version":"0.1.4","notes":{"ru":"Р","en":"E"},"publishedAt":"2026-10-01T10:00:00Z",
        "assets":{"x86_64":{"fileName":"Melogold-x86_64.AppImage","sizeBytes":123,"sha256":"ab"}},
        "packages":{"deb":{"x86_64":{"fileName":"melogold_0.1.4_amd64.deb","sizeBytes":10,"sha256":"cd"}},
                    "rpm":{"x86_64":{"fileName":"melogold-0.1.4-1.fc43.x86_64.rpm","sizeBytes":20,"sha256":"ef"}}}}"#;

    /// Разбор 0.1.3 и старше: те же поля, что были до `packages`, и никаких `deny_unknown_fields`.
    #[derive(Debug, Default, Deserialize)]
    #[serde(rename_all = "camelCase", default)]
    struct ManifestOf013 {
        version: String,
        notes: HashMap<String, String>,
        published_at: Option<String>,
        assets: HashMap<String, UpdateAsset>,
    }

    #[test]
    fn old_reader_skips_new_keys() {
        let old: ManifestOf013 = serde_json::from_str(NEW_FORMAT).unwrap();
        assert_eq!(old.version, "0.1.4");
        assert_eq!(old.notes.len(), 2);
        assert!(old.published_at.is_some());
        assert_eq!(old.assets["x86_64"].file_name, "Melogold-x86_64.AppImage");
        assert_eq!(old.assets["x86_64"].size_bytes, 123);
    }

    #[test]
    fn new_reader_takes_packages_by_format() {
        let manifest: UpdateManifest = serde_json::from_str(NEW_FORMAT).unwrap();
        assert_eq!(manifest.assets["x86_64"].file_name, "Melogold-x86_64.AppImage");
        if std::env::consts::ARCH == "x86_64" {
            let deb = manifest.package(PackageFormat::Deb).unwrap();
            assert_eq!(deb.file_name, "melogold_0.1.4_amd64.deb");
            assert_eq!(manifest.download_url(deb), format!("{DOWNLOAD_BASE}/v0.1.4/melogold_0.1.4_amd64.deb"));
            assert_eq!(manifest.package(PackageFormat::Rpm).unwrap().file_name, "melogold-0.1.4-1.fc43.x86_64.rpm");
        }
        assert_eq!(PackageFormat::parse("rpm"), Some(PackageFormat::Rpm));
        assert_eq!(PackageFormat::parse("snap"), None);
    }

    #[test]
    fn asset_check_size_and_hash() {
        let data = b"melogold";
        let sha = hex::encode(Sha256::digest(data));
        let asset = UpdateAsset { file_name: "a.deb".into(), size_bytes: 8, sha256: sha.to_uppercase() };
        assert_eq!(asset.check(data), Ok(()));
        assert_eq!(asset.check(b"melogolD"), Err(AssetMismatch::Sha256));
        assert_eq!(asset.check(b"melogol"), Err(AssetMismatch::Size { actual: 7, expected: 8 }));
    }

    #[test]
    fn file_names_are_plain() {
        let name = |n: &str| UpdateAsset { file_name: n.into(), ..Default::default() }.has_safe_name();
        assert!(name("melogold_0.1.4_amd64.deb"));
        assert!(name("melogold-0.1.4-1.fc43.x86_64.rpm"));
        assert!(!name(""));
        assert!(!name("../evil.rpm"));
        assert!(!name("a/b.rpm"));
        assert!(!name(".hidden"));
        assert!(!name("a b.rpm"));
        assert!(!name("a;rm.rpm"));
    }
}
