//! Синхронизация библиотеки и истории (API §4.8, Windows `LibrarySync.cs`): Избранное, плейлисты с
//! порядком, сохранённые альбомы, исполнители и каналы, свои названия треков, закреплённые тексты,
//! прослушивания; свои тексты песен — отдельными маршрутами (§4.10).
//!
//! **Вариант со снимком** (REWRITE §4.12a Android): вместо журнала правок библиотека сравнивается с
//! тем, что было на сервере после прошлой синхронизации (таблицы `synced_*`), разница уходит ops,
//! ответ сервера обновляет и библиотеку, и снимок. Каждая op несёт `base` — курсор снимка: она
//! выигрывает у того, что устройство видело, и сравнивается по времени с тем, чего не видело.
//!
//! Синхронизация одна за раз: при входе и запуске, через 2 с после правки библиотеки, по
//! `sync.changed` и `system.connected`, когда возвращается сеть, по «Синхронизировать сейчас» и
//! перед выходом.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use melogold_core::ids::new_uuid;
use melogold_core::iso;
use melogold_core::lyrics::pins::LyricsPin;
use melogold_core::lyrics::sync_rules::{self, LyricsPayload, LyricsSend, StoredLyrics, REJECTED};
use melogold_core::music::{ArtistRef, Track};
use melogold_core::playlist_diff::{self, ItemChange};
use melogold_core::text::{now_ms, truncate_utf16};
use melogold_data::overrides::TrackOverride;
use melogold_data::sync_store::{SyncTx, SyncedPlaylist};
use melogold_data::{Change, Library};
use serde_json::{json, Map, Value};

use crate::account::{Account, AccountState};
use crate::api::ApiError;
use crate::dto::*;

const MAX_OPS: usize = 500;
const NAME_MAX: usize = 200;
const LOCAL_CHANGE_DELAY: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(300);
const FEATURES_MAX_AGE: Duration = Duration::from_secs(1800);
/// Сколько последних прослушиваний отправить при первой синхронизации, если сервер не сказал.
const DEFAULT_MERGE_UPLOAD_MAX: usize = 20_000;
const MAX_PLAY_TIME_MS: i64 = 86_400_000;
const BASELINE_CHUNK: usize = 500;

const KEY_BINDING: &str = "binding";
const KEY_CURSOR: &str = "cursor";
const KEY_MERGE: &str = "needsMerge";
const KEY_LAST_SYNC: &str = "lastSyncAt";
const KEY_HISTORY_MERGE: &str = "historyMerge";
const KEY_HISTORY_RETRY_AT: &str = "historyRetryAt";
const KEY_LYRICS_REV: &str = "lyricsRev";

/// Что делает синхронизация — для Настроек и экрана аккаунта.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    /// Нет аккаунта.
    Off,
    Idle {
        last_sync_at: Option<i64>,
    },
    Syncing,
    Failed {
        offline: bool,
        last_sync_at: Option<i64>,
    },
}

type StatusListener = Box<dyn Fn(&SyncStatus) + Send + Sync>;
type Listener = Box<dyn Fn() + Send + Sync>;
type VideoListener = Box<dyn Fn(&str) + Send + Sync>;
type EventListener = Box<dyn Fn(&LiveEvent) + Send + Sync>;

pub struct LibrarySync {
    account: Arc<Account>,
    library: Arc<Library>,
    runtime: tokio::runtime::Handle,
    mutex: tokio::sync::Mutex<()>,
    status: Mutex<SyncStatus>,
    status_listeners: Mutex<Vec<StatusListener>>,
    devices_listeners: Mutex<Vec<Listener>>,
    rejected_listeners: Mutex<Vec<VideoListener>>,
    /// Все живые события как есть: вход по коду (`link.updated`), пульт (`playback.*`).
    event_listeners: Mutex<Vec<EventListener>>,
    /// Разрешено ли управлять этим устройством с других: SSE с `remote=1` (§6, задание 0011).
    remote_control: AtomicBool,
    /// Поколение сессии: смена аккаунта или сети останавливает живые события прошлой.
    session: AtomicU64,
    debounce: AtomicU64,
    pending_local: AtomicBool,
    online: AtomicBool,
    features_at: Mutex<Option<Instant>>,
}

/// Одна op `POST /sync` и её ключ (для результата).
struct Op {
    kind: &'static str,
    key: String,
    json: Value,
}

impl LibrarySync {
    pub fn new(account: Arc<Account>, library: Arc<Library>, runtime: tokio::runtime::Handle) -> Arc<LibrarySync> {
        Arc::new(LibrarySync {
            account,
            library,
            runtime,
            mutex: tokio::sync::Mutex::new(()),
            status: Mutex::new(SyncStatus::Off),
            status_listeners: Mutex::default(),
            devices_listeners: Mutex::default(),
            rejected_listeners: Mutex::default(),
            event_listeners: Mutex::default(),
            remote_control: AtomicBool::new(true),
            session: AtomicU64::new(0),
            debounce: AtomicU64::new(0),
            pending_local: AtomicBool::new(false),
            online: AtomicBool::new(true),
            features_at: Mutex::new(None),
        })
    }

    pub fn start(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        self.account.subscribe(move |state| {
            if let Some(this) = weak.upgrade() {
                this.on_account_changed(state);
            }
        });
        let weak = Arc::downgrade(self);
        self.library.subscribe(move |change| {
            if let Some(this) = weak.upgrade() {
                this.on_library_changed(change);
            }
        });
        self.on_account_changed(&self.account.state());
    }

    pub fn status(&self) -> SyncStatus {
        self.status.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn subscribe_status(&self, listener: impl Fn(&SyncStatus) + Send + Sync + 'static) {
        self.status_listeners.lock().unwrap_or_else(|p| p.into_inner()).push(Box::new(listener));
    }

    /// Список устройств изменился на сервере (`devices.updated`).
    pub fn subscribe_devices(&self, listener: impl Fn() + Send + Sync + 'static) {
        self.devices_listeners.lock().unwrap_or_else(|p| p.into_inner()).push(Box::new(listener));
    }

    /// Свой текст не ушёл на сервер: он длиннее лимита (§4.10) и остаётся только здесь.
    pub fn subscribe_lyrics_rejected(&self, listener: impl Fn(&str) + Send + Sync + 'static) {
        self.rejected_listeners.lock().unwrap_or_else(|p| p.into_inner()).push(Box::new(listener));
    }

    /// Живое событие сервера (§6), любое. Слушатель не должен ждать: работа — в свой поток.
    pub fn subscribe_events(&self, listener: impl Fn(&LiveEvent) + Send + Sync + 'static) {
        self.event_listeners.lock().unwrap_or_else(|p| p.into_inner()).push(Box::new(listener));
    }

    /// «Управление с других устройств» (Настройки › Плеер): переоткрывает поток событий с `remote=1` или без.
    pub fn set_remote_control(self: &Arc<Self>, allowed: bool) {
        if self.remote_control.swap(allowed, Ordering::SeqCst) != allowed && matches!(self.account.state(), AccountState::SignedIn { .. }) {
            self.on_account_changed(&self.account.state());
        }
    }

    pub fn remote_control(&self) -> bool {
        self.remote_control.load(Ordering::SeqCst)
    }

    fn lyrics_rejected(&self, video_id: &str) {
        for listener in self.rejected_listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            listener(video_id);
        }
    }

    fn set_status(&self, status: SyncStatus) {
        {
            let mut current = self.status.lock().unwrap_or_else(|p| p.into_inner());
            if *current == status {
                return;
            }
            *current = status.clone();
        }
        for listener in self.status_listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            listener(&status);
        }
    }

    fn last_sync_at(&self) -> Option<i64> {
        self.library.sync_state(KEY_LAST_SYNC).and_then(|v| v.parse().ok())
    }

    fn on_account_changed(self: &Arc<Self>, state: &AccountState) {
        let generation = self.session.fetch_add(1, Ordering::SeqCst) + 1;
        if !matches!(state, AccountState::SignedIn { .. }) {
            self.set_status(SyncStatus::Off);
            return;
        }
        if !self.online.load(Ordering::SeqCst) {
            // Без сети — ничего не пробовать; когда она вернётся, всё начнётся заново.
            self.set_status(SyncStatus::Failed { offline: true, last_sync_at: self.last_sync_at() });
            return;
        }
        self.set_status(SyncStatus::Idle { last_sync_at: self.last_sync_at() });
        let this = Arc::clone(self);
        self.runtime.spawn(async move {
            this.sync(true).await;
        });
        let this = Arc::clone(self);
        self.runtime.spawn(async move { this.follow_live_events(generation).await });
    }

    /// Сеть пропала или вернулась (GNetworkMonitor): с сетью всё начинается заново.
    pub fn network_changed(self: &Arc<Self>, available: bool) {
        let was = self.online.swap(available, Ordering::SeqCst);
        if was == available || !matches!(self.account.state(), AccountState::SignedIn { .. }) {
            return;
        }
        if available {
            self.on_account_changed(&self.account.state());
        } else {
            self.session.fetch_add(1, Ordering::SeqCst);
            self.set_status(SyncStatus::Failed { offline: true, last_sync_at: self.last_sync_at() });
        }
    }

    /// Правка Избранного, плейлистов, закладок, правок, текстов или истории уходит через 2 с.
    fn on_library_changed(self: &Arc<Self>, change: Change) {
        let mask = Change::LIKES.0 | Change::PLAYLISTS.0 | Change::BOOKMARKS.0 | Change::HISTORY.0 | Change::OVERRIDES.0 | Change::LYRICS.0;
        if change.0 & mask == 0 || !matches!(self.account.state(), AccountState::SignedIn { .. }) {
            return;
        }
        self.pending_local.store(true, Ordering::SeqCst);
        let ticket = self.debounce.fetch_add(1, Ordering::SeqCst) + 1;
        let this = Arc::clone(self);
        self.runtime.spawn(async move {
            tokio::time::sleep(LOCAL_CHANGE_DELAY).await;
            if this.debounce.load(Ordering::SeqCst) == ticket {
                this.sync(false).await;
            }
        });
    }

    /// Перед выходом: ещё не отправленные правки — одной синхронизацией, не дольше `timeout`.
    pub async fn flush(&self, timeout: Duration) {
        if !self.pending_local.load(Ordering::SeqCst) || self.account.session().is_none() {
            return;
        }
        let _ = tokio::time::timeout(timeout, self.sync(false)).await;
    }

    /// Синхронизировать сейчас; `force` — спросить сервер, даже если здесь ничего не менялось.
    pub async fn sync(&self, force: bool) -> bool {
        if self.account.session().is_none() {
            return true;
        }
        let _guard = self.mutex.lock().await;
        let last = self.last_sync_at();
        self.set_status(SyncStatus::Syncing);
        self.pending_local.store(false, Ordering::SeqCst);
        match self.sync_once(force).await {
            Ok(()) => {
                self.set_status(SyncStatus::Idle { last_sync_at: self.last_sync_at() });
                true
            }
            Err(error) => {
                tracing::warn!(%error, "синхронизация не прошла");
                let status = if self.account.session().is_none() {
                    SyncStatus::Off
                } else {
                    SyncStatus::Failed { offline: error.is_network(), last_sync_at: last }
                };
                self.set_status(status);
                false
            }
        }
    }

    async fn sync_once(&self, force: bool) -> Result<(), ApiError> {
        let Some(session) = self.account.session() else { return Ok(()) };
        let info = self.server_info().await;
        let binding = format!("{}:{}", session.server_id, session.user_id);
        self.db(|tx| {
            if tx.state(KEY_BINDING)?.as_deref() == Some(binding.as_str()) {
                return Ok(());
            }
            // Другой аккаунт или сервер: здесь с ним ничего не синхронизировано, уходит (и сливается) всё.
            tx.forget_binding()?;
            tx.set_state(KEY_BINDING, Some(&binding))?;
            tx.set_state(KEY_MERGE, Some("1"))?;
            tx.set_state(KEY_HISTORY_MERGE, Some("1"))
        })?;
        if self.library.sync_state(KEY_MERGE).as_deref() == Some("1") {
            self.plan_merge().await?;
        }
        let ops = self.db(|tx| build_ops(tx, info.as_ref()))?;
        let sent = !ops.is_empty();
        if sent || force {
            self.sync_library(ops).await?;
        }
        // После всех прослушиваний первой синхронизации — накопленное время (play.baseline): раньше
        // его нельзя, иначе отложенные лимитом play.add прибавились бы к нему ещё раз.
        if sent && self.library.sync_state(KEY_HISTORY_MERGE).as_deref() == Some("1") {
            let more = self.db(|tx| build_ops(tx, info.as_ref()))?;
            if !more.is_empty() {
                self.sync_library(more).await?;
            }
        }
        self.sync_lyrics(force, info.as_ref()).await
    }

    /// Свои тексты (§4.10, Windows `SyncLyricsAsync`): сначала изменившиеся со снимка — `PUT` и
    /// `DELETE`, затем свои версии с сервера после `lyricsRev`. Без своих правок и без `force` сеть
    /// не трогается; без модуля текстов на сервере (`features.lyrics`) — тоже.
    async fn sync_lyrics(&self, force: bool, info: Option<&ServerInfo>) -> Result<(), ApiError> {
        let sends = self.db(|tx| Ok(sync_rules::plan_sends(&tx.own_lyrics()?, &tx.synced_lyrics()?)))?;
        if (sends.is_empty() && !force) || !info.is_some_and(lyrics_available) {
            return Ok(());
        }
        for send in sends {
            match send {
                LyricsSend::Put { video_id, payload, hash } if sync_rules::too_large(&payload) => {
                    self.db(|tx| tx.set_synced_lyrics(&video_id, REJECTED, &hash))?;
                    self.lyrics_rejected(&video_id);
                }
                LyricsSend::Put { video_id, payload, hash } => {
                    let text = LyricsText::from(&payload);
                    let result = self
                        .account
                        .authorized(|api, token| {
                            let (text, video_id) = (text.clone(), video_id.clone());
                            async move { api.put_lyrics(&token, &video_id, &text).await }
                        })
                        .await;
                    match result {
                        Ok(mine) => self.db(|tx| tx.set_synced_lyrics(&video_id, mine.rev, &hash))?,
                        // Не отправлять снова, пока текст не изменится; слишком большой — сказать человеку.
                        Err(error) if error.status == 413 || error.status == 400 => {
                            tracing::warn!(%error, video_id, "сервер не принял текст");
                            self.db(|tx| tx.set_synced_lyrics(&video_id, REJECTED, &hash))?;
                            if error.status == 413 {
                                self.lyrics_rejected(&video_id);
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
                LyricsSend::Delete { video_id } => {
                    let result = self
                        .account
                        .authorized(|api, token| {
                            let video_id = video_id.clone();
                            async move { api.delete_lyrics(&token, &video_id).await }
                        })
                        .await;
                    match result {
                        Ok(()) => {}
                        Err(error) if error.status == 404 => {}
                        Err(error) => return Err(error),
                    }
                    self.db(|tx| tx.forget_synced_lyrics(&video_id))?;
                }
                LyricsSend::Forget { video_id } => self.db(|tx| tx.forget_synced_lyrics(&video_id))?,
            }
        }
        let mut after: i64 = self.library.sync_state(KEY_LYRICS_REV).and_then(|v| v.parse().ok()).unwrap_or(0);
        loop {
            let page = self.account.authorized(|api, token| async move { api.lyrics_changes(&token, after).await }).await?;
            self.db(|tx| {
                for item in &page.items {
                    apply_lyrics(tx, item)?;
                }
                tx.set_state(KEY_LYRICS_REV, Some(&page.rev.to_string()))
            })?;
            if !page.more || page.rev <= after {
                return Ok(());
            }
            after = page.rev;
        }
    }

    /// Текст с сервера для цепочки поиска, когда поставщики не нашли синхронный: своя версия (ещё не
    /// пришла синком) или общая другого пользователя. `Ok(None)` — модуля нет, текста нет, не вошли;
    /// `Err` — нет связи (такой пустой итог не кэшируется как «текста нет»).
    pub async fn lookup_lyrics(&self, video_id: &str) -> Result<Option<(LyricsPayload, bool)>, ApiError> {
        if self.account.session().is_none() || !self.server_info().await.as_ref().is_some_and(lyrics_available) {
            return Ok(None);
        }
        let response = match self
            .account
            .authorized(|api, token| {
                let video_id = video_id.to_owned();
                async move { api.lyrics(&token, &video_id).await }
            })
            .await
        {
            Ok(response) => response,
            Err(error) if error.is_network() => return Err(error),
            Err(error) => {
                tracing::warn!(%error, video_id, "текст с сервера не пришёл");
                return Ok(None);
            }
        };
        if let Some(text) = response.mine.filter(|m| !m.deleted).and_then(|m| m.text) {
            return Ok(Some((LyricsPayload::from(&text), true)));
        }
        Ok(response.shared.and_then(|s| s.text).map(|text| (LyricsPayload::from(&text), false)))
    }

    /// `/server/info` раз в 30 минут: какие виды ops сервер принимает, лимиты истории.
    async fn server_info(&self) -> Option<ServerInfo> {
        let stale = self.features_at.lock().unwrap_or_else(|p| p.into_inner()).is_none_or(|at| at.elapsed() > FEATURES_MAX_AGE);
        if self.account.server_info().is_none() || stale {
            match self.account.check(&self.account.server_url()).await {
                Ok(_) => *self.features_at.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now()),
                Err(error) => tracing::warn!(%error, "/server/info не ответил"),
            }
        }
        self.account.server_info()
    }

    fn db<T>(&self, work: impl FnOnce(&SyncTx) -> rusqlite::Result<T>) -> Result<T, ApiError> {
        self.library.sync(work).map_err(|error| ApiError::new(0, "database", error.to_string()))
    }

    async fn sync_library(&self, mut ops: Vec<Op>) -> Result<(), ApiError> {
        let mut cursor = self.library.sync_state(KEY_CURSOR).unwrap_or_default();
        let mut restarted = false;
        loop {
            let batch: Vec<Op> = ops.drain(..ops.len().min(MAX_OPS)).collect();
            let request = SyncRequest {
                cursor: cursor.clone(),
                limit: None,
                streams: vec!["library".into(), "history".into()],
                ops: batch.iter().map(|o| o.json.clone()).collect(),
            };
            let response = match self
                .account
                .authorized(|api, token| {
                    let request = request.clone();
                    async move { api.sync(&token, &request).await }
                })
                .await
            {
                Ok(response) => response,
                // Сервер восстановлен из копии или забыл курсор: прочитать всё заново (§4.8, 410).
                Err(error) if error.status == 410 && !restarted => {
                    restarted = true;
                    cursor.clear();
                    ops.splice(0..0, batch);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let retry_after = self.db(|tx| {
                let retry = apply_results(tx, &batch, &response.results)?;
                apply_rows(tx, &response)?;
                tx.set_state(KEY_CURSOR, Some(&response.cursor))?;
                Ok(retry)
            })?;
            if let Some(delay) = retry_after {
                // Больше 2000 прослушиваний в час (op_rate_limited): остальные — после паузы сервера.
                ops.retain(|o| o.kind != "play.add");
                tracing::info!(секунды = delay.as_secs(), "прослушивания отложены сервером");
            }
            cursor = response.cursor;
            if !response.has_more && ops.is_empty() {
                break;
            }
        }
        self.db(|tx| {
            tx.set_state(KEY_LAST_SYNC, Some(&now_ms().to_string()))?;
            tx.set_state(KEY_MERGE, Some("0"))
        })?;
        Ok(())
    }

    /// Первая синхронизация с аккаунтом: свои плейлисты занимают серверных двойников, а не задваивают их.
    async fn plan_merge(&self) -> Result<(), ApiError> {
        let locals = self.db(|tx| tx.playlists())?;
        if locals.is_empty() {
            return Ok(());
        }
        let request: Vec<MergePlanInput> = locals
            .iter()
            .map(|p| MergePlanInput {
                local_key: p.id.to_string(),
                sync_id: None,
                name: playlist_name(&p.name),
                browse_id: p.browse_id.clone(),
            })
            .collect();
        let plan = self
            .account
            .authorized(|api, token| {
                let request = request.clone();
                async move { api.merge_plan(&token, &request).await }
            })
            .await?;
        self.db(|tx| {
            for entry in &plan.plan {
                let Ok(id) = entry.local_key.parse::<i64>() else { continue };
                if entry.action == "merge" || entry.action == "create" {
                    tx.set_playlist_sync_id(id, Some(&entry.playlist_id))?;
                }
            }
            Ok(())
        })
    }

    /// Живые события (§6): синхронизация на `sync.changed`, выход на `session.invalidated`.
    async fn follow_live_events(self: Arc<Self>, generation: u64) {
        let mut backoff = Duration::ZERO;
        while self.session.load(Ordering::SeqCst) == generation && self.account.session().is_some() {
            let started = Instant::now();
            let this = Arc::clone(&self);
            let result = self
                .account
                .authorized(|api, token| {
                    let this = Arc::clone(&this);
                    async move {
                        let runtime = this.runtime.clone();
                        let remote = this.remote_control();
                        api.events(&token, remote, |event| {
                            if this.session.load(Ordering::SeqCst) == generation {
                                let this = Arc::clone(&this);
                                runtime.spawn(async move { this.on_live_event(event).await });
                            }
                        })
                        .await
                    }
                })
                .await;
            if let Err(error) = result {
                tracing::info!(%error, "живые события закрылись");
            }
            if self.session.load(Ordering::SeqCst) != generation || self.account.session().is_none() {
                return;
            }
            // Долгий поток — не сбой: сервер закрывает его, когда истекает токен.
            backoff =
                if started.elapsed() > MAX_BACKOFF { Duration::ZERO } else { (backoff * 2).clamp(Duration::from_secs(1), MAX_BACKOFF) };
            let mut jitter = [0u8; 2];
            melogold_core::ids::fill_random(&mut jitter);
            let jitter = Duration::from_millis(u64::from(u16::from_le_bytes(jitter)) % (backoff.as_millis() as u64 / 4 + 1));
            tokio::time::sleep(backoff + jitter).await;
        }
    }

    async fn on_live_event(self: Arc<Self>, event: LiveEvent) {
        tracing::debug!(событие = %event.kind, "живое событие");
        for listener in self.event_listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            listener(&event);
        }
        match event.kind.as_str() {
            "system.connected" | "sync.changed" | "lyrics.changed" => {
                self.sync(true).await;
            }
            "devices.updated" => {
                for listener in self.devices_listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
                    listener();
                }
            }
            "session.invalidated" => self.account.end_session().await,
            _ => {}
        }
    }
}

// ── ops (что изменилось здесь с прошлой синхронизации) ──

struct OpBuilder {
    base: Option<String>,
}

impl OpBuilder {
    fn make(&self, kind: &'static str, key: String, at: i64, op_id: Option<&str>, fields: Map<String, Value>) -> Op {
        let mut json = Map::new();
        json.insert("opId".into(), json!(op_id.map(str::to_owned).unwrap_or_else(new_uuid)));
        json.insert("kind".into(), json!(kind));
        json.insert("at".into(), json!(iso::format(at)));
        if let Some(base) = &self.base {
            json.insert("base".into(), json!(base));
        }
        json.extend(fields);
        Op { kind, key, json: Value::Object(json) }
    }
}

fn fields(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

/// Метаданные треков, о которых говорит op (`tracks`): другие устройства смогут их показать.
fn put_tracks(map: &mut Map<String, Value>, tracks: &[Track]) {
    let list: Vec<Value> = tracks
        .iter()
        .map(|track| {
            let input = TrackInput::from_track(track);
            serde_json::to_value(input).unwrap_or(Value::Null)
        })
        .collect();
    if !list.is_empty() {
        map.insert("tracks".into(), Value::Array(list));
    }
}

fn playlist_name(name: &str) -> String {
    let trimmed = truncate_utf16(name, NAME_MAX);
    if trimmed.trim().is_empty() {
        "—".into()
    } else {
        trimmed
    }
}

fn build_ops(tx: &SyncTx, info: Option<&ServerInfo>) -> rusqlite::Result<Vec<Op>> {
    let mut ops = Vec::new();
    let now = now_ms();
    let builder = OpBuilder { base: tx.state(KEY_CURSOR)?.filter(|c| !c.is_empty()) };

    // Избранное
    let liked = tx.likes()?;
    let liked_ids: HashSet<&str> = liked.iter().map(|l| l.track.video_id.as_str()).collect();
    let synced_likes = tx.synced_likes()?;
    for like in liked.iter().filter(|l| !synced_likes.contains(&l.track.video_id)) {
        let mut map =
            fields(&[("videoId", json!(like.track.video_id)), ("liked", json!(true)), ("likedAt", json!(iso::format(like.liked_at)))]);
        put_tracks(&mut map, std::slice::from_ref(&like.track));
        ops.push(builder.make("like.set", format!("like:{}", like.track.video_id), like.liked_at, None, map));
    }
    for video_id in synced_likes.iter().filter(|id| !liked_ids.contains(id.as_str())) {
        ops.push(builder.make(
            "like.set",
            format!("like:{video_id}"),
            now,
            None,
            fields(&[("videoId", json!(video_id)), ("liked", json!(false))]),
        ));
    }

    // Плейлисты
    let synced = tx.synced_playlists()?;
    let playlists = tx.playlists()?;
    for playlist in &playlists {
        let songs = tx.playlist_video_ids(playlist.id)?;
        let tracks_of = |ids: &[String]| -> rusqlite::Result<Vec<Track>> { ids.iter().filter_map(|id| tx.track(id).transpose()).collect() };
        let whole = |kind: &'static str, sync_id: &str| -> rusqlite::Result<Op> {
            let mut map = fields(&[
                ("playlistId", json!(sync_id)),
                ("name", json!(playlist_name(&playlist.name))),
                ("browseId", json!(playlist.browse_id)),
                ("thumbnailUrl", json!(playlist.thumbnail_url)),
                ("videoIds", json!(songs)),
            ]);
            put_tracks(&mut map, &tracks_of(&songs)?);
            Ok(builder.make(kind, format!("pl:{sync_id}"), now, None, map))
        };
        match &playlist.sync_id {
            None => {
                let new_id = new_uuid();
                tx.set_playlist_sync_id(playlist.id, Some(&new_id))?;
                ops.push(whole("playlist.create", &new_id)?);
            }
            // Занят по плану слияния или создан и ещё не подтверждён: import сливает.
            Some(sync_id) if !synced.contains_key(sync_id) => ops.push(whole("playlist.import", sync_id)?),
            Some(sync_id) => {
                let previous = &synced[sync_id];
                if previous.name.as_deref() != Some(playlist.name.as_str()) || previous.thumbnail_url != playlist.thumbnail_url {
                    let map = fields(&[
                        ("playlistId", json!(sync_id)),
                        ("name", json!(playlist_name(&playlist.name))),
                        ("thumbnailUrl", json!(playlist.thumbnail_url)),
                    ]);
                    ops.push(builder.make("playlist.update", format!("pl:{sync_id}"), now, None, map));
                }
                if previous.video_ids != songs {
                    for change in playlist_diff::changes(&previous.video_ids, &songs) {
                        ops.push(item_op(&builder, tx, sync_id, change, now)?);
                    }
                }
            }
        }
    }
    let present: HashSet<&str> = playlists.iter().filter_map(|p| p.sync_id.as_deref()).collect();
    for sync_id in synced.keys().filter(|id| !present.contains(id.as_str())) {
        ops.push(builder.make("playlist.delete", format!("pl:{sync_id}"), now, None, fields(&[("playlistId", json!(sync_id))])));
    }

    // Сохранённые альбомы, исполнители и каналы
    let bookmarks = tx.bookmarks()?;
    let bookmark_keys: HashSet<(String, String)> = bookmarks.iter().map(|b| (b.kind.clone(), b.browse_id.clone())).collect();
    let synced_bookmarks = tx.synced_bookmarks()?;
    for bookmark in bookmarks.iter().filter(|b| !synced_bookmarks.contains(&(b.kind.clone(), b.browse_id.clone()))) {
        let map = fields(&[
            ("type", json!(bookmark.kind)),
            ("browseId", json!(bookmark.browse_id)),
            ("bookmarked", json!(true)),
            ("bookmarkedAt", json!(iso::format(bookmark.bookmarked_at))),
            ("title", json!(bookmark.title)),
            ("subtitle", json!(bookmark.subtitle)),
            ("thumbnailUrl", json!(bookmark.thumbnail_url)),
            ("year", json!(bookmark.year)),
        ]);
        ops.push(builder.make("bookmark.set", format!("bm:{}:{}", bookmark.kind, bookmark.browse_id), now, None, map));
    }
    for (kind, browse_id) in synced_bookmarks.iter().filter(|k| !bookmark_keys.contains(*k)) {
        let map = fields(&[("type", json!(kind)), ("browseId", json!(browse_id)), ("bookmarked", json!(false))]);
        ops.push(builder.make("bookmark.set", format!("bm:{kind}:{browse_id}"), now, None, map));
    }

    // Свои названия треков (задание 0005) — только если сервер их принимает.
    if info.is_some_and(|i| i.supports("track.override.set")) {
        let local = tx.overrides()?;
        let synced = tx.synced_overrides()?;
        for (video_id, edit) in local.iter().filter(|(id, edit)| synced.get(*id).is_none_or(|s| !s.same_fields(edit))) {
            let map = fields(&[
                ("videoId", json!(video_id)),
                ("title", json!(edit.title)),
                ("artistsText", json!(edit.artists_text)),
                ("albumTitle", json!(edit.album_title)),
            ]);
            ops.push(builder.make("track.override.set", format!("ovr:{video_id}"), edit.updated_at, None, map));
        }
        for video_id in synced.keys().filter(|id| !local.contains_key(*id)) {
            ops.push(builder.make("track.override.set", format!("ovr:{video_id}"), now, None, fields(&[("videoId", json!(video_id))])));
        }
    }

    // Закреплённые тексты (задание 0006) — только если сервер их принимает.
    if info.is_some_and(|i| i.supports("lyrics.pin.set")) {
        let local = tx.lyrics_pins()?;
        let synced = tx.synced_lyrics_pins()?;
        for (video_id, pin) in local.iter().filter(|(id, pin)| synced.get(*id).is_none_or(|s| !s.same_fields(pin))) {
            let map = fields(&[
                ("videoId", json!(video_id)),
                ("source", json!(pin.source)),
                ("ref", json!(pin.reference)),
                ("startTimeMs", json!(pin.start_time_ms)),
            ]);
            ops.push(builder.make("lyrics.pin.set", format!("lpin:{video_id}"), pin.updated_at, None, map));
        }
        for video_id in synced.keys().filter(|id| !local.contains_key(*id)) {
            ops.push(builder.make("lyrics.pin.set", format!("lpin:{video_id}"), now, None, fields(&[("videoId", json!(video_id))])));
        }
    }

    ops.extend(history_ops(tx, &builder, info, now)?);
    Ok(ops)
}

fn lyrics_available(info: &ServerInfo) -> bool {
    info.features.lyrics.as_ref().is_some_and(|l| l.version >= 1)
}

/// Своя версия с сервера (Windows `ApplyLyrics`): записать с источниками как есть — она своя, из
/// какого бы источника ни пришла (задание 0002). Надгробие удаляет свой текст, только если он не
/// менялся с прошлого синка; выбранный при этом остаётся здесь найденным.
fn apply_lyrics(tx: &SyncTx, item: &MyLyrics) -> rusqlite::Result<()> {
    let local = tx.lyrics(&item.video_id)?;
    let Some(text) = item.text.as_ref().filter(|_| !item.deleted) else {
        let snapshot = tx.synced_lyrics_of(&item.video_id)?;
        if let Some(local) = local.filter(|l| sync_rules::delete_on_tombstone(Some(l), snapshot.as_ref())) {
            if sync_rules::is_chosen_only(&local) {
                tx.save_lyrics(&item.video_id, &StoredLyrics { chosen: false, ..local })?;
            } else {
                tx.delete_lyrics(&item.video_id)?;
            }
        }
        return tx.forget_synced_lyrics(&item.video_id);
    };
    let stored = sync_rules::from_payload(&LyricsPayload::from(text));
    if !sync_rules::same_content(local.as_ref(), &stored) {
        tx.save_lyrics(&item.video_id, &stored)?;
    } else if let Some(local) = local.filter(|l| !l.chosen) {
        // Тот же текст, найденный здесь автоматически, становится своим.
        tx.save_lyrics(&item.video_id, &StoredLyrics { chosen: true, ..local })?;
    }
    tx.set_synced_lyrics(&item.video_id, item.rev, &sync_rules::hash(&sync_rules::to_payload(&stored)))
}

fn item_op(builder: &OpBuilder, tx: &SyncTx, sync_id: &str, change: ItemChange, now: i64) -> rusqlite::Result<Op> {
    let key = format!("pl:{sync_id}");
    Ok(match change {
        ItemChange::Remove(video_id) => {
            builder.make("playlist.item.remove", key, now, None, fields(&[("playlistId", json!(sync_id)), ("videoId", json!(video_id))]))
        }
        ItemChange::Add { video_ids, after, before } => {
            let tracks: Vec<Track> = video_ids.iter().filter_map(|id| tx.track(id).transpose()).collect::<rusqlite::Result<_>>()?;
            let mut map = fields(&[
                ("playlistId", json!(sync_id)),
                ("videoIds", json!(video_ids)),
                ("after", json!(after)),
                ("before", json!(before)),
            ]);
            put_tracks(&mut map, &tracks);
            builder.make("playlist.items.add", key, now, None, map)
        }
        ItemChange::Move { video_id, after, before } => builder.make(
            "playlist.item.move",
            key,
            now,
            None,
            fields(&[("playlistId", json!(sync_id)), ("videoId", json!(video_id)), ("after", json!(after)), ("before", json!(before))]),
        ),
    })
}

/// История (задание Windows 0002 §3.2): свои неотправленные прослушивания — `play.add` (opId =
/// eventId), «Убрать из истории» и «Очистить историю» — `history.forget` и `history.clear`. При
/// первой синхронизации с аккаунтом — не больше `mergeUploadMax` самых свежих прослушиваний, а когда
/// все они на сервере — накопленное время треков `play.baseline atLeast`.
fn history_ops(tx: &SyncTx, builder: &OpBuilder, info: Option<&ServerInfo>, now: i64) -> rusqlite::Result<Vec<Op>> {
    let mut ops = Vec::new();
    let merge = tx.state(KEY_HISTORY_MERGE)?.as_deref() == Some("1");
    let retry_at: i64 = tx.state(KEY_HISTORY_RETRY_AT)?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut plays = tx.unsent_plays()?;
    if retry_at <= now {
        let limit = info
            .and_then(|i| i.limit(&["history", "mergeUploadMax"]))
            .filter(|v| *v > 0)
            .map(|v| v as usize)
            .unwrap_or(DEFAULT_MERGE_UPLOAD_MAX);
        if merge && plays.len() > limit {
            // Старше последних mergeUploadMax — не отправляются: сервер их всё равно не примет.
            let old = plays.len() - limit;
            for play in plays.drain(..old) {
                tx.mark_play_sent(&play.event_id)?;
            }
        }
        for play in &plays {
            let mut map = fields(&[
                ("videoId", json!(play.video_id)),
                ("playedAt", json!(iso::format(play.played_at))),
                ("playTimeMs", json!(play.play_time_ms.clamp(1, MAX_PLAY_TIME_MS))),
                ("history", json!(true)),
                ("playtime", json!(true)),
            ]);
            if let Some(track) = tx.track(&play.video_id)? {
                put_tracks(&mut map, &[track]);
            }
            ops.push(builder.make("play.add", format!("play:{}", play.event_id), play.played_at, Some(&play.event_id), map));
        }
    }
    for op in tx.history_ops()? {
        let kind = if op.kind == "history.forget" { "history.forget" } else { "history.clear" };
        let mut map = fields(&[("eventsBefore", json!(iso::format(op.events_before)))]);
        if kind == "history.forget" {
            map.insert("videoId".into(), json!(op.video_id));
            // Общее время трека обнуляется на всех устройствах (задание Windows 0008).
            map.insert("resetTotal".into(), json!(true));
        }
        ops.push(builder.make(kind, format!("hop:{}", op.op_id), op.events_before, Some(&op.op_id), map));
    }
    if merge && plays.is_empty() {
        let totals = tx.play_totals()?;
        if totals.is_empty() {
            tx.set_state(KEY_HISTORY_MERGE, Some("0"))?;
        }
        for chunk in totals.chunks(BASELINE_CHUNK) {
            let entries: Vec<Value> = chunk.iter().map(|(id, ms)| json!({ "videoId": id, "totalMs": ms })).collect();
            ops.push(builder.make(
                "play.baseline",
                "stat:batch".into(),
                now,
                None,
                fields(&[("mode", json!("atLeast")), ("entries", json!(entries))]),
            ));
        }
    }
    Ok(ops)
}

// ── ответ сервера ──

/// Результаты ops: плейлист, который сервер перенёс в копию восстановления (`redirected`), переходит
/// туда и здесь; прослушивания и действия с историей, которые сервер принял или отверг, больше не
/// отправляются. Возвращает паузу, если сервер отложил прослушивания (`deferred`, лимит в час).
fn apply_results(tx: &SyncTx, batch: &[Op], results: &[OpResult]) -> rusqlite::Result<Option<Duration>> {
    let mut retry: Option<Duration> = None;
    let mut baseline_done: Option<bool> = None;
    for (op, result) in batch.iter().zip(results) {
        let deferred = result.status == "deferred";
        match op.kind {
            "play.add" => {
                // applied (и replayed), superseded, rejected — больше не слать; deferred — после паузы.
                if !deferred {
                    tx.mark_play_sent(op.key.trim_start_matches("play:"))?;
                } else if retry.is_none() {
                    retry = Some(Duration::from_secs(result.retry_after_seconds.unwrap_or(3600).max(1) as u64));
                }
                continue;
            }
            "history.forget" | "history.clear" => {
                if !deferred {
                    tx.delete_history_op(op.key.trim_start_matches("hop:"))?;
                }
                continue;
            }
            "play.baseline" => {
                baseline_done = Some(baseline_done.unwrap_or(true) && !deferred);
                continue;
            }
            _ => {}
        }
        let (Some(old), Some(new_id)) = (op.key.strip_prefix("pl:"), result.playlist_id.as_deref()) else { continue };
        if result.status != "redirected" {
            continue;
        }
        if let Some(local) = tx.playlist_by_sync_id(old)? {
            tx.set_playlist_sync_id(local.id, Some(new_id))?;
            // В копии только то, что несла op: остальные треки уйдут туда как новые.
            tx.clear_sort_keys(local.id)?;
        }
        tx.delete_synced_playlist(old)?;
    }
    if let Some(done) = baseline_done {
        tx.set_state(KEY_HISTORY_MERGE, Some(if done { "0" } else { "1" }))?;
    }
    if let Some(pause) = retry {
        tx.set_state(KEY_HISTORY_RETRY_AT, Some(&(now_ms() + pause.as_millis() as i64).to_string()))?;
    }
    Ok(retry)
}

fn to_track(dto: &TrackDto) -> Option<Track> {
    if dto.metadata_stub || dto.title.trim().is_empty() {
        return None;
    }
    Some(Track {
        video_id: dto.video_id.clone(),
        title: dto.title.clone(),
        artists_text: dto.artists_text.clone(),
        artists: dto.artists.iter().map(|a| ArtistRef { id: a.id.clone(), name: a.name.clone() }).collect(),
        album_id: dto.album_id.clone(),
        album_title: dto.album_title.clone(),
        duration_ms: dto.duration_ms,
        duration_text: dto.duration_text.clone(),
        thumbnail_url: dto.thumbnail_url.clone(),
        explicit: dto.explicit,
        video_type: dto.video_type.clone(),
        ..Default::default()
    })
}

/// Ответ сервера в порядке API §4.8 (треки, плейлисты по `createdAt`, их треки, лайки, закладки,
/// правки, история): библиотека и снимок становятся тем, что на сервере.
fn apply_rows(tx: &SyncTx, response: &SyncResponse) -> rusqlite::Result<()> {
    for track in &response.tracks {
        tx.ensure_track(&track.video_id, to_track(track).as_ref())?;
    }

    let mut touched: HashSet<i64> = HashSet::new();
    let synced = tx.synced_playlists()?;
    let mut playlists: Vec<&PlaylistRow> = response.playlists.iter().collect();
    playlists.sort_by_key(|p| iso::parse(&p.created_at).unwrap_or(0));
    for row in playlists {
        let local = tx.playlist_by_sync_id(&row.id)?;
        if row.deleted {
            if let Some(local) = local {
                tx.delete_playlist(local.id)?;
            }
            tx.delete_synced_playlist(&row.id)?;
        } else if let Some(local) = local {
            if local.name != row.name || local.thumbnail_url != row.thumbnail_url {
                tx.update_playlist(local.id, &row.name, row.thumbnail_url.as_deref())?;
            }
            let previous = synced.get(&row.id).map(|s| s.video_ids.clone()).unwrap_or_default();
            tx.upsert_synced_playlist(&SyncedPlaylist {
                sync_id: row.id.clone(),
                name: Some(row.name.clone()),
                thumbnail_url: row.thumbnail_url.clone(),
                video_ids: previous,
            })?;
        } else {
            let created = iso::parse(&row.created_at).unwrap_or_else(now_ms);
            let id = tx.insert_playlist(&row.name, row.browse_id.as_deref(), row.thumbnail_url.as_deref(), &row.id, created)?;
            tx.upsert_synced_playlist(&SyncedPlaylist {
                sync_id: row.id.clone(),
                name: Some(row.name.clone()),
                thumbnail_url: row.thumbnail_url.clone(),
                video_ids: Vec::new(),
            })?;
            touched.insert(id);
        }
    }

    for item in &response.items {
        let Some(playlist) = tx.playlist_by_sync_id(&item.playlist_id)? else { continue };
        if item.present {
            tx.ensure_track(&item.video_id, None)?;
            tx.upsert_item(playlist.id, &item.video_id, &item.sort_key, iso::parse(&item.added_at).unwrap_or_else(now_ms))?;
        } else {
            tx.delete_item(playlist.id, &item.video_id)?;
        }
        touched.insert(playlist.id);
    }
    for id in touched {
        tx.reorder(id)?;
    }

    for row in &response.likes {
        tx.ensure_track(&row.video_id, None)?;
        let liked_at = row.liked.then(|| row.liked_at.as_deref().and_then(iso::parse).unwrap_or_else(now_ms));
        tx.set_like(&row.video_id, liked_at)?;
    }

    for row in &response.bookmarks {
        if row.kind != "album" && row.kind != "artist" {
            continue;
        }
        let at = row.bookmarked.then(|| row.bookmarked_at.as_deref().and_then(iso::parse).unwrap_or_else(now_ms));
        tx.set_bookmark(
            &row.kind,
            &row.browse_id,
            at,
            row.title.as_deref(),
            row.subtitle.as_deref(),
            row.thumbnail_url.as_deref(),
            row.year.as_deref(),
        )?;
    }

    for row in &response.overrides {
        let edit = TrackOverride::new(
            &row.video_id,
            row.title.as_deref(),
            row.artists_text.as_deref(),
            row.album_title.as_deref(),
            iso::parse(&row.updated_at).unwrap_or_else(now_ms),
        );
        tx.apply_override(&edit, row.deleted)?;
    }

    for row in &response.lyrics_pins {
        let pin = match (&row.source, &row.reference) {
            (Some(source), Some(reference)) if !row.deleted && !reference.is_empty() => Some(LyricsPin {
                video_id: row.video_id.clone(),
                source: source.clone(),
                reference: reference.clone(),
                start_time_ms: row.start_time_ms,
                updated_at: iso::parse(&row.updated_at).unwrap_or_else(now_ms),
            }),
            _ => None,
        };
        tx.apply_lyrics_pin(&row.video_id, pin.as_ref())?;
    }

    apply_history_rows(tx, response)
}

/// История с сервера (задание Windows 0002 §3.3): общее время трека (уже по всем устройствам),
/// прослушивания любого устройства (своё вернувшееся не задваивается), забытые события. Забытое с
/// `totalBefore` обнуляет общее время трека, если в том же ответе нет его свежего `playStats`.
fn apply_history_rows(tx: &SyncTx, response: &SyncResponse) -> rusqlite::Result<()> {
    let with_stats: HashSet<&str> = response.play_stats.iter().map(|r| r.video_id.as_str()).collect();
    for row in &response.play_stats {
        tx.ensure_track(&row.video_id, None)?;
        tx.set_play_total(&row.video_id, row.total_play_time_ms)?;
    }
    for row in &response.plays {
        let Some(played_at) = iso::parse(&row.played_at) else { continue };
        tx.ensure_track(&row.video_id, None)?;
        tx.insert_play(&row.event_id, &row.video_id, played_at, row.play_time_ms, row.device_id.as_deref())?;
    }
    for row in &response.play_forgets {
        if let Some(before) = iso::parse(&row.events_before) {
            tx.forget_plays(&row.video_id, before)?;
        }
        if row.total_before.is_some() && row.video_id != "*" && !with_stats.contains(row.video_id.as_str()) {
            tx.set_play_total(&row.video_id, 0)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use melogold_data::Database;

    fn track(id: &str) -> Track {
        Track { video_id: id.into(), title: format!("Трек {id}"), ..Default::default() }
    }

    fn info(kinds: &[&str]) -> ServerInfo {
        ServerInfo {
            features: Features {
                sync: Some(SyncFeature {
                    protocol: 1,
                    min_protocol: 1,
                    kinds: kinds.iter().map(|k| (*k).to_owned()).collect(),
                    streams: vec![],
                }),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn local_changes_become_ops_and_server_rows_close_the_loop() {
        let lib = Library::open(Database::in_memory().unwrap());
        lib.set_liked(&[track("aaaaaaaaaaa")], true).unwrap();
        let playlist = lib.create_playlist("Дорога", &[track("bbbbbbbbbbb")]).unwrap();
        lib.set_override("aaaaaaaaaaa", Some("Песня"), None, None).unwrap();
        lib.record_play(&track("ccccccccccc"), 60_000, 1_000).unwrap();
        let kinds = |ops: &[Op]| ops.iter().map(|o| o.kind).collect::<Vec<_>>();

        // Сервер без правок: op правки не создаётся.
        let ops = lib.sync(|tx| build_ops(tx, Some(&info(&["like.set"])))).unwrap();
        assert_eq!(kinds(&ops), ["like.set", "playlist.create", "play.add"]);
        let create = &ops[1].json;
        assert_eq!(create["name"], "Дорога");
        assert_eq!(create["videoIds"], json!(["bbbbbbbbbbb"]));
        assert_eq!(create["tracks"][0]["title"], "Трек bbbbbbbbbbb");
        let sync_id = create["playlistId"].as_str().unwrap().to_owned();

        // Ответ сервера: лайк, плейлист с ключами, правка — снимок совпадает с библиотекой.
        let response: SyncResponse = serde_json::from_value(json!({
            "results": [], "cursor": "c.1.1", "hasMore": false,
            "playlists": [{"id": sync_id, "name": "Дорога", "createdAt": "2026-09-23T10:00:00.000Z", "deleted": false}],
            "items": [{"playlistId": sync_id, "videoId": "bbbbbbbbbbb", "present": true, "sortKey": "a0", "addedAt": "2026-09-23T10:00:00.000Z"}],
            "likes": [{"videoId": "aaaaaaaaaaa", "liked": true, "likedAt": "2026-09-23T10:00:00.000Z"}],
            "overrides": [{"videoId": "aaaaaaaaaaa", "title": "Песня", "artistsText": null, "albumTitle": null, "updatedAt": "2026-09-23T10:00:00.000Z", "deleted": false}]
        }))
        .unwrap();
        lib.sync(|tx| {
            apply_results(tx, &ops, &[])?;
            apply_rows(tx, &response)?;
            tx.mark_play_sent(ops[2].key.trim_start_matches("play:"))?;
            tx.set_state(KEY_CURSOR, Some("c.1.1"))
        })
        .unwrap();
        let all = info(&["like.set", "track.override.set"]);
        let ops = lib.sync(|tx| build_ops(tx, Some(&all))).unwrap();
        assert!(ops.is_empty(), "всё уже на сервере: {:?}", kinds(&ops));

        // Правка здесь: перенос трека и снятый лайк — одна op на каждое; base — курсор снимка.
        lib.add_to_playlist(playlist, &[track("ddddddddddd")]).unwrap();
        lib.set_liked(&[track("aaaaaaaaaaa")], false).unwrap();
        lib.set_override("aaaaaaaaaaa", None, None, None).unwrap();
        let ops = lib.sync(|tx| build_ops(tx, Some(&all))).unwrap();
        assert_eq!(kinds(&ops), ["like.set", "playlist.items.add", "track.override.set"]);
        assert_eq!(ops[0].json["liked"], false);
        assert_eq!(ops[1].json["after"], "bbbbbbbbbbb");
        assert_eq!(ops[1].json["base"], "c.1.1");
        assert!(ops[2].json.get("title").is_none(), "снятая правка — без полей");
    }

    #[test]
    fn lyrics_pins_go_up_and_come_back() {
        use melogold_core::lyrics::sync_rules::sources;
        let lib = Library::open(Database::in_memory().unwrap());
        let found = StoredLyrics {
            synced: Some("[00:01.00]Строка".into()),
            plain: Some(String::new()),
            synced_source: Some(sources::KUGOU.into()),
            synced_ref: Some("42:abc".into()),
            offset_ms: -300,
            ..Default::default()
        };
        lib.save_lyrics("aaaaaaaaaaa", &found).unwrap();
        assert!(lib.pin_played("aaaaaaaaaaa").unwrap());
        // Сервер без закреплений — op нет.
        assert!(lib.sync(|tx| build_ops(tx, Some(&info(&["like.set"])))).unwrap().is_empty());
        let pins = info(&["lyrics.pin.set"]);
        let ops = lib.sync(|tx| build_ops(tx, Some(&pins))).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].json["kind"], "lyrics.pin.set");
        assert_eq!(
            (&ops[0].json["source"], &ops[0].json["ref"], &ops[0].json["startTimeMs"]),
            (&json!("kugou"), &json!("42:abc"), &json!(300))
        );
        // Сервер вернул закрепление и чужое — снимок совпал, чужое легло здесь.
        let response: SyncResponse = serde_json::from_value(json!({
            "results": [], "cursor": "c.1.1", "hasMore": false,
            "lyricsPins": [
                {"videoId": "aaaaaaaaaaa", "source": "kugou", "ref": "42:abc", "startTimeMs": 300, "updatedAt": "2026-09-23T10:00:00.000Z", "deleted": false},
                {"videoId": "bbbbbbbbbbb", "source": "lrclib", "ref": "7", "startTimeMs": null, "updatedAt": "2026-09-23T10:00:00.000Z", "deleted": false}
            ]
        }))
        .unwrap();
        lib.sync(|tx| apply_rows(tx, &response)).unwrap();
        assert!(lib.sync(|tx| build_ops(tx, Some(&pins))).unwrap().is_empty());
        assert_eq!(lib.lyrics_pin("bbbbbbbbbbb").unwrap().unwrap().reference, "7");
        // Снятое на сервере снято и здесь.
        let removed: SyncResponse = serde_json::from_value(json!({
            "lyricsPins": [{"videoId": "bbbbbbbbbbb", "source": null, "ref": null, "updatedAt": "2026-09-23T10:00:01.000Z", "deleted": true}]
        }))
        .unwrap();
        lib.sync(|tx| apply_rows(tx, &removed)).unwrap();
        assert_eq!(lib.lyrics_pin("bbbbbbbbbbb").unwrap(), None);
    }

    /// Задание 0002: версия с сервера с источником `lrclib` — своя; следующая выгрузка её не удаляет.
    #[test]
    fn chosen_lyrics_from_server_stay_own() {
        let lib = Library::open(Database::in_memory().unwrap());
        let item: MyLyrics = serde_json::from_value(json!({
            "id": "5d2c7e1a-9b3f-4c6d-8e2a-1f0b3c4d5e6f", "videoId": "aaaaaaaaaaa", "rev": 3, "deleted": false,
            "text": {"plain": "Строка", "plainSource": "lrclib", "synced": "[00:01.00]Строка", "syncedFormat": "lrc", "syncedSource": "lrclib", "startTimeMs": null, "language": null},
            "updatedAt": "2026-09-25T12:00:00.000Z"
        }))
        .unwrap();
        lib.sync(|tx| apply_lyrics(tx, &item)).unwrap();
        let stored = lib.lyrics("aaaaaaaaaaa").unwrap().unwrap();
        assert!(stored.chosen && sync_rules::is_own(&stored));
        let sends = lib.sync(|tx| Ok(sync_rules::plan_sends(&tx.own_lyrics()?, &tx.synced_lyrics()?))).unwrap();
        assert!(sends.is_empty(), "ни PUT, ни DELETE: {sends:?}");
        // Надгробие: выбранный текст остаётся здесь найденным.
        let tombstone = MyLyrics { deleted: true, text: None, rev: 4, ..item };
        lib.sync(|tx| apply_lyrics(tx, &tombstone)).unwrap();
        let stored = lib.lyrics("aaaaaaaaaaa").unwrap().unwrap();
        assert!(!stored.chosen && stored.synced.is_some());
        assert!(lib.sync(|tx| Ok(sync_rules::plan_sends(&tx.own_lyrics()?, &tx.synced_lyrics()?))).unwrap().is_empty());
    }

    #[test]
    fn history_rows_and_baseline() {
        let lib = Library::open(Database::in_memory().unwrap());
        lib.record_play(&track("aaaaaaaaaaa"), 90_000, 1_000).unwrap();
        lib.sync(|tx| tx.set_state(KEY_HISTORY_MERGE, Some("1"))).unwrap();
        let ops = lib.sync(|tx| build_ops(tx, None)).unwrap();
        assert_eq!(ops.iter().map(|o| o.kind).collect::<Vec<_>>(), ["play.add"], "baseline — только когда все прослушивания на сервере");
        let results: Vec<OpResult> = vec![OpResult { status: "applied".into(), ..Default::default() }];
        lib.sync(|tx| apply_results(tx, &ops, &results)).unwrap();
        let ops = lib.sync(|tx| build_ops(tx, None)).unwrap();
        assert_eq!(ops.iter().map(|o| o.kind).collect::<Vec<_>>(), ["play.baseline"]);
        assert_eq!(ops[0].json["entries"], json!([{"videoId": "aaaaaaaaaaa", "totalMs": 90000}]));

        // С сервера: чужое прослушивание, общее время, забытый трек.
        let response: SyncResponse = serde_json::from_value(json!({
            "plays": [{"eventId": "e-other", "videoId": "bbbbbbbbbbb", "playedAt": "2026-09-23T10:00:00.000Z", "playTimeMs": 30000, "deviceId": "dev-2"}],
            "playStats": [{"videoId": "aaaaaaaaaaa", "totalPlayTimeMs": 500000, "lastPlayedAt": null}],
            "playForgets": [{"videoId": "bbbbbbbbbbb", "eventsBefore": "2026-09-23T09:00:00.000Z", "totalBefore": "2026-09-23T09:00:00.000Z"}]
        }))
        .unwrap();
        lib.sync(|tx| apply_rows(tx, &response)).unwrap();
        assert_eq!(lib.play_count().unwrap(), 2, "забыто только то, что раньше eventsBefore");
        let top = lib.most_played(None, 10).unwrap();
        assert_eq!(top[0].track.video_id, "aaaaaaaaaaa");
        assert_eq!(top[0].play_time_ms, 500_000);
    }

    #[test]
    fn redirected_playlist_moves_to_the_copy() {
        let lib = Library::open(Database::in_memory().unwrap());
        let playlist = lib.create_playlist("Старый", &[track("aaaaaaaaaaa")]).unwrap();
        let ops = lib.sync(|tx| build_ops(tx, None)).unwrap();
        let old = ops[0].json["playlistId"].as_str().unwrap().to_owned();
        let result = OpResult { status: "redirected".into(), playlist_id: Some("new-copy".into()), ..Default::default() };
        lib.sync(|tx| apply_results(tx, &ops, &[result])).unwrap();
        let sync_id = lib.sync(|tx| tx.playlist_by_sync_id("new-copy")).unwrap().map(|p| p.id);
        assert_eq!(sync_id, Some(playlist));
        assert!(lib.sync(|tx| tx.playlist_by_sync_id(&old)).unwrap().is_none());
    }
}
