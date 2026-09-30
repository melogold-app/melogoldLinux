//! Снимки экранов заданий 0008–0011 (docs/PROMPT.md §3 «Снимки окна»). Экраны без сети рисуются из
//! заданных состояний; живые (`MELOGOLD_SNAPSHOT_LIVE=1` и `MELOGOLD_SERVER_URL` — локальный сервер)
//! ходят на сервер по-настоящему. Общий список шагов и способ съёмки — `snapshot.rs`.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
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

/// Шаги вошедшего пользователя (`MELOGOLD_SNAPSHOT_ACCOUNT=1`, локальный сервер): идут между созданием
/// временного аккаунта и его удалением.
pub fn account_steps() -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    steps.extend(share_account_steps());
    steps
}

/// Задание 0010 на настоящем сервере: снимок своего плейлиста, открытие ссылки, «Мои ссылки», удалённая ссылка.
fn share_account_steps() -> Vec<Step> {
    let shared_url: Rc<RefCell<Option<String>>> = Rc::default();
    let (read_url, open_url, delete_url) = (Rc::clone(&shared_url), Rc::clone(&shared_url), Rc::clone(&shared_url));
    vec![
        (
            "19a-share-copy",
            Box::new(|w| {
                back_to_settings_root(w);
                if let Some(playlist) = w.library_view.playlists().first() {
                    w.share_local_playlist(playlist.id);
                }
            }),
            2500,
        ),
        (
            "19b-share-open",
            Box::new(move |w| {
                let (weak, slot) = (w.downgrade(), Rc::clone(&read_url));
                glib::spawn_future_local(async move {
                    let Some(window) = weak.upgrade() else { return };
                    if let Ok(Some(text)) = window.window.clipboard().read_text_future().await {
                        println!("ссылка из буфера: {text}");
                        slot.replace(Some(text.to_string()));
                        window.open_text(&text);
                    }
                });
            }),
            3000,
        ),
        ("19c-my-links", Box::new(|w| w.push(&crate::pages::shares::my_links_page(w))), 3000),
        (
            "19d-share-deleted",
            Box::new(move |w| {
                // Автор удалил ссылку: она перестаёт открываться.
                let Some(url) = delete_url.borrow().clone() else { return };
                let Some(id) = url.rsplit('/').next().map(str::to_owned) else { return };
                let account = std::sync::Arc::clone(&w.ctx.services.account);
                let task = w.ctx.services.run(async move { account.delete_share(&id).await });
                let (weak, url) = (w.downgrade(), url);
                glib::spawn_future_local(async move {
                    let _ = task.await;
                    if let Some(window) = weak.upgrade() {
                        window.open_text(&url);
                    }
                });
            }),
            3000,
        ),
        (
            "19e-share-cleanup",
            Box::new(move |w| {
                let _ = &open_url;
                back_to_settings_root(w);
            }),
            300,
        ),
    ]
}

pub fn steps() -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    steps.extend(link_steps());
    steps.extend(share_steps());
    steps
}

/// Задание 0010 без сети: «Плейлист по ссылке» на готовом снимке.
fn share_steps() -> Vec<Step> {
    vec![
        // Ссылка Spotify в поле поиска: страница ссылки → название и исполнитель → поиск → трек играет.
        (
            "19f-spotify-link",
            Box::new(|w| {
                back_to_settings_root(w);
                w.open_text("https://open.spotify.com/track/4PTG3Z6ehGkBFwjybzWkR8?si=abc");
            }),
            9000,
        ),
        (
            "19-shared-playlist",
            Box::new(|w| {
                use melogold_server::dto::{ArtistRefDto, ShareDto, TrackDto};
                let track = |id: &str, title: &str, artist: &str, ms: i64| TrackDto {
                    video_id: id.into(),
                    title: title.into(),
                    artists_text: Some(artist.into()),
                    artists: vec![ArtistRefDto { id: None, name: artist.into() }],
                    duration_ms: Some(ms),
                    thumbnail_url: Some(format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg")),
                    ..Default::default()
                };
                let share = ShareDto {
                    share_id: "a1B2c3D4e5".into(),
                    kind: "playlist".into(),
                    name: "Вечер".into(),
                    url: "https://music.example.com/s/a1B2c3D4e5".into(),
                    tracks: vec![
                        track("dQw4w9WgXcQ", "Never Gonna Give You Up", "Rick Astley", 213_000),
                        track("fJ9rUzIMcZQ", "Bohemian Rhapsody", "Queen", 355_000),
                        track("cYKAr38pZcY", "Photosynthesis", "Saba", 236_000),
                    ],
                    created_at: "2026-09-30T10:00:00.000Z".into(),
                };
                settings_page(w, &crate::pages::shares::shared_playlist_preview(w, share));
            }),
            3500,
        ),
    ]
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
