//! Идентификатор устройства для сервера (API §1.6 `Hwid`, векторы `spec/hwid.vectors.json`):
//! `hex(sha256("melogold-hwid-v1|" + platformId + "|" + serverId))` — у каждого сервера свой.
//! У Linux `platformId` = `/etc/machine-id|installSalt`; нет `machine-id` (контейнер) — запасной
//! случайный UUID в данных приложения.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::paths::AppPaths;

pub fn compute(platform_id: &str, server_id: &str) -> String {
    hex::encode(Sha256::digest(format!("melogold-hwid-v1|{platform_id}|{server_id}").as_bytes()))
}

/// `platformId` этой установки: `machine-id|installSalt`. Соль создаётся один раз при первом входе.
pub fn platform_id(paths: &AppPaths) -> String {
    let machine = read_trimmed(Path::new("/etc/machine-id"))
        .or_else(|| read_trimmed(Path::new("/var/lib/dbus/machine-id")))
        .unwrap_or_else(|| stored_or_new(&paths.fallback_machine_id()));
    format!("{machine}|{}", stored_or_new(&paths.install_salt()))
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// UUID из файла, а если его нет — новый, записанный туда же.
fn stored_or_new(path: &Path) -> String {
    if let Some(value) = read_trimmed(path) {
        return value;
    }
    let value = crate::ids::new_uuid();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(error) = std::fs::write(path, &value) {
        tracing::warn!(%error, "соль установки не записалась: hwid сменится при следующем запуске");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vectors() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/hwid.vectors.json")).unwrap();
        let spec: serde_json::Value = serde_json::from_str(&text).unwrap();
        let vectors = spec["vectors"].as_array().unwrap();
        assert!(vectors.len() >= 4);
        for vector in vectors {
            let expected = vector["hwid"].as_str().unwrap();
            assert_eq!(compute(vector["platformId"].as_str().unwrap(), vector["serverId"].as_str().unwrap()), expected, "{vector}");
        }
    }

    #[test]
    fn salt_is_kept_between_runs() {
        let dir = std::env::temp_dir().join(format!("melogold-hwid-{}", crate::ids::new_uuid()));
        let paths = AppPaths::with_roots(dir.join("data"), dir.join("cache"), dir.join("config"));
        let first = platform_id(&paths);
        assert_eq!(platform_id(&paths), first);
        assert!(first.contains('|'));
        let _ = std::fs::remove_dir_all(dir);
    }
}
