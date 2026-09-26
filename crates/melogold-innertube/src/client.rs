//! Клиент `/youtubei/v1` (Windows `InnerTubeClient.cs`). У каждого клиента InnerTube свой
//! User-Agent и заголовки; глобального нет (docs/PROMPT.md §3 «Сеть»).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::json::Json;
use crate::{at, ErrorKind, YouTubeError};

/// Клиент InnerTube: имя, версия, заголовки, поля устройства. Формат — как в `config/stream-clients.json`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientProfile {
    pub name: String,
    pub id: u32,
    pub version: String,
    pub host: String,
    pub user_agent: String,
    #[serde(default)]
    pub referer: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub device_make: Option<String>,
    #[serde(default)]
    pub device_model: Option<String>,
    #[serde(default)]
    pub os_name: Option<String>,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub android_sdk_version: Option<u32>,
    /// User-Agent запросов к googlevideo; `None` — любой.
    #[serde(default)]
    pub media_user_agent: Option<String>,
}

const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

fn profile(name: &str, id: u32, version: &str, host: &str, user_agent: &str) -> ClientProfile {
    ClientProfile {
        name: name.into(),
        id,
        version: version.into(),
        host: host.into(),
        user_agent: user_agent.into(),
        referer: None,
        platform: None,
        device_make: None,
        device_model: None,
        os_name: None,
        os_version: None,
        android_sdk_version: None,
        media_user_agent: None,
    }
}

impl ClientProfile {
    /// YouTube Music в браузере: поиск, страницы, очередь.
    pub fn web_remix() -> Self {
        ClientProfile {
            referer: Some("https://music.youtube.com/".into()),
            platform: Some("DESKTOP".into()),
            ..profile("WEB_REMIX", 67, "1.20260922.01.00", "music.youtube.com", CHROME_UA)
        }
    }

    /// Обычный YouTube: видео, каналы, трансляции вне каталога YTM.
    pub fn web() -> Self {
        ClientProfile {
            referer: Some("https://www.youtube.com/".into()),
            platform: Some("DESKTOP".into()),
            ..profile("WEB", 1, "2.20260924.00.00", "www.youtube.com", CHROME_UA)
        }
    }

    /// Поток (сентябрь 2026): прямые ссылки без PO-токена; нужен `visitorData` (грабли §9 п. 2).
    pub fn vision_os() -> Self {
        ClientProfile {
            referer: Some("https://www.youtube.com/".into()),
            device_make: Some("Apple".into()),
            device_model: Some("RealityDevice17,1".into()),
            os_name: Some("visionOS".into()),
            os_version: Some("26.5.23O471".into()),
            ..profile(
                "VISIONOS",
                101,
                "1.02",
                "www.youtube.com",
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15",
            )
        }
    }

    /// Синхронный текст YouTube Music (`timedLyricsModel`).
    pub fn android_music() -> Self {
        ClientProfile {
            platform: Some("MOBILE".into()),
            os_name: Some("Android".into()),
            os_version: Some("11".into()),
            android_sdk_version: Some(30),
            ..profile(
                "ANDROID_MUSIC",
                21,
                "7.27.52",
                "music.youtube.com",
                "com.google.android.apps.youtube.music/7.27.52 (Linux; U; Android 11) gzip",
            )
        }
    }
}

#[derive(Default)]
struct State {
    visitor_data: Option<String>,
}

/// Запросы к `/youtubei/v1`. `visitorData` запоминается из первого ответа.
#[derive(Clone)]
pub struct InnerTube {
    http: reqwest::Client,
    state: Arc<Mutex<State>>,
    language: String,
    region: String,
}

impl InnerTube {
    pub fn new(language: &str, region: &str) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .pool_idle_timeout(Duration::from_secs(90))
            .gzip(true)
            .brotli(true)
            .build()
            .expect("HTTP-клиент собирается");
        Self { http, state: Arc::default(), language: language.into(), region: region.into() }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn region(&self) -> &str {
        &self.region
    }

    pub fn visitor_data(&self) -> Option<String> {
        self.state.lock().ok().and_then(|s| s.visitor_data.clone())
    }

    pub fn set_visitor_data(&self, value: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.visitor_data = value.filter(|v| !v.is_empty());
        }
    }

    /// `visitorData`, если его ещё нет: самый лёгкий запрос YouTube Music.
    pub async fn ensure_visitor_data(&self) -> Result<(), YouTubeError> {
        if self.visitor_data().is_some() {
            return Ok(());
        }
        self.post(&ClientProfile::web_remix(), "music/get_search_suggestions", json!({"input": ""})).await.map(|_| ())
    }

    /// `anonymous` — диагноз отказа (задание 0001): без нашего `visitorData`, чтобы страну YouTube
    /// определил заново, и по-английски — причину сверяют с английскими фразами YouTube.
    pub fn context(&self, client: &ClientProfile, anonymous: bool) -> Value {
        let mut c = Map::new();
        c.insert("clientName".into(), client.name.clone().into());
        c.insert("clientVersion".into(), client.version.clone().into());
        c.insert("hl".into(), if anonymous { "en".into() } else { self.language.clone().into() });
        c.insert("gl".into(), self.region.clone().into());
        c.insert("timeZone".into(), "UTC".into());
        c.insert("utcOffsetMinutes".into(), 0.into());
        let optional = [
            ("platform", &client.platform),
            ("deviceMake", &client.device_make),
            ("deviceModel", &client.device_model),
            ("osName", &client.os_name),
            ("osVersion", &client.os_version),
        ];
        for (key, value) in optional {
            if let Some(value) = value {
                c.insert(key.into(), value.clone().into());
            }
        }
        if let Some(sdk) = client.android_sdk_version {
            c.insert("androidSdkVersion".into(), sdk.into());
        }
        if !anonymous {
            if let Some(visitor) = self.visitor_data() {
                c.insert("visitorData".into(), visitor.into());
            }
        }
        json!({"client": Value::Object(c), "user": {"lockedSafetyMode": false}})
    }

    pub async fn post(&self, client: &ClientProfile, endpoint: &str, body: Value) -> Result<Value, YouTubeError> {
        self.post_with(client, endpoint, body, None, false, None).await
    }

    pub async fn post_with(
        &self,
        client: &ClientProfile,
        endpoint: &str,
        mut body: Value,
        host: Option<&str>,
        anonymous: bool,
        timeout: Option<Duration>,
    ) -> Result<Value, YouTubeError> {
        if let Value::Object(map) = &mut body {
            map.insert("context".into(), self.context(client, anonymous));
        }
        let url = format!("https://{}/youtubei/v1/{endpoint}?prettyPrint=false", host.unwrap_or(&client.host));
        let mut request = self
            .http
            .post(url)
            .header("User-Agent", &client.user_agent)
            .header("X-YouTube-Client-Name", client.id.to_string())
            .header("X-YouTube-Client-Version", &client.version)
            .header("Accept-Language", if anonymous { "en" } else { &self.language })
            .header("Content-Type", "application/json")
            .body(body.to_string());
        if let Some(referer) = &client.referer {
            request = request.header("Referer", referer).header("Origin", referer.trim_end_matches('/'));
        }
        if !anonymous {
            if let Some(visitor) = self.visitor_data() {
                request = request.header("X-Goog-Visitor-Id", visitor);
            }
        }
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let started = std::time::Instant::now();
        let response = request.send().await.map_err(|error| {
            let message = if error.is_timeout() { "timeout".to_owned() } else { error.to_string() };
            YouTubeError::new(ErrorKind::Offline, format!("{endpoint}: {message}"))
        })?;
        let status = response.status();
        let text = response.text().await.map_err(|e| YouTubeError::new(ErrorKind::Offline, format!("{endpoint}: {e}")))?;
        tracing::debug!(клиент = %client.name, %endpoint, код = status.as_u16(), мс = started.elapsed().as_millis() as u64, "InnerTube");
        if !status.is_success() {
            let kind = match status.as_u16() {
                403 | 429 => ErrorKind::Blocked,
                500.. => ErrorKind::Offline,
                _ => ErrorKind::Unknown,
            };
            return Err(YouTubeError::new(kind, format!("HTTP {} from {endpoint}", status.as_u16())));
        }
        let value: Value =
            serde_json::from_str(&text).map_err(|_| YouTubeError::new(ErrorKind::Parser, format!("{endpoint}: not JSON")))?;
        if !anonymous && self.visitor_data().is_none() {
            if let Some(visitor) = at!(&value, "responseContext", "visitorData").str() {
                self.set_visitor_data(Some(visitor.to_owned()));
            }
        }
        Ok(value)
    }
}

const SUPPORTED_LANGUAGES: &[&str] = &[
    "af", "az", "id", "ms", "ca", "cs", "da", "de", "et", "en-GB", "en", "es", "es-419", "eu", "fil", "fr", "fr-CA", "gl", "hr", "zu",
    "is", "it", "sw", "lt", "hu", "nl", "no", "uz", "pl", "pt-PT", "pt", "ro", "sq", "sk", "sl", "fi", "sv", "vi", "tr", "bg", "ky", "kk",
    "mk", "mn", "ru", "sr", "uk", "el", "hy", "iw", "ur", "ar", "fa", "ne", "mr", "hi", "bn", "pa", "gu", "ta", "te", "kn", "ml", "si",
    "th", "lo", "my", "ka", "am", "km", "zh-CN", "zh-TW", "zh-HK", "ja", "ko",
];

/// `hl` и `gl` по локали: `ru_RU.UTF-8` → (`ru`, `RU`); неизвестный язык — `en`, регион — `US`
/// (грабли §9 п. 8: язык контента — по языку системы).
pub fn locale_from(tag: &str) -> (String, String) {
    let tag = tag.split(['.', '@']).next().unwrap_or_default();
    let (language, region) = match tag.split_once(['_', '-']) {
        Some((language, region)) => (language.to_ascii_lowercase(), Some(region.to_ascii_uppercase())),
        None => (tag.to_ascii_lowercase(), None),
    };
    let full = region.as_ref().map(|r| format!("{language}-{r}"));
    let hl = match full {
        Some(full) if SUPPORTED_LANGUAGES.contains(&full.as_str()) => full,
        _ if SUPPORTED_LANGUAGES.contains(&language.as_str()) => language,
        _ => "en".into(),
    };
    let gl = region.filter(|r| r.len() == 2 && r.bytes().all(|b| b.is_ascii_uppercase())).unwrap_or_else(|| "US".into());
    (hl, gl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales() {
        assert_eq!(locale_from("ru_RU.UTF-8"), ("ru".into(), "RU".into()));
        assert_eq!(locale_from("en_US.UTF-8"), ("en".into(), "US".into()));
        assert_eq!(locale_from("pt_PT"), ("pt-PT".into(), "PT".into()));
        assert_eq!(locale_from("C"), ("en".into(), "US".into()));
        assert_eq!(locale_from("de"), ("de".into(), "US".into()));
    }

    #[test]
    fn context_has_visitor_data_unless_anonymous() {
        let client = InnerTube::new("ru", "RU");
        client.set_visitor_data(Some("abc".into()));
        let context = client.context(&ClientProfile::vision_os(), false);
        assert_eq!(context["client"]["visitorData"], "abc");
        assert_eq!(context["client"]["deviceModel"], "RealityDevice17,1");
        assert_eq!(context["client"]["hl"], "ru");
        let anonymous = client.context(&ClientProfile::web(), true);
        assert!(anonymous["client"].get("visitorData").is_none());
        assert_eq!(anonymous["client"]["hl"], "en");
    }
}
