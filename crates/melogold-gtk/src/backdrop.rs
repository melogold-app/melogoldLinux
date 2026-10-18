//! Фон «Сейчас играет» — размытая обложка (задание 0031): два слоя уменьшенной размытой обложки,
//! вращающиеся навстречу, под матрицей цвета и размытием, с виньеткой. Числа — из задания, они общие
//! для всех платформ. Новая обложка сменяет старую затуханием; часы идут, только пока играет,
//! страницу видно, «Живой фон» включён и анимации системы не выключены.

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};
use melogold_core::artwork_colors::{BackdropPreview, PREVIEW_SIDE};

/// Смена обложки — затуханием за столько микросекунд.
const CROSSFADE_US: f64 = 400_000.0;

/// Готовая к рисованию обложка фона.
#[derive(Clone)]
struct Layer {
    blurred: gdk::Texture,
    luma: f64,
    base: gdk::RGBA,
}

impl Layer {
    fn new(preview: &BackdropPreview) -> Layer {
        let blurred = gdk::MemoryTexture::new(
            PREVIEW_SIDE as i32,
            PREVIEW_SIDE as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from(&preview.rgba),
            PREVIEW_SIDE * 4,
        )
        .upcast();
        Layer { blurred, luma: preview.luma, base: rgba(preview.base, 1.0) }
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Backdrop {
        pub(super) current: RefCell<Option<Layer>>,
        /// Прежняя обложка и время кадра, когда началась смена.
        pub(super) previous: RefCell<Option<(Layer, Option<i64>)>>,
        pub(super) seconds: Cell<f64>,
        pub(super) last_frame: Cell<Option<i64>>,
        pub(super) moving: Cell<bool>,
        pub(super) tick: RefCell<Option<gtk::TickCallbackId>>,
        pub(super) on_resize: RefCell<Option<Box<dyn Fn(i32, i32)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Backdrop {
        const NAME: &'static str = "MelogoldBackdrop";
        type Type = super::Backdrop;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Backdrop {}

    impl WidgetImpl for Backdrop {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if let Some(callback) = self.on_resize.borrow().as_ref() {
                callback(width, height);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let t = self.seconds.get() as f32;
            let now = widget.frame_clock().map(|clock| clock.frame_time());
            if let Some((layer, started)) = self.previous.borrow().as_ref() {
                draw_layer(snapshot, layer, t, w, h);
                let progress = match (started, now) {
                    (Some(started), Some(now)) => ((now - started) as f64 / CROSSFADE_US).clamp(0.0, 1.0),
                    _ => 0.0,
                };
                if let Some(current) = self.current.borrow().as_ref() {
                    snapshot.push_opacity(progress);
                    draw_layer(snapshot, current, t, w, h);
                    snapshot.pop();
                }
            } else if let Some(current) = self.current.borrow().as_ref() {
                draw_layer(snapshot, current, t, w, h);
            }
            // Виньетка: края темнее — внимание остаётся на обложке и тексте.
            snapshot.append_radial_gradient(
                &graphene::Rect::new(0.0, 0.0, w, h),
                &graphene::Point::new(w / 2.0, h / 2.0),
                w * 0.75,
                h * 0.75,
                0.0,
                1.0,
                &[gsk::ColorStop::new(0.55, rgba(0, 0.0)), gsk::ColorStop::new(1.0, rgba(0, 0.30))],
            );
        }
    }
}

glib::wrapper! {
    pub struct Backdrop(ObjectSubclass<imp::Backdrop>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Backdrop {
    fn default() -> Self {
        let backdrop: Backdrop = glib::Object::new();
        backdrop.set_can_target(false);
        backdrop.set_hexpand(true);
        backdrop.set_vexpand(true);
        backdrop
    }
}

impl Backdrop {
    /// Новая обложка; прежняя гаснет под ней.
    pub fn set_cover(&self, preview: &BackdropPreview) {
        let imp = self.imp();
        let layer = Layer::new(preview);
        if let Some(old) = imp.current.replace(Some(layer)) {
            imp.previous.replace(Some((old, None)));
            self.ensure_tick();
        }
        self.queue_draw();
    }

    /// Двигаться ли фону. Неподвижный фон — тот же кадр: часы стоят, а не сбрасываются.
    pub fn set_moving(&self, moving: bool) {
        self.imp().moving.set(moving);
        if moving {
            self.ensure_tick();
        }
    }

    /// Размер области «Сейчас играет» — по нему страница ставит размер обложки (задание 0031).
    pub fn connect_resized(&self, callback: impl Fn(i32, i32) + 'static) {
        self.imp().on_resize.replace(Some(Box::new(callback)));
    }

    fn ensure_tick(&self) {
        let imp = self.imp();
        if imp.tick.borrow().is_some() {
            return;
        }
        let id = self.add_tick_callback(|widget, clock| {
            let imp = widget.imp();
            let now = clock.frame_time();
            let dt = imp.last_frame.replace(Some(now)).map_or(0.0, |last| (now - last).max(0) as f64 / 1_000_000.0);
            if imp.moving.get() {
                imp.seconds.set(imp.seconds.get() + dt.min(0.1));
            }
            let mut fading = false;
            {
                let mut previous = imp.previous.borrow_mut();
                if let Some((_, started)) = previous.as_mut() {
                    let start = *started.get_or_insert(now);
                    if (now - start) as f64 >= CROSSFADE_US {
                        *previous = None;
                    } else {
                        fading = true;
                    }
                }
            }
            widget.queue_draw();
            if imp.moving.get() || fading {
                glib::ControlFlow::Continue
            } else {
                imp.tick.replace(None);
                imp.last_frame.set(None);
                glib::ControlFlow::Break
            }
        });
        imp.tick.replace(Some(id));
    }
}

fn rgba(rgb: u32, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(((rgb >> 16) & 0xFF) as f32 / 255.0, ((rgb >> 8) & 0xFF) as f32 / 255.0, (rgb & 0xFF) as f32 / 255.0, alpha)
}

/// Одна обложка: подложка, матрица цвета, размытие 32 и два вращающихся слоя (задание 0031, §1).
fn draw_layer(snapshot: &gtk::Snapshot, layer: &Layer, t: f32, w: f32, h: f32) {
    snapshot.append_color(&layer.base, &graphene::Rect::new(0.0, 0.0, w, h));
    let k = (0.30 / layer.luma.max(0.01)).clamp(0.30, 1.0) as f32;
    let s = 1.6f32;
    let (lr, lg, lb) = (0.2126 * (1.0 - s), 0.7152 * (1.0 - s), 0.0722 * (1.0 - s));
    #[rustfmt::skip]
    let matrix = graphene::Matrix::from_float([
        k * (lr + s), k * lr, k * lr, 0.0,
        k * lg, k * (lg + s), k * lg, 0.0,
        k * lb, k * lb, k * (lb + s), 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]);
    snapshot.push_color_matrix(&matrix, &graphene::Vec4::zero());
    snapshot.push_blur(32.0);
    let big = w.max(h);
    turn(snapshot, &layer.blurred, w / 2.0, h / 2.0, big * 1.6, t * 2.4, 1.0);
    let (dx, dy) = ((t / 17.0).sin() * w * 0.18, (t / 23.0).cos() * h * 0.15);
    turn(snapshot, &layer.blurred, w / 2.0 + dx, h / 2.0 + dy, big * 1.15, 90.0 - t * 3.6, 0.7);
    snapshot.pop();
    snapshot.pop();
}

fn turn(snapshot: &gtk::Snapshot, texture: &gdk::Texture, cx: f32, cy: f32, side: f32, degrees: f32, opacity: f64) {
    snapshot.push_opacity(opacity);
    snapshot.save();
    snapshot.translate(&graphene::Point::new(cx, cy));
    snapshot.rotate(degrees);
    snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Linear, &graphene::Rect::new(-side / 2.0, -side / 2.0, side, side));
    snapshot.restore();
    snapshot.pop();
}
