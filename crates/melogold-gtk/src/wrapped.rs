//! Итоги года (задание 0009): шесть карточек на всю страницу, листаются касанием, стрелками и
//! свайпом; «Поделиться» — картинка 1080×1920 PNG, фон — цвет обложки трека года, надпись
//! «Melogold · Итоги 2026». Картинка рисуется в `GskRenderNode` и отдаётся отрисовщиком окна — на
//! экран она не попадает; дальше «Сохранить как…» и буфер обмена.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene, gsk, pango};
use melogold_core::stats_window::{self as calendar, Period};
use melogold_core::text::now_ms;
use melogold_core::thumbnails;
use melogold_data::stats::{wrapped_cards, ListeningStats, WrappedCard};
use melogold_data::DeviceFilter;

use crate::localization::{format_count, plural, tr, trf};
use crate::stats_text as text;
use crate::widgets::Cover;
use crate::window::MainWindow;

/// Цвет-зерно бренда — для года без обложки, у которой можно взять цвет.
const BRAND_SEED: u32 = 0xFE6B08;
const SHARE_WIDTH: f32 = 1080.0;
const SHARE_HEIGHT: f32 = 1920.0;
/// Строк в списках карточек.
const LIST_SHOWN: usize = 5;

/// Итоги года `year` на всех устройствах — из своей истории, не из главного потока.
pub fn load(window: &MainWindow, year: i64) -> impl std::future::Future<Output = Option<ListeningStats>> {
    let task = window.ctx.services.db(move |library| {
        let tz = glib::TimeZone::local();
        let zone = move |utc_ms: i64| {
            let interval = tz.find_interval(glib::TimeType::Universal, utc_ms.div_euclid(1000));
            i64::from(tz.offset(interval)) * 1000
        };
        let today = calendar::local_day(now_ms(), &zone);
        let current_year = calendar::civil_from_days(today).0;
        let window = calendar::window(Period::Year, year - current_year, today, &zone);
        library.listening_stats(window, &zone, today, &DeviceFilter::All).ok()
    });
    async move { task.await.flatten() }
}

pub fn page(window: &MainWindow, year: i64) -> adw::NavigationPage {
    let stack = gtk::Stack::builder().vexpand(true).hexpand(true).build();
    let spinner =
        adw::Spinner::builder().halign(gtk::Align::Center).valign(gtk::Align::Center).width_request(32).height_request(32).build();
    stack.add_named(&spinner, Some("loading"));
    let empty = adw::StatusPage::builder().icon_name("x-office-calendar-symbolic").title(tr("LinuxWrappedEmpty")).build();
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name("loading");

    let overlay = gtk::Overlay::builder().child(&stack).build();
    let nav_page = adw::NavigationPage::builder().title(trf("LinuxWrappedTitle", &[&year])).child(&overlay).build();
    nav_page.add_css_class("wrapped-page");

    // Верх: полоски листания и «Закрыть».
    let progress = gtk::Box::builder().spacing(4).hexpand(true).valign(gtk::Align::Center).build();
    let close = gtk::Button::builder().icon_name("window-close-symbolic").tooltip_text(tr("Close")).build();
    close.add_css_class("circular");
    close.add_css_class("flat");
    close.add_css_class("wrapped-control");
    close.update_property(&[gtk::accessible::Property::Label(tr("Close"))]);
    let top = gtk::Box::builder().spacing(8).valign(gtk::Align::Start).margin_top(12).margin_start(16).margin_end(8).visible(false).build();
    top.append(&progress);
    top.append(&close);
    overlay.add_overlay(&top);

    // Низ: назад, «Поделиться», дальше.
    let back = round_button("go-previous-symbolic", tr("LinuxWrappedBack"));
    let forward = round_button("go-next-symbolic", tr("LinuxWrappedNext"));
    let share = gtk::MenuButton::builder()
        .child(&adw::ButtonContent::builder().label(tr("MenuShare")).icon_name("send-to-symbolic").build())
        .valign(gtk::Align::Center)
        .build();
    share.add_css_class("wrapped-share");
    let bottom = gtk::CenterBox::builder().valign(gtk::Align::End).margin_bottom(14).margin_start(12).margin_end(12).visible(false).build();
    bottom.set_start_widget(Some(&back));
    bottom.set_center_widget(Some(&share));
    bottom.set_end_widget(Some(&forward));
    overlay.add_overlay(&bottom);

    let shown: Rc<RefCell<Option<Rc<ListeningStats>>>> = Rc::default();
    let popover = gtk::Popover::new();
    let menu =
        gtk::Box::builder().orientation(gtk::Orientation::Vertical).margin_top(4).margin_bottom(4).margin_start(4).margin_end(4).build();
    for (label, icon, save) in
        [(tr("LinuxShareCopyImage"), "edit-copy-symbolic", false), (tr("LinuxShareSaveAs"), "document-save-symbolic", true)]
    {
        let button = gtk::Button::builder()
            .child(&adw::ButtonContent::builder().label(label).icon_name(icon).halign(gtk::Align::Start).build())
            .build();
        button.add_css_class("flat");
        let (weak, shown, popover) = (window.downgrade(), Rc::clone(&shown), popover.clone());
        button.connect_clicked(move |_| {
            popover.popdown();
            let (Some(window), Some(stats)) = (weak.upgrade(), shown.borrow().clone()) else { return };
            glib::spawn_future_local(async move {
                match share_texture(&window, &stats, year).await {
                    Some(texture) if save => save_as(&window, &texture, year),
                    Some(texture) => {
                        window.window.clipboard().set_texture(&texture);
                        window.toast(tr("LinuxShareCopied"));
                    }
                    None => window.toast(tr("LinuxShareFailed")),
                }
            });
        });
        menu.append(&button);
    }
    popover.set_child(Some(&menu));
    share.set_popover(Some(&popover));

    let weak = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.go_back();
        }
    });

    let (weak, stack, top, bottom, progress, shown_ref, overlay_ref) =
        (window.downgrade(), stack.clone(), top.clone(), bottom.clone(), progress.clone(), Rc::clone(&shown), overlay.clone());
    let (back_ref, forward_ref, page_ref) = (back.clone(), forward.clone(), nav_page.clone());
    let task = load(window, year);
    glib::spawn_future_local(async move {
        let (Some(window), Some(stats)) = (weak.upgrade(), task.await) else { return };
        let cards = wrapped_cards(&stats);
        if cards.is_empty() {
            stack.set_visible_child_name("empty");
            return;
        }
        let stats = Rc::new(stats);
        shown_ref.replace(Some(Rc::clone(&stats)));
        let carousel = adw::Carousel::builder().hexpand(true).vexpand(true).interactive(true).allow_mouse_drag(true).build();
        for (index, card) in cards.iter().enumerate() {
            carousel.append(&card_page(&window, &stats, *card, index, year));
        }
        stack.add_named(&carousel, Some("cards"));
        stack.set_visible_child_name("cards");
        top.set_visible(true);
        bottom.set_visible(true);
        let segments: Vec<gtk::Box> = cards
            .iter()
            .map(|_| {
                let segment = gtk::Box::builder().hexpand(true).height_request(4).build();
                segment.add_css_class("wrapped-segment");
                progress.append(&segment);
                segment
            })
            .collect();
        let count = cards.len() as u32;
        let update = {
            let (segments, back, forward, progress) = (segments.clone(), back_ref.clone(), forward_ref.clone(), progress.clone());
            move |index: u32| {
                for (i, segment) in segments.iter().enumerate() {
                    if i as u32 <= index {
                        segment.add_css_class("done");
                    } else {
                        segment.remove_css_class("done");
                    }
                }
                back.set_sensitive(index > 0);
                forward.set_sensitive(index + 1 < count);
                progress.update_property(&[gtk::accessible::Property::Label(&trf("LinuxWrappedPage", &[&(index + 1), &count]))]);
            }
        };
        update(0);
        carousel.connect_page_changed(move |_, index| update(index));
        // Шаг назад и вперёд: кнопками, стрелками, касанием слева и справа.
        let go = {
            let carousel = carousel.clone();
            move |delta: i32| {
                let target = (carousel.position().round() as i32 + delta).clamp(0, count as i32 - 1);
                carousel.scroll_to(&carousel.nth_page(target as u32), true);
            }
        };
        let go = Rc::new(go);
        let (g, h) = (Rc::clone(&go), Rc::clone(&go));
        back_ref.connect_clicked(move |_| g(-1));
        forward_ref.connect_clicked(move |_| h(1));
        let keys = gtk::EventControllerKey::new();
        let g = Rc::clone(&go);
        keys.connect_key_pressed(move |_, key, _, _| {
            let rtl = gtk::Widget::default_direction() == gtk::TextDirection::Rtl;
            match key {
                gdk::Key::Left => g(if rtl { 1 } else { -1 }),
                gdk::Key::Right => g(if rtl { -1 } else { 1 }),
                gdk::Key::Page_Down | gdk::Key::space => g(1),
                gdk::Key::Page_Up => g(-1),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        page_ref.add_controller(keys);
        let tap = gtk::GestureClick::new();
        let carousel_ref = carousel.clone();
        tap.connect_released(move |_, _, x, _| {
            // Как в историях: левая треть — назад, остальное — вперёд.
            go(if x < f64::from(carousel_ref.width()) / 3.0 { -1 } else { 1 });
        });
        carousel.add_controller(tap);
        carousel.set_focusable(true);
        carousel.grab_focus();
        let _ = &overlay_ref;
    });
    nav_page
}

fn round_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder().icon_name(icon).tooltip_text(label).build();
    button.add_css_class("circular");
    button.add_css_class("flat");
    button.add_css_class("wrapped-control");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

// ── карточки ──

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder().label(text).wrap(true).justify(gtk::Justification::Center).build();
    for class in classes {
        label.add_css_class(class);
    }
    label
}

fn card_page(window: &MainWindow, stats: &ListeningStats, card: WrappedCard, index: usize, year: i64) -> gtk::ScrolledWindow {
    let inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    inner.append(&label(&trf("LinuxWrappedTitle", &[&year]), &["wrapped-kicker"]));
    inner.append(&gtk::Box::builder().height_request(10).build());
    match card {
        WrappedCard::Minutes => {
            let minutes = stats.minutes();
            inner.append(&label(&format_count(minutes), &["wrapped-huge"]));
            inner.append(&label(plural("LinuxWrappedMinutes", minutes).replace(&format_count(minutes), "").trim(), &["wrapped-heading"]));
            let summary = format!("{} · {}", plural("LinuxStatsPlays", stats.plays), plural("Tracks", stats.tracks as i64));
            let summary = label(&summary, &["wrapped-note"]);
            summary.set_margin_top(20);
            inner.append(&summary);
        }
        WrappedCard::TrackOfYear => {
            if let Some(top) = stats.top_tracks.first() {
                inner.append(&label(tr("LinuxWrappedTrack"), &["wrapped-heading"]));
                let cover = Cover::new(200);
                cover.root.add_css_class("large");
                cover.root.set_margin_top(12);
                cover.root.set_margin_bottom(12);
                cover.set(&window.ctx.services.images, top.track.thumbnail_url.as_deref(), 400);
                inner.append(&cover.root);
                inner.append(&label(&top.title, &["wrapped-track-title"]));
                if let Some(artist) = &top.artist {
                    inner.append(&label(artist, &["wrapped-artist"]));
                }
                let detail = format!("{} · {}", plural("LinuxStatsPlays", top.plays), text::duration(top.ms));
                let detail = label(&detail, &["wrapped-note"]);
                detail.set_margin_top(8);
                inner.append(&detail);
            }
        }
        WrappedCard::TopArtists => {
            inner.append(&label(tr("LinuxStatsTopArtists"), &["wrapped-heading"]));
            let rows = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(14).width_request(300).build();
            for (place, artist) in stats.top_artists.iter().take(LIST_SHOWN).enumerate() {
                rows.append(&list_row(
                    window,
                    place + 1,
                    artist.thumbnail_url.as_deref(),
                    true,
                    &artist.name,
                    None,
                    &text::duration(artist.ms),
                ));
            }
            inner.append(&rows);
        }
        WrappedCard::TopTracks => {
            inner.append(&label(tr("LinuxStatsTopTracks"), &["wrapped-heading"]));
            let rows = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(10).margin_top(14).width_request(300).build();
            for (place, top) in stats.top_tracks.iter().take(LIST_SHOWN).enumerate() {
                let cover = top.track.thumbnail_url.as_deref();
                rows.append(&list_row(window, place + 1, cover, false, &top.title, top.artist.as_deref(), &text::duration(top.ms)));
            }
            inner.append(&rows);
        }
        WrappedCard::FavoriteTime => {
            inner.append(&label(tr("LinuxWrappedMonth"), &["wrapped-subheading"]));
            if let Some(bar) = stats.busiest_bar() {
                let (_, month, _) = calendar::civil_from_days(bar.day);
                inner.append(&label(&text::month_name(month), &["wrapped-big"]));
                inner.append(&label(&text::duration(bar.ms), &["wrapped-note"]));
            }
            let gap = gtk::Box::builder().height_request(28).build();
            inner.append(&gap);
            inner.append(&label(tr("LinuxWrappedTime"), &["wrapped-subheading"]));
            if let Some(part) = stats.favorite_day_part() {
                inner.append(&label(text::day_part_name(part), &["wrapped-big"]));
            }
            if let Some(hour) = stats.peak_hour() {
                inner.append(&label(&trf("LinuxWrappedPeak", &[&text::hour_text(hour)]), &["wrapped-note"]));
            }
        }
        WrappedCard::Discoveries => {
            let count = stats.discoveries.as_ref().map_or(0, |d| d.count) as i64;
            inner.append(&label(tr("LinuxWrappedDiscoveries"), &["wrapped-subheading"]));
            inner.append(&label(&format_count(count), &["wrapped-huge"]));
            inner.append(&label(plural("LinuxWrappedNewTracks", count).replace(&format_count(count), "").trim(), &["wrapped-heading"]));
            let rows = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).margin_top(16).width_request(300).build();
            for (place, top) in stats.discoveries.iter().flat_map(|d| d.top.iter()).enumerate() {
                rows.append(&list_row(window, place + 1, top.track.thumbnail_url.as_deref(), false, &top.title, top.artist.as_deref(), ""));
            }
            inner.append(&rows);
        }
    }
    let clamp =
        adw::Clamp::builder().maximum_size(460).tightening_threshold(360).child(&inner).valign(gtk::Align::Center).vexpand(true).build();
    let outer = gtk::Box::builder().orientation(gtk::Orientation::Vertical).vexpand(true).build();
    outer.add_css_class("wrapped-card");
    outer.add_css_class(&format!("c{}", index % 6));
    outer.append(&clamp);
    let scroller =
        gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&outer).vexpand(true).hexpand(true).build();
    scroller.add_css_class("wrapped-scroller");
    scroller
}

/// Строка списка карточки: место, обложка, название (и исполнитель), время.
fn list_row(
    window: &MainWindow,
    place: usize,
    cover_url: Option<&str>,
    round: bool,
    title: &str,
    subtitle: Option<&str>,
    time: &str,
) -> gtk::Box {
    let row = gtk::Box::builder().spacing(12).build();
    let rank = gtk::Label::builder().label(place.to_string()).width_chars(2).build();
    rank.add_css_class("wrapped-rank");
    row.append(&rank);
    let cover = Cover::new(48);
    if round {
        cover.root.add_css_class("round");
    }
    cover.set(&window.ctx.services.images, cover_url, 96);
    row.append(&cover.root);
    let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).hexpand(true).build();
    let title = gtk::Label::builder().label(title).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
    title.add_css_class("wrapped-row-title");
    texts.append(&title);
    if let Some(subtitle) = subtitle {
        let subtitle = gtk::Label::builder().label(subtitle).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
        subtitle.add_css_class("wrapped-row-subtitle");
        texts.append(&subtitle);
    }
    row.append(&texts);
    if !time.is_empty() {
        let time = gtk::Label::builder().label(time).build();
        time.add_css_class("wrapped-row-time");
        row.append(&time);
    }
    row
}

// ── картинка «Поделиться» ──

/// Картинка года: обложка трека года берётся из сети, цвет фона — из неё.
pub async fn share_texture(window: &MainWindow, stats: &ListeningStats, year: i64) -> Option<gdk::Texture> {
    let track = stats.top_tracks.first().map(|t| &t.track);
    let cover = match track.and_then(|t| thumbnails::sized(t.thumbnail_url.as_deref(), 640)) {
        Some(url) => window.ctx.services.images.load(url).await,
        None => None,
    };
    let seed = match &cover {
        Some(texture) => crate::widgets::artwork_seed(texture).await,
        None => None,
    };
    render_share(&window.window, stats, year, cover.as_ref(), seed.unwrap_or(BRAND_SEED))
}

fn rgba(rgb: u32, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(((rgb >> 16) & 0xFF) as f32 / 255.0, ((rgb >> 8) & 0xFF) as f32 / 255.0, (rgb & 0xFF) as f32 / 255.0, alpha)
}

fn mix(a: u32, b: u32, share: f32) -> u32 {
    let channel = |shift: u32| {
        let (x, y) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        (x + (y - x) * share).round() as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

fn luminance(rgb: u32) -> f32 {
    let linear = |c: u32| {
        let c = c as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear((rgb >> 16) & 0xFF) + 0.7152 * linear((rgb >> 8) & 0xFF) + 0.0722 * linear(rgb & 0xFF)
}

/// Одна строка текста картинки: возвращает её высоту. Шрифт — интерфейса, размер в пикселях картинки.
struct Painter<'a> {
    snapshot: gtk::Snapshot,
    widget: &'a adw::ApplicationWindow,
}

impl Painter<'_> {
    fn text(&self, text: &str, size: f64, weight: pango::Weight, color: gdk::RGBA, y: f32, lines: i32) -> f32 {
        let layout = self.widget.create_pango_layout(Some(text));
        let mut font = layout.context().font_description().unwrap_or_default();
        font.set_absolute_size(size * f64::from(pango::SCALE));
        font.set_weight(weight);
        layout.set_font_description(Some(&font));
        layout.set_width((f64::from(SHARE_WIDTH - 192.0) * f64::from(pango::SCALE)) as i32);
        layout.set_alignment(pango::Alignment::Center);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        layout.set_wrap(pango::WrapMode::WordChar);
        layout.set_height(-lines);
        let height = layout.pixel_size().1 as f32;
        self.snapshot.save();
        self.snapshot.translate(&graphene::Point::new(96.0, y));
        self.snapshot.append_layout(&layout, &color);
        self.snapshot.restore();
        height
    }
}

/// Нарисовать картинку «Итогов года» 1080×1920 и отдать отрисовщиком окна. `None` — окно без отрисовщика.
pub fn render_share(
    window: &adw::ApplicationWindow,
    stats: &ListeningStats,
    year: i64,
    cover: Option<&gdk::Texture>,
    seed: u32,
) -> Option<gdk::Texture> {
    let (top, bottom) = (seed, mix(seed, 0x000000, 0.45));
    let ink_rgb = if luminance(mix(top, bottom, 0.5)) > 0.45 { 0x1A1A1A } else { 0xFFFFFF };
    let (ink, soft, quiet) = (rgba(ink_rgb, 1.0), rgba(ink_rgb, 0.85), rgba(ink_rgb, 0.75));
    let painter = Painter { snapshot: gtk::Snapshot::new(), widget: window };
    let snapshot = &painter.snapshot;
    let full = graphene::Rect::new(0.0, 0.0, SHARE_WIDTH, SHARE_HEIGHT);
    snapshot.append_linear_gradient(
        &full,
        &graphene::Point::new(0.0, 0.0),
        &graphene::Point::new(0.0, SHARE_HEIGHT),
        &[gsk::ColorStop::new(0.0, rgba(top, 1.0)), gsk::ColorStop::new(1.0, rgba(bottom, 1.0))],
    );

    let mut y = 100.0;
    y += painter.text(&trf("LinuxShareWatermark", &[&year]), 52.0, pango::Weight::Bold, ink, y, 1) + 48.0;

    // Обложка трека года 560×560 со скруглением; крупная картинка обрезается по центру.
    let side = 560.0;
    let frame = graphene::Rect::new((SHARE_WIDTH - side) / 2.0, y, side, side);
    snapshot.push_rounded_clip(&gsk::RoundedRect::new(
        frame,
        graphene::Size::new(56.0, 56.0),
        graphene::Size::new(56.0, 56.0),
        graphene::Size::new(56.0, 56.0),
        graphene::Size::new(56.0, 56.0),
    ));
    snapshot.append_color(&rgba(ink_rgb, 0.12), &frame);
    if let Some(texture) = cover {
        let (width, height) = (texture.width() as f32, texture.height() as f32);
        let scale = (side / width).max(side / height);
        let (drawn_width, drawn_height) = (width * scale, height * scale);
        let target =
            graphene::Rect::new(frame.x() + (side - drawn_width) / 2.0, frame.y() + (side - drawn_height) / 2.0, drawn_width, drawn_height);
        snapshot.append_texture(texture, &target);
    }
    snapshot.pop();
    y += side + 40.0;

    if let Some(track) = stats.top_tracks.first() {
        y += painter.text(tr("LinuxWrappedTrack"), 40.0, pango::Weight::Medium, quiet, y, 1) + 4.0;
        y += painter.text(&track.title, 76.0, pango::Weight::Bold, ink, y, 2) + 6.0;
        if let Some(artist) = &track.artist {
            y += painter.text(artist, 48.0, pango::Weight::Normal, soft, y, 1);
        }
    }
    y += 48.0;
    y += painter.text(tr("LinuxStatsTopArtists"), 40.0, pango::Weight::Medium, quiet, y, 1) + 12.0;
    for (place, artist) in stats.top_artists.iter().take(LIST_SHOWN).enumerate() {
        y += painter.text(&format!("{}  {}", place + 1, artist.name), 52.0, pango::Weight::Semibold, ink, y, 1) + 12.0;
    }

    // Минуты — внизу.
    let minutes = stats.minutes();
    let block = 128.0 * 1.25 + 48.0 * 1.3;
    let mut bottom_y = (SHARE_HEIGHT - 100.0 - block).max(y + 24.0);
    bottom_y += painter.text(&format_count(minutes), 128.0, pango::Weight::Bold, ink, bottom_y, 1);
    let unit = plural("LinuxWrappedMinutes", minutes).replace(&format_count(minutes), "").trim().to_owned();
    painter.text(&unit, 48.0, pango::Weight::Medium, ink, bottom_y, 1);

    let node = painter.snapshot.to_node()?;
    let renderer = window.native().and_then(|native| native.renderer())?;
    Some(renderer.render_texture(&node, Some(&full)))
}

/// «Сохранить как…»: PNG туда, куда выберет человек (по умолчанию «Изображения»).
fn save_as(window: &MainWindow, texture: &gdk::Texture, year: i64) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(tr("LinuxShareFileType")));
    filter.add_suffix("png");
    filter.add_mime_type("image/png");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title(tr("LinuxShareSaveAs"))
        .modal(true)
        .initial_name(format!("melogold-insights-{year}.png"))
        .filters(&filters)
        .default_filter(&filter)
        .build();
    if let Some(pictures) = glib::user_special_dir(glib::UserDirectory::Pictures) {
        dialog.set_initial_folder(Some(&gio::File::for_path(pictures)));
    }
    let (weak, texture) = (window.downgrade(), texture.clone());
    let done = Cell::new(false);
    dialog.save(Some(&window.window), gio::Cancellable::NONE, move |result| {
        // Отмена выбора — не ошибка.
        let (Some(window), Ok(file)) = (weak.upgrade(), result) else { return };
        if done.replace(true) {
            return;
        }
        match file.path().map(|path| texture.save_to_png(path)) {
            Some(Ok(())) => window.toast(tr("LinuxShareSaved")),
            _ => window.toast(tr("LinuxShareFailed")),
        }
    });
}
