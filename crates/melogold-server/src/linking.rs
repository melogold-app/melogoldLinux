//! Вход по коду (API §4.6, задание 0008, образец — Android `DeviceLinking.kt`). Две стороны и два режима:
//!
//! * режим `request`: НОВОЕ устройство показывает код, вошедшее вводит его и выбирает число;
//! * режим `invite`: ВОШЕДШЕЕ устройство показывает код, новое вводит его и показывает число.
//!
//! [`NewDeviceLinker`] — новое устройство: оба режима кончаются одним длинным опросом, который приносит
//! сессию. [`InviteLinker`] — вошедшее устройство, которое показывает код и ждёт, пока новое его введёт.
//! Состояние — значение, которое читают экраны; сеть спрятана за портом, поэтому логика проверяется без неё.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use melogold_core::iso;
use melogold_core::text::now_ms;
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::account::Account;
use crate::api::ApiError;
use crate::dto::{LinkClaimed, LinkCreated, LinkDetails, LinkPollResponse};

/// Пауза перед повтором опроса при сети, 429 и 5xx.
pub const RETRY_DELAY: Duration = Duration::from_secs(3);
/// Опрос приглашения без SSE (и страховка с ним): раз в 3 с (§4.6).
pub const INVITE_POLL: Duration = Duration::from_secs(3);
/// Код живёт 5 минут, если время сервера не читается.
const DEFAULT_TTL_MS: i64 = 5 * 60_000;

/// Почему привязка никуда не привела, в словах экранов (API §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkFailure {
    /// На другом устройстве отказали или выбрали не то число.
    Denied,
    Expired,
    Cancelled,
    DeviceLimit,
    NotFound,
    AlreadyClaimed,
    WrongMode,
    Throttled,
    Network,
    Unknown,
}

/// Ветвится только по `code` (сервер отвечает `410 link_expired` и для отменённой привязки).
pub fn link_failure_of(error: &ApiError) -> LinkFailure {
    match error.code.as_str() {
        "link_denied" | "link_verify_mismatch" => LinkFailure::Denied,
        "link_expired" => LinkFailure::Expired,
        "link_cancelled" => LinkFailure::Cancelled,
        "link_not_found" => LinkFailure::NotFound,
        "link_already_claimed" => LinkFailure::AlreadyClaimed,
        "link_wrong_mode" => LinkFailure::WrongMode,
        "device_limit_reached" => LinkFailure::DeviceLimit,
        "rate_limited" | "login_throttled" => LinkFailure::Throttled,
        _ if error.is_network() => LinkFailure::Network,
        _ => LinkFailure::Unknown,
    }
}

/// Статус привязки, которая кончилась, как причина; `None`, пока она жива или кончилась хорошо.
pub fn status_failure(status: &str) -> Option<LinkFailure> {
    match status {
        "denied" => Some(LinkFailure::Denied),
        "expired" => Some(LinkFailure::Expired),
        "cancelled" => Some(LinkFailure::Cancelled),
        _ => None,
    }
}

/// Конец срока кода в миллисекундах эпохи; время сервера не читается — пять минут от сейчас.
pub fn expiry_of(iso_time: &str) -> i64 {
    iso::parse(iso_time).unwrap_or_else(|| now_ms() + DEFAULT_TTL_MS)
}

type Listener<S> = Box<dyn Fn(&S) + Send + Sync>;

/// Состояние и подписчики: общая часть обоих исполнителей.
struct Cell<S> {
    state: Mutex<S>,
    listeners: Mutex<Vec<Listener<S>>>,
}

impl<S: Clone + PartialEq> Cell<S> {
    fn new(initial: S) -> Cell<S> {
        Cell { state: Mutex::new(initial), listeners: Mutex::default() }
    }

    fn get(&self) -> S {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set(&self, value: S) {
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if *state == value {
                return;
            }
            *state = value.clone();
        }
        for listener in self.listeners.lock().unwrap_or_else(|p| p.into_inner()).iter() {
            listener(&value);
        }
    }

    fn update(&self, change: impl FnOnce(&S) -> Option<S>) {
        let next = change(&self.get());
        if let Some(next) = next {
            self.set(next);
        }
    }

    fn subscribe(&self, listener: impl Fn(&S) + Send + Sync + 'static) {
        self.listeners.lock().unwrap_or_else(|p| p.into_inner()).push(Box::new(listener));
    }
}

// ── новое устройство ──

/// Где стоит вход по коду на этом (новом) устройстве.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewDeviceLinkState {
    /// Ничего не просили или отказались.
    Idle,
    /// Сервер просят о коде (режим `request`) или об одобрении введённого (режим `invite`).
    Starting,
    /// Режим `request`: код этого устройства. `reconnecting` — сервер не отвечает.
    ShowingCode { user_code: String, expires_at: i64, reconnecting: bool },
    /// Другая сторона получила код: число, которое нужно выбрать там, и для кого.
    Verify { verify_code: String, login: String, approver_name: String, approver_platform: String, expires_at: i64, reconnecting: bool },
    /// Сессия взята: так же, как после входа по паролю.
    SignedIn,
    /// Привязка кончилась. `started == false`: она даже не началась (кода нет, введённый отклонили).
    Failed { failure: LinkFailure, started: bool },
}

/// Что исполнителю нужно от сервера: в приложении — [`AccountLinkPort`], в тестах — заглушка.
pub trait NewDeviceLinkPort: Send + Sync + 'static {
    fn request(&self) -> impl Future<Output = Result<LinkCreated, ApiError>> + Send;
    fn claim(&self, user_code: &str) -> impl Future<Output = Result<LinkClaimed, ApiError>> + Send;
    /// При `completed` сессия уже взята.
    fn poll(&self, poll_secret: &str, known_status: &str) -> impl Future<Output = Result<LinkPollResponse, ApiError>> + Send;
    fn cancel(&self, poll_secret: &str) -> impl Future<Output = Result<(), ApiError>> + Send;
}

pub struct AccountLinkPort(pub Arc<Account>);

impl NewDeviceLinkPort for AccountLinkPort {
    fn request(&self) -> impl Future<Output = Result<LinkCreated, ApiError>> + Send {
        self.0.request_link()
    }

    fn claim(&self, user_code: &str) -> impl Future<Output = Result<LinkClaimed, ApiError>> + Send {
        let user_code = user_code.to_owned();
        async move { self.0.claim_link(&user_code).await }
    }

    fn poll(&self, poll_secret: &str, known_status: &str) -> impl Future<Output = Result<LinkPollResponse, ApiError>> + Send {
        let (poll_secret, known_status) = (poll_secret.to_owned(), known_status.to_owned());
        async move { self.0.poll_link(&poll_secret, &known_status).await }
    }

    fn cancel(&self, poll_secret: &str) -> impl Future<Output = Result<(), ApiError>> + Send {
        let poll_secret = poll_secret.to_owned();
        async move { self.0.cancel_link_request(&poll_secret).await }
    }
}

struct NewInner<P> {
    port: P,
    handle: Handle,
    retry: Duration,
    cell: Cell<NewDeviceLinkState>,
    job: Mutex<Option<JoinHandle<()>>>,
    poll_secret: Mutex<Option<String>>,
}

/// Вход по коду на новом устройстве: [`show_code`](Self::show_code) просит код и ждёт, пока вошедшее
/// устройство его одобрит (режим `request`); [`claim`](Self::claim) вводит код, который показывает
/// вошедшее (режим `invite`). Оба пути идут в [`NewDeviceLinkState::Verify`] (число на другом
/// устройстве) и кончаются [`NewDeviceLinkState::SignedIn`] с уже взятой сессией.
pub struct NewDeviceLinker<P: NewDeviceLinkPort>(Arc<NewInner<P>>);

impl<P: NewDeviceLinkPort> Clone for NewDeviceLinker<P> {
    fn clone(&self) -> Self {
        NewDeviceLinker(Arc::clone(&self.0))
    }
}

impl<P: NewDeviceLinkPort> NewDeviceLinker<P> {
    pub fn new(port: P, handle: Handle) -> NewDeviceLinker<P> {
        NewDeviceLinker::with_retry(port, handle, RETRY_DELAY)
    }

    pub fn with_retry(port: P, handle: Handle, retry: Duration) -> NewDeviceLinker<P> {
        NewDeviceLinker(Arc::new(NewInner {
            port,
            handle,
            retry,
            cell: Cell::new(NewDeviceLinkState::Idle),
            job: Mutex::new(None),
            poll_secret: Mutex::new(None),
        }))
    }

    pub fn state(&self) -> NewDeviceLinkState {
        self.0.cell.get()
    }

    /// Состояние сменилось (из любого потока).
    pub fn subscribe(&self, listener: impl Fn(&NewDeviceLinkState) + Send + Sync + 'static) {
        self.0.cell.subscribe(listener);
    }

    /// Режим `request`: код этого устройства.
    pub fn show_code(&self) {
        let this = self.clone();
        self.start(async move {
            let created = match this.0.port.request().await {
                Ok(created) => created,
                Err(error) => {
                    this.0.cell.set(NewDeviceLinkState::Failed { failure: link_failure_of(&error), started: false });
                    return;
                }
            };
            let Some(secret) = created.poll_secret.clone() else {
                this.0.cell.set(NewDeviceLinkState::Failed { failure: LinkFailure::Unknown, started: false });
                return;
            };
            *this.0.poll_secret.lock().unwrap_or_else(|p| p.into_inner()) = Some(secret.clone());
            this.0.cell.set(NewDeviceLinkState::ShowingCode {
                user_code: created.user_code,
                expires_at: expiry_of(&created.expires_at),
                reconnecting: false,
            });
            this.follow(&secret, "pending").await;
        });
    }

    /// Режим `invite`: `user_code` (уже `K7QX-M2PD`) показывает вошедшее устройство.
    pub fn claim(&self, user_code: &str) {
        let (this, user_code) = (self.clone(), user_code.to_owned());
        self.start(async move {
            let claimed = match this.0.port.claim(&user_code).await {
                Ok(claimed) => claimed,
                Err(error) => {
                    this.0.cell.set(NewDeviceLinkState::Failed { failure: link_failure_of(&error), started: false });
                    return;
                }
            };
            *this.0.poll_secret.lock().unwrap_or_else(|p| p.into_inner()) = Some(claimed.poll_secret.clone());
            this.0.cell.set(NewDeviceLinkState::Verify {
                verify_code: claimed.verify_code,
                login: claimed.account.login,
                approver_name: claimed.approver_device.name,
                approver_platform: claimed.approver_device.platform,
                expires_at: expiry_of(&claimed.expires_at),
                reconnecting: false,
            });
            this.follow(&claimed.poll_secret, "claimed").await;
        });
    }

    /// Отказаться: здесь и на сервере (ответа не ждём). Кроме случая, когда сессия уже взята, — в `Idle`.
    pub fn cancel(&self) {
        if let Some(job) = self.0.job.lock().unwrap_or_else(|p| p.into_inner()).take() {
            job.abort();
        }
        let secret = self.0.poll_secret.lock().unwrap_or_else(|p| p.into_inner()).take();
        if self.0.cell.get() != NewDeviceLinkState::SignedIn {
            self.0.cell.set(NewDeviceLinkState::Idle);
        }
        if let Some(secret) = secret {
            let this = self.clone();
            self.0.handle.spawn(async move {
                let _ = this.0.port.cancel(&secret).await;
            });
        }
    }

    fn start(&self, work: impl Future<Output = ()> + Send + 'static) {
        self.cancel();
        self.0.cell.set(NewDeviceLinkState::Starting);
        let job = self.0.handle.spawn(work);
        *self.0.job.lock().unwrap_or_else(|p| p.into_inner()) = Some(job);
    }

    /// Длинный опрос (§4.6): ответ сразу, если статус сдвинулся, иначе через 25 с.
    async fn follow(&self, secret: &str, known_status: &str) {
        let mut known = known_status.to_owned();
        loop {
            match self.0.port.poll(secret, &known).await {
                Err(error) if error.is_transient() => {
                    self.reconnecting(true);
                    tokio::time::sleep(self.0.retry).await;
                }
                Err(error) => {
                    // Отказ, срок, отмена, лимит устройств: привязка кончилась, отменять нечего.
                    *self.0.poll_secret.lock().unwrap_or_else(|p| p.into_inner()) = None;
                    self.0.cell.set(NewDeviceLinkState::Failed { failure: link_failure_of(&error), started: true });
                    return;
                }
                Ok(answer) => match answer.status.as_str() {
                    "completed" => {
                        *self.0.poll_secret.lock().unwrap_or_else(|p| p.into_inner()) = None;
                        self.0.cell.set(NewDeviceLinkState::SignedIn);
                        return;
                    }
                    "claimed" => {
                        known = "claimed".into();
                        match verify_of(&answer) {
                            Some(verify) => self.0.cell.set(verify),
                            None => {
                                *self.0.poll_secret.lock().unwrap_or_else(|p| p.into_inner()) = None;
                                self.0.cell.set(NewDeviceLinkState::Failed { failure: LinkFailure::Unknown, started: true });
                                return;
                            }
                        }
                    }
                    _ => self.reconnecting(false),
                },
            }
        }
    }

    /// Код (или число) на экране: «сервер не отвечает» или снова свежий.
    fn reconnecting(&self, value: bool) {
        self.0.cell.update(|current| match current {
            NewDeviceLinkState::ShowingCode { user_code, expires_at, .. } => {
                Some(NewDeviceLinkState::ShowingCode { user_code: user_code.clone(), expires_at: *expires_at, reconnecting: value })
            }
            NewDeviceLinkState::Verify { verify_code, login, approver_name, approver_platform, expires_at, .. } => {
                Some(NewDeviceLinkState::Verify {
                    verify_code: verify_code.clone(),
                    login: login.clone(),
                    approver_name: approver_name.clone(),
                    approver_platform: approver_platform.clone(),
                    expires_at: *expires_at,
                    reconnecting: value,
                })
            }
            _ => None,
        });
    }
}

fn verify_of(answer: &LinkPollResponse) -> Option<NewDeviceLinkState> {
    let approver = answer.approver_device.as_ref()?;
    Some(NewDeviceLinkState::Verify {
        verify_code: answer.verify_code.clone()?,
        login: answer.account.as_ref()?.login.clone(),
        approver_name: approver.name.clone(),
        approver_platform: approver.platform.clone(),
        expires_at: expiry_of(&answer.expires_at),
        reconnecting: false,
    })
}

// ── вошедшее устройство, которое показывает код ──

#[derive(Clone, Debug, PartialEq)]
pub enum InviteState {
    Idle,
    Starting,
    /// Код, который вводят на новом устройстве.
    Waiting {
        user_code: String,
        expires_at: i64,
    },
    /// Новое устройство ввело код: его карточка и три числа (экран одобрения).
    Claimed(Box<LinkDetails>),
    /// Приглашение кончилось. `started == false`: кода не получилось получить.
    Failed {
        failure: LinkFailure,
        started: bool,
    },
}

pub trait InvitePort: Send + Sync + 'static {
    fn create(&self) -> impl Future<Output = Result<LinkCreated, ApiError>> + Send;
    fn get(&self, link_id: &str) -> impl Future<Output = Result<LinkDetails, ApiError>> + Send;
    fn cancel(&self, link_id: &str) -> impl Future<Output = Result<(), ApiError>> + Send;
}

pub struct AccountInvitePort(pub Arc<Account>);

impl InvitePort for AccountInvitePort {
    fn create(&self) -> impl Future<Output = Result<LinkCreated, ApiError>> + Send {
        self.0.create_invite()
    }

    fn get(&self, link_id: &str) -> impl Future<Output = Result<LinkDetails, ApiError>> + Send {
        let link_id = link_id.to_owned();
        async move { self.0.link(&link_id).await }
    }

    fn cancel(&self, link_id: &str) -> impl Future<Output = Result<(), ApiError>> + Send {
        let link_id = link_id.to_owned();
        async move { self.0.cancel_invite(&link_id).await }
    }
}

struct InviteInner<P> {
    port: P,
    handle: Handle,
    poll: Duration,
    cell: Cell<InviteState>,
    job: Mutex<Option<JoinHandle<()>>>,
    link_id: Mutex<Option<String>>,
    nudge: Notify,
}

/// «Показать код для нового устройства» (режим `invite`): делает приглашение и следит за ним, пока новое
/// устройство его не введёт (событие `link.updated` читает приглашение сразу — [`nudge`](Self::nudge);
/// раз в [`INVITE_POLL`] — страховка и единственный способ без SSE). Слежение идёт и после ввода: если
/// новое устройство передумало или код истёк, состояние скажет об этом.
pub struct InviteLinker<P: InvitePort>(Arc<InviteInner<P>>);

impl<P: InvitePort> Clone for InviteLinker<P> {
    fn clone(&self) -> Self {
        InviteLinker(Arc::clone(&self.0))
    }
}

impl<P: InvitePort> InviteLinker<P> {
    pub fn new(port: P, handle: Handle) -> InviteLinker<P> {
        InviteLinker::with_poll(port, handle, INVITE_POLL)
    }

    pub fn with_poll(port: P, handle: Handle, poll: Duration) -> InviteLinker<P> {
        InviteLinker(Arc::new(InviteInner {
            port,
            handle,
            poll,
            cell: Cell::new(InviteState::Idle),
            job: Mutex::new(None),
            link_id: Mutex::new(None),
            nudge: Notify::new(),
        }))
    }

    pub fn state(&self) -> InviteState {
        self.0.cell.get()
    }

    pub fn subscribe(&self, listener: impl Fn(&InviteState) + Send + Sync + 'static) {
        self.0.cell.subscribe(listener);
    }

    pub fn start(&self) {
        self.cancel();
        self.0.cell.set(InviteState::Starting);
        let this = self.clone();
        let job = self.0.handle.spawn(async move {
            let created = match this.0.port.create().await {
                Ok(created) => created,
                Err(error) => {
                    this.0.cell.set(InviteState::Failed { failure: link_failure_of(&error), started: false });
                    return;
                }
            };
            let id = created.link_id.clone();
            *this.0.link_id.lock().unwrap_or_else(|p| p.into_inner()) = Some(id.clone());
            this.0.cell.set(InviteState::Waiting { user_code: created.user_code, expires_at: expiry_of(&created.expires_at) });
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(this.0.poll) => {}
                    _ = this.0.nudge.notified() => {}
                }
                if this.refresh(&id).await {
                    return;
                }
            }
        });
        *self.0.job.lock().unwrap_or_else(|p| p.into_inner()) = Some(job);
    }

    /// Пришло `link.updated` про эту привязку: прочитать её сейчас, не дожидаясь опроса.
    pub fn nudge(&self, link_id: &str) {
        if self.0.link_id.lock().unwrap_or_else(|p| p.into_inner()).as_deref() == Some(link_id) {
            self.0.nudge.notify_one();
        }
    }

    /// Приглашение читается один раз; `true` — следить больше не за чем.
    async fn refresh(&self, id: &str) -> bool {
        let details = match self.0.port.get(id).await {
            Ok(details) => details,
            Err(error) if error.is_transient() => return false,
            Err(error) => {
                *self.0.link_id.lock().unwrap_or_else(|p| p.into_inner()) = None;
                self.0.cell.set(InviteState::Failed { failure: link_failure_of(&error), started: true });
                return true;
            }
        };
        if let Some(failure) = status_failure(&details.status) {
            *self.0.link_id.lock().unwrap_or_else(|p| p.into_inner()) = None;
            self.0.cell.set(InviteState::Failed { failure, started: true });
            return true;
        }
        match details.status.as_str() {
            "claimed" if !details.verify_choices.is_empty() => self.0.cell.set(InviteState::Claimed(Box::new(details))),
            // Решено здесь (approved) и закончено там (completed): экран уже всё сказал.
            "approved" | "completed" => return true,
            _ => {}
        }
        false
    }

    /// Перестать следить, не отменяя: решение принято (одобрено или отклонено).
    pub fn release(&self) {
        if let Some(job) = self.0.job.lock().unwrap_or_else(|p| p.into_inner()).take() {
            job.abort();
        }
        *self.0.link_id.lock().unwrap_or_else(|p| p.into_inner()) = None;
        self.0.cell.set(InviteState::Idle);
    }

    /// Отменить приглашение здесь и на сервере (иначе набегут: активных не больше трёх).
    pub fn cancel(&self) {
        let id = self.0.link_id.lock().unwrap_or_else(|p| p.into_inner()).clone();
        self.release();
        if let Some(id) = id {
            let this = self.clone();
            self.0.handle.spawn(async move {
                let _ = this.0.port.cancel(&id).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::dto::{LinkAccount, LinkApprover};

    fn error(status: u16, code: &str) -> ApiError {
        ApiError::new(status, code, "")
    }

    fn created(poll: Option<&str>) -> LinkCreated {
        LinkCreated {
            link_id: "l1".into(),
            user_code: "K7QX-M2PD".into(),
            poll_secret: poll.map(str::to_owned),
            expires_at: "2026-09-30T10:05:00.000Z".into(),
            ..Default::default()
        }
    }

    fn answer(status: &str) -> LinkPollResponse {
        let claimed = status != "pending";
        LinkPollResponse {
            status: status.into(),
            expires_at: "2026-09-30T10:05:00.000Z".into(),
            account: claimed.then(|| LinkAccount { login: "maxim".into() }),
            approver_device: claimed.then(|| LinkApprover { name: "MacBook Air".into(), platform: "macos".into() }),
            verify_code: claimed.then(|| "47".into()),
            ..Default::default()
        }
    }

    /// Порт по сценарию: ответы берутся из очередей по порядку; пустая очередь опроса — ждать вечно.
    #[derive(Default)]
    struct Script {
        request: Mutex<VecDeque<Result<LinkCreated, ApiError>>>,
        claim: Mutex<VecDeque<Result<LinkClaimed, ApiError>>>,
        poll: Mutex<VecDeque<Result<LinkPollResponse, ApiError>>>,
        polls: Mutex<Vec<(String, String)>>,
        cancels: Mutex<Vec<String>>,
    }

    struct FakePort(Arc<Script>);

    impl NewDeviceLinkPort for FakePort {
        async fn request(&self) -> Result<LinkCreated, ApiError> {
            self.0.request.lock().unwrap().pop_front().unwrap_or_else(|| Err(error(0, "network")))
        }

        async fn claim(&self, _user_code: &str) -> Result<LinkClaimed, ApiError> {
            self.0.claim.lock().unwrap().pop_front().unwrap_or_else(|| Err(error(0, "network")))
        }

        async fn poll(&self, secret: &str, known: &str) -> Result<LinkPollResponse, ApiError> {
            self.0.polls.lock().unwrap().push((secret.to_owned(), known.to_owned()));
            let next = self.0.poll.lock().unwrap().pop_front();
            match next {
                Some(result) => result,
                None => std::future::pending().await,
            }
        }

        async fn cancel(&self, secret: &str) -> Result<(), ApiError> {
            self.0.cancels.lock().unwrap().push(secret.to_owned());
            Ok(())
        }
    }

    fn linker(script: &Arc<Script>) -> (NewDeviceLinker<FakePort>, Arc<Mutex<Vec<NewDeviceLinkState>>>) {
        let linker = NewDeviceLinker::new(FakePort(Arc::clone(script)), Handle::current());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        linker.subscribe(move |state| sink.lock().unwrap().push(state.clone()));
        (linker, seen)
    }

    async fn settle() {
        // С остановленным временем ждём, пока задачи дойдут до следующей точки ожидания.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn request_mode_shows_code_then_number_then_signs_in() {
        let script = Arc::new(Script::default());
        script.request.lock().unwrap().push_back(Ok(created(Some("mgps_x"))));
        script.poll.lock().unwrap().extend([Ok(answer("claimed")), Ok(answer("completed"))]);
        let (linker, seen) = linker(&script);
        linker.show_code();
        settle().await;
        let states = seen.lock().unwrap().clone();
        assert_eq!(states[0], NewDeviceLinkState::Starting);
        assert!(matches!(&states[1], NewDeviceLinkState::ShowingCode { user_code, reconnecting: false, .. } if user_code == "K7QX-M2PD"));
        assert!(matches!(
            &states[2],
            NewDeviceLinkState::Verify { verify_code, login, approver_name, approver_platform, .. }
                if verify_code == "47" && login == "maxim" && approver_name == "MacBook Air" && approver_platform == "macos"
        ));
        assert_eq!(states[3], NewDeviceLinkState::SignedIn);
        // Сначала спрашиваем `pending`, после `claimed` — `claimed`.
        let polls = script.polls.lock().unwrap().clone();
        assert_eq!(polls, [("mgps_x".to_owned(), "pending".to_owned()), ("mgps_x".to_owned(), "claimed".to_owned())]);
        // Сессия взята: отменять на сервере нечего.
        linker.cancel();
        settle().await;
        assert!(script.cancels.lock().unwrap().is_empty());
        assert_eq!(linker.state(), NewDeviceLinkState::SignedIn, "после входа отмена состояния не трогает");
    }

    #[tokio::test(start_paused = true)]
    async fn invite_mode_goes_straight_to_the_number() {
        let script = Arc::new(Script::default());
        script.claim.lock().unwrap().push_back(Ok(LinkClaimed {
            poll_secret: "mgps_y".into(),
            account: LinkAccount { login: "maxim".into() },
            approver_device: LinkApprover { name: "Pixel".into(), platform: "android".into() },
            verify_code: "85".into(),
            expires_at: "2026-09-30T10:05:00.000Z".into(),
            ..Default::default()
        }));
        script.poll.lock().unwrap().push_back(Ok(answer("completed")));
        let (linker, seen) = linker(&script);
        linker.claim("K7QX-M2PD");
        settle().await;
        let states = seen.lock().unwrap().clone();
        assert_eq!(states[0], NewDeviceLinkState::Starting);
        assert!(matches!(&states[1], NewDeviceLinkState::Verify { verify_code, .. } if verify_code == "85"));
        assert_eq!(states[2], NewDeviceLinkState::SignedIn);
        assert_eq!(script.polls.lock().unwrap()[0].1, "claimed", "после claim опрос сразу с knownStatus=claimed");
    }

    #[tokio::test(start_paused = true)]
    async fn no_code_without_network_or_secret() {
        let script = Arc::new(Script::default());
        script.request.lock().unwrap().extend([Err(error(0, "network")), Ok(created(None)), Err(error(429, "rate_limited"))]);
        let (linker, _) = linker(&script);
        for expected in [LinkFailure::Network, LinkFailure::Unknown, LinkFailure::Throttled] {
            linker.show_code();
            settle().await;
            assert_eq!(linker.state(), NewDeviceLinkState::Failed { failure: expected, started: false });
        }
    }

    #[tokio::test(start_paused = true)]
    async fn claim_refusals_keep_the_field() {
        let script = Arc::new(Script::default());
        script.claim.lock().unwrap().extend([
            Err(error(404, "link_not_found")),
            Err(error(409, "link_wrong_mode")),
            Err(error(410, "link_expired")),
        ]);
        let (linker, _) = linker(&script);
        for expected in [LinkFailure::NotFound, LinkFailure::WrongMode, LinkFailure::Expired] {
            linker.claim("K7QX-M2PD");
            settle().await;
            assert_eq!(linker.state(), NewDeviceLinkState::Failed { failure: expected, started: false });
        }
    }

    #[tokio::test(start_paused = true)]
    async fn network_in_the_poll_is_not_an_error_and_retries_in_three_seconds() {
        let script = Arc::new(Script::default());
        script.request.lock().unwrap().push_back(Ok(created(Some("s"))));
        script.poll.lock().unwrap().extend([
            Err(error(0, "timeout")),
            Err(error(503, "server_busy")),
            Err(error(429, "rate_limited")),
            Ok(answer("pending")),
        ]);
        let (linker, _) = linker(&script);
        linker.show_code();
        settle().await;
        assert!(matches!(linker.state(), NewDeviceLinkState::ShowingCode { reconnecting: true, .. }));
        assert_eq!(script.polls.lock().unwrap().len(), 1);
        tokio::time::advance(Duration::from_millis(2_900)).await;
        settle().await;
        assert_eq!(script.polls.lock().unwrap().len(), 1, "раньше трёх секунд не повторяем");
        tokio::time::advance(Duration::from_millis(200)).await;
        settle().await;
        assert_eq!(script.polls.lock().unwrap().len(), 2);
        for _ in 0..2 {
            tokio::time::advance(Duration::from_millis(3_100)).await;
            settle().await;
        }
        assert_eq!(script.polls.lock().unwrap().len(), 5, "429 и 5xx — тоже молча повторяем, потом опрос как обычно");
        // Связь вернулась: пометка «нет связи» снимается.
        assert!(matches!(linker.state(), NewDeviceLinkState::ShowingCode { reconnecting: false, .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn poll_endings_come_from_the_server() {
        for (code, status, expected) in [
            ("link_denied", 403, LinkFailure::Denied),
            ("link_expired", 410, LinkFailure::Expired),
            ("link_cancelled", 410, LinkFailure::Cancelled),
            ("device_limit_reached", 409, LinkFailure::DeviceLimit),
        ] {
            let script = Arc::new(Script::default());
            script.request.lock().unwrap().push_back(Ok(created(Some("s"))));
            script.poll.lock().unwrap().push_back(Err(error(status, code)));
            let (linker, _) = linker(&script);
            linker.show_code();
            settle().await;
            assert_eq!(linker.state(), NewDeviceLinkState::Failed { failure: expected, started: true }, "{code}");
            linker.cancel();
            settle().await;
            assert!(script.cancels.lock().unwrap().is_empty(), "{code}: привязка кончилась, отменять нечего");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_reaches_the_server_and_stops_the_poll() {
        let script = Arc::new(Script::default());
        script.request.lock().unwrap().push_back(Ok(created(Some("mgps_z"))));
        let (linker, _) = linker(&script);
        linker.show_code();
        settle().await;
        assert!(matches!(linker.state(), NewDeviceLinkState::ShowingCode { .. }));
        linker.cancel();
        settle().await;
        assert_eq!(linker.state(), NewDeviceLinkState::Idle);
        assert_eq!(*script.cancels.lock().unwrap(), ["mgps_z"]);
        // Второй раз ничего не отправляется.
        linker.cancel();
        settle().await;
        assert_eq!(script.cancels.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn switching_to_another_code_cancels_the_shown_one() {
        let script = Arc::new(Script::default());
        script.request.lock().unwrap().push_back(Ok(created(Some("mgps_a"))));
        script.claim.lock().unwrap().push_back(Err(error(404, "link_not_found")));
        let (linker, _) = linker(&script);
        linker.show_code();
        settle().await;
        linker.claim("K7QX-M2PD");
        settle().await;
        assert_eq!(*script.cancels.lock().unwrap(), ["mgps_a"]);
        assert_eq!(linker.state(), NewDeviceLinkState::Failed { failure: LinkFailure::NotFound, started: false });
    }

    #[test]
    fn failures_by_code_not_by_text() {
        let cases = [
            (403, "link_denied", LinkFailure::Denied),
            (409, "link_verify_mismatch", LinkFailure::Denied),
            (410, "link_expired", LinkFailure::Expired),
            (410, "link_cancelled", LinkFailure::Cancelled),
            (404, "link_not_found", LinkFailure::NotFound),
            (409, "link_already_claimed", LinkFailure::AlreadyClaimed),
            (409, "link_wrong_mode", LinkFailure::WrongMode),
            (409, "device_limit_reached", LinkFailure::DeviceLimit),
            (429, "rate_limited", LinkFailure::Throttled),
            (429, "login_throttled", LinkFailure::Throttled),
            (0, "network", LinkFailure::Network),
            (0, "timeout", LinkFailure::Network),
            (500, "internal", LinkFailure::Unknown),
            (400, "invalid_request", LinkFailure::Unknown),
        ];
        for (status, code, expected) in cases {
            assert_eq!(link_failure_of(&error(status, code)), expected, "{code}");
        }
        assert_eq!(status_failure("denied"), Some(LinkFailure::Denied));
        assert_eq!(status_failure("expired"), Some(LinkFailure::Expired));
        assert_eq!(status_failure("cancelled"), Some(LinkFailure::Cancelled));
        assert_eq!(status_failure("claimed"), None);
    }

    // ── приглашение ──

    #[derive(Default)]
    struct InviteScript {
        create: Mutex<VecDeque<Result<LinkCreated, ApiError>>>,
        get: Mutex<VecDeque<Result<LinkDetails, ApiError>>>,
        gets: Mutex<usize>,
        cancels: Mutex<Vec<String>>,
    }

    struct FakeInvite(Arc<InviteScript>);

    impl InvitePort for FakeInvite {
        async fn create(&self) -> Result<LinkCreated, ApiError> {
            self.0.create.lock().unwrap().pop_front().unwrap_or_else(|| Err(error(0, "network")))
        }

        async fn get(&self, _id: &str) -> Result<LinkDetails, ApiError> {
            *self.0.gets.lock().unwrap() += 1;
            self.0.get.lock().unwrap().pop_front().unwrap_or_else(|| Ok(details("pending")))
        }

        async fn cancel(&self, id: &str) -> Result<(), ApiError> {
            self.0.cancels.lock().unwrap().push(id.to_owned());
            Ok(())
        }
    }

    fn details(status: &str) -> LinkDetails {
        LinkDetails {
            link_id: "l1".into(),
            status: status.into(),
            verify_choices: if status == "claimed" { vec!["12".into(), "47".into(), "85".into()] } else { vec![] },
            ..Default::default()
        }
    }

    fn invite(script: &Arc<InviteScript>) -> InviteLinker<FakeInvite> {
        InviteLinker::new(FakeInvite(Arc::clone(script)), Handle::current())
    }

    #[tokio::test(start_paused = true)]
    async fn invite_waits_polls_every_three_seconds_and_shows_the_claimed_card() {
        let script = Arc::new(InviteScript::default());
        script.create.lock().unwrap().push_back(Ok(created(None)));
        script.get.lock().unwrap().extend([Ok(details("pending")), Ok(details("claimed"))]);
        let linker = invite(&script);
        linker.start();
        settle().await;
        assert!(matches!(linker.state(), InviteState::Waiting { ref user_code, .. } if user_code == "K7QX-M2PD"));
        assert_eq!(*script.gets.lock().unwrap(), 0);
        tokio::time::advance(Duration::from_millis(3_100)).await;
        settle().await;
        assert_eq!(*script.gets.lock().unwrap(), 1);
        assert!(matches!(linker.state(), InviteState::Waiting { .. }));
        tokio::time::advance(Duration::from_millis(3_100)).await;
        settle().await;
        assert!(matches!(linker.state(), InviteState::Claimed(ref d) if d.verify_choices.len() == 3));
    }

    #[tokio::test(start_paused = true)]
    async fn invite_event_reads_the_link_at_once() {
        let script = Arc::new(InviteScript::default());
        script.create.lock().unwrap().push_back(Ok(created(None)));
        script.get.lock().unwrap().push_back(Ok(details("claimed")));
        let linker = invite(&script);
        linker.start();
        settle().await;
        linker.nudge("other-link");
        settle().await;
        assert_eq!(*script.gets.lock().unwrap(), 0, "чужая привязка не в счёт");
        linker.nudge("l1");
        settle().await;
        assert!(matches!(linker.state(), InviteState::Claimed(_)), "без ожидания трёх секунд");
    }

    #[tokio::test(start_paused = true)]
    async fn invite_keeps_following_a_claimed_link() {
        let script = Arc::new(InviteScript::default());
        script.create.lock().unwrap().push_back(Ok(created(None)));
        script.get.lock().unwrap().extend([Ok(details("claimed")), Ok(details("cancelled"))]);
        let linker = invite(&script);
        linker.start();
        settle().await;
        for _ in 0..2 {
            tokio::time::advance(Duration::from_millis(3_100)).await;
            settle().await;
        }
        assert_eq!(linker.state(), InviteState::Failed { failure: LinkFailure::Cancelled, started: true });
    }

    #[tokio::test(start_paused = true)]
    async fn invite_failures_and_transient_errors() {
        let script = Arc::new(InviteScript::default());
        script.create.lock().unwrap().push_back(Ok(created(None)));
        script.get.lock().unwrap().extend([Err(error(0, "network")), Err(error(503, "server_busy")), Ok(details("expired"))]);
        let linker = invite(&script);
        linker.start();
        settle().await;
        for _ in 0..2 {
            tokio::time::advance(Duration::from_millis(3_100)).await;
            settle().await;
            assert!(matches!(linker.state(), InviteState::Waiting { .. }), "сеть и 5xx молча повторяются");
        }
        tokio::time::advance(Duration::from_millis(3_100)).await;
        settle().await;
        assert_eq!(linker.state(), InviteState::Failed { failure: LinkFailure::Expired, started: true });

        let script = Arc::new(InviteScript::default());
        script.create.lock().unwrap().push_back(Err(error(429, "rate_limited")));
        let linker = invite(&script);
        linker.start();
        settle().await;
        assert_eq!(linker.state(), InviteState::Failed { failure: LinkFailure::Throttled, started: false });
    }

    #[tokio::test(start_paused = true)]
    async fn invite_cancel_and_release() {
        let script = Arc::new(InviteScript::default());
        script.create.lock().unwrap().extend([Ok(created(None)), Ok(created(None))]);
        let linker = invite(&script);
        linker.start();
        settle().await;
        linker.cancel();
        settle().await;
        assert_eq!(*script.cancels.lock().unwrap(), ["l1"]);
        assert_eq!(linker.state(), InviteState::Idle);
        // Решение принято: приглашение не отменяется, новое устройство как раз получает сессию.
        linker.start();
        settle().await;
        linker.release();
        settle().await;
        assert_eq!(script.cancels.lock().unwrap().len(), 1);
        let gets = *script.gets.lock().unwrap();
        tokio::time::advance(Duration::from_secs(10)).await;
        settle().await;
        assert_eq!(*script.gets.lock().unwrap(), gets, "после release опроса нет");
    }
}
