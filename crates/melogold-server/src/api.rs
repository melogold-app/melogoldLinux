//! HTTP-клиент API Melogold (контракт `melogoldServer/docs/API.md`, Windows `MelogoldApi.cs`):
//! User-Agent `melogold-linux/<версия>` (§1.2), `Accept-Language` — язык интерфейса,
//! `X-Sync-Protocol: 1` там, где он обязателен. Ошибка — HTTP-статус и `code` конверта (§2): только
//! по нему ветвится клиент.

use std::time::Duration;

use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::dto::*;

/// Протокол синхронизации этого клиента (§1.1).
pub const SYNC_PROTOCOL: i64 = 1;
/// Версия HTTP API, которую понимает клиент.
pub const API_VERSION: i64 = 1;
/// Таймаут длинного опроса привязки: сервер держит ответ до 25 с (§4.6).
const LINK_POLL_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Clone, Debug, thiserror::Error)]
#[error("{code} ({status}): {message}")]
pub struct ApiError {
    /// 0 — ответа не было (сеть, DNS, TLS, таймаут).
    pub status: u16,
    pub code: String,
    pub message: String,
    pub retry_after_seconds: Option<i64>,
}

impl ApiError {
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> ApiError {
        ApiError { status, code: code.to_owned(), message: message.into(), retry_after_seconds: None }
    }

    pub fn is_network(&self) -> bool {
        self.status == 0
    }

    /// Сеть, 5xx, 429: данные и токены не трогаются, попытка повторяется позже.
    pub fn is_transient(&self) -> bool {
        self.is_network() || self.status >= 500 || self.status == 429
    }
}

#[derive(Clone)]
pub struct Api {
    base: String,
    http: reqwest::Client,
    /// Для живых событий: без общего таймаута — поток держится часами.
    stream: reqwest::Client,
}

impl Api {
    pub fn new(base: &str, client_version: &str, language: &str) -> Api {
        let user_agent = format!("melogold-linux/{client_version}");
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(value) = reqwest::header::HeaderValue::from_str(language) {
            headers.insert(reqwest::header::ACCEPT_LANGUAGE, value);
        }
        let build = |timeout: Option<Duration>| {
            let mut builder = reqwest::Client::builder()
                .user_agent(user_agent.clone())
                .default_headers(headers.clone())
                .connect_timeout(Duration::from_secs(10))
                .gzip(true);
            if let Some(timeout) = timeout {
                builder = builder.timeout(timeout);
            }
            builder.build().expect("HTTP-клиент собирается")
        };
        Api { base: base.trim_end_matches('/').to_owned(), http: build(Some(Duration::from_secs(35))), stream: build(None) }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    async fn raw(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
        token: Option<&str>,
        sync: bool,
        timeout: Option<Duration>,
    ) -> Result<reqwest::Response, ApiError> {
        let mut request =
            self.http.request(method.clone(), format!("{}{path}", self.base)).header(reqwest::header::ACCEPT, "application/json");
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if sync {
            request = request.header("X-Sync-Protocol", SYNC_PROTOCOL.to_string());
        }
        // Тело POST/PUT/PATCH — JSON-объект, как минимум `{}` (§1.2).
        if method != reqwest::Method::GET && method != reqwest::Method::DELETE {
            let body = body.cloned().unwrap_or_else(|| json!({}));
            request = request.header(reqwest::header::CONTENT_TYPE, "application/json").body(body.to_string());
        }
        if let Some(timeout) = timeout {
            request = request.timeout(timeout);
        }
        let response = request.send().await.map_err(|error| {
            let code = if error.is_timeout() { "timeout" } else { "network" };
            ApiError::new(0, code, error.to_string())
        })?;
        if response.status().is_success() {
            return Ok(response);
        }
        Err(error_from(response).await)
    }

    async fn call<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        token: Option<&str>,
        sync: bool,
    ) -> Result<T, ApiError> {
        let response = self.raw(method, path, body.as_ref(), token, sync, None).await?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| ApiError::new(0, "network", e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| ApiError::new(status, "invalid_response", e.to_string()))
    }

    async fn call_empty(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
        token: Option<&str>,
        sync: bool,
    ) -> Result<(), ApiError> {
        self.raw(method, path, body.as_ref(), token, sync, None).await.map(|_| ())
    }

    // ── сервер (§4.2) ──

    pub async fn server_info(&self) -> Result<ServerInfo, ApiError> {
        let response = self.raw(reqwest::Method::GET, "/server/info", None, None, false, Some(Duration::from_secs(10))).await?;
        let text = response.text().await.map_err(|e| ApiError::new(0, "network", e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| ApiError::new(200, "not_melogold", e.to_string()))
    }

    // ── регистрация, вход, сессии (§4.3) ──

    pub async fn register_challenge(&self) -> Result<RegisterChallenge, ApiError> {
        self.call(reqwest::Method::GET, "/auth/register/challenge", None, None, false).await
    }

    pub async fn register(
        &self,
        login: &str,
        password: &str,
        device: &DeviceInput,
        pow: Option<&PowSolution>,
    ) -> Result<AuthSession, ApiError> {
        let mut body = json!({ "login": login, "password": password, "device": to_value(device) });
        if let Some(pow) = pow {
            body["pow"] = to_value(pow);
        }
        self.call(reqwest::Method::POST, "/auth/register", Some(body), None, false).await
    }

    pub async fn login(&self, login: &str, password: &str, device: &DeviceInput) -> Result<AuthSession, ApiError> {
        let body = json!({ "login": login, "password": password, "device": to_value(device) });
        self.call(reqwest::Method::POST, "/auth/login", Some(body), None, false).await
    }

    pub async fn refresh(&self, refresh_token: &str, device: &DevicePatch) -> Result<RefreshResponse, ApiError> {
        let body = json!({ "refreshToken": refresh_token, "device": to_value(device) });
        self.call(reqwest::Method::POST, "/auth/refresh", Some(body), None, false).await
    }

    pub async fn logout(&self, refresh_token: &str) -> Result<(), ApiError> {
        self.call_empty(reqwest::Method::POST, "/auth/logout", Some(json!({ "refreshToken": refresh_token })), None, false).await
    }

    pub async fn recover(
        &self,
        login: &str,
        recovery_code: &str,
        new_password: &str,
        device: &DeviceInput,
    ) -> Result<AuthSession, ApiError> {
        let body = json!({ "login": login, "recoveryCode": recovery_code, "newPassword": new_password, "device": to_value(device) });
        self.call(reqwest::Method::POST, "/auth/recover", Some(body), None, false).await
    }

    pub async fn me(&self, token: &str) -> Result<MeResponse, ApiError> {
        self.call(reqwest::Method::GET, "/auth/me", None, Some(token), false).await
    }

    // ── устройства и аккаунт (§4.4–§4.5) ──

    pub async fn devices(&self, token: &str) -> Result<DeviceListResponse, ApiError> {
        self.call(reqwest::Method::GET, "/auth/me/devices", None, Some(token), false).await
    }

    pub async fn rename_device(
        &self,
        token: &str,
        device_id: &str,
        name: Option<&str>,
        password: Option<&str>,
    ) -> Result<DeviceDto, ApiError> {
        let body = json!({ "name": name, "password": password });
        self.call(reqwest::Method::PATCH, &format!("/auth/me/devices/{device_id}"), Some(body), Some(token), false).await
    }

    pub async fn revoke_device(&self, token: &str, device_id: &str, password: Option<&str>) -> Result<(), ApiError> {
        let body = password.map(|p| json!({ "password": p }));
        self.call_empty(reqwest::Method::POST, &format!("/auth/me/devices/{device_id}/revoke"), body, Some(token), false).await
    }

    pub async fn revoke_others(&self, token: &str, password: Option<&str>) -> Result<RevokeOthersResponse, ApiError> {
        let body = password.map(|p| json!({ "password": p }));
        self.call(reqwest::Method::POST, "/auth/me/devices/revoke-others", body, Some(token), false).await
    }

    pub async fn change_password(
        &self,
        token: &str,
        current: Option<&str>,
        new: &str,
        sign_out_others: bool,
    ) -> Result<ChangePasswordResponse, ApiError> {
        let body = json!({ "currentPassword": current, "newPassword": new, "signOutOtherDevices": sign_out_others });
        self.call(reqwest::Method::POST, "/auth/me/password", Some(body), Some(token), false).await
    }

    pub async fn rotate_recovery_code(&self, token: &str, password: &str) -> Result<RecoveryCodeResponse, ApiError> {
        self.call(reqwest::Method::POST, "/auth/me/recovery-code", Some(json!({ "password": password })), Some(token), false).await
    }

    pub async fn confirm_recovery_code(&self, token: &str, created_at: &str) -> Result<(), ApiError> {
        let body = json!({ "recoveryCodeCreatedAt": created_at });
        self.call_empty(reqwest::Method::POST, "/auth/me/recovery-code/confirm", Some(body), Some(token), false).await
    }

    pub async fn delete_account(&self, token: &str, password: &str) -> Result<(), ApiError> {
        self.call_empty(reqwest::Method::POST, "/auth/me/delete", Some(json!({ "password": password })), Some(token), false).await
    }

    // ── вход нового устройства по коду (§4.6, режим request) ──

    pub async fn resolve_link(&self, token: &str, user_code: &str) -> Result<LinkDetails, ApiError> {
        self.call(reqwest::Method::POST, "/auth/me/links/resolve", Some(json!({ "userCode": user_code })), Some(token), false).await
    }

    pub async fn approve_link(&self, token: &str, link_id: &str, verify_code: &str) -> Result<LinkDecisionResponse, ApiError> {
        let body = json!({ "verifyCode": verify_code });
        self.call(reqwest::Method::POST, &format!("/auth/me/links/{link_id}/approve"), Some(body), Some(token), false).await
    }

    pub async fn deny_link(&self, token: &str, link_id: &str) -> Result<LinkDecisionResponse, ApiError> {
        self.call(reqwest::Method::POST, &format!("/auth/me/links/{link_id}/deny"), None, Some(token), false).await
    }

    // ── вход по коду, устройство новое (§4.6) ──

    /// Режим `request`: это устройство показывает код (тот же `DeviceInput`, что у входа по паролю).
    pub async fn create_link_request(&self, device: &DeviceInput) -> Result<LinkCreated, ApiError> {
        self.call(reqwest::Method::POST, "/auth/link/requests", Some(json!({ "device": to_value(device) })), None, false).await
    }

    /// Режим `invite`: код, который показывает вошедшее устройство.
    pub async fn claim_link(&self, user_code: &str, device: &DeviceInput) -> Result<LinkClaimed, ApiError> {
        let body = json!({ "userCode": user_code, "device": to_value(device) });
        self.call(reqwest::Method::POST, "/auth/link/claim", Some(body), None, false).await
    }

    /// Длинный опрос: сервер держит ответ до 25 с, таймаут запроса — 35 с (§4.6).
    pub async fn poll_link(&self, poll_secret: &str, known_status: &str) -> Result<LinkPollResponse, ApiError> {
        let body = json!({ "pollSecret": poll_secret, "knownStatus": known_status });
        let response = self.raw(reqwest::Method::POST, "/auth/link/poll", Some(&body), None, false, Some(LINK_POLL_TIMEOUT)).await?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| ApiError::new(0, "network", e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| ApiError::new(status, "invalid_response", e.to_string()))
    }

    pub async fn cancel_link_request(&self, poll_secret: &str) -> Result<(), ApiError> {
        self.call_empty(reqwest::Method::POST, "/auth/link/cancel", Some(json!({ "pollSecret": poll_secret })), None, false).await
    }

    // ── вход по коду, устройство вошло: приглашение (§4.6, режим invite) ──

    pub async fn create_invite(&self, token: &str) -> Result<LinkCreated, ApiError> {
        self.call(reqwest::Method::POST, "/auth/me/links", Some(json!({})), Some(token), false).await
    }

    pub async fn link(&self, token: &str, link_id: &str) -> Result<LinkDetails, ApiError> {
        self.call(reqwest::Method::GET, &format!("/auth/me/links/{link_id}"), None, Some(token), false).await
    }

    pub async fn cancel_invite(&self, token: &str, link_id: &str) -> Result<(), ApiError> {
        self.call_empty(reqwest::Method::POST, &format!("/auth/me/links/{link_id}/cancel"), Some(json!({})), Some(token), false).await
    }

    // ── синхронизация (§4.7–§4.8) ──

    pub async fn merge_plan(&self, token: &str, playlists: &[MergePlanInput]) -> Result<MergePlanResponse, ApiError> {
        self.call(reqwest::Method::POST, "/sync/merge-plan", Some(json!({ "playlists": to_value(&playlists) })), Some(token), true).await
    }

    pub async fn sync(&self, token: &str, request: &SyncRequest) -> Result<SyncResponse, ApiError> {
        self.call(reqwest::Method::POST, "/sync", Some(to_value(request)), Some(token), true).await
    }

    // ── тексты песен (§4.10) ──

    pub async fn put_lyrics(&self, token: &str, video_id: &str, text: &LyricsText) -> Result<MyLyrics, ApiError> {
        self.call(reqwest::Method::PUT, &format!("/lyrics/{video_id}"), Some(to_value(text)), Some(token), false).await
    }

    pub async fn delete_lyrics(&self, token: &str, video_id: &str) -> Result<(), ApiError> {
        self.call_empty(reqwest::Method::DELETE, &format!("/lyrics/{video_id}"), None, Some(token), false).await
    }

    pub async fn lyrics(&self, token: &str, video_id: &str) -> Result<LyricsResponse, ApiError> {
        self.call(reqwest::Method::GET, &format!("/lyrics/{video_id}"), None, Some(token), false).await
    }

    pub async fn lyrics_changes(&self, token: &str, after: i64) -> Result<MyLyricsPage, ApiError> {
        self.call(reqwest::Method::POST, "/auth/me/lyrics/changes", Some(json!({ "after": after })), Some(token), false).await
    }

    // ── живые события (§6) ──

    /// Поток событий: кадры `id:` + `data:` без `event:`, heartbeat — комментарий. Заканчивается,
    /// когда сервер его закрывает (истёк токен, отзыв) — вызывающий переоткрывает с паузой.
    ///
    /// `remote`: устройство разрешает управлять собой с других (`?remote=1`, §6) и получает `playback.command`.
    pub async fn events(&self, token: &str, remote: bool, mut on_event: impl FnMut(LiveEvent)) -> Result<(), ApiError> {
        let response = self
            .stream
            .get(format!("{}/auth/me/events{}", self.base, if remote { "?remote=1" } else { "" }))
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(|e| ApiError::new(0, "network", e.to_string()))?;
        if !response.status().is_success() {
            return Err(error_from(response).await);
        }
        let mut parser = SseParser::default();
        let mut body = response.bytes_stream();
        while let Some(chunk) = body.next().await {
            let Ok(chunk) = chunk else { return Ok(()) };
            for event in parser.push(&chunk) {
                on_event(event);
            }
        }
        Ok(())
    }
}

fn to_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// Ответ-отказ: код из конверта; не JSON (Caddy, шлюз) — решает статус (§1.2).
async fn error_from(response: reqwest::Response) -> ApiError {
    let status = response.status().as_u16();
    let retry_header =
        response.headers().get(reqwest::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok());
    let text = response.text().await.unwrap_or_default();
    match serde_json::from_str::<ErrorEnvelope>(&text) {
        Ok(envelope) if !envelope.code.is_empty() => ApiError {
            status,
            code: envelope.code,
            message: envelope.message,
            retry_after_seconds: envelope.retry_after_seconds.or(retry_header),
        },
        _ => ApiError { status, code: format!("http_{status}"), message: String::new(), retry_after_seconds: retry_header },
    }
}

/// Разбор `text/event-stream` по кускам сети: событие — строки `data:` до пустой строки.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
    data: String,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<LiveEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    if let Ok(event) = serde_json::from_str::<LiveEvent>(&self.data) {
                        events.push(event);
                    }
                    self.data.clear();
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_frames_split_anywhere() {
        let stream = "retry: 5000\n\nid: 1f2e\ndata: {\"id\":\"1f2e\",\"type\":\"system.connected\",\"at\":\"2026-09-30T08:00:00.200Z\",\"payload\":{\"heartbeatMs\":25000}}\n\n: heartbeat 1790157625000\n\nid: 2\r\ndata: {\"id\":\"2\",\"type\":\"sync.changed\",\"at\":\"x\",\"payload\":{\"cursor\":\"a.1.1\"}}\r\n\r\n";
        for split in [1, 7, 33, stream.len()] {
            let mut parser = SseParser::default();
            let mut events = Vec::new();
            for chunk in stream.as_bytes().chunks(split) {
                events.extend(parser.push(chunk));
            }
            let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
            assert_eq!(kinds, ["system.connected", "sync.changed"], "куски по {split}");
            assert_eq!(events[1].payload["cursor"], "a.1.1");
        }
    }
}
