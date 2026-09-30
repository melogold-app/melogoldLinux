//! Снимки экранов задания 0009: «Итоги» (месяц, неделя без прослушиваний, год, всё время), карточки «Итогов
//! года» и картинка «Поделиться». Истории у чистого профиля снимков нет, поэтому первый шаг кладёт в базу
//! придуманную: чуть больше года прослушиваний двух десятков треков (обложки — с YouTube), последнее —
//! за неделю с лишним до дня снимка, чтобы текущая неделя была пустой.

use adw::prelude::*;
use gtk::glib;
use melogold_core::music::{ArtistRef, Track};
use melogold_core::settings::Tab;
use melogold_core::text::now_ms;

use crate::snapshot::Step;
use crate::window::MainWindow;

/// (videoId, название, исполнитель, альбом).
const DEMO: [(&str, &str, &str, Option<&str>); 22] = [
    ("dQw4w9WgXcQ", "Never Gonna Give You Up", "Rick Astley", Some("Whenever You Need Somebody")),
    ("fJ9rUzIMcZQ", "Bohemian Rhapsody", "Queen", Some("A Night at the Opera")),
    ("cYKAr38pZcY", "Photosynthesis", "Saba", None),
    ("kJQP7kiw5Fk", "Despacito", "Luis Fonsi", None),
    ("JGwWNGJdvx8", "Shape of You", "Ed Sheeran", Some("÷")),
    ("lp-EO5I60KA", "Thinking Out Loud", "Ed Sheeran", Some("x")),
    ("OPf0YbXqDm0", "Uptown Funk", "Mark Ronson feat. Bruno Mars", None),
    ("60ItHLz5WEA", "Faded", "Alan Walker", None),
    ("hT_nvWreIhg", "Counting Stars", "OneRepublic", Some("Native")),
    ("YQHsXMglC9A", "Hello", "Adele", Some("25")),
    ("RgKAFK5djSk", "See You Again", "Wiz Khalifa", None),
    ("e-ORhEE9VVg", "Blank Space", "Taylor Swift", Some("1989")),
    ("nfWlot6h_JM", "Shake It Off", "Taylor Swift", Some("1989")),
    ("CevxZvSJLk8", "Roar", "Katy Perry", Some("Prism")),
    ("09R8_2nJtjg", "Sugar", "Maroon 5", Some("V")),
    ("YykjpeuMNEk", "Hymn for the Weekend", "Coldplay", Some("A Head Full of Dreams")),
    ("4NRXx6U8ABQ", "Blinding Lights", "The Weeknd", Some("After Hours")),
    ("fRh_vgS2dFE", "Sorry", "Justin Bieber", Some("Purpose")),
    ("2Vv-BfVoq4g", "Perfect", "Ed Sheeran", Some("÷")),
    ("JRfuAukYTKg", "Lean On", "Major Lazer", None),
    ("pXRviuL6vMY", "Stressed Out", "twenty one pilots", Some("Blurryface")),
    ("Zi_XLOBDo_Y", "Billie Jean", "Michael Jackson", Some("Thriller")),
];

/// Следующее число детерминированного генератора: снимки одинаковы от запуска к запуску.
fn next(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *seed >> 33
}

/// Придуманная история: тем чаще слушают трек, чем он «популярнее»; трек не играет, пока его не «открыли».
fn seed_history(window: &MainWindow) {
    let library = std::sync::Arc::clone(&window.ctx.services.library);
    let tracks: Vec<Track> = DEMO
        .iter()
        .map(|(id, title, artist, album)| Track {
            video_id: (*id).into(),
            title: (*title).into(),
            artists: vec![ArtistRef { id: None, name: artist.split(" feat. ").next().unwrap_or(artist).into() }],
            artists_text: Some((*artist).into()),
            album_title: album.map(str::to_owned),
            duration_ms: Some(215_000),
            thumbnail_url: Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")),
            ..Default::default()
        })
        .collect();
    let _ = library.save_tracks(&tracks);
    let (day_ms, now) = (86_400_000i64, now_ms());
    let midnight = now - now.rem_euclid(day_ms);
    let mut seed = 20_260_930u64;
    // День, с которого трек «открыт»: у популярных — давно, у остальных — в разные месяцы.
    let opened: Vec<i64> = (0..DEMO.len()).map(|i| if i < 4 { 400 } else { 20 + (next(&mut seed) % 380) as i64 }).collect();
    let _ = library.sync(|tx| {
        let mut count = 0;
        for age in 8..400i64 {
            let day = midnight - age * day_ms;
            let weekday = (day / day_ms + 3).rem_euclid(7);
            let plays = if weekday >= 5 { 7 } else { 4 } + (next(&mut seed) % 4) as i64;
            for _ in 0..plays {
                // Популярное — чаще: индекс из смещённого распределения.
                let pick = ((next(&mut seed) % 1000) as f64 / 1000.0).powf(2.2);
                let index = (pick * DEMO.len() as f64) as usize % DEMO.len();
                if opened[index] < age {
                    continue;
                }
                // Время суток: пик вечером, немного утром.
                let hour = match next(&mut seed) % 10 {
                    0 => 1 + (next(&mut seed) % 4) as i64,
                    1..=2 => 7 + (next(&mut seed) % 3) as i64,
                    3..=5 => 12 + (next(&mut seed) % 5) as i64,
                    _ => 18 + (next(&mut seed) % 5) as i64,
                };
                let at = day + hour * 3_600_000 + (next(&mut seed) % 3_600_000) as i64;
                let played = 60_000 + (next(&mut seed) % 170_000) as i64;
                count += 1;
                tx.ensure_track(DEMO[index].0, None)?;
                tx.insert_play(&format!("demo-{count}"), DEMO[index].0, at, played, None)?;
            }
        }
        Ok(())
    });
}

/// Страница «Итогов» после выбора периода: выбрать `name` в переключателе и подождать подсчёт.
fn choose_period(window: &MainWindow, name: &str) {
    let Some(page) = window.nav(Tab::Library).visible_page() else { return };
    if let Some(group) = find::<adw::ToggleGroup>(page.upcast_ref()) {
        group.set_active_name(Some(name));
    }
}

fn find<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Option<T> {
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = current.downcast_ref::<T>() {
            return Some(found.clone());
        }
        if let Some(found) = find::<T>(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

/// Прокрутить страницу на долю высоты.
fn scroll(window: &MainWindow, fraction: f64) {
    let Some(page) = window.nav(Tab::Library).visible_page() else { return };
    if let Some(scroller) = find::<gtk::ScrolledWindow>(page.upcast_ref()) {
        let adjustment = scroller.vadjustment();
        adjustment.set_value((adjustment.upper() - adjustment.page_size()) * fraction);
    }
}

fn wrapped_card(window: &MainWindow, index: u32) {
    let Some(page) = window.nav(Tab::Library).visible_page() else { return };
    if let Some(carousel) = find::<adw::Carousel>(page.upcast_ref()) {
        if index < carousel.n_pages() {
            carousel.scroll_to(&carousel.nth_page(index), false);
        }
    }
}

fn current_year() -> i64 {
    glib::DateTime::now_local().map(|now| i64::from(now.year())).unwrap_or(2026)
}

pub fn steps() -> Vec<Step> {
    let mut steps: Vec<Step> = vec![
        (
            "30a-stats-month",
            Box::new(|w| {
                seed_history(w);
                w.show_tab(Tab::Library);
                w.push(&crate::pages::stats::page(w));
            }),
            3500,
        ),
        ("30b-stats-month-tops", Box::new(|w| scroll(w, 0.3)), 900),
        ("30c-stats-month-when", Box::new(|w| scroll(w, 0.62)), 900),
        ("30d-stats-month-end", Box::new(|w| scroll(w, 1.0)), 900),
        (
            "30e-stats-week-empty",
            Box::new(|w| {
                scroll(w, 0.0);
                choose_period(w, "week");
            }),
            1200,
        ),
        (
            "30f-stats-year",
            Box::new(|w| {
                choose_period(w, "year");
            }),
            3000,
        ),
        ("30g-stats-year-tops", Box::new(|w| scroll(w, 0.35)), 900),
        (
            "30h-stats-all-time",
            Box::new(|w| {
                scroll(w, 0.0);
                choose_period(w, "all");
            }),
            2500,
        ),
        (
            "31a-wrapped",
            Box::new(|w| {
                w.push(&crate::wrapped::page(w, current_year()));
            }),
            4000,
        ),
    ];
    for index in 1..6u32 {
        let name: &'static str =
            ["31b-wrapped-track", "31c-wrapped-artists", "31d-wrapped-tracks", "31e-wrapped-favorite", "31f-wrapped-discoveries"]
                [index as usize - 1];
        steps.push((name, Box::new(move |w| wrapped_card(w, index)), 900));
    }
    steps.push((
        "31g-share-image",
        Box::new(|w| {
            let weak = w.downgrade();
            glib::spawn_future_local(async move {
                let Some(window) = weak.upgrade() else { return };
                let year = current_year();
                let Some(stats) = crate::wrapped::load(&window, year).await else { return };
                let Some(texture) = crate::wrapped::share_texture(&window, &stats, year).await else { return };
                if let Some(dir) = std::env::var_os("MELOGOLD_SCREENSHOT_DIR") {
                    let path = std::path::Path::new(&dir).join("31g-share-image-1080x1920.png");
                    if texture.save_to_png(&path).is_ok() {
                        println!("снимок: {}", path.display());
                    }
                }
            });
        }),
        3500,
    ));
    steps
}
