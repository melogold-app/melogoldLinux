//! Запрос `player`: форматы с прямыми ссылками, громкость, длительность (Windows `Player.cs`);
//! диагноз отказа — почему YouTube не отдаёт видео, его собственными словами (задание 0001,
//! Windows `Playability.cs`, Android `Playability.kt`).

use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};

use crate::client::{ClientProfile, InnerTube};
use crate::json::Json;
use crate::{at, YouTubeError};

/// Аудиоформат из `streamingData.adaptiveFormats` с прямой ссылкой (без `signatureCipher`).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioFormat {
    pub itag: u32,
    pub url: String,
    pub mime_type: String,
    pub content_length: Option<u64>,
    pub bitrate: Option<u32>,
    /// Громкость формата относительно эталона YouTube, дБ (docs/PROMPT.md §4 «Громкость»).
    pub loudness_db: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerResponse {
    /// `OK`, `UNPLAYABLE`, `LOGIN_REQUIRED`, `ERROR`…
    pub status: String,
    pub reason: Option<String>,
    pub audio_formats: Vec<AudioFormat>,
    pub loudness_db: Option<f64>,
    pub duration_ms: Option<i64>,
}

impl PlayerResponse {
    pub fn parse(response: &Value) -> PlayerResponse {
        let audio_formats = at!(response, "streamingData", "adaptiveFormats")
            .items()
            .iter()
            .filter_map(|f| {
                let mime_type = at!(f, "mimeType").str().filter(|m| m.starts_with("audio/"))?;
                Some(AudioFormat {
                    itag: at!(f, "itag").i64().unwrap_or(0) as u32,
                    url: at!(f, "url").string()?,
                    mime_type: mime_type.to_owned(),
                    content_length: at!(f, "contentLength").i64().map(|n| n as u64),
                    bitrate: at!(f, "bitrate").i64().map(|n| n as u32),
                    loudness_db: at!(f, "loudnessDb").f64(),
                })
            })
            .collect();
        let seconds = at!(response, "videoDetails", "lengthSeconds").i64();
        // Громкость трека без формата: `loudnessDb`, иначе `trackAbsoluteLoudnessLkfs − loudnessTargetLkfs`.
        let audio = at!(response, "playerConfig", "audioConfig");
        let track_loudness = at!(audio, "loudnessDb")
            .f64()
            .or_else(|| Some(at!(audio, "trackAbsoluteLoudnessLkfs").f64()? - at!(audio, "loudnessTargetLkfs").f64()?));
        PlayerResponse {
            status: at!(response, "playabilityStatus", "status").str().unwrap_or("ERROR").to_owned(),
            reason: at!(response, "playabilityStatus", "reason").string(),
            audio_formats,
            loudness_db: track_loudness,
            duration_ms: seconds.filter(|s| *s > 0).map(|s| s * 1000),
        }
    }

    /// AAC в m4a: itag 140, запасной — лучший `audio/mp4` (139), иначе лучший вообще.
    pub fn choose(&self) -> Option<&AudioFormat> {
        self.audio_formats
            .iter()
            .find(|f| f.itag == 140)
            .or_else(|| self.audio_formats.iter().filter(|f| f.mime_type.starts_with("audio/mp4")).max_by_key(|f| f.bitrate.unwrap_or(0)))
            .or_else(|| self.audio_formats.iter().max_by_key(|f| f.bitrate.unwrap_or(0)))
    }
}

/// `/youtubei/v1/player` клиентом, который отдаёт прямые ссылки: расшифровка подписи и JS-проверки не нужны.
pub async fn player(
    client: &InnerTube,
    profile: &ClientProfile,
    video_id: &str,
    timeout: Duration,
) -> Result<PlayerResponse, YouTubeError> {
    let body = json!({"videoId": video_id, "contentCheckOk": true, "racyCheckOk": true});
    let response = client.post_with(profile, "player", body, None, false, Some(timeout)).await?;
    Ok(PlayerResponse::parse(&response))
}

/// Почему YouTube не отдаёт видео: ответ `player` клиента WEB, который присылает список стран,
/// даже когда видео не играет.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playability {
    pub status: Option<String>,
    pub reason: Option<String>,
    /// Страна, в которой YouTube видит устройство (из `visitorData`).
    pub country: Option<String>,
    /// Где правообладатель открыл видео; пусто — YouTube не сказал.
    pub available_countries: Vec<String>,
}

impl Playability {
    pub fn parse(response: &Value) -> Playability {
        // WEB отвечает playerMicroformatRenderer, WEB_REMIX — microformatDataRenderer.
        let countries = at!(response, "microformat", "playerMicroformatRenderer", "availableCountries")
            .or_else(|| at!(response, "microformat", "microformatDataRenderer", "availableCountries"));
        Playability {
            status: at!(response, "playabilityStatus", "status").string(),
            reason: at!(response, "playabilityStatus", "reason").string(),
            country: visitor_country(at!(response, "responseContext", "visitorData").str()),
            available_countries: countries.items().iter().filter_map(|c| c.as_str().map(str::to_owned)).collect(),
        }
    }

    /// Правообладатель закрыл видео в стране, где YouTube видит устройство.
    pub fn is_blocked_here(&self) -> bool {
        self.country.as_ref().is_some_and(|c| !self.available_countries.is_empty() && !self.available_countries.contains(c))
    }
}

/// Один запрос `player` клиентом WEB на `youtubei.googleapis.com`, без нашего `visitorData`: страну
/// YouTube определяет заново по адресу. Ответ не проверяется на пригодность — он и есть диагноз. 8 с.
pub async fn playability(client: &InnerTube, video_id: &str) -> Result<Playability, YouTubeError> {
    let response = client
        .post_with(
            &ClientProfile::web(),
            "player",
            json!({"videoId": video_id}),
            Some("youtubei.googleapis.com"),
            true,
            Some(Duration::from_secs(8)),
        )
        .await?;
    Ok(Playability::parse(&response))
}

/// Страна запроса из `visitorData`: base64 (url-safe, `%3D` вместо `=`) protobuf; поле 6 —
/// сообщение, в его поле 1 — код страны из двух букв. `None` — нет или не разобрать.
pub fn visitor_country(visitor_data: Option<&str>) -> Option<String> {
    let text = visitor_data?.trim();
    if text.is_empty() {
        return None;
    }
    let text = text.replace("%3D", "=").replace("%3d", "=");
    let text = text.trim_end_matches('=').replace('-', "+").replace('_', "/");
    let bytes = base64::engine::general_purpose::STANDARD_NO_PAD.decode(text).ok()?;
    let (start, end) = field(&bytes, 0, bytes.len(), 6)?;
    let (s, e) = field(&bytes, start, end, 1)?;
    let country = std::str::from_utf8(&bytes[s..e]).ok()?;
    (country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase())).then(|| country.to_owned())
}

/// Границы первого поля `number` с длиной (тип 2) в сообщении `bytes[start..end]`.
fn field(bytes: &[u8], start: usize, end: usize, number: u64) -> Option<(usize, usize)> {
    let mut index = start;
    while index < end {
        let key = varint(bytes, &mut index, end)?;
        match key & 7 {
            0 => {
                varint(bytes, &mut index, end)?;
            }
            1 => index += 8,
            2 => {
                let length = varint(bytes, &mut index, end)? as usize;
                if length > end.saturating_sub(index) {
                    return None;
                }
                if key >> 3 == number {
                    return Some((index, index + length));
                }
                index += length;
            }
            5 => index += 4,
            _ => return None,
        }
    }
    None
}

fn varint(bytes: &[u8], index: &mut usize, end: usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    while *index < end && shift < 64 {
        let byte = bytes[*index];
        *index += 1;
        value |= u64::from(byte & 0x7F) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_from_visitor_data() {
        // Образцы из задания 0001 (полные строки — VisitorDataTest.kt Android).
        assert_eq!(
            visitor_country(Some("CgtRTWpHWl9XellHZyjBhN7VBjIoCgJOTBIiEh4SHAsMDg8QERITFBUWFxgZGhscHR4fICEiIyQlJicgYw%3D%3D")).as_deref(),
            Some("NL")
        );
        let germany = concat!(
            "Cgs4bmZBZU9NZ2hGVSiLhd7VBjIOCgJERRIIEgAgRlICCHE6AggBYuACCt0CMTguWVRFPUdHUjNxZUdSUDVfUTFNMXVSVVVVYVRs",
            "Q1BWb3VINWZaVmpWMlk5TkFJYWtwenZQdGlTS2NMMmRiV1pFbDFjNllRb0Nwd3pMRXhlVk5FYlVfSnhCb21TZnBZRE5pbEZjTzJP",
            "NVdxR211MnlwSTJSWEt4TVllX1BiTzJ0YmlGM0gxWEpJckV4REU1TnliV1RRY0NWQ19tamZfMkIzVWw4Szg5cnRRYm1KbXc0aEtH",
            "YmY0ZVA4c3pXWXhBM3hVaXNhOWd0eFlxYjNvUU1kMVZjQ01PWFFJTElTRG1ZdnpNU2IyOFpPWHNpRGY3RHZNeEFVX2kxYUZVOHk3",
            "WTV4LVd0akg2c1RXRkZ5cGR1emxiX0Q1Ykg5b0VHN1pfVWFiNUlzNW9iUWhmOUQ2X2IwNGJIaTRTVWRKc1laSTZBM0FEcGdDR256",
            "NnNkd1E2ZVRWYzNqQ0I0eFc5Zw%3D%3D"
        );
        assert_eq!(visitor_country(Some(germany)).as_deref(), Some("DE"));
        assert_eq!(visitor_country(Some("not base64 at all!")), None);
        assert_eq!(visitor_country(Some("CgtRTWpHWl9XellHZw")), None);
        assert_eq!(visitor_country(Some("мусор")), None);
        assert_eq!(visitor_country(Some("")), None);
        assert_eq!(visitor_country(Some("CgtBTk9OWU1JWkVEMCiAgICABg%3D%3D")), None);
    }

    #[test]
    fn blocked_here_needs_country_and_list() {
        let mut p = Playability { country: Some("RU".into()), available_countries: vec!["NL".into(), "DE".into()], ..Default::default() };
        assert!(p.is_blocked_here());
        p.available_countries.push("RU".into());
        assert!(!p.is_blocked_here());
        p.available_countries.clear();
        assert!(!p.is_blocked_here());
    }
}
