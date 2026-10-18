//! Живой фон «Сейчас играет» (задание 0032): размытая обложка течёт (верхние частоты), по ней плавают
//! капли её цветов (бас), на ударах размытие собирается — шейдер `resources/shaders/fluid.frag` в
//! GtkGLArea. Кадр — по часам кадров (частота монитора). Нет OpenGL ES 3 или шейдер не собрался —
//! страница остаётся с фоном из задания 0031 (`backdrop.rs`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use glow::HasContext;
use gtk::prelude::*;
use gtk::{gdk, glib};
use melogold_core::artwork_colors::{self, SHADER_SIDE};
use melogold_playback::spectrum::BANDS;

const VERTEX: &str = include_str!("../resources/shaders/fluid.vert");
const FRAGMENT: &str = include_str!("../resources/shaders/fluid.frag");

/// Обложка для шейдера: текстура 256 × 256, яркость и цвета по долям.
pub struct FluidCover {
    pub pixels: Vec<u8>,
    pub luma: f32,
    pub palette: Vec<([f32; 3], f32)>,
}

impl FluidCover {
    /// Из пикселей обложки (RGBA): текстура, цвета по долям; яркость — та же, что у фона из 0031.
    pub fn from_pixels(rgba: &[u8], width: usize, height: usize, stride: usize, luma: f64) -> Option<FluidCover> {
        let pixels = artwork_colors::shader_texture(rgba, width, height, stride)?;
        let channel = |rgb: u32, shift: u32| ((rgb >> shift) & 0xFF) as f32 / 255.0;
        let palette = artwork_colors::shares(rgba, width, height, stride)
            .iter()
            .map(|c| ([channel(c.rgb, 16), channel(c.rgb, 8), channel(c.rgb, 0)], c.share as f32))
            .collect();
        Some(FluidCover { pixels, luma: luma as f32, palette })
    }
}

/// Звук → полосы, огибающие и удар (задание 0032, §3).
struct Analysis {
    bands: [f32; BANDS],
    targets: [f32; BANDS],
    bass: f32,
    high: f32,
    loud: f32,
    beat: f32,
    flux_average: f32,
    previous_bass: f32,
    since_beat: f32,
    /// Время последнего учтённого измерения: одно измерение — один раз.
    last_time: i64,
}

impl Default for Analysis {
    fn default() -> Self {
        Analysis {
            bands: [0.0; BANDS],
            targets: [0.0; BANDS],
            bass: 0.0,
            high: 0.0,
            loud: 0.0,
            beat: 0.0,
            flux_average: 0.0,
            previous_bass: 0.0,
            since_beat: 1.0,
            last_time: i64::MIN,
        }
    }
}

impl Analysis {
    fn feed(&mut self, time: i64, bands: &[f32; BANDS]) {
        if time == self.last_time {
            return;
        }
        self.last_time = time;
        self.targets = *bands;
        let bass = bands[..10].iter().sum::<f32>() / 10.0;
        let flux = (bass - self.previous_bass).max(0.0);
        self.previous_bass = bass;
        self.flux_average = self.flux_average * 0.94 + flux * 0.06;
        if flux > self.flux_average * 2.2 + 0.035 && self.since_beat > 0.2 {
            self.beat = 1.0;
            self.since_beat = 0.0;
        }
    }

    fn step(&mut self, dt: f32, playing: bool) {
        for (value, target) in self.bands.iter_mut().zip(self.targets.iter_mut()) {
            if !playing {
                *target *= (1.0 - dt * 3.0).max(0.0);
            }
            let rate = if *target > *value { 18.0 } else { 4.0 };
            *value += (*target - *value) * (rate * dt).min(1.0);
        }
        let range = |from: usize, to: usize| self.bands[from..to].iter().sum::<f32>() / (to - from) as f32;
        let follow = |current: f32, target: f32| current + (target - current) * (12.0 * dt).min(1.0);
        self.bass = follow(self.bass, range(0, 10));
        self.high = follow(self.high, range(30, BANDS));
        self.loud = follow(self.loud, range(0, BANDS));
        self.beat *= (1.0 - dt * 4.0).max(0.0);
        self.since_beat += dt;
    }
}

/// Движение варианта 1 (задание 0032, §4).
#[derive(Clone, Copy, Default)]
struct Motion {
    liquid: f32,
    lava: f32,
    /// Размер капель: ещё одно сглаживание `lava` — капли раздуваются мягче, чем течёт фон.
    blob: f32,
    pulse: f32,
    liquid_flow: f32,
    lava_flow: f32,
}

impl Motion {
    fn step(&mut self, analysis: &Analysis, dt: f32, playing: bool) {
        let follow = |current: f32, target: f32| {
            let rate = if target > current { 5.0 } else { 1.8 };
            current + (target - current) * (rate * dt).min(1.0)
        };
        self.liquid = follow(self.liquid, analysis.high.min(1.5));
        self.lava = follow(self.lava, analysis.bass.min(1.5));
        let pulse = (analysis.beat + analysis.bass * 0.4).min(1.2);
        self.pulse = if pulse > self.pulse { pulse } else { follow(self.pulse, pulse) };
        self.blob = follow(self.blob, self.lava.min(1.5));
        if playing {
            self.liquid_flow += dt * (0.25 + 1.4 * self.liquid);
            self.lava_flow += dt * (0.2 + 1.2 * self.lava);
        }
    }

    fn calm(&self) -> bool {
        self.liquid < 0.005 && self.lava < 0.005 && self.blob < 0.005 && self.pulse < 0.005
    }
}

struct Gl {
    gl: glow::Context,
    program: glow::Program,
    vao: glow::VertexArray,
    texture: glow::Texture,
}

#[derive(Default)]
struct Inner {
    gl: RefCell<Option<Gl>>,
    failed: Cell<bool>,
    on_failed: RefCell<Option<Box<dyn Fn()>>>,
    on_ready: RefCell<Option<Box<dyn Fn()>>>,
    cover: RefCell<Option<Rc<FluidCover>>>,
    uploaded: Cell<bool>,
    analysis: RefCell<Analysis>,
    motion: Cell<Motion>,
    moving: Cell<bool>,
    playing: Cell<bool>,
    audio: RefCell<Option<Box<dyn Fn() -> Option<(i64, [f32; BANDS])>>>>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    last_frame: Cell<Option<i64>>,
    /// Счётчик кадров для отладочной строки (RUST_LOG=debug): кадры и с какого времени.
    frames: Cell<(u32, i64)>,
}

#[derive(Clone)]
pub struct Fluid {
    pub area: gtk::GLArea,
    inner: Rc<Inner>,
}

type GetProc = unsafe extern "C" fn(*const std::ffi::c_char) -> *const std::ffi::c_void;

/// `eglGetProcAddress`: GTK рисует через EGL (Wayland и X11), а Mesa и NVIDIA отдают так и функции ядра
/// GLES (`EGL_KHR_get_all_proc_addresses`). Библиотека грузится один раз и не выгружается.
fn proc_loader() -> Option<GetProc> {
    static LOADER: std::sync::OnceLock<Option<GetProc>> = std::sync::OnceLock::new();
    *LOADER.get_or_init(|| {
        let library = unsafe { libloading::Library::new("libEGL.so.1") }.ok()?;
        let get = unsafe { library.get::<GetProc>(b"eglGetProcAddress\0") }.ok().map(|symbol| *symbol);
        std::mem::forget(library);
        get
    })
}

impl Fluid {
    pub fn new() -> Fluid {
        let area = gtk::GLArea::builder().auto_render(false).has_depth_buffer(false).hexpand(true).vexpand(true).can_target(false).build();
        area.set_allowed_apis(gdk::GLAPI::GLES);
        let fluid = Fluid { area, inner: Rc::default() };
        let inner = Rc::downgrade(&fluid.inner);
        fluid.area.connect_realize(move |area| {
            let Some(inner) = inner.upgrade() else { return };
            match realize(area) {
                Ok(gl) => {
                    inner.gl.replace(Some(gl));
                    inner.uploaded.set(false);
                    if let Some(ready) = inner.on_ready.borrow().as_ref() {
                        ready();
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "живой фон недоступен — остаётся размытая обложка");
                    inner.failed.set(true);
                    if let Some(failed) = inner.on_failed.borrow().as_ref() {
                        failed();
                    }
                }
            }
        });
        let inner = Rc::downgrade(&fluid.inner);
        fluid.area.connect_unrealize(move |area| {
            let Some(inner) = inner.upgrade() else { return };
            area.make_current();
            if let Some(state) = inner.gl.take() {
                unsafe {
                    state.gl.delete_program(state.program);
                    state.gl.delete_vertex_array(state.vao);
                    state.gl.delete_texture(state.texture);
                }
            }
        });
        let inner = Rc::downgrade(&fluid.inner);
        fluid.area.connect_render(move |area, _| {
            if let Some(inner) = inner.upgrade() {
                render(area, &inner);
            }
            glib::Propagation::Stop
        });
        fluid
    }

    /// Шейдеров нет — страница возвращается к фону из 0031.
    pub fn connect_failed(&self, callback: impl Fn() + 'static) {
        self.inner.on_failed.replace(Some(Box::new(callback)));
    }

    /// Шейдер собрался — фон из 0031 больше не нужен.
    pub fn connect_ready(&self, callback: impl Fn() + 'static) {
        self.inner.on_ready.replace(Some(Box::new(callback)));
    }

    pub fn failed(&self) -> bool {
        self.inner.failed.get()
    }

    /// Шейдер собран и рисует: фон из 0031 под ним не нужен.
    pub fn active(&self) -> bool {
        self.inner.gl.borrow().is_some()
    }

    /// Откуда брать спектр: измерение, которое сейчас звучит.
    pub fn set_audio(&self, audio: Box<dyn Fn() -> Option<(i64, [f32; BANDS])>>) {
        self.inner.audio.replace(Some(audio));
    }

    pub fn set_cover(&self, cover: Rc<FluidCover>) {
        self.inner.cover.replace(Some(cover));
        self.inner.uploaded.set(false);
        self.area.queue_render();
    }

    /// Двигаться ли (играет, видно, «Живой фон», анимации) и играет ли трек.
    pub fn set_state(&self, moving: bool, playing: bool) {
        self.inner.moving.set(moving);
        self.inner.playing.set(playing);
        if moving {
            self.ensure_tick();
        } else {
            // Замерший кадр — без сил звука, время стоит (задание 0032, §6).
            let mut motion = self.inner.motion.get();
            (motion.liquid, motion.lava, motion.blob, motion.pulse) = (0.0, 0.0, 0.0, 0.0);
            self.inner.motion.set(motion);
            self.area.queue_render();
        }
    }

    fn ensure_tick(&self) {
        if self.inner.tick.borrow().is_some() {
            return;
        }
        let inner = Rc::downgrade(&self.inner);
        let id = self.area.add_tick_callback(move |area, clock| {
            let Some(inner) = inner.upgrade() else { return glib::ControlFlow::Break };
            let now = clock.frame_time();
            let dt = inner.last_frame.replace(Some(now)).map_or(1.0 / 60.0, |last| ((now - last).max(0) as f32 / 1e6).min(0.1));
            let playing = inner.playing.get() && inner.moving.get();
            let mut analysis = inner.analysis.borrow_mut();
            if playing {
                if let Some((time, bands)) = inner.audio.borrow().as_ref().and_then(|audio| audio()) {
                    analysis.feed(time, &bands);
                }
            }
            analysis.step(dt, playing);
            let (count, since) = inner.frames.get();
            if now - since >= 5_000_000 {
                if since > 0 {
                    let fps = f64::from(count) * 1e6 / (now - since) as f64;
                    tracing::debug!(fps = format!("{fps:.0}"), bass = analysis.bass, high = analysis.high, "живой фон");
                }
                inner.frames.set((0, now));
            } else {
                inner.frames.set((count + 1, since));
            }
            let mut motion = inner.motion.get();
            motion.step(&analysis, dt, playing);
            inner.motion.set(motion);
            area.queue_render();
            if inner.moving.get() || !motion.calm() {
                glib::ControlFlow::Continue
            } else {
                inner.tick.replace(None);
                inner.last_frame.set(None);
                glib::ControlFlow::Break
            }
        });
        self.inner.tick.replace(Some(id));
    }
}

fn realize(area: &gtk::GLArea) -> Result<Gl, String> {
    area.make_current();
    if let Some(error) = area.error() {
        return Err(error.to_string());
    }
    let loader = proc_loader().ok_or("нет libEGL")?;
    let gl = unsafe {
        glow::Context::from_loader_function(|name| {
            let name = std::ffi::CString::new(name).unwrap_or_default();
            loader(name.as_ptr())
        })
    };
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
            gl.delete_shader(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(gl.get_program_info_log(program));
        }
        let vao = gl.create_vertex_array()?;
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::MIRRORED_REPEAT as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::MIRRORED_REPEAT as i32);
        Ok(Gl { gl, program, vao, texture })
    }
}

fn render(area: &gtk::GLArea, inner: &Inner) {
    let borrowed = inner.gl.borrow();
    let Some(state) = borrowed.as_ref() else { return };
    let gl = &state.gl;
    let Some(cover) = inner.cover.borrow().clone() else {
        // Обложки ещё нет — прозрачно: видна страница под фоном.
        unsafe {
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        return;
    };
    let scale = area.scale_factor();
    let (width, height) = (area.width() * scale, area.height() * scale);
    let motion = inner.motion.get();
    let loud = inner.analysis.borrow().loud;
    unsafe {
        gl.bind_texture(glow::TEXTURE_2D, Some(state.texture));
        if !inner.uploaded.replace(true) {
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                SHADER_SIDE as i32,
                SHADER_SIDE as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&cover.pixels)),
            );
            gl.generate_mipmap(glow::TEXTURE_2D);
        }
        gl.viewport(0, 0, width, height);
        gl.use_program(Some(state.program));
        gl.bind_vertex_array(Some(state.vao));
        let at = |name: &str| gl.get_uniform_location(state.program, name);
        let set = |name: &str, value: f32| gl.uniform_1_f32(at(name).as_ref(), value);
        gl.uniform_1_i32(at("uCover").as_ref(), 0);
        gl.uniform_2_f32(at("uRes").as_ref(), width as f32, height as f32);
        set("uLiquid", motion.liquid);
        set("uLiquidFlow", motion.liquid_flow);
        set("uLavaFlow", motion.lava_flow);
        set("uPulse", motion.pulse);
        set("uLoud", loud);
        set("uLuma", cover.luma);
        gl.uniform_1_f32_slice(at("uBlob").as_ref(), &[motion.blob; 6]);
        // Вариант 1 (задание 0032, §4): постоянные.
        set("uLavaMix", 0.55);
        set("uLiquidBase", 0.16);
        set("uBlobScale", 1.0);
        set("uZoom", 0.02);
        set("uLodBase", 5.3);
        set("uLodPulse", 3.0);
        set("uBright", 1.0);
        set("uSat", 1.6);
        let mut palette = [0f32; 18];
        let mut shares = [0f32; 6];
        for (i, (rgb, share)) in cover.palette.iter().take(6).enumerate() {
            palette[i * 3..i * 3 + 3].copy_from_slice(rgb);
            shares[i] = *share;
        }
        gl.uniform_3_f32_slice(at("uPal").as_ref(), &palette);
        gl.uniform_1_f32_slice(at("uShare").as_ref(), &shares);
        gl.uniform_1_i32(at("uPalCount").as_ref(), cover.palette.len().min(6) as i32);
        gl.draw_arrays(glow::TRIANGLES, 0, 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ступенька баса — удар; через 0,25 с огибающая удара заметно ниже (задание 0032, проверка).
    #[test]
    fn a_bass_step_is_a_beat_that_fades() {
        let mut analysis = Analysis::default();
        let quiet = [0.0f32; BANDS];
        let mut loud = [0.0f32; BANDS];
        loud[..10].fill(0.8);
        let mut time = 0;
        for _ in 0..30 {
            time += 11;
            analysis.feed(time, &quiet);
            analysis.step(1.0 / 60.0, true);
        }
        time += 11;
        analysis.feed(time, &loud);
        assert_eq!(analysis.beat, 1.0);
        for _ in 0..15 {
            analysis.step(1.0 / 60.0, true);
        }
        assert!(analysis.beat < 0.4, "{}", analysis.beat);
    }

    /// Одно и то же измерение на нескольких кадрах не считается заново.
    #[test]
    fn a_repeated_frame_is_not_a_new_beat() {
        let mut analysis = Analysis::default();
        let mut loud = [0.0f32; BANDS];
        loud[..10].fill(0.8);
        analysis.feed(1, &loud);
        analysis.beat = 0.0;
        analysis.since_beat = 1.0;
        analysis.feed(1, &loud);
        assert_eq!(analysis.beat, 0.0);
    }

    #[test]
    fn motion_flows_only_while_playing() {
        let analysis = Analysis { high: 0.5, bass: 0.5, ..Default::default() };
        let mut motion = Motion::default();
        motion.step(&analysis, 0.1, false);
        assert_eq!((motion.liquid_flow, motion.lava_flow), (0.0, 0.0));
        motion.step(&analysis, 0.1, true);
        assert!(motion.liquid_flow > 0.0 && motion.lava_flow > 0.0);
    }
}
