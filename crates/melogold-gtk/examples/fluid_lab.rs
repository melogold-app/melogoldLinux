//! Лаборатория фона «Сейчас играет», третий прогон: фон рисует шейдер видеокарты — сама размытая
//! обложка течёт, рябит, расходится каплями, а её размытие собирается и расплывается в такт.
//! Восемь плиток 4 × 2 в одном окне на весь экран: четыре вида по отдельности (жидкая обложка,
//! обложка как вода, лава-лампа, пульс размытия) и четыре их сочетания. Звук один на все плитки.
//!
//!     cargo run --release -p melogold-gtk --example fluid_lab -- <папка с обложками> [кэш Melogold]
//!
//! Пробел или ▶ — играть, ←/→ — трек. Кадр — от часов монитора; счётчик к/с в заголовке.
//! Размытие — уровни мип-текстуры обложки с выборкой диском: дёшево и без граней, и сила размытия
//! меняется плавно, дробным уровнем.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use adw::prelude::*;
use glow::HasContext;
use gst::prelude::*;
use gtk::{gdk, glib};
use melogold_core::artwork_colors;

const BANDS: usize = 48;
const COVER_SIDE: usize = 256;

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

/// Плитки: какие виды движения включены (жидкость, вода, лава, пульс).
const VARIANTS: [(&str, [bool; 4]); 8] = [
    ("1 · Жидкая обложка", [true, false, false, false]),
    ("2 · Обложка как вода", [false, true, false, false]),
    ("3 · Лава-лампа", [false, false, true, false]),
    ("4 · Пульс размытия", [false, false, false, true]),
    ("5 · Жидкая + пульс", [true, false, false, true]),
    ("6 · Вода + пульс", [false, true, false, true]),
    ("7 · Лава + жидкая", [true, false, true, false]),
    ("8 · Всё вместе", [true, true, true, true]),
];

const VERTEX: &str = "#version 300 es
out vec2 vUv;
void main() {
    vec2 pos = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    vUv = pos;
    gl_Position = vec4(pos * 2.0 - 1.0, 0.0, 1.0);
}";

const FRAGMENT: &str = "#version 300 es
precision highp float;
in vec2 vUv;
out vec4 frag;
uniform sampler2D uCover;
uniform vec2 uRes;
uniform float uTime, uFlow, uBass, uMid, uHigh, uLoud, uBeat, uLuma;
uniform ivec4 uKinds;
uniform vec4 uDrops[8];
uniform int uDropCount;
uniform vec3 uPal[6];
uniform float uShare[6];
uniform int uPalCount;

vec2 rot(vec2 p, float a) { float c = cos(a), s = sin(a); return mat2(c, s, -s, c) * p; }

// Размытие: уровень мип-текстуры и пять выборок по кругу — мягко, без квадратов мипов.
vec3 blurAt(vec2 uv, float lod) {
    uv.y = 1.0 - uv.y;
    float r = exp2(lod) / 256.0 * 1.4;
    vec3 c = textureLod(uCover, uv, lod).rgb * 0.36;
    for (int i = 0; i < 4; i++) {
        float a = float(i) * 1.5708 + 0.785;
        c += textureLod(uCover, uv + r * vec2(cos(a), sin(a)), lod).rgb * 0.16;
    }
    return c;
}

void main() {
    bool liquid = uKinds.x == 1, water = uKinds.y == 1, lava = uKinds.z == 1, pulse = uKinds.w == 1;
    float aspect = uRes.x / uRes.y;
    vec2 p = (vUv - 0.5) * vec2(aspect, 1.0);

    // Сила размытия: пульс собирает её на ударах, в тишине — мягче.
    float lod = 5.2;
    if (pulse) lod = mix(5.6, 2.2, clamp(uBeat * 0.95 + uBass * 0.35, 0.0, 1.0));

    // Основа — как фон A: обложка крупно, медленно вращается.
    vec2 q = rot(p, uFlow * 0.045);
    vec2 q2 = rot(p * 0.82, -uFlow * 0.065 + 1.5708) + 0.18 * vec2(sin(uFlow * 0.059), cos(uFlow * 0.043));

    if (liquid) {
        // Жидкость: поле течения искажает само себя; бас раздувает и ускоряет его.
        float amp = 0.22 + 0.6 * uBass;
        for (int i = 0; i < 3; i++) {
            float fi = float(i);
            q += amp * 0.32 * vec2(sin(q.y * 2.3 + uFlow * 0.7 + fi * 1.3), cos(q.x * 2.0 - uFlow * 0.55 + fi * 2.1));
            q2 += amp * 0.28 * vec2(cos(q2.y * 1.9 - uFlow * 0.6 + fi), sin(q2.x * 2.4 + uFlow * 0.5 + fi * 1.7));
        }
    }

    vec3 light = vec3(0.0);
    if (water) {
        // Вода: постоянная рябь от середины спектра и круги от капель, падающих на ударах.
        vec2 off = (0.010 + 0.030 * uMid) * vec2(sin(p.y * 13.0 + uTime * 2.3 + sin(p.x * 3.0)), cos(p.x * 11.0 - uTime * 1.9));
        for (int i = 0; i < 8; i++) {
            if (i >= uDropCount) break;
            vec4 d = uDrops[i];
            vec2 c = (d.xy - 0.5) * vec2(aspect, 1.0);
            vec2 dv = p - c;
            float r = length(dv) + 1e-4;
            float front = d.z * 0.62;
            float band = exp(-abs(r - front) * 10.0);
            float wave = sin((r - front) * 42.0) * band * exp(-d.z * 1.1) * d.w;
            off += dv / r * wave * 0.05;
            light += vec3(max(wave, 0.0)) * 0.10;
        }
        q += off;
        q2 += off * 0.8;
    }

    vec3 col = mix(blurAt(0.5 + q * 0.62, lod), blurAt(0.5 + q2 * 0.62, lod + 0.5), 0.38);

    if (lava) {
        // Лава-лампа: капли цветов обложки, размер — по доле цвета; сливаются и расходятся.
        vec3 acc = vec3(0.0);
        float field = 0.0;
        for (int i = 0; i < 6; i++) {
            if (i >= uPalCount) break;
            float fi = float(i);
            float rad = (0.14 + 0.5 * sqrt(uShare[i])) * (1.0 + 0.4 * uBass);
            vec2 c = vec2(sin(uFlow * 0.21 + fi * 2.4) * aspect * 0.42, cos(uFlow * 0.17 + fi * 1.7) * 0.42);
            c += 0.06 * vec2(sin(uFlow * 0.9 + fi), cos(uFlow * 1.1 + fi * 2.0));
            float f = rad * rad / (dot(p - c, p - c) + 0.003);
            field += f;
            acc += uPal[i] * f;
        }
        vec3 lavaCol = acc / max(field, 1e-3);
        float m = smoothstep(0.7, 1.8, field);
        col = mix(col * 0.75, lavaCol, m * 0.8 + 0.12);
    }

    // Цвет как у фона A: яркость по обложке, насыщенность 1,6; пульс слегка поднимает свет.
    float k = clamp(0.30 / max(uLuma, 0.01), 0.30, 1.0) * (1.0 + (pulse ? 0.18 * uBeat : 0.0) + 0.12 * uLoud);
    float l = dot(col, vec3(0.2126, 0.7152, 0.0722));
    col = mix(vec3(l), col, 1.6) * k + light;
    float v = smoothstep(0.55, 1.0, length((vUv - 0.5) / 0.75));
    col *= 1.0 - 0.3 * v;
    frag = vec4(clamp(col, 0.0, 1.0), 1.0);
}";

struct Cover {
    texture: gdk::Texture,
    /// 256 × 256 RGBA для мип-текстуры.
    pixels: Vec<u8>,
    luma: f32,
    palette: Vec<([f32; 3], f32)>,
}

fn load_cover(path: &std::path::Path) -> Option<Cover> {
    let texture = gdk::Texture::from_filename(path).ok()?;
    let mut downloader = gdk::TextureDownloader::new(&texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let preview = artwork_colors::backdrop_preview(&bytes, width, height, stride)?;
    let palette = artwork_colors::shares(&bytes, width, height, stride)
        .iter()
        .map(|c| {
            ([((c.rgb >> 16) & 0xFF) as f32 / 255.0, ((c.rgb >> 8) & 0xFF) as f32 / 255.0, (c.rgb & 0xFF) as f32 / 255.0], c.share as f32)
        })
        .collect();
    let mut pixels = Vec::with_capacity(COVER_SIDE * COVER_SIDE * 4);
    for y in 0..COVER_SIDE {
        for x in 0..COVER_SIDE {
            let (x0, y0) = (x * width / COVER_SIDE, y * height / COVER_SIDE);
            let (x1, y1) = (((x + 1) * width / COVER_SIDE).max(x0 + 1).min(width), ((y + 1) * height / COVER_SIDE).max(y0 + 1).min(height));
            let mut sum = [0u32; 3];
            let mut n = 0u32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = yy * stride + xx * 4;
                    for c in 0..3 {
                        sum[c] += u32::from(bytes[i + c]);
                    }
                    n += 1;
                }
            }
            pixels.extend(sum.map(|s| (s / n.max(1)) as u8));
            pixels.push(255);
        }
    }
    Some(Cover { texture, pixels, luma: preview.luma as f32, palette })
}

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

struct Drop {
    x: f32,
    y: f32,
    age: f32,
    strength: f32,
}

struct Gl {
    gl: glow::Context,
    program: glow::Program,
    vao: glow::VertexArray,
    texture: glow::Texture,
    uploaded: Cell<Option<usize>>,
}

struct Shared {
    playbin: gst::Element,
    spectrum: gst::Element,
    queue: RefCell<VecDeque<(u64, Vec<f32>)>>,
    analysis: RefCell<Analysis>,
    playing: Cell<bool>,
    track: Cell<usize>,
    covers: Vec<Rc<Cover>>,
    time: Cell<f32>,
    flow: Cell<f32>,
    drops: RefCell<Vec<Drop>>,
    seen_beats: Cell<u64>,
    seed: Cell<u32>,
    last_frame: Cell<Option<i64>>,
    frames: RefCell<VecDeque<i64>>,
    gl: RefCell<Option<Gl>>,
}

impl Shared {
    fn random(&self) -> f32 {
        let mut x = self.seed.get().max(1);
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.seed.set(x);
        (x % 10_000) as f32 / 10_000.0
    }

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
        let (loud, beats, bass) = {
            let mut analysis = self.analysis.borrow_mut();
            if let Some(running) = self.playbin.current_running_time().map(|t| t.nseconds()) {
                let mut queue = self.queue.borrow_mut();
                while queue.front().is_some_and(|(end, _)| *end <= running) {
                    let (_, magnitudes) = queue.pop_front().expect("есть");
                    analysis.feed(&magnitudes, rate);
                }
            }
            analysis.step(dt, self.playing.get());
            (analysis.loud, analysis.beats, analysis.bass)
        };
        if self.playing.get() {
            self.time.set(self.time.get() + dt);
            // Течение: быстрее, когда громче.
            self.flow.set(self.flow.get() + dt * (0.35 + 1.6 * loud * loud));
        }
        let mut drops = self.drops.borrow_mut();
        for drop in drops.iter_mut() {
            drop.age += dt;
        }
        drops.retain(|d| d.age < 3.0);
        if beats != self.seen_beats.replace(beats) && drops.len() < 8 {
            let (x, y) = (0.15 + self.random() * 0.7, 0.15 + self.random() * 0.7);
            drops.push(Drop { x, y, age: 0.0, strength: 0.6 + 0.8 * bass });
        }
    }

    fn fps(&self) -> usize {
        self.frames.borrow().len()
    }
}

fn compile(gl: &glow::Context) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        for (kind, source) in [(glow::VERTEX_SHADER, VERTEX), (glow::FRAGMENT_SHADER, FRAGMENT)] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(gl.get_shader_info_log(shader));
            }
            gl.attach_shader(program, shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(gl.get_program_info_log(program));
        }
        Ok(program)
    }
}

fn render(area: &gtk::GLArea, shared: &Shared) {
    let borrowed = shared.gl.borrow();
    let Some(state) = borrowed.as_ref() else { return };
    let gl = &state.gl;
    let cover = &shared.covers[shared.track.get()];
    let scale = area.scale_factor();
    let (width, height) = (area.width() * scale, area.height() * scale);
    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(state.texture));
        if state.uploaded.get() != Some(shared.track.get()) {
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                COVER_SIDE as i32,
                COVER_SIDE as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&cover.pixels)),
            );
            gl.generate_mipmap(glow::TEXTURE_2D);
            state.uploaded.set(Some(shared.track.get()));
        }
        gl.clear_color(0.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.use_program(Some(state.program));
        gl.bind_vertex_array(Some(state.vao));
        let at = |name: &str| gl.get_uniform_location(state.program, name);
        let a = shared.analysis.borrow();
        gl.uniform_1_i32(at("uCover").as_ref(), 0);
        gl.uniform_1_f32(at("uTime").as_ref(), shared.time.get());
        gl.uniform_1_f32(at("uFlow").as_ref(), shared.flow.get());
        gl.uniform_1_f32(at("uBass").as_ref(), a.bass);
        gl.uniform_1_f32(at("uMid").as_ref(), a.mid);
        gl.uniform_1_f32(at("uHigh").as_ref(), a.high);
        gl.uniform_1_f32(at("uLoud").as_ref(), a.loud);
        gl.uniform_1_f32(at("uBeat").as_ref(), a.beat);
        gl.uniform_1_f32(at("uLuma").as_ref(), cover.luma);
        let drops = shared.drops.borrow();
        let mut flat = [0f32; 32];
        for (i, drop) in drops.iter().take(8).enumerate() {
            flat[i * 4..i * 4 + 4].copy_from_slice(&[drop.x, drop.y, drop.age, drop.strength]);
        }
        gl.uniform_4_f32_slice(at("uDrops").as_ref(), &flat);
        gl.uniform_1_i32(at("uDropCount").as_ref(), drops.len().min(8) as i32);
        let mut palette = [0f32; 18];
        let mut shares = [0f32; 6];
        for (i, (rgb, share)) in cover.palette.iter().take(6).enumerate() {
            palette[i * 3..i * 3 + 3].copy_from_slice(rgb);
            shares[i] = *share;
        }
        gl.uniform_3_f32_slice(at("uPal").as_ref(), &palette);
        gl.uniform_1_f32_slice(at("uShare").as_ref(), &shares);
        gl.uniform_1_i32(at("uPalCount").as_ref(), cover.palette.len().min(6) as i32);

        let gap = 2 * scale;
        let (cell_w, cell_h) = ((width - 3 * gap) / 4, (height - gap) / 2);
        for (index, (_, kinds)) in VARIANTS.iter().enumerate() {
            let (column, row) = ((index % 4) as i32, (index / 4) as i32);
            // У GL начало координат внизу: верхний ряд плиток — выше.
            let (x, y) = (column * (cell_w + gap), (1 - row) * (cell_h + gap));
            gl.viewport(x, y, cell_w, cell_h);
            gl.uniform_2_f32(at("uRes").as_ref(), cell_w as f32, cell_h as f32);
            let kinds = kinds.map(i32::from);
            gl.uniform_4_i32(at("uKinds").as_ref(), kinds[0], kinds[1], kinds[2], kinds[3]);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }
}

fn main() -> glib::ExitCode {
    let mut args = std::env::args().skip(1);
    let covers_dir = args.next().unwrap_or_else(|| ".".into());
    let cache_dir = args.next().unwrap_or_else(|| format!("{}/.local/share/melogold/cache", std::env::var("HOME").unwrap_or_default()));
    gst::init().expect("GStreamer");
    // Функции GL — через eglGetProcAddress: GTK на Wayland работает через EGL, а Mesa отдаёт так и
    // функции ядра GLES (EGL_KHR_get_all_proc_addresses).
    let egl = unsafe { libloading::Library::new("libEGL.so.1") }.expect("libEGL");
    type GetProc = unsafe extern "C" fn(*const std::ffi::c_char) -> *const std::ffi::c_void;
    let get_proc: GetProc = unsafe { *egl.get::<GetProc>(b"eglGetProcAddress\0").expect("eglGetProcAddress") };
    std::mem::forget(egl);

    let app = adw::Application::builder().application_id("app.melogold.FluidLab").build();
    app.connect_activate(move |app| {
        let get_proc = get_proc;
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let css = gtk::CssProvider::new();
        css.load_from_string(
            ".fluid-label { color: white; font-weight: 800; text-shadow: 0 1px 4px alpha(black, 0.6); }
             .fluid-lyric { color: white; font-weight: 800; font-size: 26px; text-shadow: 0 1px 6px alpha(black, 0.35); }
             .fluid-cover { border-radius: 10px; box-shadow: 0 12px 32px alpha(black, 0.45); }",
        );
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
            time: Cell::new(0.0),
            flow: Cell::new(0.0),
            drops: RefCell::new(Vec::new()),
            seen_beats: Cell::new(0),
            seed: Cell::new(0x9E37_79B9),
            last_frame: Cell::new(None),
            frames: RefCell::new(VecDeque::new()),
            gl: RefCell::new(None),
        });

        let area = gtk::GLArea::builder().hexpand(true).vexpand(true).auto_render(false).build();
        area.set_allowed_apis(gdk::GLAPI::GLES);
        {
            let shared = Rc::clone(&shared);
            area.connect_realize(move |area| {
                area.make_current();
                if let Some(error) = area.error() {
                    eprintln!("GLArea: {error}");
                    return;
                }
                let gl = unsafe {
                    glow::Context::from_loader_function(|name| {
                        let name = std::ffi::CString::new(name).expect("имя функции");
                        get_proc(name.as_ptr())
                    })
                };
                let program = match compile(&gl) {
                    Ok(program) => program,
                    Err(log) => {
                        eprintln!("шейдер: {log}");
                        return;
                    }
                };
                unsafe {
                    let vao = gl.create_vertex_array().expect("VAO");
                    let texture = gl.create_texture().expect("текстура");
                    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
                    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
                    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::MIRRORED_REPEAT as i32);
                    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::MIRRORED_REPEAT as i32);
                    shared.gl.replace(Some(Gl { gl, program, vao, texture, uploaded: Cell::new(None) }));
                }
            });
        }
        {
            let shared = Rc::clone(&shared);
            area.connect_render(move |area, _| {
                render(area, &shared);
                glib::Propagation::Stop
            });
        }

        // Подписи и обложка поверх каждой плитки.
        let cells =
            gtk::Grid::builder().row_homogeneous(true).column_homogeneous(true).row_spacing(2).column_spacing(2).can_target(false).build();
        let mut pictures = Vec::new();
        for (index, (title, _)) in VARIANTS.iter().enumerate() {
            let label = gtk::Label::builder()
                .label(*title)
                .xalign(0.0)
                .valign(gtk::Align::Start)
                .margin_start(14)
                .margin_top(10)
                .css_classes(["fluid-label"])
                .build();
            let picture = gtk::Picture::builder()
                .content_fit(gtk::ContentFit::Cover)
                .width_request(150)
                .height_request(150)
                .css_classes(["fluid-cover"])
                .overflow(gtk::Overflow::Hidden)
                .build();
            let lyric = gtk::Label::builder().label("Строка песни\nвот так читается").xalign(0.0).css_classes(["fluid-lyric"]).build();
            let middle = gtk::Box::builder().spacing(24).valign(gtk::Align::Center).halign(gtk::Align::Center).vexpand(true).build();
            middle.append(&picture);
            middle.append(&lyric);
            let cell = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
            cell.append(&label);
            cell.append(&middle);
            cells.attach(&cell, (index % 4) as i32, (index / 4) as i32, 1, 1);
            pictures.push(picture);
        }
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&area));
        overlay.add_overlay(&cells);

        let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
        play.add_css_class("circular");
        play.add_css_class("suggested-action");
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
        let toolbar = adw::ToolbarView::builder().content(&overlay).build();
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
                for picture in &pictures {
                    picture.set_paintable(Some(&shared.covers[index].texture));
                }
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

        let (shared_tick, fps_label) = (Rc::clone(&shared), fps.clone());
        let last_fps = Cell::new(0usize);
        area.add_tick_callback(move |area, clock| {
            shared_tick.advance(clock.frame_time());
            let fps = shared_tick.fps();
            if fps != last_fps.replace(fps) {
                fps_label.set_text(&format!("{fps} к/с"));
            }
            area.queue_render();
            glib::ControlFlow::Continue
        });
        window.maximize();
        window.present();
    });
    app.run_with_args::<&str>(&[])
}
