//! Лаборатория фона «Сейчас играет» (задание 0031): четыре окна с одной и той же обложкой — нынешний
//! фон и три варианта нового, чтобы выбрать глазами, а не по описанию.
//!
//!     cargo run --release -p melogold-gtk --example backdrop_lab -- <папка с обложками>
//!
//! ←/→ — обложка во всех окнах сразу, пробел — остановить движение, P — полоска долей цветов.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use melogold_core::artwork_colors::{self, ColorShare};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Variant {
    Current,
    Blur,
    Blobs,
    Both,
}

impl Variant {
    const ALL: [Variant; 4] = [Variant::Current, Variant::Blur, Variant::Blobs, Variant::Both];

    fn title(self) -> &'static str {
        match self {
            Variant::Current => "0 · Сейчас",
            Variant::Blur => "A · Размытая обложка",
            Variant::Blobs => "B · Пятна по долям",
            Variant::Both => "C · Размытие + пятна",
        }
    }
}

/// Обложка и всё, что из неё посчитано один раз.
pub(crate) struct Cover {
    name: String,
    texture: gdk::Texture,
    /// Обложка 24×24 после сильного размытия: растянутая на окно, она и есть «размытая обложка» —
    /// без размытия всего окна на каждом кадре.
    blurred: gdk::Texture,
    shares: Vec<ColorShare>,
    seed: Option<u32>,
    /// Средняя яркость (0..1): светлую обложку под белым текстом надо приглушить сильнее.
    luma: f64,
}

fn load_cover(path: &std::path::Path) -> Option<Cover> {
    let texture = gdk::Texture::from_filename(path).ok()?;
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let shares = artwork_colors::shares(&bytes, width, height, stride);
    let seed = artwork_colors::seed(&bytes, width, height, stride);

    const SMALL: usize = 24;
    let mut small = vec![[0f64; 3]; SMALL * SMALL];
    let mut luma = 0.0;
    for (index, cell) in small.iter_mut().enumerate() {
        let (cx, cy) = (index % SMALL, index / SMALL);
        let (x0, x1) = (cx * width / SMALL, ((cx + 1) * width / SMALL).max(cx * width / SMALL + 1));
        let (y0, y1) = (cy * height / SMALL, ((cy + 1) * height / SMALL).max(cy * height / SMALL + 1));
        let mut n = 0.0f64;
        for y in y0..y1.min(height) {
            for x in x0..x1.min(width) {
                let i = y * stride + x * 4;
                for c in 0..3 {
                    cell[c] += f64::from(bytes[i + c]);
                }
                n += 1.0;
            }
        }
        for c in cell.iter_mut() {
            *c /= n.max(1.0);
        }
        luma += (0.2126 * cell[0] + 0.7152 * cell[1] + 0.0722 * cell[2]) / 255.0;
    }
    luma /= (SMALL * SMALL) as f64;
    // Размытие: три прохода «коробкой» радиусом 2 по кругу краёв.
    for _ in 0..3 {
        let copy = small.clone();
        for y in 0..SMALL {
            for x in 0..SMALL {
                let mut sum = [0f64; 3];
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let (sx, sy) = ((x as i32 + dx).clamp(0, SMALL as i32 - 1), (y as i32 + dy).clamp(0, SMALL as i32 - 1));
                        let p = copy[sy as usize * SMALL + sx as usize];
                        for c in 0..3 {
                            sum[c] += p[c];
                        }
                    }
                }
                small[y * SMALL + x] = sum.map(|v| v / 25.0);
            }
        }
    }
    let rgba: Vec<u8> = small.iter().flat_map(|p| [p[0] as u8, p[1] as u8, p[2] as u8, 255]).collect();
    let blurred =
        gdk::MemoryTexture::new(SMALL as i32, SMALL as i32, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(rgba), SMALL * 4)
            .upcast();

    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let name = stem.split_once('-').map_or(stem.as_str(), |(_, rest)| rest).replace('-', " ");
    Some(Cover { name, texture, blurred, shares, seed, luma })
}

// ── виджет фона ──

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Backdrop {
        pub(crate) variant: Cell<Option<Variant>>,
        pub(crate) cover: RefCell<Option<Rc<Cover>>>,
        pub(crate) clock: RefCell<Option<Rc<Clock>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Backdrop {
        const NAME: &'static str = "MelogoldLabBackdrop";
        type Type = super::Backdrop;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Backdrop {}

    impl WidgetImpl for Backdrop {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            let (Some(variant), Some(cover), Some(clock)) = (self.variant.get(), self.cover.borrow().clone(), self.clock.borrow().clone())
            else {
                return;
            };
            super::draw(snapshot, variant, &cover, clock.seconds.get() as f32, w, h, clock.palette.get());
        }
    }
}

glib::wrapper! {
    pub struct Backdrop(ObjectSubclass<imp::Backdrop>) @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// Общие часы окон: все варианты движутся одинаково, пауза — во всех сразу.
#[derive(Default)]
pub(crate) struct Clock {
    seconds: Cell<f64>,
    last_frame: Cell<Option<i64>>,
    paused: Cell<bool>,
    palette: Cell<bool>,
}

fn rgba(rgb: u32, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(((rgb >> 16) & 0xFF) as f32 / 255.0, ((rgb >> 8) & 0xFF) as f32 / 255.0, (rgb & 0xFF) as f32 / 255.0, alpha)
}

fn draw(snapshot: &gtk::Snapshot, variant: Variant, cover: &Cover, t: f32, w: f32, h: f32, palette: bool) {
    let all = graphene::Rect::new(0.0, 0.0, w, h);
    let dominant = cover.shares.first().map_or(0x101010, |c| c.rgb);
    match variant {
        Variant::Current => {
            let colors = cover.seed.map(|seed| artwork_colors::palette(seed, true));
            snapshot.append_color(&rgba(colors.map_or(0x242424, |p| p.background), 1.0), &all);
            if let Some(colors) = colors {
                let center = graphene::Point::new(w * 0.27, h * 0.42);
                let radius = w.min(h) * 0.45;
                radial(snapshot, &all, center, radius, colors.glow, &[(0.0, 0.55), (1.0, 0.0)]);
            }
        }
        Variant::Blur => {
            snapshot.append_color(&rgba(artwork_colors::dark_backdrop(dominant), 1.0), &all);
            blurred_cover(snapshot, cover, t, w, h);
        }
        Variant::Blobs => {
            snapshot.append_color(&rgba(artwork_colors::dark_backdrop(dominant), 1.0), &all);
            blobs(snapshot, cover, t, w, h, 1.0);
        }
        Variant::Both => {
            snapshot.append_color(&rgba(artwork_colors::dark_backdrop(dominant), 1.0), &all);
            blurred_cover(snapshot, cover, t, w, h);
            blobs(snapshot, cover, t, w, h, 0.55);
        }
    }
    if variant != Variant::Current {
        // Виньетка: края темнее — текст и обложка держатся в центре внимания.
        let center = graphene::Point::new(w / 2.0, h / 2.0);
        snapshot.append_radial_gradient(
            &all,
            &center,
            w * 0.75,
            h * 0.75,
            0.0,
            1.0,
            &[gsk::ColorStop::new(0.55, rgba(0, 0.0)), gsk::ColorStop::new(1.0, rgba(0, 0.3))],
        );
    }
    if palette {
        palette_strip(snapshot, cover, h);
    }
}

fn radial(snapshot: &gtk::Snapshot, all: &graphene::Rect, center: graphene::Point, radius: f32, rgb: u32, stops: &[(f32, f32)]) {
    let stops: Vec<gsk::ColorStop> = stops.iter().map(|&(at, alpha)| gsk::ColorStop::new(at, rgba(rgb, alpha))).collect();
    snapshot.append_radial_gradient(all, &center, radius, radius, 0.0, 1.0, &stops);
}

/// Размытая обложка двумя слоями, вращающимися навстречу. Светлую обложку приглушает матрица
/// цвета: белый текст должен читаться и на белом альбоме.
fn blurred_cover(snapshot: &gtk::Snapshot, cover: &Cover, t: f32, w: f32, h: f32) {
    let k = (0.30 / cover.luma.max(0.01)).clamp(0.30, 1.0) as f32;
    let saturation = 1.6f32;
    // Яркость k и насыщенность: смесь с серым по Rec. 709.
    let (lr, lg, lb) = (0.2126 * (1.0 - saturation), 0.7152 * (1.0 - saturation), 0.0722 * (1.0 - saturation));
    let matrix = graphene::Matrix::from_float([
        k * (lr + saturation),
        k * lr,
        k * lr,
        0.0,
        k * lg,
        k * (lg + saturation),
        k * lg,
        0.0,
        k * lb,
        k * lb,
        k * (lb + saturation),
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ]);
    snapshot.push_color_matrix(&matrix, &graphene::Vec4::zero());
    snapshot.push_blur(32.0);
    let big = w.max(h);
    layer(snapshot, &cover.blurred, w / 2.0, h / 2.0, big * 1.6, t * 2.4, 1.0);
    let (dx, dy) = ((t / 17.0).sin() * w * 0.18, (t / 23.0).cos() * h * 0.15);
    layer(snapshot, &cover.blurred, w / 2.0 + dx, h / 2.0 + dy, big * 1.15, -t * 3.6 + 90.0, 0.7);
    snapshot.pop();
    snapshot.pop();
}

fn layer(snapshot: &gtk::Snapshot, texture: &gdk::Texture, cx: f32, cy: f32, side: f32, degrees: f32, opacity: f64) {
    snapshot.push_opacity(opacity);
    snapshot.save();
    snapshot.translate(&graphene::Point::new(cx, cy));
    snapshot.rotate(degrees);
    snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Linear, &graphene::Rect::new(-side / 2.0, -side / 2.0, side, side));
    snapshot.restore();
    snapshot.pop();
}

/// Пятна цветов обложки: площадь пятна — доля цвета; у преобладающего два пятна (светлее и
/// темнее), чтобы ровная заливка ожила. Каждое плывёт по своей фигуре Лиссажу.
fn blobs(snapshot: &gtk::Snapshot, cover: &Cover, t: f32, w: f32, h: f32, opacity: f64) {
    let all = graphene::Rect::new(0.0, 0.0, w, h);
    let reach = w.hypot(h) * 0.62;
    snapshot.push_opacity(opacity);
    let mut index = 0usize;
    for (rank, color) in cover.shares.iter().enumerate() {
        let tones: &[f32] = if rank == 0 { &[1.25, 0.8] } else { &[1.0] };
        for &tone in tones {
            let phase = index as f32 * 1.7;
            let (px, py) = (19.0 + index as f32 * 5.3, 27.0 + index as f32 * 3.1);
            let cx = w * (0.5 + 0.38 * ((t / px) * std::f32::consts::TAU / 3.0 + phase).sin());
            let cy = h * (0.5 + 0.36 * ((t / py) * std::f32::consts::TAU / 3.0 + phase * 0.7).cos());
            let radius = (reach * (color.share as f32).sqrt()).max(w.max(h) * 0.12);
            let shade = scale_rgb(artwork_colors::dark_backdrop(color.rgb), tone);
            radial(snapshot, &all, graphene::Point::new(cx, cy), radius, shade, &[(0.0, 0.95), (0.5, 0.55), (1.0, 0.0)]);
            index += 1;
        }
    }
    snapshot.pop();
}

fn scale_rgb(rgb: u32, k: f32) -> u32 {
    let c = |shift: u32| ((((rgb >> shift) & 0xFF) as f32 * k).round().clamp(0.0, 255.0) as u32) << shift;
    c(16) | c(8) | c(0)
}

fn palette_strip(snapshot: &gtk::Snapshot, cover: &Cover, h: f32) {
    let (x0, y0, width, height) = (16.0f32, h - 30.0, 260.0f32, 14.0f32);
    let mut x = x0;
    for color in &cover.shares {
        let part = width * color.share as f32;
        snapshot.append_color(&rgba(color.rgb, 1.0), &graphene::Rect::new(x, y0, part, height));
        x += part;
    }
    let border = gsk::RoundedRect::from_rect(graphene::Rect::new(x0, y0, width, height), 0.0);
    snapshot.append_border(&border, &[1.0; 4], &[rgba(0xFFFFFF, 0.6); 4]);
}

// ── окно ──

const LYRICS: [&str; 4] = [
    "Baggage, your passports ready, and follow the green line to customs and then to immigration",
    "BA two-one-five to Rome, Prado, Naples, may I have your attention, please",
    "Customs will be receiving passengers for BA two-one-five to Rome, Prado, Naples",
    "Live for today, gone tomorrow, that's me, ha-ha-ha-a-a-a-a!",
];

struct Parts {
    window: adw::ApplicationWindow,
    variant: Variant,
    backdrop: Backdrop,
    picture: gtk::Picture,
    frame: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    lines: Vec<gtk::Label>,
    sized_for: Cell<(i32, i32)>,
}

impl Parts {
    fn show(&self, cover: &Rc<Cover>) {
        self.backdrop.imp().cover.replace(Some(Rc::clone(cover)));
        self.picture.set_paintable(Some(&cover.texture));
        self.title.set_text(&cover.name);
        self.window.set_title(Some(&format!("{} — {}", self.variant.title(), cover.name)));
        self.backdrop.queue_draw();
    }

    /// Размеры — от высоты окна: обложка и текст сохраняют пропорции при любом окне.
    fn resize(&self) {
        let (w, h) = (self.backdrop.width(), self.backdrop.height());
        if (w, h) == self.sized_for.get() || h <= 0 {
            return;
        }
        self.sized_for.set((w, h));
        let side = ((h as f32) * 0.50).min(w as f32 * 0.38) as i32;
        self.frame.set_size_request(side, side);
        let font = (h as f64 * 0.042).clamp(18.0, 64.0);
        for (index, label) in self.lines.iter().enumerate() {
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrSize::new_size_absolute((font * f64::from(pango::SCALE)) as i32));
            attrs.insert(pango::AttrInt::new_weight(pango::Weight::Bold));
            if index != 1 {
                attrs.insert(pango::AttrInt::new_foreground_alpha((0.42 * 65535.0) as u16));
            }
            label.set_attributes(Some(&attrs));
        }
    }
}

fn build_window(app: &adw::Application, variant: Variant, clock: &Rc<Clock>, cover: &Rc<Cover>) -> Rc<Parts> {
    let backdrop: Backdrop = glib::Object::new();
    backdrop.imp().variant.set(Some(variant));
    backdrop.imp().clock.replace(Some(Rc::clone(clock)));
    backdrop.set_hexpand(true);
    backdrop.set_vexpand(true);

    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).can_shrink(true).build();
    let frame = gtk::Box::builder().overflow(gtk::Overflow::Hidden).css_classes(["lab-cover"]).build();
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    frame.append(&picture);
    let title = gtk::Label::builder().xalign(0.0).css_classes(["title-2"]).build();
    let subtitle = gtk::Label::builder().xalign(0.0).label("Pink Floyd · обложка для примера").css_classes(["dim-label"]).build();
    let left = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    left.append(&frame);
    left.append(&title);
    left.append(&subtitle);

    let right = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).valign(gtk::Align::Center).hexpand(true).build();
    let lines: Vec<gtk::Label> = LYRICS
        .iter()
        .map(|text| {
            let label = gtk::Label::builder().label(*text).wrap(true).xalign(0.0).max_width_chars(28).build();
            right.append(&label);
            label
        })
        .collect();

    let halves = gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(64).margin_start(56).margin_end(56).build();
    left.set_hexpand(true);
    halves.append(&left);
    halves.append(&right);
    halves.set_homogeneous(true);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&backdrop));
    overlay.add_overlay(&halves);

    let header = adw::HeaderBar::new();
    let toolbar =
        adw::ToolbarView::builder().content(&overlay).extend_content_to_top_edge(true).top_bar_style(adw::ToolbarStyle::Flat).build();
    toolbar.add_top_bar(&header);

    let window = adw::ApplicationWindow::builder().application(app).default_width(960).default_height(600).content(&toolbar).build();
    let parts = Rc::new(Parts { window, variant, backdrop, picture, frame, title, subtitle, lines, sized_for: Cell::new((0, 0)) });
    parts.show(cover);
    let _ = &parts.subtitle;
    parts
}

fn shot(window: &adw::ApplicationWindow, path: &std::path::Path) -> Option<()> {
    let (width, height) = (window.width(), window.height());
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let node = snapshot.to_node()?;
    let renderer = window.native()?.renderer()?;
    renderer.render_texture(&node, None).save_to_png(path).ok()
}

fn main() -> glib::ExitCode {
    let folder = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let app = adw::Application::builder().application_id("app.melogold.BackdropLab").build();
    app.connect_activate(move |app| {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let css = gtk::CssProvider::new();
        css.load_from_string(
            ".lab-cover { border-radius: 14px; box-shadow: 0 18px 48px alpha(black, 0.45); }
             label { color: white; }",
        );
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("дисплей"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let mut paths: Vec<_> = std::fs::read_dir(&folder)
            .expect("папка с обложками")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jpg" || x == "png"))
            .collect();
        paths.sort();
        let covers: Rc<Vec<Rc<Cover>>> = Rc::new(paths.iter().filter_map(|p| load_cover(p).map(Rc::new)).collect());
        for cover in covers.iter() {
            let shares: Vec<String> = cover.shares.iter().map(|c| format!("#{:06x} {:.0}%", c.rgb, c.share * 100.0)).collect();
            println!("{:<32} {}", cover.name, shares.join("  "));
        }
        let index = Rc::new(Cell::new(covers.iter().position(|c| c.name.contains("dark side")).unwrap_or(0)));
        let clock = Rc::new(Clock::default());
        let windows: Rc<Vec<Rc<Parts>>> =
            Rc::new(Variant::ALL.iter().map(|&variant| build_window(app, variant, &clock, &covers[index.get()])).collect());

        for parts in windows.iter() {
            let keys = gtk::EventControllerKey::new();
            let (covers, index, windows_all, keys_clock) = (Rc::clone(&covers), Rc::clone(&index), Rc::clone(&windows), Rc::clone(&clock));
            keys.connect_key_pressed(move |_, key, _, _| {
                let step: i64 = match key {
                    gdk::Key::Right => 1,
                    gdk::Key::Left => -1,
                    gdk::Key::space => {
                        keys_clock.paused.set(!keys_clock.paused.get());
                        return glib::Propagation::Stop;
                    }
                    gdk::Key::p | gdk::Key::P => {
                        keys_clock.palette.set(!keys_clock.palette.get());
                        windows_all.iter().for_each(|w| w.backdrop.queue_draw());
                        return glib::Propagation::Stop;
                    }
                    _ => return glib::Propagation::Proceed,
                };
                let next = (index.get() as i64 + step).rem_euclid(covers.len() as i64) as usize;
                index.set(next);
                windows_all.iter().for_each(|w| w.show(&covers[next]));
                glib::Propagation::Stop
            });
            parts.window.add_controller(keys);

            let (this, clock) = (Rc::clone(parts), Rc::clone(&clock));
            parts.backdrop.add_tick_callback(move |_, frame_clock| {
                let now = frame_clock.frame_time();
                // Время двигают первые пришедшие часы кадра; остальные окна берут то же.
                if let Some(last) = clock.last_frame.get() {
                    if now > last {
                        if !clock.paused.get() {
                            clock.seconds.set(clock.seconds.get() + (now - last) as f64 / 1_000_000.0);
                        }
                        clock.last_frame.set(Some(now));
                    }
                } else {
                    clock.last_frame.set(Some(now));
                }
                this.resize();
                this.backdrop.queue_draw();
                glib::ControlFlow::Continue
            });
            parts.window.present();
        }

        // Проверка без человека: BACKDROP_LAB_SHOTS=<папка> — снимки окон по трём обложкам и выход.
        if let Some(folder) = std::env::var_os("BACKDROP_LAB_SHOTS") {
            let folder = std::path::PathBuf::from(folder);
            let (covers, windows, clock, app) = (Rc::clone(&covers), Rc::clone(&windows), Rc::clone(&clock), app.clone());
            let step = Rc::new(Cell::new(0usize));
            glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
                let picks = ["dark side", "invincible", "head full", "call me", "persona"];
                let n = step.get();
                if n > 0 {
                    let name = picks[n - 1].replace(' ', "-");
                    for (i, parts) in windows.iter().enumerate() {
                        let path = folder.join(format!("{name}-{i}.png"));
                        if shot(&parts.window, &path).is_none() {
                            eprintln!("не снялось: {}", path.display());
                        }
                    }
                }
                if n >= picks.len() {
                    app.quit();
                    return glib::ControlFlow::Break;
                }
                if let Some(cover) = covers.iter().find(|c| c.name.contains(picks[n])) {
                    clock.seconds.set(7.0 + n as f64 * 11.0);
                    windows.iter().for_each(|w| w.show(cover));
                }
                step.set(n + 1);
                glib::ControlFlow::Continue
            });
        }
    });
    app.run_with_args::<&str>(&[])
}
