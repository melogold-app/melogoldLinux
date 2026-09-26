//! Список клиентов InnerTube для потока (docs/PROMPT.md §4): `config/stream-clients.json` в этом
//! репозитории. Приложение берёт сохранённую копию, а свежий файл с `main` подтягивает в фоне —
//! поломку на стороне YouTube чинят без релиза. Встроенный список остаётся запасным, негодный
//! файл не применяется (Windows `StreamClients.cs`).

use std::path::Path;
use std::time::Duration;

use melogold_innertube::ClientProfile;
use serde::Deserialize;

pub const URL: &str = "https://raw.githubusercontent.com/melogold-app/melogoldLinux/main/config/stream-clients.json";
const SCHEMA: u32 = 1;

pub fn built_in() -> Vec<ClientProfile> {
    vec![ClientProfile::vision_os()]
}

#[derive(Deserialize)]
struct Config {
    schema: u32,
    #[serde(default)]
    clients: Vec<serde_json::Value>,
}

/// Профили из файла; `None` — не та схема, пустой список или клиент без обязательных полей.
pub fn parse(json: &str) -> Option<Vec<ClientProfile>> {
    let config: Config = serde_json::from_str(json).ok()?;
    if config.schema != SCHEMA || config.clients.is_empty() {
        return None;
    }
    let mut profiles = Vec::new();
    for client in config.clients {
        let profile: ClientProfile = serde_json::from_value(client).ok()?;
        let blank = |s: &str| s.trim().is_empty();
        if blank(&profile.name) || profile.id == 0 || blank(&profile.version) || blank(&profile.host) || blank(&profile.user_agent) {
            return None;
        }
        profiles.push(profile);
    }
    Some(profiles)
}

/// Сохранённая копия или встроенный список — сразу, без сети.
pub fn load_saved(path: &Path) -> Vec<ClientProfile> {
    std::fs::read_to_string(path).ok().and_then(|text| parse(&text)).unwrap_or_else(built_in)
}

/// Свежий список с GitHub; годный — сохраняется и возвращается.
pub async fn refresh(http: &reqwest::Client, path: &Path, user_agent: &str) -> Option<Vec<ClientProfile>> {
    let response = http.get(URL).header("User-Agent", user_agent).timeout(Duration::from_secs(15)).send().await;
    let text = match response {
        Ok(response) if response.status().is_success() => response.text().await.ok()?,
        Ok(response) => {
            tracing::warn!(код = response.status().as_u16(), "клиенты потока с GitHub не получены");
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, "клиенты потока с GitHub не получены");
            return None;
        }
    };
    let Some(fresh) = parse(&text) else {
        tracing::warn!("клиенты потока с GitHub отклонены: файл негоден");
        return None;
    };
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, &text).and_then(|()| std::fs::rename(&temp, path)).is_err() {
        tracing::warn!("клиенты потока не сохранились");
    }
    tracing::info!(клиенты = %fresh.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(","), "клиенты потока обновлены");
    Some(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_file_is_valid() {
        let text = include_str!("../../../config/stream-clients.json");
        let clients = parse(text).expect("config/stream-clients.json годен");
        assert_eq!(clients[0], ClientProfile::vision_os(), "встроенный список совпадает с файлом");
    }

    #[test]
    fn bad_files_are_rejected() {
        assert!(parse("{}").is_none());
        assert!(parse(r#"{"schema": 2, "clients": [{}]}"#).is_none());
        assert!(parse(r#"{"schema": 1, "clients": []}"#).is_none());
        assert!(parse(r#"{"schema": 1, "clients": [{"name": "X", "id": 1, "version": "", "host": "h", "userAgent": "u"}]}"#).is_none());
        assert!(parse("not json").is_none());
        let ok = parse(r#"{"schema": 1, "clients": [{"name": "X", "id": 1, "version": "1", "host": "h", "userAgent": "u", "future": 1}]}"#);
        assert_eq!(ok.unwrap()[0].name, "X");
    }
}
