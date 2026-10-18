//! Лаборатория визуализатора «Сейчас играет» (этап 2 после задания 0031): фон — выбранная Максимом
//! размытая обложка, поверх — четыре варианта визуализатора по всему спектру. Звук один на все окна:
//! кнопка ▶ или пробел в любом окне — все варианты играют одновременно под один и тот же трек.
//!
//!     cargo run --release -p melogold-gtk --example visualizer_lab -- <папка с обложками> <кэш Melogold>
//!
//! ←/→ — трек, пробел — играть/пауза. Спектр — элемент GStreamer `spectrum` (1024 полосы, 60 раз в
//! секунду); кадр рисуется по тому измерению, которое сейчас звучит (по времени конвейера), а не по
//! пришедшему последним: иначе картинка опережала бы звук на задержку вывода.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use adw::prelude::*;
use gst::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use melogold_core::artwork_colors::{self, ColorShare};

/// Полосы визуализатора: от 30 Гц до 16 кГц по логарифму, как слышит ухо.
const BANDS: usize = 48;
const FFT_BANDS: u32 = 1024;

const PLAYLIST: [(&str, &str, &str, &str); 8] = [
    ("QtXby3twMmI", "Adventure of a Lifetime", "Coldplay", "head full"),
    ("nbqMIBYJlvk", "Godzilla (feat. Juice WRLD)", "Eminem", "murdered"),
    ("fcTQCNntGEs", "Born On The Bayou", "Creedence Clearwater Revival", "bayou"),
    ("45cYwDMibGo", "Come Together (2019 Mix)", "The Beatles", "abbey"),
    ("HLEC20OYJw0", "NARCISSISTIC PERSONALITY DISORDER", "Odetari", "xiii"),
    ("gv9Ti43Au3w", "Colors Flying High", "Lyn", "persona"),
    ("TDVlDUAIz5k", "Heaven Can Wait", "Michael Jackson", "invincible"),
    ("6UF_qprQiu0", "SPIN", "Kroi", "spin"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Variant {
    Calm,
    Waves,
    Aurora,
    Everything,
}

impl Variant {
    const ALL: [Variant; 4] = [Variant::Calm, Variant::Waves, Variant::Aurora, Variant::Everything];

    fn title(self) -> &'static str {
        match self {
            Variant::Calm => "1 · Спокойный",
            Variant::Waves => "2 · Волны и искры",
            Variant::Aurora => "3 · Сияние",
            Variant::Everything => "4 · Всё сразу",
        }
    }

    /// Насколько фон «дышит» от баса.
    fn breath(self) -> f32 {
        match self {
            Variant::Calm => 0.5,
            Variant::Waves => 1.0,
            Variant::Aurora => 0.8,
            Variant::Everything => 1.3,
        }
    }
}

// ── обложка: то же, что в backdrop_lab (вариант A), плюс яркие цвета для визуализатора ──

pub(crate) struct Cover {
    texture: gdk::Texture,
    blurred: gdk::Texture,
    shares: Vec<ColorShare>,
    luma: f64,
    /// Цвета обложки, поднятые до яркости, на которой они светятся поверх тёмного фона.
    vivid: Vec<u32>,
}

fn load_cover(path: &std::path::Path) -> Option<Cover> {
    let texture = gdk::Texture::from_filename(path).ok()?;
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let shares = artwork_colors::shares(&bytes, width, height, stride);

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

    let mut vivid: Vec<u32> = Vec::new();
    for color in &shares {
        let bright = vivid_of(color.rgb);
        if !vivid.contains(&bright) {
            vivid.push(bright);
        }
    }
    if vivid.len() < 2 {
        vivid.push(0xFFFFFF);
    }
    Some(Cover { texture, blurred, shares, luma, vivid })
}

/// Цвет, который светится поверх тёмного фона: светлота 0,62, насыщенность не меньше 0,7; серый и
/// чёрный — белым.
fn vivid_of(rgb: u32) -> u32 {
    let (r, g, b) = (((rgb >> 16) & 0xFF) as f32 / 255.0, ((rgb >> 8) & 0xFF) as f32 / 255.0, (rgb & 0xFF) as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    let s = if d == 0.0 { 0.0 } else { d / (1.0 - (2.0 * l - 1.0).abs()) };
    if s < 0.12 || d < 0.06 {
        return 0xFFFFFF;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    let (s, l) = (s.max(0.7), 0.62f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f32| (((v + m).clamp(0.0, 1.0)) * 255.0).round() as u32;
    (to(r) << 16) | (to(g) << 8) | to(b)
}

// ── анализ звука ──

pub(crate) struct Analysis {
    bands: [f32; BANDS],
    targets: [f32; BANDS],
    bass: f32,
    mid: f32,
    high: f32,
    loud: f32,
    /// Удар (по резкому росту баса): 1 в момент удара, гаснет за ~0,3 с.
    beat: f32,
    flux_average: f32,
    previous_bass: f32,
    since_beat: f32,
}

impl Default for Analysis {
    fn default() -> Self {
        Analysis {
            bands: [0.0; BANDS],
            targets: [0.0; BANDS],
            bass: 0.0,
            mid: 0.0,
            high: 0.0,
            loud: 0.0,
            beat: 0.0,
            flux_average: 0.0,
            previous_bass: 0.0,
            since_beat: 0.0,
        }
    }
}

impl Analysis {
    /// Новое измерение: магнитуды полос FFT в дБ.
    fn feed(&mut self, magnitudes: &[f32], rate: f32) {
        let bin = rate / 2.0 / magnitudes.len().max(1) as f32;
        for (band, target) in self.targets.iter_mut().enumerate() {
            let lo = 30.0 * (16000.0f32 / 30.0).powf(band as f32 / BANDS as f32);
            let hi = 30.0 * (16000.0f32 / 30.0).powf((band + 1) as f32 / BANDS as f32);
            let (from, to) = ((lo / bin) as usize, ((hi / bin) as usize).max((lo / bin) as usize + 1));
            let power: f32 =
                magnitudes[from.min(magnitudes.len() - 1)..to.min(magnitudes.len())].iter().map(|db| 10f32.powf(db / 10.0)).sum::<f32>()
                    / (to - from).max(1) as f32;
            // Наклон +3 дБ на октаву: у музыки верхи тише низов, без него правая половина спит.
            let tilt = 3.0 * (lo / 30.0).log2();
            *target = ((10.0 * power.max(1e-9).log10() + tilt + 70.0) / 55.0).clamp(0.0, 1.0);
        }
        let mean = |from: f32, to: f32| {
            let picked: Vec<f32> = (0..BANDS)
                .filter(|&b| {
                    let f = 30.0 * (16000.0f32 / 30.0).powf((b as f32 + 0.5) / BANDS as f32);
                    f >= from && f < to
                })
                .map(|b| self.targets[b])
                .collect();
            picked.iter().sum::<f32>() / picked.len().max(1) as f32
        };
        let bass = mean(30.0, 150.0);
        let flux = (bass - self.previous_bass).max(0.0);
        self.previous_bass = bass;
        self.flux_average = self.flux_average * 0.94 + flux * 0.06;
        if flux > self.flux_average * 2.2 + 0.035 && self.since_beat > 0.2 {
            self.beat = 1.0;
            self.since_beat = 0.0;
        }
    }

    /// Шаг сглаживания на кадр: вверх быстро, вниз плавно.
    fn step(&mut self, dt: f32, playing: bool) {
        for (value, target) in self.bands.iter_mut().zip(self.targets.iter_mut()) {
            if !playing {
                *target *= (1.0 - dt * 3.0).max(0.0);
            }
            let speed = if *target > *value { 18.0 } else { 4.0 };
            *value += (*target - *value) * (speed * dt).min(1.0);
        }
        let range = |from: usize, to: usize| self.bands[from..to].iter().sum::<f32>() / (to - from) as f32;
        let follow = |current: f32, target: f32| current + (target - current) * (10.0 * dt).min(1.0);
        self.bass = follow(self.bass, range(0, 10));
        self.mid = follow(self.mid, range(10, 30));
        self.high = follow(self.high, range(30, BANDS));
        self.loud = follow(self.loud, range(0, BANDS));
        self.beat *= (1.0 - dt * 4.5).max(0.0);
        self.since_beat += dt;
    }
}

// ── общий звук и часы ──

pub(crate) struct Shared {
    playbin: gst::Element,
    spectrum: gst::Element,
    queue: RefCell<VecDeque<(u64, Vec<f32>)>>,
    analysis: RefCell<Analysis>,
    playing: Cell<bool>,
    seconds: Cell<f64>,
    last_frame: Cell<Option<i64>>,
    track: Cell<usize>,
}

impl Shared {
    /// Один раз за кадр (кадры всех окон приходят от одних часов — двигает первый).
    fn advance(&self, frame_time: i64) {
        let dt = match self.last_frame.get() {
            Some(last) if frame_time > last => (frame_time - last) as f32 / 1_000_000.0,
            Some(_) => return,
            None => 0.016,
        };
        self.last_frame.set(Some(frame_time));
        if self.playing.get() {
            self.seconds.set(self.seconds.get() + f64::from(dt));
        }
        let running = self.playbin.current_running_time().map(|t| t.nseconds());
        let rate = self
            .spectrum
            .static_pad("sink")
            .and_then(|pad| pad.current_caps())
            .and_then(|caps| caps.structure(0).and_then(|s| s.get::<i32>("rate").ok()))
            .unwrap_or(44_100) as f32;
        let mut analysis = self.analysis.borrow_mut();
        if let Some(running) = running {
            let mut queue = self.queue.borrow_mut();
            while queue.front().is_some_and(|(end, _)| *end <= running) {
                let (_, magnitudes) = queue.pop_front().expect("есть");
                analysis.feed(&magnitudes, rate);
            }
        }
        analysis.step(dt.min(0.05), self.playing.get());
    }
}

// ── виджет ──

#[derive(Clone, Copy)]
struct Sparkle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    life: f32,
    max_life: f32,
    size: f32,
    color: u32,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Visual {
        pub(crate) variant: Cell<Option<Variant>>,
        pub(crate) cover: RefCell<Option<Rc<Cover>>>,
        pub(crate) shared: RefCell<Option<Rc<Shared>>>,
        pub(crate) frame: RefCell<Option<gtk::Widget>>,
        pub(crate) sparkles: RefCell<Vec<Sparkle>>,
        pub(crate) seed: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Visual {
        const NAME: &'static str = "MelogoldLabVisual";
        type Type = super::Visual;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Visual {}

    impl WidgetImpl for Visual {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            let (Some(variant), Some(cover), Some(shared)) =
                (self.variant.get(), self.cover.borrow().clone(), self.shared.borrow().clone())
            else {
                return;
            };
            let cover_rect = self.frame.borrow().as_ref().and_then(|frame| frame.compute_bounds(&*widget));
            let analysis = shared.analysis.borrow();
            super::draw(snapshot, variant, &cover, &analysis, shared.seconds.get() as f32, w, h, cover_rect, &self.sparkles.borrow());
        }
    }
}

glib::wrapper! {
    pub struct Visual(ObjectSubclass<imp::Visual>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Visual {
    fn random(&self) -> f32 {
        let mut x = self.imp().seed.get().max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.imp().seed.set(x);
        (x % 10_000) as f32 / 10_000.0
    }

    /// Искры: рождаются от верхов и ударов, всплывают и гаснут.
    fn step_sparkles(&self, dt: f32, analysis: &Analysis, cover: &Cover) {
        let variant = self.imp().variant.get();
        if !matches!(variant, Some(Variant::Waves | Variant::Aurora | Variant::Everything)) {
            return;
        }
        let (w, h) = (self.width() as f32, self.height() as f32);
        let mut sparkles = self.imp().sparkles.borrow_mut();
        for spark in sparkles.iter_mut() {
            spark.x += spark.vx * dt;
            spark.y += spark.vy * dt;
            spark.life -= dt;
        }
        sparkles.retain(|s| s.life > 0.0);
        let rate = analysis.high.powf(1.6) * 70.0 + analysis.beat * 30.0;
        let mut births = rate * dt;
        while births > 0.0 && sparkles.len() < 220 {
            if births < 1.0 && self.random() > births {
                break;
            }
            births -= 1.0;
            let life = 1.0 + self.random() * 1.8;
            let color = cover.vivid[(self.random() * cover.vivid.len() as f32) as usize % cover.vivid.len()];
            sparkles.push(Sparkle {
                x: self.random() * w,
                y: h * (0.35 + self.random() * 0.65),
                vx: (self.random() - 0.5) * 18.0,
                vy: -(14.0 + self.random() * 46.0),
                life,
                max_life: life,
                size: 1.5 + self.random() * 3.0,
                color,
            });
        }
    }
}

fn rgba(rgb: u32, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(((rgb >> 16) & 0xFF) as f32 / 255.0, ((rgb >> 8) & 0xFF) as f32 / 255.0, (rgb & 0xFF) as f32 / 255.0, alpha)
}

#[allow(clippy::too_many_arguments)]
fn draw(
    snapshot: &gtk::Snapshot,
    variant: Variant,
    cover: &Cover,
    a: &Analysis,
    t: f32,
    w: f32,
    h: f32,
    cover_rect: Option<graphene::Rect>,
    sparkles: &[Sparkle],
) {
    let all = graphene::Rect::new(0.0, 0.0, w, h);
    let dominant = cover.shares.first().map_or(0x101010, |c| c.rgb);
    snapshot.append_color(&rgba(artwork_colors::dark_backdrop(dominant), 1.0), &all);
    let breath = variant.breath();
    blurred_cover(snapshot, cover, t, w, h, a.bass * breath, a.loud * breath);

    match variant {
        Variant::Calm => {
            glow(snapshot, cover_rect, cover, a, 0.8);
            waves(snapshot, cover, a, t, w, h, 1, false, 0.30);
        }
        Variant::Waves => {
            glow(snapshot, cover_rect, cover, a, 1.0);
            waves(snapshot, cover, a, t, w, h, 3, true, 0.5);
            sparks(snapshot, sparkles);
        }
        Variant::Aurora => {
            aurora(snapshot, cover, a, t, w, h, 0.6);
            glow(snapshot, cover_rect, cover, a, 0.9);
            sparks(snapshot, sparkles);
        }
        Variant::Everything => {
            aurora(snapshot, cover, a, t, w, h, 0.32);
            glow(snapshot, cover_rect, cover, a, 1.2);
            waves(snapshot, cover, a, t, w, h, 3, false, 0.45);
            sparks(snapshot, sparkles);
            if a.beat > 0.01 {
                snapshot.append_color(&rgba(cover.vivid[0], 0.07 * a.beat), &all);
            }
        }
    }

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

/// Вариант A из задания 0031, плюс «дыхание»: бас слегка раздувает слои, громкость поднимает яркость.
fn blurred_cover(snapshot: &gtk::Snapshot, cover: &Cover, t: f32, w: f32, h: f32, swell: f32, lift: f32) {
    let k = ((0.30 / cover.luma.max(0.01)).clamp(0.30, 1.0) as f32) * (1.0 + 0.30 * lift);
    let s = 1.6f32;
    let (lr, lg, lb) = (0.2126 * (1.0 - s), 0.7152 * (1.0 - s), 0.0722 * (1.0 - s));
    let matrix = graphene::Matrix::from_float([
        k * (lr + s),
        k * lr,
        k * lr,
        0.0,
        k * lg,
        k * (lg + s),
        k * lg,
        0.0,
        k * lb,
        k * lb,
        k * (lb + s),
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ]);
    snapshot.push_color_matrix(&matrix, &graphene::Vec4::zero());
    snapshot.push_blur(32.0);
    let big = w.max(h) * (1.0 + 0.07 * swell);
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

/// Свечение за обложкой: дышит от баса, вспыхивает на ударе.
fn glow(snapshot: &gtk::Snapshot, rect: Option<graphene::Rect>, cover: &Cover, a: &Analysis, strength: f32) {
    let Some(rect) = rect else { return };
    let center = rect.center();
    let radius = rect.width().max(rect.height()) * (0.85 + 0.25 * a.bass + 0.1 * a.beat);
    let bounds = graphene::Rect::new(center.x() - radius, center.y() - radius, radius * 2.0, radius * 2.0);
    let alpha = ((0.12 + 0.45 * a.bass + 0.25 * a.beat) * strength).min(0.85);
    snapshot.append_radial_gradient(
        &bounds,
        &center,
        radius,
        radius,
        0.0,
        1.0,
        &[gsk::ColorStop::new(0.35, rgba(cover.vivid[0], alpha)), gsk::ColorStop::new(1.0, rgba(cover.vivid[0], 0.0))],
    );
}

/// Волны спектра снизу (и отражением сверху): несколько слоёв цветов обложки, каждый — своя часть
/// спектра со сдвигом, края сглажены кривыми.
#[allow(clippy::too_many_arguments)]
fn waves(snapshot: &gtk::Snapshot, cover: &Cover, a: &Analysis, t: f32, w: f32, h: f32, layers: usize, mirrored: bool, alpha: f32) {
    for flip in [false, true] {
        if flip && !mirrored {
            continue;
        }
        for layer in (0..layers).rev() {
            let color = cover.vivid[layer % cover.vivid.len()];
            let scale = 1.0 - layer as f32 * 0.2;
            let points: Vec<(f32, f32)> = (0..BANDS)
                .map(|j| {
                    let source = (j + layer * 5) % BANDS;
                    let value = a.bands[source].powf(1.4);
                    let ripple = ((t * 0.9 + j as f32 * 0.33 + layer as f32 * 1.7).sin() * 0.5 + 0.5) * 0.02;
                    let amplitude = (value * 0.30 * scale + ripple) * h * if flip { 0.5 } else { 1.0 };
                    (w * j as f32 / (BANDS - 1) as f32, amplitude)
                })
                .collect();
            let y = |amplitude: f32| if flip { amplitude } else { h - amplitude };
            let builder = gsk::PathBuilder::new();
            builder.move_to(0.0, if flip { 0.0 } else { h });
            builder.line_to(0.0, y(points[0].1));
            for pair in points.windows(2) {
                let ((x0, a0), (x1, a1)) = (pair[0], pair[1]);
                let mid = (x0 + x1) / 2.0;
                builder.cubic_to(mid, y(a0), mid, y(a1), x1, y(a1));
            }
            builder.line_to(w, if flip { 0.0 } else { h });
            builder.close();
            let path = builder.to_path();
            snapshot.push_fill(&path, gsk::FillRule::Winding);
            let top = if flip { 0.0 } else { h * 0.45 };
            let alpha = alpha * if flip { 0.45 } else { 1.0 };
            let (start, end) = if flip {
                (graphene::Point::new(0.0, 0.0), graphene::Point::new(0.0, h * 0.55))
            } else {
                (graphene::Point::new(0.0, h), graphene::Point::new(0.0, h * 0.45))
            };
            snapshot.append_linear_gradient(
                &graphene::Rect::new(0.0, top, w, h * 0.55),
                &start,
                &end,
                &[gsk::ColorStop::new(0.0, rgba(color, alpha)), gsk::ColorStop::new(1.0, rgba(color, 0.0))],
            );
            snapshot.pop();
        }
    }
}

/// Сияние: вертикальные занавеси света на весь экран, высота — своя полоса спектра, сильно размыто.
fn aurora(snapshot: &gtk::Snapshot, cover: &Cover, a: &Analysis, t: f32, w: f32, h: f32, alpha: f32) {
    const CURTAINS: usize = 24;
    snapshot.push_blur(26.0);
    for k in 0..CURTAINS {
        let value = a.bands[(k * BANDS / CURTAINS + 1).min(BANDS - 1)];
        let color = cover.vivid[k % cover.vivid.len()];
        let x = w * (k as f32 + 0.5) / CURTAINS as f32 + (t * 0.35 + k as f32).sin() * w * 0.012;
        let width = w / CURTAINS as f32 * 1.7;
        let height = h * (0.18 + 0.72 * value.powf(1.2));
        let rect = graphene::Rect::new(x - width / 2.0, h - height, width, height);
        snapshot.append_linear_gradient(
            &rect,
            &graphene::Point::new(0.0, h),
            &graphene::Point::new(0.0, h - height),
            &[
                gsk::ColorStop::new(0.0, rgba(color, 0.0)),
                gsk::ColorStop::new(0.35, rgba(color, alpha * (0.4 + 0.6 * value))),
                gsk::ColorStop::new(1.0, rgba(color, 0.0)),
            ],
        );
    }
    snapshot.pop();
}

fn sparks(snapshot: &gtk::Snapshot, sparkles: &[Sparkle]) {
    for spark in sparkles {
        let fade = (spark.life / spark.max_life).clamp(0.0, 1.0);
        let alpha = (fade * (1.0 - fade) * 4.0).min(1.0) * 0.9;
        let radius = spark.size * 3.0;
        let center = graphene::Point::new(spark.x, spark.y);
        let bounds = graphene::Rect::new(spark.x - radius, spark.y - radius, radius * 2.0, radius * 2.0);
        snapshot.append_radial_gradient(
            &bounds,
            &center,
            radius,
            radius,
            0.0,
            1.0,
            &[
                gsk::ColorStop::new(0.0, rgba(0xFFFFFF, alpha)),
                gsk::ColorStop::new(0.25, rgba(spark.color, alpha * 0.8)),
                gsk::ColorStop::new(1.0, rgba(spark.color, 0.0)),
            ],
        );
    }
}

// ── окна ──

const LYRICS: [&str; 4] = [
    "Строка, которая уже прозвучала, остаётся видна выше",
    "Сейчас звучит вот эта строка — она самая яркая",
    "Следующая строка ждёт своей очереди внизу",
    "И ещё одна, чтобы было видно, как читается текст",
];

struct Parts {
    window: adw::ApplicationWindow,
    variant: Variant,
    visual: Visual,
    picture: gtk::Picture,
    frame: gtk::Box,
    title: gtk::Label,
    subtitle: gtk::Label,
    play: gtk::Button,
    lines: Vec<gtk::Label>,
    sized_for: Cell<(i32, i32)>,
}

impl Parts {
    fn show(&self, cover: &Rc<Cover>, track: &(&str, &str, &str, &str)) {
        self.visual.imp().cover.replace(Some(Rc::clone(cover)));
        self.picture.set_paintable(Some(&cover.texture));
        self.title.set_text(track.1);
        self.subtitle.set_text(track.2);
        self.window.set_title(Some(&format!("{} — {}", self.variant.title(), track.1)));
    }

    fn set_playing(&self, playing: bool) {
        self.play.set_icon_name(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
    }

    /// Размеры — от высоты окна (задание 0031): обложка и текст в одних пропорциях при любом окне.
    fn resize(&self) {
        let (w, h) = (self.visual.width(), self.visual.height());
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

fn build_window(app: &adw::Application, variant: Variant, shared: &Rc<Shared>) -> Rc<Parts> {
    let visual: Visual = glib::Object::new();
    visual.imp().variant.set(Some(variant));
    visual.imp().shared.replace(Some(Rc::clone(shared)));
    visual.imp().seed.set(0x9E37_79B9 ^ ((variant as u32 + 1) * 0x85EB_CA6B));
    visual.set_hexpand(true);
    visual.set_vexpand(true);

    let picture = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).can_shrink(true).hexpand(true).vexpand(true).build();
    let frame = gtk::Box::builder().overflow(gtk::Overflow::Hidden).css_classes(["lab-cover"]).build();
    frame.append(&picture);
    visual.imp().frame.replace(Some(frame.clone().upcast()));
    let title = gtk::Label::builder().xalign(0.0).css_classes(["title-2"]).ellipsize(pango::EllipsizeMode::End).build();
    let subtitle = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).build();
    let left = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .hexpand(true)
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

    let halves =
        gtk::Box::builder().orientation(gtk::Orientation::Horizontal).spacing(64).margin_start(56).margin_end(56).homogeneous(true).build();
    halves.append(&left);
    halves.append(&right);

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&visual));
    overlay.add_overlay(&halves);

    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    play.add_css_class("circular");
    play.add_css_class("suggested-action");
    play.set_tooltip_text(Some("Играть во всех окнах (пробел)"));
    let header = adw::HeaderBar::new();
    header.pack_start(&play);
    let toolbar =
        adw::ToolbarView::builder().content(&overlay).extend_content_to_top_edge(true).top_bar_style(adw::ToolbarStyle::Flat).build();
    toolbar.add_top_bar(&header);

    let window = adw::ApplicationWindow::builder().application(app).default_width(960).default_height(600).content(&toolbar).build();
    Rc::new(Parts { window, variant, visual, picture, frame, title, subtitle, play, lines, sized_for: Cell::new((0, 0)) })
}

fn main() -> glib::ExitCode {
    let mut args = std::env::args().skip(1);
    let covers_dir = args.next().unwrap_or_else(|| ".".into());
    let cache_dir = args.next().unwrap_or_else(|| format!("{}/.local/share/melogold/cache", std::env::var("HOME").unwrap_or_default()));
    gst::init().expect("GStreamer");

    let app = adw::Application::builder().application_id("app.melogold.VisualizerLab").build();
    app.connect_activate(move |app| {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let css = gtk::CssProvider::new();
        css.load_from_string(".lab-cover { border-radius: 14px; box-shadow: 0 18px 48px alpha(black, 0.45); } label { color: white; }");
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("дисплей"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let files: Vec<std::path::PathBuf> = std::fs::read_dir(&covers_dir)
            .expect("папка с обложками")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
            .collect();
        let covers: Rc<Vec<Rc<Cover>>> = Rc::new(
            PLAYLIST
                .iter()
                .map(|(_, _, _, hint)| {
                    let key = hint.replace(' ', "-");
                    let path = files.iter().find(|p| p.to_string_lossy().contains(&key)).cloned().unwrap_or_else(|| files[0].clone());
                    Rc::new(load_cover(&path).expect("обложка"))
                })
                .collect(),
        );

        let spectrum = gst::ElementFactory::make("spectrum")
            .property("bands", FFT_BANDS)
            .property("interval", 16_666_667u64)
            .property("threshold", -90i32)
            .property("post-messages", true)
            .property("message-magnitude", true)
            .build()
            .expect("spectrum");
        let playbin = gst::ElementFactory::make("playbin").build().expect("playbin");
        playbin.set_property("audio-filter", &spectrum);
        playbin.set_property("volume", 0.8f64);
        let shared = Rc::new(Shared {
            playbin: playbin.clone(),
            spectrum,
            queue: RefCell::new(VecDeque::new()),
            analysis: RefCell::new(Analysis::default()),
            playing: Cell::new(false),
            seconds: Cell::new(0.0),
            last_frame: Cell::new(None),
            track: Cell::new(0),
        });
        let windows: Rc<Vec<Rc<Parts>>> = Rc::new(Variant::ALL.iter().map(|&variant| build_window(app, variant, &shared)).collect());

        let load = {
            let (shared, covers, windows, cache_dir) = (Rc::clone(&shared), Rc::clone(&covers), Rc::clone(&windows), cache_dir.clone());
            Rc::new(move |index: usize| {
                shared.track.set(index);
                let track = &PLAYLIST[index];
                let _ = shared.playbin.set_state(gst::State::Null);
                shared.queue.borrow_mut().clear();
                shared.playbin.set_property("uri", format!("file://{cache_dir}/{}.140.data", track.0));
                let _ = shared.playbin.set_state(if shared.playing.get() { gst::State::Playing } else { gst::State::Paused });
                windows.iter().for_each(|w| w.show(&covers[index], track));
            })
        };
        load(0);

        let toggle = {
            let (shared, windows) = (Rc::clone(&shared), Rc::clone(&windows));
            Rc::new(move || {
                let playing = !shared.playing.get();
                shared.playing.set(playing);
                let _ = shared.playbin.set_state(if playing { gst::State::Playing } else { gst::State::Paused });
                windows.iter().for_each(|w| w.set_playing(playing));
            })
        };

        // Сообщения спектра — в очередь по времени конца измерения; конец трека — следующий трек.
        let bus = playbin.bus().expect("шина");
        let watch = {
            let (shared, load) = (Rc::clone(&shared), Rc::clone(&load));
            bus.add_watch_local(move |_, message| {
                match message.view() {
                    gst::MessageView::Element(element) => {
                        if let Some(s) = element.structure().filter(|s| s.name() == "spectrum") {
                            let end = s.get::<u64>("endtime").unwrap_or(0);
                            if let Ok(list) = s.get::<gst::List>("magnitude") {
                                let magnitudes: Vec<f32> = list.iter().filter_map(|v| v.get::<f32>().ok()).collect();
                                let mut queue = shared.queue.borrow_mut();
                                queue.push_back((end, magnitudes));
                                while queue.len() > 240 {
                                    queue.pop_front();
                                }
                            }
                        }
                    }
                    gst::MessageView::Eos(_) => load((shared.track.get() + 1) % PLAYLIST.len()),
                    gst::MessageView::Error(error) => eprintln!("GStreamer: {}", error.error()),
                    _ => {}
                }
                glib::ControlFlow::Continue
            })
            .expect("наблюдение шины")
        };
        std::mem::forget(watch);

        for parts in windows.iter() {
            let toggle_click = Rc::clone(&toggle);
            parts.play.connect_clicked(move |_| toggle_click());

            let keys = gtk::EventControllerKey::new();
            let (toggle_key, load_key, shared_key) = (Rc::clone(&toggle), Rc::clone(&load), Rc::clone(&shared));
            keys.connect_key_pressed(move |_, key, _, _| {
                let step: i64 = match key {
                    gdk::Key::Right => 1,
                    gdk::Key::Left => -1,
                    gdk::Key::space => {
                        toggle_key();
                        return glib::Propagation::Stop;
                    }
                    _ => return glib::Propagation::Proceed,
                };
                load_key((shared_key.track.get() as i64 + step).rem_euclid(PLAYLIST.len() as i64) as usize);
                glib::Propagation::Stop
            });
            parts.window.add_controller(keys);

            let (this, shared_tick, covers_tick) = (Rc::clone(parts), Rc::clone(&shared), Rc::clone(&covers));
            parts.visual.add_tick_callback(move |_, clock| {
                let now = clock.frame_time();
                let last = this.visual.imp().seed.get();
                let _ = last;
                shared_tick.advance(now);
                let cover = Rc::clone(&covers_tick[shared_tick.track.get()]);
                this.visual.step_sparkles(1.0 / 60.0, &shared_tick.analysis.borrow(), &cover);
                this.resize();
                this.visual.queue_draw();
                glib::ControlFlow::Continue
            });
            parts.window.present();
        }
    });
    app.run_with_args::<&str>(&[])
}
