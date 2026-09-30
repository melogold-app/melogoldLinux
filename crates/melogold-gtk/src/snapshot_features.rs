//! Снимки экранов заданий 0008–0011 (docs/PROMPT.md §3 «Снимки окна»). Экраны без сети рисуются из
//! заданных состояний; живые (`MELOGOLD_SNAPSHOT_LIVE=1` и `MELOGOLD_SERVER_URL` — локальный сервер)
//! ходят на сервер по-настоящему. Общий список шагов и способ съёмки — `snapshot.rs`.

use adw::prelude::*;
use melogold_core::settings::Tab;
use melogold_core::text::now_ms;
use melogold_server::dto::{LinkDetails, LinkDeviceInfo};
use melogold_server::linking::{LinkFailure, NewDeviceLinkState};

use crate::snapshot::Step;
use crate::window::MainWindow;

fn close_dialog(window: &MainWindow) {
    if let Some(dialog) = window.window.visible_dialog() {
        dialog.force_close();
    }
}

/// Открыть страницу в «Настройках» поверх корня.
fn settings_page(window: &MainWindow, page: &adw::NavigationPage) {
    back_to_settings_root(window);
    window.push(page);
}

fn back_to_settings_root(window: &MainWindow) {
    close_dialog(window);
    window.show_tab(Tab::Settings);
    let nav = window.nav(Tab::Settings);
    if let Some(root) = nav.navigation_stack().item(0).and_downcast::<adw::NavigationPage>() {
        nav.pop_to_page(&root);
    }
}

pub fn steps() -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    steps.extend(link_steps());
    steps
}

/// Задание 0008: вход по коду.
fn link_steps() -> Vec<Step> {
    let code = || NewDeviceLinkState::ShowingCode { user_code: "K7QX-M2PD".into(), expires_at: now_ms() + 272_000, reconnecting: false };
    let verify = |reconnecting: bool| NewDeviceLinkState::Verify {
        verify_code: "47".into(),
        login: "maxim".into(),
        approver_name: "MacBook Air".into(),
        approver_platform: "macos".into(),
        expires_at: now_ms() + 272_000,
        reconnecting,
    };
    let mut steps: Vec<Step> = vec![
        ("18a-link-code", Box::new(move |w| settings_page(w, &crate::pages::link_code::preview_page(w, &code(), false))), 1500),
        (
            "18b-link-code-offline",
            Box::new(move |w| {
                let state =
                    NewDeviceLinkState::ShowingCode { user_code: "K7QX-M2PD".into(), expires_at: now_ms() + 200_000, reconnecting: true };
                settings_page(w, &crate::pages::link_code::preview_page(w, &state, false));
            }),
            900,
        ),
        ("18c-link-number", Box::new(move |w| settings_page(w, &crate::pages::link_code::preview_page(w, &verify(false), false))), 900),
        (
            "18d-link-claim",
            Box::new(|w| {
                let state = NewDeviceLinkState::Failed { failure: LinkFailure::WrongMode, started: false };
                settings_page(w, &crate::pages::link_code::preview_page(w, &state, true));
            }),
            900,
        ),
        (
            "18e-link-failed",
            Box::new(|w| {
                let state = NewDeviceLinkState::Failed { failure: LinkFailure::Expired, started: true };
                settings_page(w, &crate::pages::link_code::preview_page(w, &state, false));
            }),
            900,
        ),
        (
            "18f-add-device-invite",
            Box::new(|w| {
                back_to_settings_root(w);
                crate::pages::link_code::invite_preview(w, "K7QX-M2PD", now_ms() + 272_000);
            }),
            900,
        ),
        (
            "18g-add-device-approve",
            Box::new(|w| {
                back_to_settings_root(w);
                let details = LinkDetails {
                    link_id: "preview".into(),
                    status: "claimed".into(),
                    expires_at: melogold_core::iso::format(now_ms() + 240_000),
                    device: Some(LinkDeviceInfo {
                        name: "Pixel 8".into(),
                        platform: "android".into(),
                        os_version: Some("Android 16".into()),
                        model: Some("Google Pixel 8".into()),
                        ..Default::default()
                    }),
                    same_network: Some(true),
                    verify_choices: vec!["12".into(), "47".into(), "85".into()],
                    ..Default::default()
                };
                crate::pages::link_code::invite_claimed_preview(w, &details);
            }),
            900,
        ),
    ];
    // Живые: настоящий код с локального сервера (нужен MELOGOLD_SERVER_URL).
    if std::env::var("MELOGOLD_SNAPSHOT_LIVE").as_deref() == Ok("1") {
        steps.push((
            "18h-link-live-code",
            Box::new(|w| {
                close_dialog(w);
                settings_page(w, &crate::pages::link_code::sign_in_by_code_page(w));
            }),
            2500,
        ));
        steps.push((
            "18i-link-live-leave",
            Box::new(|w| {
                w.go_back();
            }),
            500,
        ));
    }
    steps
}
