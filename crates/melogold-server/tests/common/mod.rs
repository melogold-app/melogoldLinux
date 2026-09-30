//! Общее для живых тестов против локального сервера (`MELOGOLD_LIVE_SERVER`).
#![allow(dead_code)]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use melogold_server::account::{Account, DeviceIdentity};
use melogold_server::session::SessionStore;

pub fn server() -> String {
    std::env::var("MELOGOLD_LIVE_SERVER").expect("MELOGOLD_LIVE_SERVER: адрес локального сервера")
}

pub fn account(name: &str, platform_id: &str, dir: &std::path::Path) -> Arc<Account> {
    let identity = DeviceIdentity {
        platform_id: platform_id.to_owned(),
        name: name.to_owned(),
        os_version: Some("Test OS".into()),
        model: None,
        client_version: "0.0.0-test".into(),
        language: "ru".into(),
    };
    Account::new(identity, SessionStore::file_only(dir.join(format!("{platform_id}.json"))), Some(server()))
}

pub async fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 {
        if let Some(found) = probe() {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("не дождались");
}

pub struct Pair {
    pub first: Arc<Account>,
    pub second: Arc<Account>,
    pub login: String,
    pub password: String,
}

pub async fn pair() -> Pair {
    let dir = std::env::temp_dir().join(format!("melogold-live-{}", melogold_core::ids::new_uuid()));
    std::fs::create_dir_all(&dir).unwrap();
    let suffix: String = melogold_core::ids::new_uuid().chars().filter(|c| c.is_ascii_alphanumeric()).take(10).collect();
    let login = format!("e2elinux{suffix}");
    let password = melogold_core::ids::new_uuid().replace('-', "");
    let first = account("Компьютер", &format!("first-{suffix}"), &dir);
    first.register(&login, &password, Arc::new(AtomicBool::new(false))).await.expect("аккаунт создаётся");
    let second = account("Новый телефон", &format!("second-{suffix}"), &dir);
    Pair { first, second, login, password }
}

impl Pair {
    pub async fn cleanup(self) {
        self.first.delete_account(&self.password).await.expect("временный аккаунт удаляется");
    }
}

/// Второе устройство входит в аккаунт первого по коду (режим `request`) — как настоящий телефон.
pub async fn link_second(pair: &Pair) {
    use melogold_server::linking::{AccountLinkPort, NewDeviceLinkState, NewDeviceLinker};
    let linker = NewDeviceLinker::new(AccountLinkPort(Arc::clone(&pair.second)), tokio::runtime::Handle::current());
    linker.show_code();
    let code = wait_for(|| match linker.state() {
        NewDeviceLinkState::ShowingCode { user_code, .. } => Some(user_code),
        _ => None,
    })
    .await;
    let details = pair.first.resolve_link(&code).await.expect("resolve");
    let shown = wait_for(|| match linker.state() {
        NewDeviceLinkState::Verify { verify_code, .. } => Some(verify_code),
        _ => None,
    })
    .await;
    pair.first.approve_link(&details.link_id, &shown).await.expect("approve");
    wait_for(|| (linker.state() == NewDeviceLinkState::SignedIn).then_some(())).await;
}

/// Поток живых событий устройства: события идут в канал, пока задача жива.
pub fn open_events(
    account: &Arc<Account>,
    remote: bool,
) -> (tokio::task::JoinHandle<()>, tokio::sync::mpsc::UnboundedReceiver<melogold_server::dto::LiveEvent>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let account = Arc::clone(account);
    let job = tokio::spawn(async move {
        let _ = account
            .authorized(|api, token| {
                let tx = tx.clone();
                async move {
                    api.events(&token, remote, |event| {
                        let _ = tx.send(event);
                    })
                    .await
                }
            })
            .await;
    });
    (job, rx)
}

/// Ждать событие нужного вида.
pub async fn next_event(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<melogold_server::dto::LiveEvent>,
    kind: &str,
) -> melogold_server::dto::LiveEvent {
    let wait = async {
        loop {
            match rx.recv().await {
                Some(event) if event.kind == kind => return event,
                Some(_) => {}
                None => panic!("поток закрыт, а ждали {kind}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), wait).await.unwrap_or_else(|_| panic!("не дождались {kind}"))
}
