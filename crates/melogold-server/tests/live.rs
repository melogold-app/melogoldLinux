//! Живая проверка на сервере (docs/PROMPT.md §7 срез 5, приёмка): два «устройства» одного
//! временного аккаунта `e2elinux…`, правки в обе стороны за секунды, общая история, живые события.
//! Аккаунт удаляется в конце, даже если проверка упала. Других аккаунтов проверка не касается.
//!
//! ```sh
//! MELOGOLD_LIVE=1 cargo test -p melogold-server --test live -- --nocapture
//! MELOGOLD_SERVER=https://… — другой сервер (по умолчанию — сервер Melogold по умолчанию)
//! ```

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use melogold_core::music::Track;
use melogold_data::{Database, Library};
use melogold_server::account::{Account, AccountState, DeviceIdentity, DEFAULT_SERVER_URL};
use melogold_server::session::SessionStore;
use melogold_server::sync::LibrarySync;

struct Device {
    account: Arc<Account>,
    library: Arc<Library>,
    sync: Arc<LibrarySync>,
}

async fn device(name: &str, server: &str, dir: &std::path::Path) -> Device {
    let identity = DeviceIdentity {
        platform_id: format!("e2e-{name}|{}", melogold_core::ids::new_uuid()),
        name: format!("e2e Linux {name}"),
        os_version: Some("e2e".into()),
        model: None,
        client_version: env!("CARGO_PKG_VERSION").into(),
        language: "ru".into(),
    };
    let store = SessionStore::file_only(dir.join(format!("{name}-session.json")));
    let account = Account::open(identity, store, Some(server.to_owned())).await;
    let library = Library::open(Database::in_memory().unwrap());
    let sync = LibrarySync::new(Arc::clone(&account), Arc::clone(&library), tokio::runtime::Handle::current());
    Device { account, library, sync }
}

fn track(id: &str, title: &str) -> Track {
    Track { video_id: id.into(), title: title.into(), artists_text: Some("e2e".into()), duration_ms: Some(200_000), ..Default::default() }
}

async fn wait_until(what: &str, timeout: Duration, check: impl Fn() -> bool) {
    let started = Instant::now();
    while !check() {
        assert!(started.elapsed() < timeout, "не дождались за {timeout:?}: {what}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    println!("  {what}: {:.1} с", started.elapsed().as_secs_f64());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_devices_share_the_library() {
    if std::env::var("MELOGOLD_LIVE").as_deref() != Ok("1") {
        eprintln!("пропущено: живые проверки — с MELOGOLD_LIVE=1");
        return;
    }
    let _ = tracing_subscriber::fmt().with_env_filter("warn,melogold=info").with_test_writer().try_init();
    let server = std::env::var("MELOGOLD_SERVER").unwrap_or_else(|_| DEFAULT_SERVER_URL.to_owned());
    let dir = std::env::temp_dir().join(format!("melogold-live-{}", melogold_core::ids::new_uuid()));
    std::fs::create_dir_all(&dir).unwrap();
    let suffix: String = melogold_core::ids::new_uuid().chars().filter(|c| c.is_ascii_alphanumeric()).take(10).collect();
    let login = format!("e2elinux{suffix}");
    // Пароль временного аккаунта живёт только в памяти этой проверки.
    let password: String = format!("{}{}", melogold_core::ids::new_uuid(), melogold_core::ids::new_uuid()).replace('-', "");

    let a = device("a", &server, &dir).await;
    println!("сервер {server}, временный аккаунт {login}");
    let started = Instant::now();
    let code = a.account.register(&login, &password, Arc::new(AtomicBool::new(false))).await.expect("регистрация");
    println!("  регистрация с доказательством работы: {:.1} с, код восстановления {} знаков", started.elapsed().as_secs_f64(), code.len());

    let outcome = std::panic::AssertUnwindSafe(scenario(&a, &login, &password, &server, &dir)).catch_unwind().await;

    // Удаление временного аккаунта — всегда.
    let deleted = a.account.delete_account(&password).await;
    println!("аккаунт {login} удалён: {deleted:?}");
    let _ = std::fs::remove_dir_all(&dir);
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    deleted.expect("временный аккаунт удаляется");
    assert_eq!(a.account.state(), AccountState::SignedOut);
}

async fn scenario(a: &Device, login: &str, password: &str, server: &str, dir: &std::path::Path) {
    // Устройство A: Избранное, свой плейлист, своё название, прослушивание, закладка.
    a.library.set_liked(&[track("dQw4w9WgXcQ", "Never Gonna Give You Up")], true).unwrap();
    let playlist = a
        .library
        .create_playlist("e2e Дорога", &[track("cYKAr38pZcY", "Photosynthesis"), track("fJ9rUzIMcZQ", "Bohemian Rhapsody")])
        .unwrap();
    a.library.set_override("fJ9rUzIMcZQ", Some("Богемская рапсодия"), None, Some("e2e Альбом")).unwrap();
    a.library.record_play(&track("dQw4w9WgXcQ", "Never Gonna Give You Up"), 120_000, melogold_core::text::now_ms()).unwrap();
    let album =
        melogold_core::music::AlbumItem {
            browse_id: "MPREb_OLmD8O5IYNS".into(), title: "Группа крови".into(), ..Default::default()
        };
    a.library.set_album_saved(&album, true).unwrap();
    assert!(a.sync.sync(true).await, "синхронизация A");

    // Устройство B входит тем же логином и получает всё.
    let b = device("b", server, dir).await;
    b.account.sign_in(login, password).await.expect("вход B");
    assert!(b.sync.sync(true).await, "синхронизация B");
    assert!(b.library.is_liked("dQw4w9WgXcQ").unwrap(), "лайк дошёл");
    let lists = b.library.playlists().unwrap();
    let remote = lists.iter().find(|p| p.name == "e2e Дорога").expect("плейлист дошёл");
    let order: Vec<String> = b.library.playlist_tracks(remote.id).unwrap().into_iter().map(|t| t.video_id).collect();
    assert_eq!(order, ["cYKAr38pZcY", "fJ9rUzIMcZQ"], "порядок плейлиста");
    let shown = b.library.display(&track("fJ9rUzIMcZQ", "Bohemian Rhapsody"));
    assert_eq!((shown.title.as_str(), shown.album_title.as_deref()), ("Богемская рапсодия", Some("e2e Альбом")), "своё название дошло");
    assert!(b.library.is_album_saved("MPREb_OLmD8O5IYNS").unwrap(), "закладка дошла");
    let history = b.library.recent_history(10).unwrap();
    assert!(history.iter().any(|h| h.track.video_id == "dQw4w9WgXcQ"), "прослушивание A в истории B");
    assert_eq!(b.account.devices().await.expect("устройства").devices.len(), 2);

    // Живые события: A слушает, B правит — у A правка появляется сама, за секунды.
    a.sync.start();
    tokio::time::sleep(Duration::from_secs(3)).await;
    b.library.remove_from_playlist(remote.id, "cYKAr38pZcY").unwrap();
    b.library.set_liked(&[track("dQw4w9WgXcQ", "Never Gonna Give You Up")], false).unwrap();
    b.library.set_override("fJ9rUzIMcZQ", None, None, None).unwrap();
    let edited = Instant::now();
    assert!(b.sync.sync(false).await, "синхронизация правок B");
    wait_until("правка B у A (SSE sync.changed)", Duration::from_secs(20), || {
        let order: Vec<String> = a.library.playlist_tracks(playlist).unwrap().into_iter().map(|t| t.video_id).collect();
        order == ["fJ9rUzIMcZQ"] && !a.library.is_liked("dQw4w9WgXcQ").unwrap() && a.library.track_override("fJ9rUzIMcZQ").is_none()
    })
    .await;
    println!("  от правки на B до библиотеки A: {:.1} с", edited.elapsed().as_secs_f64());

    // И обратно: удаление плейлиста на A доходит до B.
    a.library.delete_playlist(playlist).unwrap();
    assert!(a.sync.sync(false).await);
    assert!(b.sync.sync(true).await);
    assert!(b.library.playlists().unwrap().iter().all(|p| p.name != "e2e Дорога"), "удаление плейлиста дошло");

    // Выход B: устройство уходит из списка.
    b.account.sign_out().await;
    let devices = a.account.devices().await.expect("устройства после выхода B");
    assert_eq!(devices.devices.len(), 1);
}
