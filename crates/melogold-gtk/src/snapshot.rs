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
        ("01-trends", Box::new(|w| w.show_tab(Tab::Trends)), 700),
        ("02-new", Box::new(|w| w.show_tab(Tab::WhatsNew)), 700),
        ("03-library", Box::new(|w| w.show_tab(Tab::Library)), 700),
        ("04-settings", Box::new(|w| w.show_tab(Tab::Settings)), 700),
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
            "13-shortcuts",
            Box::new(|w| {
                w.go_back();
                w.ctx.services.player.send(melogold_playback::engine::Command::Pause);
                crate::shortcuts::present(Some(w.window.upcast_ref()));
            }),
            700,
        ),
    ];

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
