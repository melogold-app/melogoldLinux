//! Типы контракта `melogoldServer/docs/API.md` §4, §6. Ответы разбираются терпимо (§1.3):
//! неизвестные поля и значения перечислений не ломают разбор, недостающее — значение по умолчанию.
//! Запросы не отправляют пустых необязательных полей.

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── общие (§4.1) ──

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ArtistRefDto {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TrackDto {
    pub video_id: String,
    pub title: String,
    pub artists_text: Option<String>,
    pub artists: Vec<ArtistRefDto>,
    pub album_id: Option<String>,
    pub album_title: Option<String>,
    pub duration_ms: Option<i64>,
    pub duration_text: Option<String>,
    pub thumbnail_url: Option<String>,
    pub explicit: bool,
    pub video_type: Option<String>,
    pub metadata_stub: bool,
}

/// Метаданные трека в op (мягкий разбор на сервере, DESIGN §3.9).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrackInput {
    pub video_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artists_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artists: Option<Vec<ArtistRefDto>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explicit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_type: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInput {
    pub hwid: String,
    pub name: String,
    pub platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DevicePatch {
    pub hwid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceDto {
    pub id: String,
    pub name: String,
    pub reported_name: String,
    pub custom_name: Option<String>,
    pub platform: String,
    pub os_version: Option<String>,
    pub model: Option<String>,
    pub client_version: Option<String>,
    pub linked_via: String,
    pub linked_by_device_id: Option<String>,
    pub created_at: String,
    pub last_seen_at: String,
    pub last_sync_at: Option<String>,
    pub recent_until: Option<String>,
    pub is_current: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecoveryCodeStatus {
    pub created_at: String,
    pub confirmed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct UserDto {
    pub id: String,
    pub login: String,
    pub created_at: String,
    pub password_changed_at: String,
    pub recovery_code_status: RecoveryCodeStatus,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TokenPair {
    pub access_token: String,
    pub access_token_expires_at: String,
    pub refresh_token: String,
    pub refresh_token_expires_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AuthSession {
    pub user: UserDto,
    pub device: DeviceDto,
    pub tokens: TokenPair,
    pub server_id: String,
    pub server_time: String,
    pub recovery_code: Option<String>,
    pub signed_out_devices: i64,
}

// ── сервер (§4.2) ──

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncFeature {
    pub protocol: i64,
    pub min_protocol: i64,
    pub kinds: Vec<String>,
    pub streams: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct VersionFeature {
    pub version: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Features {
    pub sync: Option<SyncFeature>,
    pub playback: Option<VersionFeature>,
    pub device_linking: Option<VersionFeature>,
    pub recovery_code: Option<VersionFeature>,
    pub export: Option<VersionFeature>,
    pub account_deletion: Option<VersionFeature>,
    pub registration_pow: Option<VersionFeature>,
    pub lyrics: Option<VersionFeature>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerInfo {
    pub software: String,
    pub version: String,
    pub revision: String,
    pub api_version: i64,
    pub min_api_version: i64,
    pub server_id: String,
    pub instance_name: String,
    pub public_url: Option<String>,
    pub secure_transport: bool,
    pub registration: String,
    pub features: Features,
    /// `ServerLimits` (§11) — читается по пути: `limits.history.mergeUploadMax`.
    pub limits: Value,
    pub server_time: String,
}

impl ServerInfo {
    /// Сервер принимает ops этого вида (`features.sync.kinds`); иначе клиент их не создаёт (§4.2).
    pub fn supports(&self, kind: &str) -> bool {
        self.features.sync.as_ref().is_some_and(|sync| sync.kinds.iter().any(|k| k == kind))
    }

    pub fn limit(&self, path: &[&str]) -> Option<i64> {
        path.iter().try_fold(&self.limits, |value, key| value.get(key))?.as_i64()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RegisterChallenge {
    pub challenge: String,
    pub bits: u32,
    pub expires_at: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct PowSolution {
    pub challenge: String,
    pub nonce: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RefreshResponse {
    pub tokens: TokenPair,
    pub device: DeviceDto,
    pub server_id: String,
    pub server_time: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MeResponse {
    pub user: UserDto,
    pub device: DeviceDto,
    pub server_id: String,
    pub server_time: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceListResponse {
    pub devices: Vec<DeviceDto>,
    pub max_devices: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RevokeOthersResponse {
    pub revoked_count: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ChangePasswordResponse {
    pub user: UserDto,
    pub tokens: TokenPair,
    pub signed_out_devices: i64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RecoveryCodeResponse {
    pub recovery_code: String,
    pub created_at: String,
}

// ── привязка устройств (§4.6) ──

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LinkDeviceInfo {
    pub name: String,
    pub platform: String,
    pub os_version: Option<String>,
    pub model: Option<String>,
    pub client_version: Option<String>,
    pub already_linked: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LinkDetails {
    pub link_id: String,
    pub mode: String,
    pub status: String,
    pub created_at: String,
    pub expires_at: String,
    pub device: Option<LinkDeviceInfo>,
    pub same_network: Option<bool>,
    pub verify_choices: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LinkDecisionResponse {
    pub link_id: String,
    pub status: String,
}

// ── синхронизация (§4.7–§4.8) ──

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct MergePlanInput {
    #[serde(rename = "localKey")]
    pub local_key: String,
    #[serde(rename = "syncId", skip_serializing_if = "Option::is_none")]
    pub sync_id: Option<String>,
    pub name: String,
    #[serde(rename = "browseId", skip_serializing_if = "Option::is_none")]
    pub browse_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MergePlanEntry {
    pub local_key: String,
    pub action: String,
    pub playlist_id: String,
    pub server_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MergePlanResponse {
    pub plan: Vec<MergePlanEntry>,
}

/// Запрос `POST /sync`: ops — готовые объекты (плоская схема `SyncOp`).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct SyncRequest {
    pub cursor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    pub streams: Vec<String>,
    pub ops: Vec<Value>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct OpResult {
    pub op_id: String,
    pub status: String,
    pub code: Option<String>,
    pub seq: Option<i64>,
    pub playlist_id: Option<String>,
    pub retry_after_seconds: Option<i64>,
    pub replayed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PlaylistRow {
    pub id: String,
    pub name: String,
    pub browse_id: Option<String>,
    pub thumbnail_url: Option<String>,
    pub created_at: String,
    pub deleted: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PlaylistItemRow {
    pub playlist_id: String,
    pub video_id: String,
    pub present: bool,
    pub sort_key: String,
    pub added_at: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LikeRow {
    pub video_id: String,
    pub liked: bool,
    pub liked_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct BookmarkRow {
    #[serde(rename = "type")]
    pub kind: String,
    pub browse_id: String,
    pub bookmarked: bool,
    pub bookmarked_at: Option<String>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub thumbnail_url: Option<String>,
    pub year: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TrackOverrideRow {
    pub video_id: String,
    pub title: Option<String>,
    pub artists_text: Option<String>,
    pub album_title: Option<String>,
    pub updated_at: String,
    pub deleted: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayRow {
    pub event_id: String,
    pub video_id: String,
    pub played_at: String,
    pub play_time_ms: i64,
    pub device_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayStatRow {
    pub video_id: String,
    pub total_play_time_ms: i64,
    pub last_played_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayForgetRow {
    pub video_id: String,
    pub events_before: String,
    pub total_before: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncResponse {
    pub results: Vec<OpResult>,
    pub cursor: String,
    pub has_more: bool,
    pub server_time: String,
    pub tracks: Vec<TrackDto>,
    pub playlists: Vec<PlaylistRow>,
    pub items: Vec<PlaylistItemRow>,
    pub likes: Vec<LikeRow>,
    pub bookmarks: Vec<BookmarkRow>,
    pub overrides: Vec<TrackOverrideRow>,
    pub plays: Vec<PlayRow>,
    pub play_stats: Vec<PlayStatRow>,
    pub play_forgets: Vec<PlayForgetRow>,
}

// ── ошибки и живые события (§2.1, §6) ──

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ErrorEnvelope {
    pub status_code: i64,
    pub message: String,
    pub code: String,
    pub retry_after_seconds: Option<i64>,
    pub device_limit: Option<i64>,
    pub device_count: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LiveEvent {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub at: String,
    pub payload: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_contract_examples_and_ignores_unknown_fields() {
        let session: AuthSession = serde_json::from_str(r#"{"user":{"id":"0c3f6a2e-5d1b-4c7a-9e8f-1a2b3c4d5e6f","login":"maxim","createdAt":"2026-09-23T10:00:00.000Z","passwordChangedAt":"2026-09-23T10:00:00.000Z","recoveryCodeStatus":{"createdAt":"2026-09-23T10:00:00.000Z","confirmed":false}},
 "device":{"id":"9b1e2f4a-7c3d-4e5f-8a9b-0c1d2e3f4a5b","name":"Google Pixel 8","reportedName":"Google Pixel 8","customName":null,"platform":"android","osVersion":"16","model":"Google Pixel 8","clientVersion":"1.3.0","linkedVia":"register","linkedByDeviceId":null,"createdAt":"2026-09-23T10:00:00.000Z","lastSeenAt":"2026-09-23T10:00:00.000Z","lastSyncAt":null,"recentUntil":null,"isCurrent":true},
 "tokens":{"accessToken":"a","accessTokenExpiresAt":"2026-09-23T10:15:00.000Z","refreshToken":"mgrt1.x.y","refreshTokenExpiresAt":"2026-12-22T10:00:00.000Z"},
 "serverId":"6f1c2c0e-8a3b-4f7e-9c1d-2b5e7a9f0c11","serverTime":"2026-09-23T10:00:00.000Z","recoveryCode":"7KQ2-MX9D-4TNP-B8RW-3HZF","signedOutDevices":0,"future":{"x":1}}"#).unwrap();
        assert_eq!(session.user.login, "maxim");
        assert_eq!(session.recovery_code.as_deref(), Some("7KQ2-MX9D-4TNP-B8RW-3HZF"));
        assert!(session.device.is_current);

        let response: SyncResponse = serde_json::from_str(r#"{"results":[{"opId":"3f0c1d2e-4a5b-4c6d-8e7f-9a0b1c2d3e4f","status":"applied","code":null,"seq":4802,"playlistId":null,"retryAfterSeconds":null,"replayed":false}],
 "cursor":"a1b2c3d4.4809.4809","hasMore":false,"serverTime":"2026-09-23T10:00:01.004Z",
 "tracks":[{"videoId":"abcdefghijk","title":"abcdefghijk","artistsText":null,"artists":[],"albumId":null,"albumTitle":null,"durationMs":null,"durationText":null,"thumbnailUrl":null,"explicit":false,"videoType":null,"metadataStub":true}],
 "playlists":[],"items":[{"playlistId":"c9d1e2f3-a4b5-5c6d-8e7f-9a0b1c2d3e4f","videoId":"abcdefghijk","present":true,"sortKey":"a0","addedAt":"2026-09-23T10:00:01.000Z"}],
 "likes":[{"videoId":"a1B2c3D4e5F","liked":true,"likedAt":"2026-09-23T10:00:00.123Z"}],
 "bookmarks":[{"type":"album","browseId":"MPREb_x","bookmarked":true,"bookmarkedAt":null,"title":null,"subtitle":null,"thumbnailUrl":null,"year":null}],
 "overrides":[],"lyricsPins":[],"plays":[],"playStats":[{"videoId":"a1B2c3D4e5F","totalPlayTimeMs":1484000,"lastPlayedAt":"2026-09-23T09:58:10.000Z"}],
 "playForgets":[{"videoId":"abcdefghijk","eventsBefore":"2026-09-23T10:00:05.000Z","totalBefore":null}]}"#).unwrap();
        assert_eq!(response.results[0].status, "applied");
        assert!(response.tracks[0].metadata_stub);
        assert_eq!(response.bookmarks[0].kind, "album");
        assert_eq!(response.play_stats[0].total_play_time_ms, 1_484_000);

        let info: ServerInfo = serde_json::from_str(r#"{"software":"melogold-server","version":"0.1.1","apiVersion":1,"minApiVersion":1,"serverId":"s","instanceName":"Melogold","registration":"open",
 "features":{"sync":{"protocol":1,"minProtocol":1,"kinds":["like.set","track.override.set"],"streams":["library","history"]},"lyrics":{"version":1},"newThing":{"version":9}},
 "limits":{"history":{"mergeUploadMax":20000}}}"#).unwrap();
        assert!(info.supports("track.override.set"));
        assert!(!info.supports("lyrics.pin.set"));
        assert_eq!(info.limit(&["history", "mergeUploadMax"]), Some(20000));
        assert_eq!(info.features.lyrics.map(|l| l.version), Some(1));
    }
}
