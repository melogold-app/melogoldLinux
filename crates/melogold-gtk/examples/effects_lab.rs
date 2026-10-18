//! Лаборатория эффектов фона «Сейчас играет», второй прогон: восемь вариантов того, как сама
//! размытая обложка (задание 0031) живёт под музыку — дышит, течёт, расходится каплями и кругами, а
//! её размытие то собирается, то расплывается в такт. Одно окно на весь экран, восемь плиток 4 × 2,
//! один звук на все.
//!
//!     cargo run --release -p melogold-gtk --example effects_lab -- <папка с обложками> [кэш Melogold]
//!
//! Пробел или ▶ — играть, ←/→ — трек. Размытие здесь не считается на видеокарте каждый кадр: у каждой
//! обложки заранее три степени размытия (мягкая, средняя, сильная), и «сила размытия» — их смесь. Так
//! кадр дешёвый, и частота кадров идёт за частотой монитора (счётчик FPS — в заголовке).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use adw::prelude::*;
use gst::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use melogold_core::artwork_colors;

const BANDS: usize = 48;

const PLAYLIST: [(&str, &str, &str, &str); 8] = [
    ("QtXby3twMmI", "Adventure of a Lifetime", "Coldplay", "head-full"),
    ("nbqMIBYJlvk", "Godzilla (feat. Juice WRLD)", "Eminem", "murdered"),
    ("fcTQCNntGEs", "Born On The Bayou", "Creedence Clearwater Revival", "bayou"),
    ("45cYwDMibGo", "Come Together (2019 Mix)", "The Beatles", "abbey"),
    ("HLEC20OYJw0", "NARCISSISTIC PERSONALITY DISORDER", "Odetari", "xiii"),
    ("gv9Ti43Au3w", "Colors Flying High", "Lyn", "persona"),
    ("TDVlDUAIz5k", "Heaven Can Wait", "Michael Jackson", "invincible"),
    ("6UF_qprQiu0", "SPIN", "Kroi", "spin"),
];

const VARIANTS: [&str; 8] = [
    "1 · Дыхание — бас раздувает обложку",
    "2 · Пульс размытия — удар делает её резче",
    "3 · Капли — удары роняют капли цвета",
    "4 · Круги на воде — от обложки расходятся круги",
    "5 · Течение — громче играет, быстрее течёт",
    "6 · Слои по частотам — у каждой полосы свой слой",
    "7 · Свет — яркость качает в такт",
    "8 · Всё вместе",
];

// ── обложка: три степени размытия ──

struct Cover {
    texture: gdk::Texture,
    /// Мягкое, среднее и сильное размытие одной обложки, 48 × 48.
    levels: [gdk::Texture; 3],
    luma: f64,
    base: u32,
    /// Цвета обложки, светящиеся поверх тёмного фона (для капель и кругов).
    vivid: Vec<u32>,
}

fn box_blur(cells: &mut [[f64; 3]], side: usize, radius: i32, passes: usize) {
    let n = f64::from((2 * radius + 1) * (2 * radius + 1));
    for _ in 0..passes {
        let copy = cells.to_vec();
        for y in 0..side {
            for x in 0..side {
                let mut sum = [0f64; 3];
                for dy in -radius..=radius {
                    for dx in -radius..=radius {
                        let sx = (x as i32 + dx).clamp(0, side as i32 - 1) as usize;
                        let sy = (y as i32 + dy).clamp(0, side as i32 - 1) as usize;
                        for c in 0..3 {
                            sum[c] += copy[sy * side + sx][c];
                        }
                    }
                }
                cells[y * side + x] = sum.map(|v| v / n);
            }
        }
    }
}

fn texture_of(cells: &[[f64; 3]], side: usize) -> gdk::Texture {
    let rgba: Vec<u8> = cells.iter().flat_map(|p| [p[0] as u8, p[1] as u8, p[2] as u8, 255]).collect();
    gdk::MemoryTexture::new(side as i32, side as i32, gdk::MemoryFormat::R8g8b8a8, &glib::Bytes::from_owned(rgba), side * 4).upcast()
}

fn load_cover(path: &std::path::Path) -> Option<Cover> {
    let texture = gdk::Texture::from_filename(path).ok()?;
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let preview = artwork_colors::backdrop_preview(&bytes, width, height, stride)?;
    let shares = artwork_colors::shares(&bytes, width, height, stride);

    const SIDE: usize = 48;
    let mut cells = vec![[0f64; 3]; SIDE * SIDE];
    for (index, cell) in cells.iter_mut().enumerate() {
        let (cx, cy) = (index % SIDE, index / SIDE);
        let (x0, y0) = (cx * width / SIDE, cy * height / SIDE);
        let (x1, y1) = (((cx + 1) * width / SIDE).max(x0 + 1).min(width), ((cy + 1) * height / SIDE).max(y0 + 1).min(height));
        let mut n = 0.0f64;
        for y in y0..y1 {
            for x in x0..x1 {
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
    }
    let mut soft = cells.clone();
    box_blur(&mut soft, SIDE, 1, 2);
    let mut medium = cells.clone();
    box_blur(&mut medium, SIDE, 2, 4);
    let mut heavy = cells;
    box_blur(&mut heavy, SIDE, 4, 6);
    let levels = [texture_of(&soft, SIDE), texture_of(&medium, SIDE), texture_of(&heavy, SIDE)];

    let mut vivid: Vec<u32> = shares.iter().map(|c| vivid_of(c.rgb)).collect();
    vivid.dedup();
    if vivid.len() < 2 {
        vivid.push(0xFFFFFF);
    }
    Some(Cover { texture, levels, luma: preview.luma, base: preview.base, vivid })
}

fn vivid_of(rgb: u32) -> u32 {
    let (r, g, b) = (((rgb >> 16) & 0xFF) as f32 / 255.0, ((rgb >> 8) & 0xFF) as f32 / 255.0, (rgb & 0xFF) as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let d = max - min;
    let l0 = (max + min) / 2.0;
    let s0 = if d == 0.0 { 0.0 } else { d / (1.0 - (2.0 * l0 - 1.0).abs()) };
    if s0 < 0.12 || d < 0.06 {
        return 0xFFFFFF;
    }
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    let (s, l) = (s0.max(0.7), 0.62f32);
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

// ── звук ──

struct Analysis {
    bands: [f32; BANDS],
    targets: [f32; BANDS],
    bass: f32,
    mid: f32,
    high: f32,
    loud: f32,
    beat: f32,
    beats: u64,
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
            beats: 0,
            flux_average: 0.0,
            previous_bass: 0.0,
            since_beat: 1.0,
        }
    }
}

impl Analysis {
    fn feed(&mut self, magnitudes: &[f32], rate: f32) {
        if magnitudes.is_empty() {
            return;
        }
        let bin = rate / 2.0 / magnitudes.len() as f32;
        for (band, target) in self.targets.iter_mut().enumerate() {
            let lo = 30.0 * (16000.0f32 / 30.0).powf(band as f32 / BANDS as f32);
            let hi = 30.0 * (16000.0f32 / 30.0).powf((band + 1) as f32 / BANDS as f32);
            let from = ((lo / bin) as usize).min(magnitudes.len() - 1);
            let to = ((hi / bin) as usize).max(from + 1).min(magnitudes.len());
            let power = magnitudes[from..to].iter().map(|db| 10f32.powf(db / 10.0)).sum::<f32>() / (to - from) as f32;
            let tilt = 3.0 * (lo / 30.0).log2();
            *target = ((10.0 * power.max(1e-9).log10() + tilt + 70.0) / 55.0).clamp(0.0, 1.0);
        }
        let bass = self.targets[..10].iter().sum::<f32>() / 10.0;
        let flux = (bass - self.previous_bass).max(0.0);
        self.previous_bass = bass;
        self.flux_average = self.flux_average * 0.94 + flux * 0.06;
        if flux > self.flux_average * 2.2 + 0.035 && self.since_beat > 0.2 {
            self.beat = 1.0;
            self.beats += 1;
            self.since_beat = 0.0;
        }
    }

    fn step(&mut self, dt: f32, playing: bool) {
        for (value, target) in self.bands.iter_mut().zip(self.targets.iter_mut()) {
            if !playing {
                *target *= (1.0 - dt * 3.0).max(0.0);
            }
            let speed = if *target > *value { 18.0 } else { 4.0 };
            *value += (*target - *value) * (speed * dt).min(1.0);
        }
        let range = |from: usize, to: usize| self.bands[from..to].iter().sum::<f32>() / (to - from) as f32;
        let follow = |current: f32, target: f32| current + (target - current) * (12.0 * dt).min(1.0);
        self.bass = follow(self.bass, range(0, 10));
        self.mid = follow(self.mid, range(10, 30));
        self.high = follow(self.high, range(30, BANDS));
        self.loud = follow(self.loud, range(0, BANDS));
        self.beat *= (1.0 - dt * 4.0).max(0.0);
        self.since_beat += dt;
    }
}

/// Капля или круг: рождается на ударе, растёт и гаснет.
#[derive(Clone, Copy)]
struct Splash {
    x: f32,
    y: f32,
    age: f32,
    life: f32,
    size: f32,
    color: u32,
}

/// Всё живое одной плитки: своё время (у «Течения» оно ускоряется), капли и круги.
#[derive(Default)]
struct TileState {
    time: f32,
    drops: Vec<Splash>,
    rings: Vec<Splash>,
    seen_beats: u64,
    seed: u32,
}

impl TileState {
    fn random(&mut self) -> f32 {
        let mut x = self.seed.max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.seed = x;
        (x % 10_000) as f32 / 10_000.0
    }
}

struct Shared {
    playbin: gst::Element,
    spectrum: gst::Element,
    queue: RefCell<VecDeque<(u64, Vec<f32>)>>,
    analysis: RefCell<Analysis>,
    playing: Cell<bool>,
    track: Cell<usize>,
    covers: Vec<Rc<Cover>>,
    tiles: RefCell<Vec<TileState>>,
    last_frame: Cell<Option<i64>>,
    frames: RefCell<VecDeque<i64>>,
}

impl Shared {
    fn advance(&self, now: i64) {
        let dt = self.last_frame.replace(Some(now)).map_or(1.0 / 60.0, |last| ((now - last).max(0) as f32 / 1e6).min(0.1));
        {
            let mut frames = self.frames.borrow_mut();
            frames.push_back(now);
            while frames.front().is_some_and(|&f| now - f > 1_000_000) {
                frames.pop_front();
            }
        }
        let rate = self
            .spectrum
            .static_pad("sink")
            .and_then(|pad| pad.current_caps())
            .and_then(|caps| caps.structure(0).and_then(|s| s.get::<i32>("rate").ok()))
            .unwrap_or(44_100) as f32;
        let mut analysis = self.analysis.borrow_mut();
        if let Some(running) = self.playbin.current_running_time().map(|t| t.nseconds()) {
            let mut queue = self.queue.borrow_mut();
            while queue.front().is_some_and(|(end, _)| *end <= running) {
                let (_, magnitudes) = queue.pop_front().expect("есть");
                analysis.feed(&magnitudes, rate);
            }
        }
        analysis.step(dt, self.playing.get());
        let playing = self.playing.get();
        let cover = &self.covers[self.track.get()];
        for (index, tile) in self.tiles.borrow_mut().iter_mut().enumerate() {
            let speed = if index == 4 || index == 7 { 0.4 + 3.2 * analysis.loud * analysis.loud } else { 1.0 };
            if playing {
                tile.time += dt * speed;
            }
            let new_beat = analysis.beats != tile.seen_beats;
            tile.seen_beats = analysis.beats;
            if new_beat && matches!(index, 2 | 7) {
                for _ in 0..if index == 7 { 2 } else { 3 } {
                    let color = cover.vivid[(tile.random() * cover.vivid.len() as f32) as usize % cover.vivid.len()];
                    let (x, y, size) = (tile.random(), tile.random(), 0.18 + tile.random() * 0.22 * (0.5 + analysis.bass));
                    let life = 1.6 + tile.random();
                    tile.drops.push(Splash { x, y, age: 0.0, life, size, color });
                }
            }
            if new_beat && matches!(index, 3 | 7) {
                tile.rings.push(Splash { x: 0.3, y: 0.5, age: 0.0, life: 1.8, size: 0.6 + analysis.bass, color: cover.vivid[0] });
            }
            for splash in tile.drops.iter_mut().chain(tile.rings.iter_mut()) {
                splash.age += dt;
            }
            tile.drops.retain(|s| s.age < s.life);
            tile.rings.retain(|s| s.age < s.life);
        }
    }

    fn fps(&self) -> usize {
        self.frames.borrow().len()
    }
}

// ── плитка ──

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Tile {
        pub(crate) index: Cell<usize>,
        pub(crate) shared: RefCell<Option<Rc<Shared>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Tile {
        const NAME: &'static str = "MelogoldEffectsTile";
        type Type = super::Tile;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Tile {}

    impl WidgetImpl for Tile {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let Some(shared) = self.shared.borrow().clone() else { return };
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            let index = self.index.get();
            let cover = Rc::clone(&shared.covers[shared.track.get()]);
            let analysis = shared.analysis.borrow();
            let tiles = shared.tiles.borrow();
            super::draw_tile(snapshot, &widget, index, &cover, &analysis, &tiles[index], w, h);
        }
    }
}

glib::wrapper! {
    pub struct Tile(ObjectSubclass<imp::Tile>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

fn rgba(rgb: u32, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(((rgb >> 16) & 0xFF) as f32 / 255.0, ((rgb >> 8) & 0xFF) as f32 / 255.0, (rgb & 0xFF) as f32 / 255.0, alpha)
}

fn color_matrix(k: f32, saturation: f32) -> graphene::Matrix {
    let s = saturation;
    let (lr, lg, lb) = (0.2126 * (1.0 - s), 0.7152 * (1.0 - s), 0.0722 * (1.0 - s));
    #[rustfmt::skip]
    let m = graphene::Matrix::from_float([
        k * (lr + s), k * lr, k * lr, 0.0,
        k * lg, k * (lg + s), k * lg, 0.0,
        k * lb, k * lb, k * (lb + s), 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]);
    m
}

fn turn(snapshot: &gtk::Snapshot, texture: &gdk::Texture, cx: f32, cy: f32, side: f32, degrees: f32, opacity: f64) {
    if opacity <= 0.001 {
        return;
    }
    snapshot.push_opacity(opacity.min(1.0));
    snapshot.save();
    snapshot.translate(&graphene::Point::new(cx, cy));
    snapshot.rotate(degrees);
    snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Linear, &graphene::Rect::new(-side / 2.0, -side / 2.0, side, side));
    snapshot.restore();
    snapshot.pop();
}

/// Два вращающихся слоя (задание 0031), с заданной смесью размытия: `sharp` 0 — сильное, 1 — мягкое.
#[allow(clippy::too_many_arguments)]
fn cover_layers(snapshot: &gtk::Snapshot, cover: &Cover, t: f32, w: f32, h: f32, swell: f32, sharp: f32, spin: f32) {
    let big = w.max(h) * (1.0 + swell);
    let (dx, dy) = ((t / 17.0).sin() * w * 0.18, (t / 23.0).cos() * h * 0.15);
    let layers =
        [(w / 2.0, h / 2.0, big * 1.6, t * 2.4 * spin, 1.0f64), (w / 2.0 + dx, h / 2.0 + dy, big * 1.15, 90.0 - t * 3.6 * spin, 0.7)];
    for (cx, cy, side, degrees, opacity) in layers {
        turn(snapshot, &cover.levels[2], cx, cy, side, degrees, opacity);
        // Резче — поверх сильного размытия проступают среднее и мягкое.
        let medium = (sharp * 2.0).min(1.0);
        let soft = (sharp * 2.0 - 1.0).max(0.0);
        turn(snapshot, &cover.levels[1], cx, cy, side, degrees, opacity * f64::from(medium));
        turn(snapshot, &cover.levels[0], cx, cy, side, degrees, opacity * f64::from(soft));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tile(snapshot: &gtk::Snapshot, widget: &Tile, index: usize, cover: &Cover, a: &Analysis, tile: &TileState, w: f32, h: f32) {
    let all = graphene::Rect::new(0.0, 0.0, w, h);
    snapshot.push_clip(&all);
    snapshot.append_color(&rgba(cover.base, 1.0), &all);
    let k = (0.30 / cover.luma.max(0.01)).clamp(0.30, 1.0) as f32;
    let t = tile.time;

    // Параметры слоёв по варианту.
    let (mut swell, mut sharp, mut light, mut spin) = (0.0f32, 0.0f32, 0.0f32, 1.0f32);
    match index {
        0 => swell = 0.14 * a.bass + 0.05 * a.beat,
        1 => sharp = (0.85 * a.beat + 0.35 * a.bass).min(1.0),
        2 | 3 => swell = 0.04 * a.bass,
        4 => spin = 1.0,
        6 => light = 0.7 * a.loud + 0.5 * a.beat,
        7 => {
            swell = 0.10 * a.bass + 0.04 * a.beat;
            sharp = (0.6 * a.beat + 0.2 * a.bass).min(1.0);
            light = 0.25 * a.loud;
        }
        _ => {}
    }

    snapshot.push_color_matrix(&color_matrix(k * (1.0 + light), 1.6 + 0.4 * light), &graphene::Vec4::zero());
    if index == 5 {
        // Слои по частотам: бас — большой медленный, середина — средний, верха — маленькие быстрые.
        let groups = [(0usize, 10usize, 1.7f32, 1.0f32), (10, 22, 1.25, 1.8), (22, 34, 0.9, 2.8), (34, BANDS, 0.6, 4.2)];
        for (n, (from, to, size, speed)) in groups.into_iter().enumerate() {
            let energy = a.bands[from..to].iter().sum::<f32>() / (to - from) as f32;
            let side = w.max(h) * size * (1.0 + 0.25 * energy);
            let (cx, cy) =
                (w * (0.5 + 0.2 * (t * 0.07 * speed + n as f32).sin()), h * (0.5 + 0.2 * (t * 0.05 * speed + n as f32 * 1.3).cos()));
            let level = if n < 2 { 2 } else { 1 };
            turn(
                snapshot,
                &cover.levels[level],
                cx,
                cy,
                side,
                t * 2.0 * speed * if n % 2 == 0 { 1.0 } else { -1.0 },
                f64::from(0.45 + 0.55 * energy),
            );
        }
    } else {
        cover_layers(snapshot, cover, t, w, h, swell, sharp, spin);
    }
    // Капли: пятно цвета обложки растёт и растекается под тем же размытием.
    for drop in &tile.drops {
        let p = drop.age / drop.life;
        let radius = w.max(h) * drop.size * (0.3 + 0.7 * (1.0 - (1.0 - p).powi(3)));
        let alpha = 0.75 * (1.0 - p).powf(1.5);
        let center = graphene::Point::new(drop.x * w, drop.y * h);
        let bounds = graphene::Rect::new(center.x() - radius, center.y() - radius, radius * 2.0, radius * 2.0);
        snapshot.append_radial_gradient(
            &bounds,
            &center,
            radius,
            radius,
            0.0,
            1.0,
            &[
                gsk::ColorStop::new(0.0, rgba(drop.color, alpha)),
                gsk::ColorStop::new(0.55, rgba(drop.color, alpha * 0.45)),
                gsk::ColorStop::new(1.0, rgba(drop.color, 0.0)),
            ],
        );
    }
    snapshot.pop();

    // Круги на воде: светлое кольцо расходится от обложки.
    for ring in &tile.rings {
        let p = ring.age / ring.life;
        let radius = w.max(h) * (0.08 + 0.9 * p) * (0.8 + 0.4 * ring.size);
        let alpha = 0.32 * (1.0 - p).powi(2);
        let center = graphene::Point::new(ring.x * w, ring.y * h);
        let bounds = graphene::Rect::new(center.x() - radius, center.y() - radius, radius * 2.0, radius * 2.0);
        snapshot.append_radial_gradient(
            &bounds,
            &center,
            radius,
            radius,
            0.0,
            1.0,
            &[
                gsk::ColorStop::new(0.70, rgba(ring.color, 0.0)),
                gsk::ColorStop::new(0.88, rgba(ring.color, alpha)),
                gsk::ColorStop::new(1.0, rgba(ring.color, 0.0)),
            ],
        );
    }
    if index == 6 && a.beat > 0.02 {
        snapshot.append_color(&rgba(0xFFFFFF, 0.06 * a.beat), &all);
    }

    // Виньетка.
    snapshot.append_radial_gradient(
        &all,
        &graphene::Point::new(w / 2.0, h / 2.0),
        w * 0.75,
        h * 0.75,
        0.0,
        1.0,
        &[gsk::ColorStop::new(0.55, rgba(0, 0.0)), gsk::ColorStop::new(1.0, rgba(0, 0.3))],
    );

    // Обложка и название варианта.
    let side = h * 0.36;
    let rect = graphene::Rect::new(w * 0.3 - side / 2.0, h * 0.5 - side / 2.0, side, side);
    snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(rect, 8.0));
    snapshot.append_texture(&cover.texture, &rect);
    snapshot.pop();
    let layout = widget.create_pango_layout(Some(VARIANTS[index]));
    let font = pango::FontDescription::from_string(&format!("Sans Bold {}", (h * 0.032).clamp(10.0, 18.0) as i32));
    layout.set_font_description(Some(&font));
    snapshot.save();
    snapshot.translate(&graphene::Point::new(14.0, 10.0));
    snapshot.append_layout(&layout, &rgba(0xFFFFFF, 0.92));
    snapshot.restore();
    let lyric = widget.create_pango_layout(Some("Строка песни\nвот так читается"));
    let font = pango::FontDescription::from_string(&format!("Sans Bold {}", (h * 0.05).clamp(12.0, 34.0) as i32));
    lyric.set_font_description(Some(&font));
    snapshot.save();
    snapshot.translate(&graphene::Point::new(w * 0.55, h * 0.5 - h * 0.06));
    snapshot.append_layout(&lyric, &rgba(0xFFFFFF, 0.95));
    snapshot.restore();
    snapshot.pop();
}

fn main() -> glib::ExitCode {
    let mut args = std::env::args().skip(1);
    let covers_dir = args.next().unwrap_or_else(|| ".".into());
    let cache_dir = args.next().unwrap_or_else(|| format!("{}/.local/share/melogold/cache", std::env::var("HOME").unwrap_or_default()));
    gst::init().expect("GStreamer");

    let app = adw::Application::builder().application_id("app.melogold.EffectsLab").build();
    app.connect_activate(move |app| {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let files: Vec<std::path::PathBuf> = std::fs::read_dir(&covers_dir)
            .expect("папка с обложками")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jpg"))
            .collect();
        let covers: Vec<Rc<Cover>> = PLAYLIST
            .iter()
            .map(|(_, _, _, hint)| {
                let path = files.iter().find(|p| p.to_string_lossy().contains(hint)).cloned().unwrap_or_else(|| files[0].clone());
                Rc::new(load_cover(&path).expect("обложка"))
            })
            .collect();

        let spectrum = gst::ElementFactory::make("spectrum")
            .property("bands", 1024u32)
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
            track: Cell::new(0),
            covers,
            tiles: RefCell::new(
                (0..8).map(|i| TileState { seed: 0x9E37_79B9 ^ ((i as u32 + 1) * 0x85EB_CA6B), ..Default::default() }).collect(),
            ),
            last_frame: Cell::new(None),
            frames: RefCell::new(VecDeque::new()),
        });

        let grid = gtk::Grid::builder().row_homogeneous(true).column_homogeneous(true).row_spacing(2).column_spacing(2).build();
        let tiles: Vec<Tile> = (0..8)
            .map(|index| {
                let tile: Tile = glib::Object::new();
                tile.imp().index.set(index);
                tile.imp().shared.replace(Some(Rc::clone(&shared)));
                tile.set_hexpand(true);
                tile.set_vexpand(true);
                grid.attach(&tile, (index % 4) as i32, (index / 4) as i32, 1, 1);
                tile
            })
            .collect();

        let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
        play.add_css_class("circular");
        play.add_css_class("suggested-action");
        play.set_tooltip_text(Some("Играть (пробел)"));
        let previous = gtk::Button::from_icon_name("media-skip-backward-symbolic");
        let next = gtk::Button::from_icon_name("media-skip-forward-symbolic");
        let title = gtk::Label::builder().css_classes(["heading"]).build();
        let fps = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
        let header = adw::HeaderBar::new();
        header.pack_start(&previous);
        header.pack_start(&play);
        header.pack_start(&next);
        header.set_title_widget(Some(&title));
        header.pack_end(&fps);
        let toolbar = adw::ToolbarView::builder().content(&grid).build();
        toolbar.add_top_bar(&header);
        let window = adw::ApplicationWindow::builder().application(app).default_width(1600).default_height(900).content(&toolbar).build();

        let load = {
            let (shared, title, cache_dir) = (Rc::clone(&shared), title.clone(), cache_dir.clone());
            Rc::new(move |index: usize| {
                shared.track.set(index);
                let track = &PLAYLIST[index];
                let _ = shared.playbin.set_state(gst::State::Null);
                shared.queue.borrow_mut().clear();
                shared.playbin.set_property("uri", format!("file://{cache_dir}/{}.140.data", track.0));
                let _ = shared.playbin.set_state(if shared.playing.get() { gst::State::Playing } else { gst::State::Paused });
                title.set_text(&format!("{} — {}", track.1, track.2));
            })
        };
        load(0);
        let toggle = {
            let (shared, play) = (Rc::clone(&shared), play.clone());
            Rc::new(move || {
                let playing = !shared.playing.get();
                shared.playing.set(playing);
                let _ = shared.playbin.set_state(if playing { gst::State::Playing } else { gst::State::Paused });
                play.set_icon_name(if playing { "media-playback-pause-symbolic" } else { "media-playback-start-symbolic" });
            })
        };
        let step = {
            let (shared, load) = (Rc::clone(&shared), Rc::clone(&load));
            Rc::new(move |delta: i64| load((shared.track.get() as i64 + delta).rem_euclid(PLAYLIST.len() as i64) as usize))
        };

        let bus = playbin.bus().expect("шина");
        let watch = {
            let (shared, step) = (Rc::clone(&shared), Rc::clone(&step));
            bus.add_watch_local(move |_, message| {
                match message.view() {
                    gst::MessageView::Element(element) => {
                        if let Some(s) = element.structure().filter(|s| s.name() == "spectrum") {
                            let end = s.get::<u64>("endtime").unwrap_or(0);
                            if let Ok(list) = s.get::<gst::List>("magnitude") {
                                let mut queue = shared.queue.borrow_mut();
                                queue.push_back((end, list.iter().filter_map(|v| v.get::<f32>().ok()).collect()));
                                while queue.len() > 240 {
                                    queue.pop_front();
                                }
                            }
                        }
                    }
                    gst::MessageView::Eos(_) => step(1),
                    gst::MessageView::Error(error) => eprintln!("GStreamer: {}", error.error()),
                    _ => {}
                }
                glib::ControlFlow::Continue
            })
            .expect("наблюдение шины")
        };
        std::mem::forget(watch);

        let toggle_click = Rc::clone(&toggle);
        play.connect_clicked(move |_| toggle_click());
        let step_previous = Rc::clone(&step);
        previous.connect_clicked(move |_| step_previous(-1));
        let step_next = Rc::clone(&step);
        next.connect_clicked(move |_| step_next(1));
        let keys = gtk::EventControllerKey::new();
        let (toggle_key, step_key) = (Rc::clone(&toggle), Rc::clone(&step));
        keys.connect_key_pressed(move |_, key, _, _| {
            match key {
                gdk::Key::space => toggle_key(),
                gdk::Key::Right => step_key(1),
                gdk::Key::Left => step_key(-1),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        window.add_controller(keys);

        // Один такт на окно: кадр — от часов монитора (144 Гц на 144-герцовом), все плитки сразу.
        let (shared_tick, fps_label) = (Rc::clone(&shared), fps.clone());
        let last_fps = Cell::new(0usize);
        window.add_tick_callback(move |_, clock| {
            shared_tick.advance(clock.frame_time());
            let fps = shared_tick.fps();
            if fps != last_fps.replace(fps) {
                fps_label.set_text(&format!("{fps} к/с"));
            }
            for tile in &tiles {
                tile.queue_draw();
            }
            glib::ControlFlow::Continue
        });
        window.maximize();
        window.present();
    });
    app.run_with_args::<&str>(&[])
}
