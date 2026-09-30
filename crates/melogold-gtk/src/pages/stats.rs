//! Итоги (задание 0009): статистика прослушиваний за неделю, месяц, год и всё время. Считается на
//! устройстве по своей истории ([`melogold_data::stats`]), сеть не нужна. Экран: период со стрелками,
//! фильтр устройств, числа, топы, «Когда вы слушали», «Открытия»; «Итоги года» — на своей странице
//! ([`crate::wrapped`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::stats_window::{self as calendar, Period};
use melogold_core::text::now_ms;
use melogold_data::stats::{ListeningStats, TopAlbum, TopArtist, TopTrack, TOP_LIMIT};
use melogold_data::Change;
use melogold_playback::engine::Command;

use crate::device_filter::DeviceFilterBox;
use crate::localization::{plural, tr, trf};
use crate::pages::library::{clear, heading, live, title_label};
use crate::stats_chart::{bar_chart, ChartBar};
use crate::stats_text as text;
use crate::widgets::Cover;
use crate::window::MainWindow;

/// Сколько строк в топе, пока не нажали «Показать все».
const TOP_SHOWN: usize = 10;

/// Что выбрал человек и что посчитано.
struct View {
    period: Cell<Period>,
    /// 0 — текущий период, -1 — прошлый.
    offset: Cell<i64>,
    /// «Показать все» у треков, исполнителей и альбомов.
    expanded: [Cell<bool>; 3],
    /// Последний ответ: пока считается новый, экран не мигает.
    stats: RefCell<Option<Rc<ListeningStats>>>,
    /// Местный день на момент подсчёта.
    today: Cell<i64>,
    generation: Cell<u64>,
}

/// Элементы над содержимым, которые меняются вместе с периодом.
#[derive(Clone)]
struct Controls {
    period_title: gtk::Label,
    previous: gtk::Button,
    next: gtk::Button,
    navigator: gtk::Box,
    note: gtk::Label,
    wrapped: gtk::Button,
}

pub fn page(window: &MainWindow) -> adw::NavigationPage {
    let p = crate::pages::library::page(tr("LinuxStats"), None);
    let view = Rc::new(View {
        period: Cell::new(Period::Month),
        offset: Cell::new(0),
        expanded: Default::default(),
        stats: RefCell::default(),
        today: Cell::new(calendar::local_day(now_ms(), &|_| 0)),
        generation: Cell::new(0),
    });

    // Заголовок и «Итоги года» справа.
    let head = gtk::Box::builder().spacing(8).build();
    let title = title_label(tr("LinuxStats"));
    title.set_hexpand(true);
    head.append(&title);
    let wrapped = gtk::Button::builder()
        .child(&adw::ButtonContent::builder().label(tr("LinuxWrapped")).icon_name("starred-symbolic").build())
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    wrapped.add_css_class("pill");
    wrapped.add_css_class("suggested-action");
    head.append(&wrapped);
    p.above(&head);

    // Период и устройства.
    let periods = adw::ToggleGroup::builder().halign(gtk::Align::Start).build();
    for (name, key) in [("week", "LinuxStatsWeek"), ("month", "LinuxStatsMonth"), ("year", "Year"), ("all", "AllTime")] {
        periods.add(adw::Toggle::builder().name(name).label(tr(key)).build());
    }
    periods.set_active_name(Some("month"));
    let device_filter = DeviceFilterBox::new();
    let controls_row = gtk::Box::builder().spacing(12).build();
    controls_row.append(&periods);
    controls_row.append(&device_filter.dropdown);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&controls_row)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .build();
    p.above(&scroller);

    // ‹ Сентябрь 2026 ›
    let previous = gtk::Button::builder().icon_name("go-previous-symbolic").tooltip_text(tr("LinuxStatsPrevious")).build();
    let next = gtk::Button::builder().icon_name("go-next-symbolic").tooltip_text(tr("LinuxStatsNext")).build();
    for (button, label) in [(&previous, tr("LinuxStatsPrevious")), (&next, tr("LinuxStatsNext"))] {
        button.add_css_class("circular");
        button.add_css_class("flat");
        button.update_property(&[gtk::accessible::Property::Label(label)]);
    }
    let period_title = gtk::Label::builder().hexpand(true).ellipsize(gtk::pango::EllipsizeMode::End).build();
    period_title.add_css_class("title-3");
    let navigator = gtk::Box::builder().spacing(8).build();
    navigator.append(&previous);
    navigator.append(&period_title);
    navigator.append(&next);
    p.above(&navigator);
    let note = gtk::Label::builder().label(tr("LinuxStatsAllTimeNote")).xalign(0.0).wrap(true).visible(false).build();
    note.add_css_class("dim-label");
    note.add_css_class("caption");
    p.above(&note);
    let controls = Controls { period_title, previous: previous.clone(), next: next.clone(), navigator, note, wrapped: wrapped.clone() };

    let (weak, content, state) = (window.downgrade(), p.content.clone(), p.state.clone());
    let (view_ref, controls_ref, device_ref) = (Rc::clone(&view), controls.clone(), device_filter.clone());
    let mask = Change(Change::HISTORY.0 | Change::OVERRIDES.0);
    let refresh = live(window, &p.page, mask, move || {
        let Some(window) = weak.upgrade() else { return };
        let generation = view_ref.generation.get() + 1;
        view_ref.generation.set(generation);
        let (period, offset, filter) = (view_ref.period.get(), view_ref.offset.get(), device_ref.current());
        let task = window.ctx.services.db(move |library| {
            let tz = glib::TimeZone::local();
            let zone = move |utc_ms: i64| {
                let interval = tz.find_interval(glib::TimeType::Universal, utc_ms.div_euclid(1000));
                i64::from(tz.offset(interval)) * 1000
            };
            let today = calendar::local_day(now_ms(), &zone);
            let window = calendar::window(period, offset, today, &zone);
            library.listening_stats(window, &zone, today, &filter).map(|stats| (stats, today))
        });
        let (weak, view, controls, content, state) =
            (window.downgrade(), Rc::clone(&view_ref), controls_ref.clone(), content.clone(), state.clone());
        glib::spawn_future_local(async move {
            let (Some(window), Some(Ok((stats, today)))) = (weak.upgrade(), task.await) else { return };
            // Пока считалось, выбрали другое: этот ответ устарел.
            if view.generation.get() != generation {
                return;
            }
            view.today.set(today);
            let stats = Rc::new(stats);
            view.stats.replace(Some(Rc::clone(&stats)));
            show(&window, &view, &controls, &content, &state, &stats);
        });
    });

    let (view_ref, refresh_ref) = (Rc::clone(&view), Rc::clone(&refresh));
    periods.connect_active_name_notify(move |group| {
        view_ref.period.set(match group.active_name().as_deref() {
            Some("week") => Period::Week,
            Some("year") => Period::Year,
            Some("all") => Period::AllTime,
            _ => Period::Month,
        });
        view_ref.offset.set(0);
        view_ref.expanded.iter().for_each(|e| e.set(false));
        refresh_ref();
    });
    let refresh_ref = Rc::clone(&refresh);
    device_filter.connect_changed(move || refresh_ref());
    device_filter.attach(window, &p.page, Rc::clone(&refresh));
    for (button, step) in [(&previous, -1), (&next, 1)] {
        let (view_ref, refresh_ref) = (Rc::clone(&view), Rc::clone(&refresh));
        button.connect_clicked(move |_| {
            view_ref.offset.set((view_ref.offset.get() + step).min(0));
            view_ref.expanded.iter().for_each(|e| e.set(false));
            refresh_ref();
        });
    }
    let (weak, view_ref) = (window.downgrade(), Rc::clone(&view));
    wrapped.connect_clicked(move |_| {
        let Some(window) = weak.upgrade() else { return };
        window.push(&crate::wrapped::page(&window, wrapped_year(&view_ref)));
    });
    p.page
}

/// Год «Итогов года» для кнопки: у «Года» — открытый, иначе — заканчивающийся или только что закончившийся.
fn wrapped_year(view: &View) -> i64 {
    let (year, month, _) = calendar::civil_from_days(view.today.get());
    match (view.period.get(), view.stats.borrow().as_ref().and_then(|s| s.window.first_date())) {
        (Period::Year, Some((viewed, ..))) => viewed,
        _ => calendar::wrapped_season_year(year, month).unwrap_or(year),
    }
}

/// Выставить элементы управления по итогам и перестроить содержимое.
fn show(
    window: &MainWindow,
    view: &Rc<View>,
    controls: &Controls,
    content: &gtk::Box,
    state: &crate::widgets::StateView,
    stats: &ListeningStats,
) {
    let (current_year, current_month, _) = calendar::civil_from_days(view.today.get());
    controls.period_title.set_label(&text::period_title(&stats.window, current_year));
    controls.navigator.set_visible(stats.window.period != Period::AllTime);
    controls.note.set_visible(stats.window.period == Period::AllTime);
    controls.previous.set_sensitive(stats.has_earlier());
    controls.next.set_sensitive(view.offset.get() < 0);
    controls
        .wrapped
        .set_visible(stats.window.period == Period::Year || calendar::wrapped_season_year(current_year, current_month).is_some());

    clear(content);
    if stats.is_empty() {
        state.empty("x-office-calendar-symbolic", tr("LinuxStatsEmpty"), "");
        return;
    }
    content.append(&numbers(stats, current_year));
    let tracks: Vec<TopTrack> = stats.top_tracks.clone();
    let queue: Rc<Vec<melogold_core::music::Track>> = Rc::new(tracks.iter().map(|t| t.track.clone()).collect());

    // Лучшие треки.
    let rows = tracks.iter().take(limit(view, 0)).enumerate().map(|(index, top)| {
        let queue = Rc::clone(&queue);
        let weak = window.downgrade();
        Row {
            cover: top.track.thumbnail_url.clone(),
            round: false,
            title: top.title.clone(),
            subtitle: top.artist.clone(),
            time: text::duration(top.ms),
            plays: plural("LinuxStatsPlays", top.plays),
            open: Box::new(move || {
                if let Some(window) = weak.upgrade() {
                    window.ctx.services.player.send(Command::PlayList { tracks: queue.to_vec(), start: index, shuffle: false });
                }
            }),
        }
    });
    section(view, content, window, "LinuxStatsTopTracks", 0, tracks.len(), rows.collect(), stats, controls, state);

    // Лучшие исполнители.
    let rows = stats.top_artists.iter().take(limit(view, 1)).map(|artist| artist_row(window, artist)).collect();
    section(view, content, window, "LinuxStatsTopArtists", 1, stats.top_artists.len(), rows, stats, controls, state);

    // Лучшие альбомы.
    let rows = stats.top_albums.iter().take(limit(view, 2)).map(|album| album_row(window, album)).collect();
    section(view, content, window, "LinuxStatsTopAlbums", 2, stats.top_albums.len(), rows, stats, controls, state);

    content.append(&when_section(stats));

    if let Some(discoveries) = stats.discoveries.as_ref().filter(|d| d.count > 0) {
        content.append(&heading(tr("LinuxStatsDiscoveries")));
        let summary = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
        let count = gtk::Label::builder().label(plural("LinuxStatsNew", discoveries.count as i64)).xalign(0.0).build();
        count.add_css_class("title-2");
        let dim = gtk::Label::builder().label(tr("LinuxStatsDiscoveriesText")).xalign(0.0).wrap(true).build();
        dim.add_css_class("dim-label");
        summary.append(&count);
        summary.append(&dim);
        content.append(&summary);
        let queue: Rc<Vec<melogold_core::music::Track>> = Rc::new(discoveries.top.iter().map(|t| t.track.clone()).collect());
        let rows = discoveries
            .top
            .iter()
            .enumerate()
            .map(|(index, top)| {
                let (queue, weak) = (Rc::clone(&queue), window.downgrade());
                Row {
                    cover: top.track.thumbnail_url.clone(),
                    round: false,
                    title: top.title.clone(),
                    subtitle: top.artist.clone(),
                    time: text::duration(top.ms),
                    plays: plural("LinuxStatsPlays", top.plays),
                    open: Box::new(move || {
                        if let Some(window) = weak.upgrade() {
                            window.ctx.services.player.send(Command::PlayList { tracks: queue.to_vec(), start: index, shuffle: false });
                        }
                    }),
                }
            })
            .collect();
        content.append(&list(window, rows));
    }
    state.content();
}

fn limit(view: &View, index: usize) -> usize {
    if view.expanded[index].get() {
        TOP_LIMIT
    } else {
        TOP_SHOWN
    }
}

// ── числа ──

fn numbers(stats: &ListeningStats, current_year: i64) -> gtk::FlowBox {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(2)
        .max_children_per_line(4)
        .column_spacing(10)
        .row_spacing(10)
        .build();
    let time = stat_card(&text::duration(stats.total_ms), tr("LinuxStatsListeningTime"), text::comparison(stats, current_year).as_deref());
    time.add_css_class("stat-hero");
    flow.append(&time);
    flow.append(&stat_card(&crate::localization::format_count(stats.plays), tr("LinuxStatsPlaysLabel"), None));
    flow.append(&stat_card(&crate::localization::format_count(stats.tracks as i64), tr("LinuxStatsTracksLabel"), None));
    flow.append(&stat_card(&crate::localization::format_count(stats.artists as i64), tr("LinuxStatsArtistsLabel"), None));
    flow
}

/// Карточка числа: крупное значение, подпись, при необходимости строка сравнения.
fn stat_card(value: &str, caption: &str, note: Option<&str>) -> gtk::Box {
    let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).build();
    card.add_css_class("stat-card");
    let number = gtk::Label::builder().label(value).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    number.add_css_class("stat-number");
    let label =
        gtk::Label::builder().label(caption).xalign(0.0).wrap(true).wrap_mode(gtk::pango::WrapMode::WordChar).max_width_chars(1).build();
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    card.append(&number);
    card.append(&label);
    if let Some(note) = note {
        let delta = gtk::Label::builder().label(note).xalign(0.0).wrap(true).margin_top(4).build();
        delta.add_css_class("stat-delta");
        card.append(&delta);
    }
    card.update_property(&[gtk::accessible::Property::Label(&format!("{caption}: {value}"))]);
    card
}

// ── топы ──

/// Строка топа: место, обложка, название, время и число прослушиваний; нажатие — `open`.
struct Row {
    cover: Option<String>,
    round: bool,
    title: String,
    subtitle: Option<String>,
    time: String,
    plays: String,
    open: Box<dyn Fn()>,
}

fn artist_row(window: &MainWindow, artist: &TopArtist) -> Row {
    let (weak, id, name) = (window.downgrade(), artist.id.clone(), artist.name.clone());
    Row {
        cover: artist.thumbnail_url.clone(),
        round: true,
        title: artist.name.clone(),
        subtitle: Some(plural("Tracks", artist.tracks as i64)),
        time: text::duration(artist.ms),
        plays: plural("LinuxStatsPlays", artist.plays),
        // Без страницы на YouTube (имя написал сам человек) — поиск по имени.
        open: Box::new(move || {
            let Some(window) = weak.upgrade() else { return };
            match &id {
                Some(id) => window.push(&crate::pages::catalog::artist_page(&window, id)),
                None => window.search_for(&name),
            }
        }),
    }
}

fn album_row(window: &MainWindow, album: &TopAlbum) -> Row {
    let (weak, id, title) = (window.downgrade(), album.id.clone(), album.title.clone());
    Row {
        cover: album.thumbnail_url.clone(),
        round: false,
        title: album.title.clone(),
        subtitle: Some(plural("Tracks", album.tracks as i64)),
        time: text::duration(album.ms),
        plays: plural("LinuxStatsPlays", album.plays),
        open: Box::new(move || {
            let Some(window) = weak.upgrade() else { return };
            match &id {
                Some(id) => window.push(&crate::pages::catalog::album_page(&window, id)),
                None => window.search_for(&title),
            }
        }),
    }
}

/// Заголовок топа с «Показать все» и сам список.
#[allow(clippy::too_many_arguments)]
fn section(
    view: &Rc<View>,
    content: &gtk::Box,
    window: &MainWindow,
    title: &'static str,
    index: usize,
    total: usize,
    rows: Vec<Row>,
    stats: &ListeningStats,
    controls: &Controls,
    state: &crate::widgets::StateView,
) {
    if rows.is_empty() {
        return;
    }
    let head = gtk::Box::builder().spacing(8).margin_top(12).build();
    let label = heading(tr(title));
    label.set_margin_top(0);
    label.set_hexpand(true);
    label.set_valign(gtk::Align::Center);
    head.append(&label);
    if total > TOP_SHOWN {
        let expanded = view.expanded[index].get();
        let toggle = gtk::Button::builder()
            .label(tr(if expanded { "LinuxStatsShowLess" } else { "LinuxStatsShowAll" }))
            .valign(gtk::Align::Center)
            .build();
        toggle.add_css_class("see-all");
        let (weak, view, controls, content, state, stats) =
            (window.downgrade(), Rc::clone(view), controls.clone(), content.clone(), state.clone(), stats.clone());
        toggle.connect_clicked(move |_| {
            view.expanded[index].set(!view.expanded[index].get());
            if let Some(window) = weak.upgrade() {
                show(&window, &view, &controls, &content, &state, &stats);
            }
        });
        head.append(&toggle);
    }
    content.append(&head);
    content.append(&list(window, rows));
}

fn list(window: &MainWindow, rows: Vec<Row>) -> gtk::ListBox {
    let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
    list.add_css_class("track-rows");
    let mut opens: Vec<Box<dyn Fn()>> = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        list.append(&row_widget(window, index + 1, &row));
        opens.push(row.open);
    }
    let opens = Rc::new(opens);
    list.connect_row_activated(move |_, row| {
        if let Some(open) = opens.get(row.index() as usize) {
            open();
        }
    });
    list
}

fn row_widget(window: &MainWindow, rank: usize, row: &Row) -> gtk::ListBoxRow {
    let line = gtk::Box::builder().spacing(12).margin_top(6).margin_bottom(6).margin_start(8).margin_end(8).build();
    let place = gtk::Label::builder().label(rank.to_string()).width_chars(2).xalign(0.5).build();
    place.add_css_class("stats-rank");
    line.append(&place);
    let cover = Cover::new(48);
    if row.round {
        cover.root.add_css_class("round");
    }
    cover.set(&window.ctx.services.images, row.cover.as_deref(), 96);
    line.append(&cover.root);
    let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).build();
    let title = gtk::Label::builder().label(&row.title).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    title.add_css_class("heading");
    texts.append(&title);
    if let Some(subtitle) = &row.subtitle {
        let subtitle = gtk::Label::builder().label(subtitle).xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
        subtitle.add_css_class("dim-label");
        texts.append(&subtitle);
    }
    line.append(&texts);
    let meta = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).build();
    let time = gtk::Label::builder().label(&row.time).xalign(1.0).build();
    time.add_css_class("numeric");
    let plays = gtk::Label::builder().label(&row.plays).xalign(1.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
    plays.add_css_class("dim-label");
    plays.add_css_class("caption");
    meta.append(&time);
    meta.append(&plays);
    line.append(&meta);
    let widget = gtk::ListBoxRow::builder().child(&line).activatable(true).build();
    widget.update_property(&[gtk::accessible::Property::Label(&format!("{rank}. {}, {}, {}", row.title, row.time, row.plays))]);
    widget
}

// ── когда слушали ──

fn when_section(stats: &ListeningStats) -> gtk::Box {
    let section = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
    section.append(&heading(tr("LinuxStatsWhen")));
    let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
    card.add_css_class("stat-card");

    let period = stats.window.period;
    let count = stats.bars.len();
    let bars: Vec<ChartBar> = stats
        .bars
        .iter()
        .enumerate()
        .map(|(index, bar)| ChartBar {
            value: bar.ms,
            label: text::bar_label(period, index, count, bar),
            tooltip: format!("{} · {}", text::bar_name(period, bar), text::duration(bar.ms)),
        })
        .collect();
    if let Some(busiest) = stats.busiest_bar() {
        let line = trf("LinuxStatsBusiest", &[&text::bar_name(period, &busiest), &text::duration(busiest.ms)]);
        let caption = gtk::Label::builder().label(&line).xalign(0.0).wrap(true).build();
        caption.add_css_class("dim-label");
        card.append(&caption);
        card.append(&bar_chart(bars, 130, &line));
    }

    let title = gtk::Label::builder().label(tr("LinuxStatsTimeOfDay")).xalign(0.0).margin_top(14).build();
    title.add_css_class("heading");
    card.append(&title);
    let hours: Vec<ChartBar> = stats
        .hours
        .iter()
        .enumerate()
        .map(|(hour, ms)| ChartBar {
            value: *ms,
            label: (hour % 6 == 0).then(|| hour.to_string()),
            tooltip: format!("{} · {}", text::hour_text(hour), text::duration(*ms)),
        })
        .collect();
    let description = match stats.peak_hour() {
        Some(hour) => {
            let part = melogold_data::stats::DayPart::of_hour(hour);
            let line = trf("LinuxStatsHoursBusiest", &[&text::hour_text(hour), &text::day_part_name(part).to_lowercase()]);
            let caption = gtk::Label::builder().label(&line).xalign(0.0).wrap(true).build();
            caption.add_css_class("dim-label");
            card.append(&caption);
            line
        }
        None => tr("LinuxStatsTimeOfDay").to_owned(),
    };
    card.append(&bar_chart(hours, 100, &description));
    section.append(&card);
    section
}
