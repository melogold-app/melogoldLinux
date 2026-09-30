//! Аккаунт на сервере Melogold (API §4.3–§4.6, Windows `AccountService.cs`): регистрация с
//! доказательством работы, вход, токены и их обновление (один за раз, за 60 с до истечения, один
//! повтор вызова на `access_token_expired`), выход, устройства, одобрение входа по коду. Экраны и
//! синхронизация ходят на сервер только через [`Account::authorized`].
//!
//! Пароль проходит через вызов и не хранится нигде: на устройстве остаются только токены.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use melogold_core::iso;
use melogold_core::text::{now_ms, truncate_utf16};

use crate::api::{Api, ApiError, API_VERSION, SYNC_PROTOCOL};
use crate::dto::*;
use crate::session::{SessionStore, StoreKind, StoredSession};

/// Сервер по умолчанию, пока нет официального домена (как у Android и Windows).
pub const DEFAULT_SERVER_URL: &str = melogold_core::app_info::DEFAULT_SERVER_URL;

const REFRESH_EARLY_MS: i64 = 60_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountState {
    /// Без аккаунта: приложение работает само по себе, как ViTune.
    SignedOut,
    SignedIn {
        login: String,
        device_id: String,
        server_url: String,
    },
    /// Сервер закончил сессию (отзыв, смена пароля): войти снова. Библиотека и привязка остаются (§1.7).
    AuthRequired {
        login: String,
        server_url: String,
    },
}

/// Что устройство сообщает о себе серверу (`DeviceInput`, §4.1).
#[derive(Clone, Debug)]
pub struct DeviceIdentity {
    /// `/etc/machine-id|installSalt` (§1.6): из него и `serverId` получается hwid.
    pub platform_id: String,
    pub name: String,
    pub os_version: Option<String>,
    pub model: Option<String>,
    pub client_version: String,
    /// `Accept-Language` запросов.
    pub language: String,
}

type Listener = Box<dyn Fn(&AccountState) + Send + Sync>;

struct Inner {
    session: Option<StoredSession>,
    server_url: String,
    server_info: Option<ServerInfo>,
    state: AccountState,
    store_kind: StoreKind,
    api: Option<Api>,
}

pub struct Account {
    identity: DeviceIdentity,
    store: SessionStore,
    inner: Mutex<Inner>,
    refresh: tokio::sync::Mutex<()>,
    listeners: Mutex<Vec<Listener>>,
}

impl Account {
    /// Аккаунт без сессии; [`Account::load`] читает её из хранилища. `server_url` — выбранный в
    /// настройках (иначе по умолчанию).
    pub fn new(identity: DeviceIdentity, store: SessionStore, server_url: Option<String>) -> Arc<Account> {
        let server_url = server_url.filter(|u| !u.is_empty()).unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned());
        Arc::new(Account {
            identity,
            store,
            inner: Mutex::new(Inner {
                session: None,
                server_url,
                server_info: None,
                state: AccountState::SignedOut,
                store_kind: StoreKind::File,
                api: None,
            }),
            refresh: tokio::sync::Mutex::new(()),
            listeners: Mutex::default(),
        })
    }

    /// Прочитать сессию (связка ключей может отвечать не сразу — окно её не ждёт).
    pub async fn load(&self) {
        let (session, store_kind) = self.store.load().await;
        let state = match &session {
            Some(s) => AccountState::SignedIn { login: s.login.clone(), device_id: s.device_id.clone(), server_url: s.server_url.clone() },
            None => AccountState::SignedOut,
        };
        {
            let mut inner = self.lock();
            inner.session = session;
            inner.state = state.clone();
            inner.store_kind = store_kind;
        }
        if state != AccountState::SignedOut {
            for listener in self.listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
                listener(&state);
            }
        }
    }

    /// Новый аккаунт с уже прочитанной сессией (проверки, примеры).
    pub async fn open(identity: DeviceIdentity, store: SessionStore, server_url: Option<String>) -> Arc<Account> {
        let account = Account::new(identity, store, server_url);
        account.load().await;
        account
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn state(&self) -> AccountState {
        self.lock().state.clone()
    }

    /// Состояние сменилось (из любого потока).
    pub fn subscribe(&self, listener: impl Fn(&AccountState) + Send + Sync + 'static) {
        self.listeners.lock().unwrap_or_else(|p| p.into_inner()).push(Box::new(listener));
    }

    pub fn session(&self) -> Option<StoredSession> {
        self.lock().session.clone()
    }

    /// Сервер, с которым работает приложение: у сессии — её, иначе выбранный.
    pub fn server_url(&self) -> String {
        let inner = self.lock();
        inner.session.as_ref().map(|s| s.server_url.clone()).unwrap_or_else(|| inner.server_url.clone())
    }

    /// Последний ответ `/server/info` текущего сервера.
    pub fn server_info(&self) -> Option<ServerInfo> {
        self.lock().server_info.clone()
    }

    /// `/server/info` текущего сервера: последний ответ или новый запрос (возможности `share` и `remote`).
    pub async fn ensure_server_info(&self) -> Option<ServerInfo> {
        if let Some(info) = self.server_info() {
            return Some(info);
        }
        self.check(&self.server_url()).await.ok()
    }

    pub fn store_kind(&self) -> StoreKind {
        self.lock().store_kind
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    /// Клиент API для адреса: у текущего сервера — один на всё время.
    pub fn api(&self, url: Option<&str>) -> Api {
        let url = url.map(str::to_owned).unwrap_or_else(|| self.server_url());
        let mut inner = self.lock();
        if let Some(api) = inner.api.as_ref().filter(|a| a.base() == url.trim_end_matches('/')) {
            return api.clone();
        }
        let api = Api::new(&url, &self.identity.client_version, &self.identity.language);
        inner.api = Some(api.clone());
        api
    }

    /// Проверка сервера (API §7.1 п. 3–4): `software == "melogold-server"`, совместимые API и протокол.
    pub async fn check(&self, url: &str) -> Result<ServerInfo, ApiError> {
        let api = Api::new(url, &self.identity.client_version, &self.identity.language);
        let info = api.server_info().await?;
        if info.software != "melogold-server" {
            return Err(ApiError::new(0, "not_melogold", "Not a Melogold server"));
        }
        if info.min_api_version > API_VERSION || info.features.sync.as_ref().is_some_and(|s| s.min_protocol > SYNC_PROTOCOL) {
            return Err(ApiError::new(0, "client_outdated", "The server needs a newer app"));
        }
        if info.api_version < API_VERSION {
            return Err(ApiError::new(0, "server_outdated", "The server is too old"));
        }
        if url.trim_end_matches('/') == self.server_url() {
            self.lock().server_info = Some(info.clone());
        }
        Ok(info)
    }

    /// Переключиться на другой сервер: сессия старого на этом устройстве заканчивается.
    pub async fn set_server(&self, url: &str) {
        {
            let mut inner = self.lock();
            inner.server_url = url.trim_end_matches('/').to_owned();
            inner.server_info = None;
        }
        self.set_session(None, AccountState::SignedOut).await;
    }

    pub fn device_input(&self, server_id: &str) -> DeviceInput {
        let identity = &self.identity;
        DeviceInput {
            hwid: melogold_core::hwid::compute(&identity.platform_id, server_id),
            name: truncate_utf16(&identity.name, 64),
            platform: "linux".into(),
            os_version: identity.os_version.as_deref().map(|v| truncate_utf16(v, 64)),
            model: identity.model.as_deref().map(|v| truncate_utf16(v, 64)),
            client_version: Some(truncate_utf16(&identity.client_version, 64)),
        }
    }

    async fn device_for_server(&self) -> Result<DeviceInput, ApiError> {
        let info = match self.server_info() {
            Some(info) => info,
            None => self.check(&self.server_url()).await?,
        };
        self.lock().server_info = Some(info.clone());
        Ok(self.device_input(&info.server_id))
    }

    /// Создать аккаунт; ответ — код восстановления, он показывается один раз (§4.3). Доказательство
    /// работы считается в отдельном потоке; `cancel` его прерывает.
    pub async fn register(&self, login: &str, password: &str, cancel: Arc<AtomicBool>) -> Result<String, ApiError> {
        let device = self.device_for_server().await?;
        let api = self.api(None);
        let solve = |challenge: RegisterChallenge, cancel: Arc<AtomicBool>| async move {
            if challenge.bits == 0 {
                return Ok(PowSolution { challenge: challenge.challenge, nonce: "0".into() });
            }
            let text = challenge.challenge.clone();
            let nonce = tokio::task::spawn_blocking(move || melogold_core::pow::solve(&text, challenge.bits, &cancel))
                .await
                .ok()
                .flatten()
                .ok_or_else(|| ApiError::new(0, "cancelled", "Cancelled"))?;
            Ok::<_, ApiError>(PowSolution { challenge: challenge.challenge, nonce })
        };
        let pow = solve(api.register_challenge().await?, Arc::clone(&cancel)).await?;
        let session = match api.register(login, password, &device, Some(&pow)).await {
            // Один повтор с новым вызовом (§2.2).
            Err(error) if error.code == "pow_required" || error.code == "pow_invalid" => {
                let pow = solve(api.register_challenge().await?, cancel).await?;
                api.register(login, password, &device, Some(&pow)).await?
            }
            other => other?,
        };
        let code = session.recovery_code.clone().unwrap_or_default();
        self.save(session).await;
        Ok(code)
    }

    pub async fn sign_in(&self, login: &str, password: &str) -> Result<(), ApiError> {
        let device = self.device_for_server().await?;
        let session = self.api(None).login(login, password, &device).await?;
        self.save(session).await;
        Ok(())
    }

    /// Вход кодом восстановления с новым паролем: прежние устройства выходят (§4.5).
    pub async fn recover(&self, login: &str, recovery_code: &str, new_password: &str) -> Result<String, ApiError> {
        let device = self.device_for_server().await?;
        let session = self.api(None).recover(login, recovery_code, new_password, &device).await?;
        let code = session.recovery_code.clone().unwrap_or_default();
        self.save(session).await;
        Ok(code)
    }

    /// Закончить сессию и на сервере; библиотека остаётся на устройстве.
    pub async fn sign_out(&self) {
        let current = self.session();
        self.set_session(None, AccountState::SignedOut).await;
        if let Some(current) = current {
            let api = self.api(Some(&current.server_url));
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), api.logout(&current.refresh_token)).await;
        }
    }

    /// Вызов с действующим access-токеном: обновляется заранее; на `access_token_expired`/`invalid` —
    /// одно обновление и один повтор. Сессию, которую закончил сервер, переводит в [`AccountState::AuthRequired`].
    pub async fn authorized<T, F, Fut>(&self, call: F) -> Result<T, ApiError>
    where
        F: Fn(Api, String) -> Fut,
        Fut: Future<Output = Result<T, ApiError>>,
    {
        let session = self.session().ok_or_else(|| ApiError::new(401, "unauthorized", "Not signed in"))?;
        let api = self.api(Some(&session.server_url));
        let token = self.fresh_token(false).await?;
        let result = match call(api.clone(), token).await {
            Err(error) if error.code == "access_token_expired" || error.code == "access_token_invalid" => {
                let token = self.fresh_token(true).await?;
                call(api, token).await
            }
            other => other,
        };
        if let Err(error) = &result {
            if error.code == "session_revoked" {
                self.end_session().await;
            }
        }
        result
    }

    /// Действующий access-токен (для живых событий: поток живёт до его `exp`).
    pub async fn access_token(&self) -> Result<String, ApiError> {
        self.fresh_token(false).await
    }

    async fn fresh_token(&self, force: bool) -> Result<String, ApiError> {
        let _guard = self.refresh.lock().await;
        let current = self.session().ok_or_else(|| ApiError::new(401, "unauthorized", "Not signed in"))?;
        if !force && current.access_token_expires_at - now_ms() > REFRESH_EARLY_MS {
            return Ok(current.access_token);
        }
        let patch = DevicePatch {
            hwid: melogold_core::hwid::compute(&self.identity.platform_id, &current.server_id),
            client_version: Some(self.identity.client_version.clone()),
        };
        let refreshed = match self.api(Some(&current.server_url)).refresh(&current.refresh_token, &patch).await {
            Ok(refreshed) => refreshed,
            Err(error) if error.status == 401 => {
                // Любая 401 от /auth/refresh — сессия окончена (§1.7); сеть и 5xx её не трогают.
                self.end_session().await;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        // Новый refresh сохраняется до первого использования нового access (§1.7).
        let updated = StoredSession {
            access_token: refreshed.tokens.access_token.clone(),
            access_token_expires_at: iso::parse(&refreshed.tokens.access_token_expires_at).unwrap_or_else(|| now_ms() + 600_000),
            refresh_token: refreshed.tokens.refresh_token.clone(),
            ..current
        };
        let kind = self.store.save(Some(&updated)).await;
        {
            let mut inner = self.lock();
            inner.store_kind = kind;
            // Сессию могли закончить, пока шёл запрос: тогда токены не возвращаются.
            if inner.session.as_ref().is_some_and(|s| s.device_id == updated.device_id) {
                inner.session = Some(updated.clone());
            }
        }
        Ok(updated.access_token)
    }

    pub async fn me(&self) -> Result<MeResponse, ApiError> {
        self.authorized(|api, token| async move { api.me(&token).await }).await
    }

    pub async fn devices(&self) -> Result<DeviceListResponse, ApiError> {
        self.authorized(|api, token| async move { api.devices(&token).await }).await
    }

    /// Имена устройств аккаунта по `id` (фильтр Истории).
    pub async fn device_names(&self) -> Result<HashMap<String, (String, String)>, ApiError> {
        let list = self.devices().await?;
        Ok(list.devices.into_iter().map(|d| (d.id.clone(), (d.name, d.platform))).collect())
    }

    pub async fn revoke(&self, device_id: &str, password: Option<&str>) -> Result<(), ApiError> {
        let (device_id, password) = (device_id.to_owned(), password.map(str::to_owned));
        self.authorized(|api, token| {
            let (device_id, password) = (device_id.clone(), password.clone());
            async move { api.revoke_device(&token, &device_id, password.as_deref()).await }
        })
        .await
    }

    pub async fn revoke_others(&self, password: Option<&str>) -> Result<RevokeOthersResponse, ApiError> {
        let password = password.map(str::to_owned);
        self.authorized(|api, token| {
            let password = password.clone();
            async move { api.revoke_others(&token, password.as_deref()).await }
        })
        .await
    }

    /// Код с нового устройства (часы, телевизор): его данные и три числа на выбор.
    pub async fn resolve_link(&self, user_code: &str) -> Result<LinkDetails, ApiError> {
        let user_code = user_code.to_owned();
        self.authorized(|api, token| {
            let user_code = user_code.clone();
            async move { api.resolve_link(&token, &user_code).await }
        })
        .await
    }

    pub async fn approve_link(&self, link_id: &str, verify_code: &str) -> Result<LinkDecisionResponse, ApiError> {
        let (link_id, verify_code) = (link_id.to_owned(), verify_code.to_owned());
        self.authorized(|api, token| {
            let (link_id, verify_code) = (link_id.clone(), verify_code.clone());
            async move { api.approve_link(&token, &link_id, &verify_code).await }
        })
        .await
    }

    pub async fn deny_link(&self, link_id: &str) -> Result<LinkDecisionResponse, ApiError> {
        let link_id = link_id.to_owned();
        self.authorized(|api, token| {
            let link_id = link_id.clone();
            async move { api.deny_link(&token, &link_id).await }
        })
        .await
    }

    // ── вход по коду (§4.6, задание 0008) ──

    /// Режим `request`: код этого (нового) устройства. Ответ несёт `pollSecret`.
    pub async fn request_link(&self) -> Result<LinkCreated, ApiError> {
        let device = self.device_for_server().await?;
        self.api(None).create_link_request(&device).await
    }

    /// Режим `invite`: ввели код, который показывает вошедшее устройство.
    pub async fn claim_link(&self, user_code: &str) -> Result<LinkClaimed, ApiError> {
        let device = self.device_for_server().await?;
        self.api(None).claim_link(user_code, &device).await
    }

    /// Длинный опрос новой привязки. При `completed` сессия берётся так же, как после входа по
    /// паролю: то же хранилище, то же состояние, тот же первый синк по подписке.
    pub async fn poll_link(&self, poll_secret: &str, known_status: &str) -> Result<LinkPollResponse, ApiError> {
        let mut response = self.api(None).poll_link(poll_secret, known_status).await?;
        if response.status == "completed" {
            match response.session.take() {
                Some(session) => self.save(session).await,
                None => return Err(ApiError::new(200, "invalid_response", "completed without a session")),
            }
        }
        Ok(response)
    }

    pub async fn cancel_link_request(&self, poll_secret: &str) -> Result<(), ApiError> {
        self.api(None).cancel_link_request(poll_secret).await
    }

    /// «Показать код для нового устройства»: приглашение (`POST /auth/me/links`).
    pub async fn create_invite(&self) -> Result<LinkCreated, ApiError> {
        self.authorized(|api, token| async move { api.create_invite(&token).await }).await
    }

    pub async fn link(&self, link_id: &str) -> Result<LinkDetails, ApiError> {
        let link_id = link_id.to_owned();
        self.authorized(|api, token| {
            let link_id = link_id.clone();
            async move { api.link(&token, &link_id).await }
        })
        .await
    }

    pub async fn cancel_invite(&self, link_id: &str) -> Result<(), ApiError> {
        let link_id = link_id.to_owned();
        self.authorized(|api, token| {
            let link_id = link_id.clone();
            async move { api.cancel_invite(&token, &link_id).await }
        })
        .await
    }

    // ── ссылки на свои плейлисты (§4.11, задание 0010) ──

    pub async fn create_share(&self, name: &str, tracks: Vec<TrackInput>) -> Result<ShareCreated, ApiError> {
        let name = name.to_owned();
        self.authorized(|api, token| {
            let (name, tracks) = (name.clone(), tracks.clone());
            async move { api.create_share(&token, &name, &tracks).await }
        })
        .await
    }

    pub async fn shares(&self) -> Result<ShareList, ApiError> {
        self.authorized(|api, token| async move { api.shares(&token).await }).await
    }

    pub async fn delete_share(&self, share_id: &str) -> Result<(), ApiError> {
        let share_id = share_id.to_owned();
        self.authorized(|api, token| {
            let share_id = share_id.clone();
            async move { api.delete_share(&token, &share_id).await }
        })
        .await
    }

    /// Снимок по ссылке на любом сервере, без входа.
    pub async fn open_share(&self, base_url: &str, share_id: &str) -> Result<ShareDto, ApiError> {
        Api::new(base_url, &self.identity.client_version, &self.identity.language).share(share_id).await
    }

    /// Удалить аккаунт (пароль — повторная проверка на сервере): сессия заканчивается.
    pub async fn delete_account(&self, password: &str) -> Result<(), ApiError> {
        let password = password.to_owned();
        self.authorized(|api, token| {
            let password = password.clone();
            async move { api.delete_account(&token, &password).await }
        })
        .await?;
        self.set_session(None, AccountState::SignedOut).await;
        Ok(())
    }

    /// Сервер закончил сессию: войти снова, данные остаются.
    pub async fn end_session(&self) {
        let Some(current) = self.session() else { return };
        self.set_session(None, AccountState::AuthRequired { login: current.login, server_url: current.server_url }).await;
    }

    async fn save(&self, session: AuthSession) {
        let stored = StoredSession {
            server_url: self.server_url(),
            server_id: session.server_id,
            user_id: session.user.id,
            login: session.user.login,
            device_id: session.device.id,
            access_token: session.tokens.access_token,
            access_token_expires_at: iso::parse(&session.tokens.access_token_expires_at).unwrap_or_else(|| now_ms() + 600_000),
            refresh_token: session.tokens.refresh_token,
        };
        let state = AccountState::SignedIn {
            login: stored.login.clone(),
            device_id: stored.device_id.clone(),
            server_url: stored.server_url.clone(),
        };
        self.set_session(Some(stored), state).await;
    }

    async fn set_session(&self, session: Option<StoredSession>, state: AccountState) {
        let kind = self.store.save(session.as_ref()).await;
        {
            let mut inner = self.lock();
            inner.session = session;
            inner.state = state.clone();
            inner.store_kind = kind;
        }
        tracing::info!(вход = !matches!(state, AccountState::SignedOut), "аккаунт");
        for listener in self.listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            listener(&state);
        }
    }
}
