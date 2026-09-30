//! Вход по коду против настоящего сервера (задание 0008). Обычный `cargo test` эти тесты не запускает:
//!
//! ```sh
//! MELOGOLD_LIVE_SERVER=http://127.0.0.1:18080 cargo test -p melogold-server --test live_link -- --ignored --test-threads=1
//! ```
//!
//! Только ЛОКАЛЬНЫЙ сервер: тесты создают временные аккаунты `e2elinux…` и удаляют их в конце.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{pair, wait_for};
use melogold_server::account::AccountState;
use melogold_server::linking::*;

#[tokio::test]
#[ignore = "нужен локальный сервер: MELOGOLD_LIVE_SERVER"]
async fn request_mode_end_to_end() {
    let pair = pair().await;
    let linker = NewDeviceLinker::new(AccountLinkPort(Arc::clone(&pair.second)), tokio::runtime::Handle::current());
    linker.show_code();
    let code = wait_for(|| match linker.state() {
        NewDeviceLinkState::ShowingCode { user_code, .. } => Some(user_code),
        NewDeviceLinkState::Failed { failure, .. } => panic!("код не получен: {failure:?}"),
        _ => None,
    })
    .await;
    // Вошедшее устройство вводит код и видит карточку с тремя числами.
    let details = pair.first.resolve_link(&code).await.expect("resolve");
    assert_eq!(details.verify_choices.len(), 3);
    assert_eq!(details.device.as_ref().map(|d| d.name.as_str()), Some("Новый телефон"));
    let shown = wait_for(|| match linker.state() {
        NewDeviceLinkState::Verify { verify_code, login, approver_name, .. } => Some((verify_code, login, approver_name)),
        _ => None,
    })
    .await;
    assert_eq!(shown.1, pair.login);
    assert_eq!(shown.2, "Компьютер");
    assert!(details.verify_choices.contains(&shown.0));
    pair.first.approve_link(&details.link_id, &shown.0).await.expect("approve");
    wait_for(|| (linker.state() == NewDeviceLinkState::SignedIn).then_some(())).await;
    match pair.second.state() {
        AccountState::SignedIn { login, .. } => assert_eq!(login, pair.login),
        other => panic!("сессия не взята: {other:?}"),
    }
    assert_eq!(pair.first.devices().await.unwrap().devices.len(), 2);
    pair.cleanup().await;
}

#[tokio::test]
#[ignore = "нужен локальный сервер: MELOGOLD_LIVE_SERVER"]
async fn invite_mode_end_to_end() {
    let pair = pair().await;
    let handle = tokio::runtime::Handle::current();
    let invite = InviteLinker::new(AccountInvitePort(Arc::clone(&pair.first)), handle.clone());
    invite.start();
    let code = wait_for(|| match invite.state() {
        InviteState::Waiting { user_code, .. } => Some(user_code),
        InviteState::Failed { failure, .. } => panic!("приглашение не создано: {failure:?}"),
        _ => None,
    })
    .await;
    let new_device = NewDeviceLinker::new(AccountLinkPort(Arc::clone(&pair.second)), handle);
    new_device.claim(&code);
    let number = wait_for(|| match new_device.state() {
        NewDeviceLinkState::Verify { verify_code, approver_name, .. } => Some((verify_code, approver_name)),
        NewDeviceLinkState::Failed { failure, .. } => panic!("код не принят: {failure:?}"),
        _ => None,
    })
    .await;
    assert_eq!(number.1, "Компьютер");
    // Без SSE приглашение узнаёт о вводе опросом раз в 3 с.
    let link = wait_for(|| match invite.state() {
        InviteState::Claimed(details) => Some(details),
        _ => None,
    })
    .await;
    assert!(link.verify_choices.contains(&number.0));
    pair.first.approve_link(&link.link_id, &number.0).await.expect("approve");
    invite.release();
    wait_for(|| (new_device.state() == NewDeviceLinkState::SignedIn).then_some(())).await;
    pair.cleanup().await;
}

#[tokio::test]
#[ignore = "нужен локальный сервер: MELOGOLD_LIVE_SERVER"]
async fn wrong_number_denies_and_deny_button_denies() {
    let pair = pair().await;
    let handle = tokio::runtime::Handle::current();
    for wrong_number in [true, false] {
        let linker = NewDeviceLinker::new(AccountLinkPort(Arc::clone(&pair.second)), handle.clone());
        linker.show_code();
        let code = wait_for(|| match linker.state() {
            NewDeviceLinkState::ShowingCode { user_code, .. } => Some(user_code),
            _ => None,
        })
        .await;
        let details = pair.first.resolve_link(&code).await.unwrap();
        if wrong_number {
            let shown = wait_for(|| match linker.state() {
                NewDeviceLinkState::Verify { verify_code, .. } => Some(verify_code),
                _ => None,
            })
            .await;
            let other = details.verify_choices.iter().find(|c| **c != shown).unwrap().clone();
            let error = pair.first.approve_link(&details.link_id, &other).await.expect_err("неверное число");
            assert_eq!(error.code, "link_verify_mismatch");
        } else {
            pair.first.deny_link(&details.link_id).await.unwrap();
        }
        wait_for(|| matches!(linker.state(), NewDeviceLinkState::Failed { failure: LinkFailure::Denied, started: true }).then_some(()))
            .await;
    }
    assert!(matches!(pair.second.state(), AccountState::SignedOut));
    pair.cleanup().await;
}

#[tokio::test]
#[ignore = "нужен локальный сервер: MELOGOLD_LIVE_SERVER"]
async fn cancel_from_either_side() {
    let pair = pair().await;
    let handle = tokio::runtime::Handle::current();
    // Новое устройство отменило: код больше не находится (410 link_expired и для отменённого).
    let linker = NewDeviceLinker::new(AccountLinkPort(Arc::clone(&pair.second)), handle.clone());
    linker.show_code();
    let code = wait_for(|| match linker.state() {
        NewDeviceLinkState::ShowingCode { user_code, .. } => Some(user_code),
        _ => None,
    })
    .await;
    linker.cancel();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let error = pair.first.resolve_link(&code).await.expect_err("код отменён");
    assert_eq!(link_failure_of(&error), LinkFailure::Expired);
    // Несуществующий код.
    let error = pair.first.resolve_link("0000-0000").await.expect_err("такого кода нет");
    assert_eq!(link_failure_of(&error), LinkFailure::NotFound);

    // Вошедшее устройство отменило приглашение: новое видит «код устарел».
    let invite = InviteLinker::new(AccountInvitePort(Arc::clone(&pair.first)), handle.clone());
    invite.start();
    let code = wait_for(|| match invite.state() {
        InviteState::Waiting { user_code, .. } => Some(user_code),
        _ => None,
    })
    .await;
    invite.cancel();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let new_device = NewDeviceLinker::new(AccountLinkPort(Arc::clone(&pair.second)), handle.clone());
    new_device.claim(&code);
    wait_for(|| matches!(new_device.state(), NewDeviceLinkState::Failed { failure: LinkFailure::Expired, started: false }).then_some(()))
        .await;

    // Код нового устройства, введённый как приглашение, — «это код нового устройства».
    linker.show_code();
    let request_code = wait_for(|| match linker.state() {
        NewDeviceLinkState::ShowingCode { user_code, .. } => Some(user_code),
        _ => None,
    })
    .await;
    new_device.claim(&request_code);
    wait_for(|| matches!(new_device.state(), NewDeviceLinkState::Failed { failure: LinkFailure::WrongMode, started: false }).then_some(()))
        .await;
    linker.cancel();
    pair.cleanup().await;
}
