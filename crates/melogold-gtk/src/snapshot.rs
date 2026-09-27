//! Снимки своего окна — только отладочная сборка (docs/PROMPT.md §2, §3 «Снимки окна»).
//!
//! ```sh
//! MELOGOLD_SCREENSHOT_DIR=/tmp/shots MELOGOLD_SCREENSHOT_SIZE=360x640 \
//! MELOGOLD_SCREENSHOT_SCHEME=dark MELOGOLD_SCREENSHOT_LANG=en ./target/debug/melogold
//! ```
//!
//! Окно рисует СЕБЯ через `WidgetPaintable`, проходит по экранам, пишет PNG и закрывается.
//! Экран пользователя не снимается никогда; сам прогон — во вложенном композиторе
//! (`scripts/snapshots.sh`), чтобы окно не забирало фокус у человека (грабли §9 п. 15).

use std::path::{Path, PathBuf};
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use melogold_core::settings::Tab;

use crate::window::MainWindow;

/// Шаг: имя снимка, что сделать, сколько ждать до снимка (сеть — дольше).
type Step = (&'static str, Box<dyn Fn(&MainWindow)>, u64);

pub fn maybe_start(window: &MainWindow) {
    let Some(directory) = std::env::var_os("MELOGOLD_SCREENSHOT_DIR").map(PathBuf::from) else { return };
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    // Во вложенном weston нет настроек GNOME, и GTK рисует свою раскладку кнопок со значком
    // окна слева. Снимок должен показывать то, что увидит человек в GNOME.
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_decoration_layout(Some("appmenu:close"));
    }
    // Схема задаётся прямо через API: проверять тёмную тему, переключая весь рабочий стол,
    // нельзя, а `ADW_DEBUG_COLOR_SCHEME` на 1.9 делает не то, что написано в названии (Clementine).
    match std::env::var("MELOGOLD_SCREENSHOT_SCHEME").unwrap_or_default().as_str() {
        "light" => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight),
        "dark" => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark),
        _ => {}
    }
    if let Some((width, height)) = std::env::var("MELOGOLD_SCREENSHOT_SIZE")
        .ok()
        .and_then(|size| size.split_once('x').map(|(w, h)| (w.parse::<i32>(), h.parse::<i32>())))
        .and_then(|(w, h)| Some((w.ok()?, h.ok()?)))
    {
        window.window.unmaximize();
        window.window.set_default_size(width, height);
    }

    let steps: Vec<Step> = vec![
        ("01-trends", Box::new(|w| w.show_tab(Tab::Trends)), 4000),
        ("02-new", Box::new(|w| w.show_tab(Tab::WhatsNew)), 4000),
        ("03-library", Box::new(|w| w.show_tab(Tab::Library)), 700),
        ("04-settings", Box::new(|w| w.show_tab(Tab::Settings)), 700),
        (
            // Полоса «Вышла новая версия» и точка у «Настроек» (обновления, срез 8).
            "04-update",
            Box::new(|w| {
                let mut notes = std::collections::HashMap::new();
                notes.insert("ru".to_owned(), "Исправления".to_owned());
                notes.insert("en".to_owned(), "Fixes".to_owned());
                w.updates.pretend_available(melogold_core::updates::UpdateManifest {
                    version: "9.9.9".into(),
                    notes,
                    ..Default::default()
                });
            }),
            700,
        ),
        ("04a-settings-storage", Box::new(|w| scroll_settings(w, 0.62)), 1500),
        ("04b-settings-about", Box::new(|w| scroll_settings(w, 1.0)), 700),
        ("05-diagnostics", Box::new(|w| w.push(&crate::pages::diagnostics::page(w))), 700),
        (
            "06-sidebar",
            Box::new(|w| {
                w.go_back();
                w.show_tab(Tab::Trends);
                if w.split_view().is_collapsed() {
                    w.split_view().set_show_sidebar(true);
                }
            }),
            700,
        ),
        (
            "07-search",
            Box::new(|w| {
                w.split_view().set_show_sidebar(!w.split_view().is_collapsed());
                w.search_for("Кино Группа крови");
            }),
            4000,
        ),
        ("07a-album", Box::new(|w| w.push(&crate::pages::catalog::album_page(w, "MPREb_OLmD8O5IYNS"))), 3500),
        ("07b-artist", Box::new(|w| w.push(&crate::pages::catalog::artist_page(w, "UCRr1xG_2WIDs18a6cIiCxeA"))), 4000),
        (
            "07c-playlist",
            Box::new(|w| w.push(&crate::pages::catalog::playlist_page(w, "RDCLAK5uy_n20FRYQXNt1p1wS55Nj2r14IouO5weaYU"))),
            3500,
        ),
        ("07d-channel", Box::new(|w| w.push(&crate::pages::catalog::artist_page(w, "UCy_vnPBNh9FqtyH9Qc-aiSA"))), 5000),
        (
            "07e-moods",
            Box::new(|w| w.push(&crate::pages::catalog::browse_page(w, "Настроения и жанры", "FEmusic_moods_and_genres", None))),
            3500,
        ),
        (
            "08-playing",
            Box::new(|w| {
                let track = melogold_core::music::Track {
                    video_id: "dQw4w9WgXcQ".into(),
                    title: "Never Gonna Give You Up".into(),
                    artists_text: Some("Rick Astley".into()),
                    album_title: Some("Whenever You Need Somebody".into()),
                    thumbnail_url: Some("https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg".into()),
                    video_type: Some("video".into()),
                    ..Default::default()
                };
                w.ctx
                    .services
                    .player
                    .send(melogold_playback::engine::Command::PlaySingle { track, start: std::time::Duration::from_secs(42) });
            }),
            6000,
        ),
        (
            "09-queue",
            Box::new(|w| {
                let _ = WidgetExt::activate_action(&w.window, "win.queue", None);
            }),
            1500,
        ),
        (
            "10-now-playing",
            Box::new(|w| {
                let _ = WidgetExt::activate_action(&w.window, "win.queue", None);
                w.show_now_playing();
            }),
            1500,
        ),
        (
            // Задание Windows 0007: видео-«статика» — обложка сингла без чёрных полей, квадратом.
            "10a-frame-bars",
            Box::new(|w| {
                let track = melogold_core::music::Track {
                    video_id: "LLhpBVFh2Zg".into(),
                    title: "БАРМАЛЕЙ".into(),
                    artists_text: Some("GORILLA GLUE, LIL NAKUR".into()),
                    thumbnail_url: Some("https://i.ytimg.com/vi/LLhpBVFh2Zg/hq720.jpg".into()),
                    video_type: Some("video".into()),
                    ..Default::default()
                };
                w.ctx.services.player.send(melogold_playback::engine::Command::PlaySingle { track, start: Duration::ZERO });
                let weak = w.downgrade();
                glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                    if let Some(window) = weak.upgrade() {
                        window.show_now_playing();
                    }
                });
            }),
            5000,
        ),
        (
            // Задание 0001: трек закрыт в стране. Скрипт снимков ставит MELOGOLD_FAKE_GEO=cYKAr38pZcY:RU.
            "11-geo",
            Box::new(|w| {
                w.go_back();
                let track = melogold_core::music::Track {
                    video_id: "cYKAr38pZcY".into(),
                    title: "Photosynthesis".into(),
                    artists_text: Some("Saba".into()),
                    ..Default::default()
                };
                w.ctx.services.player.send(melogold_playback::engine::Command::PlaySingle { track, start: std::time::Duration::ZERO });
            }),
            7000,
        ),
        ("12-geo-now-playing", Box::new(|w| w.show_now_playing()), 1200),
        (
            // Библиотека с данными: ♡ двум трекам, свой плейлист, сохранённый альбом (данные — во временных папках).
            "12a-library",
            Box::new(|w| {
                w.go_back();
                let tracks = sample_tracks();
                w.set_liked(tracks.clone(), true);
                let task = w.ctx.services.db(move |library| {
                    let _ = library.create_playlist("Вечер", &tracks);
                    let album = melogold_core::music::AlbumItem {
                        browse_id: "MPREb_OLmD8O5IYNS".into(),
                        title: "Группа крови".into(),
                        artists_text: Some("Кино".into()),
                        year: Some("1988".into()),
                        ..Default::default()
                    };
                    let _ = library.set_album_saved(&album, true);
                });
                glib::spawn_future_local(async move {
                    let _ = task.await;
                });
                w.show_tab(Tab::Library);
            }),
            1500,
        ),
        ("12b-favorites", Box::new(|w| w.push(&crate::pages::library::favorites(w))), 1200),
        ("12c-history", Box::new(|w| w.push(&crate::pages::library::history(w))), 1200),
        (
            "12d-playlist",
            Box::new(|w| {
                if let Some(playlist) = w.library_view.playlists().first() {
                    w.push(&crate::pages::library::local_playlist(w, playlist.id));
                }
            }),
            1200,
        ),
        ("12e-downloads", Box::new(|w| w.push(&crate::pages::library::downloads_page(w))), 1200),
        ("12f-albums", Box::new(|w| w.push(&crate::pages::library::saved_albums(w))), 2500),
        (
            // Задание 0004: три выделенных трека и панель действий над плеером.
            "12g-selection",
            Box::new(|w| {
                w.push(&crate::pages::library::favorites(w));
                let weak = w.downgrade();
                glib::timeout_add_local_once(Duration::from_millis(900), move || {
                    let Some(window) = weak.upgrade() else { return };
                    let page = window.nav(Tab::Library).visible_page();
                    // Список треков, а не список внутри выпадающей сортировки.
                    let list = page.and_then(|p| descendant::<gtk::ListView>(p.upcast_ref(), "track-list"));
                    if let Some(list) = list {
                        let _ = list.activate_action("list.select-all", None);
                    }
                });
            }),
            1800,
        ),
        (
            // Задание 0005: окно «Сведения о треке» у видео фаната.
            "12h-details",
            Box::new(|w| {
                w.clear_selection();
                let tracks = sample_tracks();
                w.edit_details(&tracks[1]);
            }),
            1000,
        ),
        (
            // Плейлист из разрозненных видео, собранный в один альбом.
            "12i-album",
            Box::new(|w| {
                if let Some(dialog) = w.window.visible_dialog() {
                    dialog.close();
                }
                let library = std::sync::Arc::clone(&w.ctx.services.library);
                let ids: Vec<String> = sample_tracks().into_iter().map(|t| t.video_id).collect();
                let _ = library.set_album(&ids, "Потерянный альбом");
                let _ = library.set_override(&ids[2], Some("Богемская рапсодия"), Some("Queen"), Some("Потерянный альбом"));
                if let Some(playlist) = w.library_view.playlists().first() {
                    w.push(&crate::pages::library::local_playlist(w, playlist.id));
                }
            }),
            1500,
        ),
        (
            // Задание Windows 0004: круг «Сохранить копию» → «Импорт копии» — итог с числами.
            "12j-import",
            Box::new(|w| {
                w.go_back();
                let path = std::env::temp_dir().join(format!("melogold-snapshot-{}.db", melogold_core::ids::new_uuid()));
                if melogold_data::backup::export(&w.ctx.services.library, &path, "linux", "snapshot").is_ok() {
                    w.run_import(path);
                }
            }),
            2500,
        ),
        (
            "13-shortcuts",
            Box::new(|w| {
                if let Some(dialog) = w.window.visible_dialog() {
                    dialog.close();
                }
                w.go_back();
                w.ctx.services.player.send(melogold_playback::engine::Command::Pause);
                crate::shortcuts::present(Some(w.window.upcast_ref()));
            }),
            1500,
        ),
        (
            "14-settings-account",
            Box::new(|w| {
                if let Some(dialog) = w.window.visible_dialog() {
                    dialog.close();
                }
                w.show_tab(Tab::Settings);
            }),
            900,
        ),
        ("14a-sign-in", Box::new(|w| w.push(&crate::pages::account::sign_in_page(w))), 900),
        (
            "14b-register",
            Box::new(|w| {
                w.go_back();
                w.push(&crate::pages::account::register_page(w));
            }),
            900,
        ),
        (
            "14b1-recovery-code",
            Box::new(|w| {
                w.go_back();
                // Код из примера контракта: экран показывается, аккаунт не создаётся.
                w.push(&crate::pages::account::recovery_code_page(w, "7KQ2-MX9D-4TNP-B8RW-3HZF"));
            }),
            900,
        ),
        (
            "14c-server",
            Box::new(|w| {
                // Страница кода не отпускает «Назад», пока код не сохранён: в снимке — к корню.
                if let Some(page) = w.nav(Tab::Settings).visible_page() {
                    page.set_can_pop(true);
                }
                w.go_back();
                w.push(&crate::pages::account::server_page(w));
            }),
            3500,
        ),
    ];
    let mut steps = steps;
    steps.extend(lyrics_steps());
    if std::env::var("MELOGOLD_SNAPSHOT_ACCOUNT").as_deref() == Ok("1") {
        steps.extend(account_steps());
    }
    // Только нужные шаги: MELOGOLD_SNAPSHOT_STEPS=17,08 — по началу имени.
    if let Ok(only) = std::env::var("MELOGOLD_SNAPSHOT_STEPS") {
        let prefixes: Vec<String> = only.split(',').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect();
        steps.retain(|(name, _, _)| prefixes.iter().any(|p| name.starts_with(p.as_str())));
    }

    let window = window.clone();
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1200)).await;
        for (name, step, wait) in steps {
            step(&window);
            glib::timeout_future(Duration::from_millis(wait)).await;
            let path = directory.join(format!("{name}.png"));
            match capture(&window.window, &path) {
                Some(()) => println!("снимок: {}", path.display()),
                None => eprintln!("снимок не вышел: {}", path.display()),
            }
        }
        if let Some(app) = window.window.application() {
            app.quit();
        }
    });
}

fn capture(window: &adw::ApplicationWindow, path: &Path) -> Option<()> {
    let (width, height) = (window.width(), window.height());
    if width <= 0 || height <= 0 {
        return None;
    }
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node()?;
    // Отрисовщик окна рисует тем же путём, что и живое окно; без него (окно не видно,
    // экран заблокирован) — свой, без поверхности (Clementine).
    if let Some(renderer) = window.native().and_then(|native| native.renderer()) {
        if renderer.render_texture(&node, None).save_to_png(path).is_ok() {
            return Some(());
        }
    }
    let renderer = gtk::gsk::CairoRenderer::new();
    renderer.realize(gtk::gdk::Surface::NONE).ok()?;
    let saved = renderer.render_texture(&node, None).save_to_png(path).ok();
    renderer.unrealize();
    saved
}

/// Настройки прокручены на долю `fraction` высоты (Хранилище, О приложении).
fn scroll_settings(window: &MainWindow, fraction: f64) {
    let Some(page) = window.nav(Tab::Settings).visible_page() else { return };
    let mut stack = vec![page.upcast::<gtk::Widget>()];
    while let Some(widget) = stack.pop() {
        if let Some(scroller) = widget.downcast_ref::<gtk::ScrolledWindow>() {
            let adjustment = scroller.vadjustment();
            adjustment.set_value((adjustment.upper() - adjustment.page_size()) * fraction);
            return;
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            stack.push(current);
        }
    }
}

/// Первый потомок нужного типа с классом CSS `class` (обход в глубину).
fn descendant<T: IsA<gtk::Widget>>(widget: &gtk::Widget, class: &str) -> Option<T> {
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = current.downcast_ref::<T>().filter(|w| w.has_css_class(class)) {
            return Some(found.clone());
        }
        if let Some(found) = descendant::<T>(&current, class) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn sample_tracks() -> Vec<melogold_core::music::Track> {
    [
        ("dQw4w9WgXcQ", "Never Gonna Give You Up", "Rick Astley", 213_000),
        ("cYKAr38pZcY", "Photosynthesis", "Saba", 236_000),
        ("fJ9rUzIMcZQ", "Bohemian Rhapsody", "Queen", 355_000),
    ]
    .into_iter()
    .map(|(id, title, artist, duration)| melogold_core::music::Track {
        video_id: id.into(),
        title: title.into(),
        artists_text: Some(artist.into()),
        duration_ms: Some(duration),
        ..Default::default()
    })
    .collect()
}

/// Текст песни (срез 6, задания 0003 и 0007): дуэт, подпевка, время слов, длинная строка с
/// переносом, проигрыш; обычный вид; «Найти другой текст»; «Текст недоступен». Текст — из
/// библиотеки: сеть нужна только потоку трека и поиску LrcLib.
fn lyrics_steps() -> Vec<Step> {
    const TTML: &str = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" xml:lang="en">
<head><metadata><ttm:agent type="person" xml:id="v1"/><ttm:agent type="person" xml:id="v2"/></metadata></head>
<body><div>
<p begin="00:18.000" end="00:21.500" ttm:agent="v1"><span begin="00:18.000" end="00:18.600">We're </span><span begin="00:18.600" end="00:19.200">no </span><span begin="00:19.200" end="00:19.900">strangers </span><span begin="00:19.900" end="00:20.300">to </span><span begin="00:20.300" end="00:21.500">love</span></p>
<p begin="00:22.000" end="00:26.000" ttm:agent="v2">You know the rules and so do I<span ttm:role="x-bg"><span begin="00:24.000" end="00:25.500">(so do I)</span></span></p>
<p begin="00:26.500" end="00:31.000" ttm:agent="v1">A full commitment's what I'm thinking of, and this line is long enough to wrap onto the next one</p>
<p begin="00:31.500" end="00:35.000" ttm:agent="v2">You wouldn't get this from any other guy</p>
<p begin="00:43.000" end="00:47.000" ttm:agent="v1">I just wanna tell you how I'm feeling</p>
<p begin="00:47.000" end="00:51.000" ttm:agent="v1">Gotta make you understand</p>
<p begin="00:51.000" end="00:55.000" ttm:agent="v2">Never gonna give you up</p>
<p begin="00:55.000" end="00:59.000" ttm:agent="v2">Never gonna let you down</p>
</div></body></tt>"#;
    let play = |video_id: &'static str, title: &'static str, start: u64| {
        move |w: &MainWindow| {
            let track = melogold_core::music::Track {
                video_id: video_id.into(),
                title: title.into(),
                artists_text: Some("Rick Astley".into()),
                album_title: Some("Whenever You Need Somebody".into()),
                ..Default::default()
            };
            w.ctx.services.player.send(melogold_playback::engine::Command::PlaySingle { track, start: Duration::from_secs(start) });
        }
    };
    let (first, second) = (play("dQw4w9WgXcQ", "Never Gonna Give You Up", 19), play("fJ9rUzIMcZQ", "Bohemian Rhapsody", 0));
    vec![
        (
            "17-lyrics",
            Box::new(move |w: &MainWindow| {
                if let Some(dialog) = w.window.visible_dialog() {
                    dialog.close();
                }
                let lyrics = melogold_core::lyrics::sync_rules::StoredLyrics {
                    synced: Some(TTML.into()),
                    plain: Some(String::new()),
                    synced_source: Some("lrclib".into()),
                    synced_ref: Some("33476831".into()),
                    ..Default::default()
                };
                let _ = w.ctx.services.library.save_lyrics("dQw4w9WgXcQ", &lyrics);
                first(w);
                // «Сейчас играет» открывается, когда трек уже в плеере.
                let weak = w.downgrade();
                glib::timeout_add_local_once(Duration::from_millis(1500), move || {
                    if let Some(window) = weak.upgrade() {
                        window.toggle_lyrics();
                    }
                });
            }),
            6000,
        ),
        (
            "17a-lyrics-interlude",
            Box::new(|w: &MainWindow| w.ctx.services.player.send(melogold_playback::engine::Command::Seek(Duration::from_secs(38)))),
            2500,
        ),
        ("17b-lyrics-plain", Box::new(|w: &MainWindow| w.lyrics.toggle_synced()), 1000),
        (
            "17c-lyrics-find",
            Box::new(|w: &MainWindow| {
                w.lyrics.toggle_synced();
                w.find_lyrics();
            }),
            4000,
        ),
        (
            // Текст из сети: в библиотеке его нет — цепочка YouTube Music → LrcLib → KuGou.
            "17c1-lyrics-online",
            Box::new(|w: &MainWindow| {
                if let Some(dialog) = w.window.visible_dialog() {
                    dialog.close();
                }
                let track = melogold_core::music::Track {
                    video_id: "BSTsnWoslP4".into(),
                    title: "Bohemian Rhapsody".into(),
                    artists_text: Some("Queen".into()),
                    album_title: Some("A Night at the Opera".into()),
                    ..Default::default()
                };
                w.ctx.services.player.send(melogold_playback::engine::Command::PlaySingle { track, start: Duration::from_secs(60) });
            }),
            9000,
        ),
        (
            "17d-lyrics-unavailable",
            Box::new(move |w: &MainWindow| {
                if let Some(dialog) = w.window.visible_dialog() {
                    dialog.close();
                }
                let none = melogold_core::lyrics::sync_rules::StoredLyrics {
                    synced: Some(String::new()),
                    plain: Some(String::new()),
                    ..Default::default()
                };
                let _ = w.ctx.services.library.save_lyrics("fJ9rUzIMcZQ", &none);
                second(w);
            }),
            3000,
        ),
        (
            // Задание 0007: «Далее» — длинная строка (японский текст) целиком, с подпевкой.
            "17f-editor-sync",
            Box::new(|w: &MainWindow| {
                let text = "君の名前を何度も呼んだ夜明けの空に消えていく声がまだ胸の奥で響いている、忘れられない約束と一緒に (ずっと)\nWe're no strangers to love\nYou know the rules and so do I (so do I)\nA full commitment's what I'm thinking of";
                let lyrics = melogold_core::lyrics::sync_rules::StoredLyrics {
                    synced: Some(String::new()),
                    plain: Some(text.into()),
                    plain_source: Some("user".into()),
                    ..Default::default()
                };
                let _ = w.ctx.services.library.save_lyrics("fJ9rUzIMcZQ", &lyrics);
                w.lyrics.reload();
                let weak = w.downgrade();
                glib::timeout_add_local_once(Duration::from_millis(500), move || {
                    if let Some(window) = weak.upgrade() {
                        window.edit_lyrics();
                    }
                });
            }),
            1800,
        ),
        (
            "17g-editor-words",
            Box::new(|w: &MainWindow| {
                if let Some(dialog) = w.window.visible_dialog() {
                    if let Some(timing) = descendant::<adw::ToggleGroup>(dialog.upcast_ref(), "editor-timing") {
                        timing.set_active_name(Some("word"));
                    }
                }
            }),
            800,
        ),
        (
            "17h-editor-text",
            Box::new(|w: &MainWindow| {
                if let Some(dialog) = w.window.visible_dialog() {
                    if let Some(tabs) = descendant::<adw::ToggleGroup>(dialog.upcast_ref(), "editor-tabs") {
                        tabs.set_active_name(Some("text"));
                    }
                }
            }),
            800,
        ),
        (
            "17i-editor-preview",
            Box::new(|w: &MainWindow| {
                if let Some(dialog) = w.window.visible_dialog() {
                    if let Some(tabs) = descendant::<adw::ToggleGroup>(dialog.upcast_ref(), "editor-tabs") {
                        tabs.set_active_name(Some("preview"));
                    }
                }
            }),
            800,
        ),
        (
            "17j-lyrics-closed",
            Box::new(|w: &MainWindow| {
                if let Some(dialog) = w.window.visible_dialog() {
                    dialog.force_close();
                }
                w.go_back();
                w.ctx.services.player.send(melogold_playback::engine::Command::Pause);
            }),
            800,
        ),
    ]
}

/// Экраны вошедшего пользователя — на временном аккаунте `e2elinux…` (docs/PROMPT.md: живые проверки
/// только так), который удаляется последним шагом. Пароль живёт только в памяти этого прогона.
fn account_steps() -> Vec<Step> {
    let suffix: String = melogold_core::ids::new_uuid().chars().filter(|c| c.is_ascii_alphanumeric()).take(10).collect();
    let login = format!("e2elinux{suffix}");
    let password = std::rc::Rc::new(melogold_core::ids::new_uuid().replace('-', "") + &melogold_core::ids::new_uuid().replace('-', ""));
    let (register_password, delete_password) = (std::rc::Rc::clone(&password), password);
    vec![
        (
            "15-account-register",
            Box::new(move |w: &MainWindow| {
                w.go_back();
                let account = std::sync::Arc::clone(&w.ctx.services.account);
                let (login, password) = (login.clone(), register_password.to_string());
                let task = w.ctx.services.run(async move {
                    account.register(&login, &password, std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).await
                });
                glib::spawn_future_local(async move {
                    match task.await {
                        Some(Ok(_)) => println!("временный аккаунт создан"),
                        other => eprintln!("временный аккаунт не создан: {other:?}"),
                    }
                });
            }),
            9000,
        ),
        ("15a-account", Box::new(|w: &MainWindow| w.push(&crate::pages::account::account_page(w))), 3500),
        ("15b-add-device", Box::new(|w: &MainWindow| crate::pages::account::link_device_dialog(w)), 1200),
        (
            "15c-history-everywhere",
            Box::new(|w: &MainWindow| {
                if let Some(dialog) = w.window.visible_dialog() {
                    dialog.close();
                }
                w.go_back();
            }),
            800,
        ),
        (
            "16-account-deleted",
            Box::new(move |w: &MainWindow| {
                let account = std::sync::Arc::clone(&w.ctx.services.account);
                let password = delete_password.to_string();
                let task = w.ctx.services.run(async move { account.delete_account(&password).await });
                glib::spawn_future_local(async move {
                    match task.await {
                        Some(Ok(())) => println!("временный аккаунт удалён"),
                        other => eprintln!("ВРЕМЕННЫЙ АККАУНТ НЕ УДАЛЁН: {other:?}"),
                    }
                });
            }),
            4000,
        ),
    ]
}
