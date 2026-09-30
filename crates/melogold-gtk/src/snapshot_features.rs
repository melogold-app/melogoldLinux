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
    steps.extend(remote_account_steps());
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
    steps.extend(remote_steps());
    steps
}

fn sample_devices() -> Vec<melogold_server::dto::RemoteDevice> {
    use melogold_server::dto::{PlaybackSummary, RemoteDevice, TrackDto};
    let playing = |title: &str, artist: &str, id: &str, position: i64| PlaybackSummary {
        rev: 1,
        device_id: "mac".into(),
        queue_length: 12,
        track: Some(TrackDto {
            video_id: id.into(),
            title: title.into(),
            artists_text: Some(artist.into()),
            thumbnail_url: Some(format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg")),
            ..Default::default()
        }),
        position_ms: position,
        duration_ms: Some(213_000),
        playing: true,
        at: melogold_core::iso::format(now_ms()),
        volume: Some(40),
        ..Default::default()
    };
    vec![
        RemoteDevice {
            device_id: "mac".into(),
            name: "MacBook Air".into(),
            platform: "macos".into(),
            online: true,
            controllable: true,
            playing: Some(playing("Never Gonna Give You Up", "Rick Astley", "dQw4w9WgXcQ", 83_000)),
            volume: Some(40),
        },
        RemoteDevice {
            device_id: "pixel".into(),
            name: "Pixel 7 Pro".into(),
            platform: "android".into(),
            online: true,
            controllable: false,
            playing: None,
            volume: None,
        },
        RemoteDevice {
            device_id: "old".into(),
            name: "Старый ноутбук".into(),
            platform: "windows".into(),
            online: false,
            controllable: false,
            playing: None,
            volume: None,
        },
    ]
}

/// Задание 0011 без сети: лист «Устройство» и пульт.
fn remote_steps() -> Vec<Step> {
    vec![
        (
            "20a-remote-sheet",
            Box::new(|w| {
                back_to_settings_root(w);
                crate::remote_sheet::preview(w, sample_devices());
            }),
            2500,
        ),
        (
            "20b-remote-bar",
            Box::new(|w| {
                close_dialog(w);
                let mac = sample_devices().remove(0);
                w.ctx.services.remote.control.connect(&mac);
            }),
            3000,
        ),
        ("20c-remote-off", Box::new(|w| w.ctx.services.remote.control.disconnect()), 500),
    ]
}

/// Задание 0011 на настоящем сервере: «телефон» — второе устройство того же временного аккаунта, играет и
/// слушает события с `remote=1`; окно видит его в листе, подключается и управляет.
fn remote_account_steps() -> Vec<Step> {
    vec![
        (
            "20d-live-phone",
            Box::new(|w| {
                back_to_settings_root(w);
                let account = std::sync::Arc::clone(&w.ctx.services.account);
                let task = w.ctx.services.run(async move { spawn_phone(account).await });
                glib::spawn_future_local(async move {
                    match task.await {
                        Some(Ok(())) => println!("телефон вошёл и играет"),
                        other => eprintln!("телефон не вошёл: {other:?}"),
                    }
                });
            }),
            6000,
        ),
        ("20e-live-sheet", Box::new(crate::remote_sheet::present), 3000),
        (
            "20f-live-remote",
            Box::new(|w| {
                close_dialog(w);
                let control = w.ctx.services.remote.control.clone();
                let task = w.ctx.services.run({
                    let control = control.clone();
                    async move { control.devices().await }
                });
                glib::spawn_future_local(async move {
                    if let Some(Ok(devices)) = task.await {
                        if let Some(phone) = devices.iter().find(|d| d.name == "Pixel 7 Pro") {
                            control.connect(phone);
                        }
                    }
                });
            }),
            3000,
        ),
        (
            "20g-live-command",
            Box::new(|w| {
                let control = &w.ctx.services.remote.control;
                control.set_volume(25);
                control.seek_to(150_000);
            }),
            2500,
        ),
        // «Слушать здесь»: очередь и место телефона — сюда, пульт выключается.
        ("20h-live-listen-here", Box::new(crate::remote_bar::listen_here), 4000),
    ]
}

/// «Телефон» для снимков: второе устройство аккаунта окна, вошедшее по коду, с потоком событий `remote=1` и
/// состоянием «играет Bohemian Rhapsody, громкость 40».
async fn spawn_phone(account: std::sync::Arc<melogold_server::account::Account>) -> Result<(), String> {
    use melogold_server::account::{Account, DeviceIdentity};
    use melogold_server::remote::{AccountReporterPort, PlayerSnapshot, Reporter, ServerClock};
    use melogold_server::session::SessionStore;

    let dir = std::env::temp_dir().join(format!("melogold-phone-{}", melogold_core::ids::new_uuid()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let identity = DeviceIdentity {
        platform_id: melogold_core::ids::new_uuid(),
        name: "Pixel 7 Pro".into(),
        os_version: Some("Android 16".into()),
        model: None,
        client_version: "0.0.0-snapshot".into(),
        language: "ru".into(),
    };
    let phone = Account::new(identity, SessionStore::file_only(dir.join("phone.json")), Some(account.server_url()));
    // Вход по коду: приглашение этого окна, телефон вводит код, окно одобряет число.
    let invite = account.create_invite().await.map_err(|e| e.to_string())?;
    let claimed = phone.claim_link(&invite.user_code).await.map_err(|e| e.to_string())?;
    let details = account.link(&invite.link_id).await.map_err(|e| e.to_string())?;
    if !details.verify_choices.contains(&claimed.verify_code) {
        return Err("число не среди вариантов".into());
    }
    account.approve_link(&invite.link_id, &claimed.verify_code).await.map_err(|e| e.to_string())?;
    let done = phone.poll_link(&claimed.poll_secret, "claimed").await.map_err(|e| e.to_string())?;
    if done.status != "completed" {
        return Err(format!("вход не завершился: {}", done.status));
    }

    // События с remote=1: телефон «в сети» и «управляемый».
    let events = std::sync::Arc::clone(&phone);
    tokio::spawn(async move {
        let _ = events.authorized(|api, token| async move { api.events(&token, true, |_| {}).await }).await;
    });
    let clock = std::sync::Arc::new(ServerClock::default());
    let shot: Box<dyn Fn() -> Option<PlayerSnapshot> + Send + Sync> = Box::new(|| {
        let track = |id: &str, title: &str, artist: &str| melogold_core::music::Track {
            video_id: id.into(),
            title: title.into(),
            artists_text: Some(artist.into()),
            thumbnail_url: Some(format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg")),
            ..Default::default()
        };
        Some(PlayerSnapshot {
            tracks: vec![
                track("fJ9rUzIMcZQ", "Bohemian Rhapsody", "Queen"),
                track("dQw4w9WgXcQ", "Never Gonna Give You Up", "Rick Astley"),
            ],
            index: 0,
            position_ms: 61_000,
            duration_ms: Some(355_000),
            playing: true,
            volume: Some(40),
        })
    });
    let reporter = Reporter::new(AccountReporterPort { account: phone, clock, snapshot: shot }, tokio::runtime::Handle::current(), |_| {});
    reporter.sound_played();
    // Докладчик живёт, пока идёт прогон: снимки берутся не позже пары минут.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    std::mem::forget(reporter);
    Ok(())
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
