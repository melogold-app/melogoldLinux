//! Текст песни в «Сейчас играет» (Windows `SyncedLyricsView.cs`, Android `SyncedLyricsView.kt`):
//!
//! - текущую строку отмечает скруглённая подложка **по размеру строки** — ширина самой длинной
//!   строки переноса, высота вместе с подпевкой и переводом; на следующую строку она переезжает и
//!   меняет размер пружиной (задание 0003). Подложка — один рисунок под строками, а не фон каждой;
//! - верх текущей строки — у самого верха области, сразу под затуханием края (задание 0007);
//! - пройденные строки приглушены сильнее будущих; при времени слов строка загорается по словам;
//! - щелчок по строке перематывает; своя прокрутка останавливает слежение на 3 с, есть «К текущей строке»;
//! - в паузе от 4 с — три точки, только пока пауза идёт;
//! - вторая сторона дуэта — у правого края, подпевка мельче под строкой, перевод — под текстом.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, pango};
use melogold_core::lyrics::rows::{self, LyricRow};
use melogold_core::lyrics::VocalSide;

use crate::localization::tr;
use crate::lyrics_service::{source_text, LyricsService, LyricsState};

/// Текст чуть опережает звук: слово загорается ровно тогда, когда его поют.
const LEAD_MS: i64 = 60;
/// Размер строки при обычном окне; всё остальное в строке считается от него.
const BASE_FONT: f64 = 28.0;
/// Затухание верхнего края: над текущей строкой видна только его кромка.
const FADE_TOP: f64 = 24.0;
const RESUME_FOLLOW: Duration = Duration::from_secs(3);
const UNSUNG_ALPHA: f64 = 0.4;

/// Размер текста по области: растёт с окном, в маленьком окне не меньше 24, больше 56 не бывает.
pub fn font_size_for(width: f64, height: f64) -> f64 {
    (width * 0.058).min(height * 0.06).clamp(24.0, 56.0).round()
}

// ── подложка: один рисунок под строками ──

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LyricsCanvas {
        pub pill: Cell<Option<graphene::Rect>>,
        pub alpha: Cell<f64>,
        pub radius: Cell<f32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LyricsCanvas {
        const NAME: &'static str = "MelogoldLyricsCanvas";
        type Type = super::LyricsCanvas;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for LyricsCanvas {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for LyricsCanvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let alpha = self.alpha.get();
            if let (Some(rect), true) = (self.pill.get(), alpha > 0.005) {
                // Без цвета обложки: белый 16 % на тёмном, чёрный 8 % на светлом (задание 0003).
                let dark = adw::StyleManager::default().is_dark();
                let color = if dark {
                    gdk::RGBA::new(1.0, 1.0, 1.0, 0.16 * alpha as f32)
                } else {
                    gdk::RGBA::new(0.0, 0.0, 0.0, 0.08 * alpha as f32)
                };
                let radius = self.radius.get();
                let rounded = gsk::RoundedRect::from_rect(rect, radius);
                snapshot.push_rounded_clip(&rounded);
                snapshot.append_color(&color, &rect);
                snapshot.pop();
            }
            self.parent_snapshot(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct LyricsCanvas(ObjectSubclass<imp::LyricsCanvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl LyricsCanvas {
    fn new(child: &impl IsA<gtk::Widget>) -> LyricsCanvas {
        let canvas: LyricsCanvas = glib::Object::new();
        child.set_parent(&canvas);
        canvas.imp().radius.set(12.0);
        canvas
    }

    fn set_pill(&self, rect: graphene::Rect) {
        self.imp().pill.set(Some(rect));
        self.queue_draw();
    }

    fn pill(&self) -> Option<graphene::Rect> {
        self.imp().pill.get()
    }

    fn set_alpha(&self, alpha: f64) {
        self.imp().alpha.set(alpha);
        self.queue_draw();
    }

    fn alpha(&self) -> f64 {
        self.imp().alpha.get()
    }
}

fn lerp_rect(from: &graphene::Rect, to: &graphene::Rect, t: f64) -> graphene::Rect {
    let t = t as f32;
    let mix = |a: f32, b: f32| a + (b - a) * t;
    graphene::Rect::new(mix(from.x(), to.x()), mix(from.y(), to.y()), mix(from.width(), to.width()), mix(from.height(), to.height()))
}

fn close(a: &graphene::Rect, b: &graphene::Rect) -> bool {
    (a.x() - b.x()).abs() < 0.5
        && (a.y() - b.y()).abs() < 0.5
        && (a.width() - b.width()).abs() < 0.5
        && (a.height() - b.height()).abs() < 0.5
}

/// Где на самом деле лежит текст подписи (перенос, выравнивание) относительно `target`.
fn text_rect(label: &gtk::Label, target: &impl IsA<gtk::Widget>) -> Option<graphene::Rect> {
    if !label.is_visible() || label.text().is_empty() {
        return None;
    }
    let bounds = label.compute_bounds(target)?;
    let (ox, oy) = label.layout_offsets();
    let (_, logical) = label.layout().pixel_extents();
    Some(graphene::Rect::new(
        bounds.x() + ox as f32 + logical.x() as f32,
        bounds.y() + oy as f32 + logical.y() as f32,
        logical.width() as f32,
        logical.height() as f32,
    ))
}

fn attrs(size: f64, weight: pango::Weight, dim_from: Option<u32>) -> pango::AttrList {
    let list = pango::AttrList::new();
    list.insert(pango::AttrSize::new_size_absolute((size * f64::from(pango::SCALE)) as i32));
    list.insert(pango::AttrInt::new_weight(weight));
    if let Some(start) = dim_from {
        let mut dim = pango::AttrInt::new_foreground_alpha((UNSUNG_ALPHA * 65535.0) as u16);
        dim.set_start_index(start);
        dim.set_end_index(u32::MAX);
        list.insert(dim);
    }
    list
}

// ── строка экрана ──

struct DotsState {
    start_ms: i64,
    end_ms: i64,
    position: i64,
    activated: Instant,
    playing: bool,
    scale: f64,
}

struct RowView {
    root: gtk::Box,
    labels: Vec<(gtk::Label, f64, pango::Weight)>,
    /// Конец каждого слова в байтах текста строки и его начало, мс (время слов).
    words: Vec<(u32, i64)>,
    dots: Option<(gtk::DrawingArea, Rc<RefCell<DotsState>>)>,
    start_ms: i64,
    active: Cell<bool>,
    sung: Cell<i32>,
    scale: Cell<f64>,
}

impl RowView {
    fn sung(row: &melogold_core::lyrics::SyncedLine) -> RowView {
        let end = row.side == VocalSide::End;
        let (halign, justify, xalign) =
            if end { (gtk::Align::End, gtk::Justification::Right, 1.0) } else { (gtk::Align::Start, gtk::Justification::Left, 0.0) };
        let label = |text: &str| {
            gtk::Label::builder()
                .label(text)
                .wrap(true)
                .wrap_mode(pango::WrapMode::WordChar)
                .justify(justify)
                .xalign(xalign)
                .halign(halign)
                .build()
        };
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(2).halign(halign).build();
        let mut words = Vec::new();
        let text = if row.words.is_empty() {
            row.text.clone()
        } else {
            let mut text = String::new();
            for word in &row.words {
                text.push_str(&word.text);
                words.push((text.len() as u32, word.start_ms));
            }
            text
        };
        let main = label(&text);
        body.append(&main);
        let mut labels = vec![(main, BASE_FONT, pango::Weight::Bold)];
        if let Some(backing) = row.background.as_ref().map(|b| b.text()).filter(|t| !t.is_empty()) {
            let backing = label(&backing);
            backing.set_opacity(0.8);
            body.append(&backing);
            labels.push((backing, 18.0, pango::Weight::Semibold));
        }
        if let Some(extra) = row.translation.clone().or_else(|| row.transliteration.clone()).filter(|t| !t.is_empty()) {
            let extra = label(&extra);
            extra.set_opacity(0.75);
            body.append(&extra);
            labels.push((extra, 16.0, pango::Weight::Normal));
        }
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        root.append(&body);
        root.add_css_class("lyric-line");
        root.add_css_class("future");
        root.update_property(&[gtk::accessible::Property::Label(&row.text)]);
        RowView {
            root,
            labels,
            words,
            dots: None,
            start_ms: row.start_ms,
            active: Cell::new(false),
            sung: Cell::new(-1),
            scale: Cell::new(1.0),
        }
    }

    fn interlude(start_ms: i64, end_ms: i64, side: VocalSide) -> RowView {
        let state = Rc::new(RefCell::new(DotsState {
            start_ms,
            end_ms,
            position: start_ms,
            activated: Instant::now(),
            playing: false,
            scale: 1.0,
        }));
        let area = gtk::DrawingArea::builder().halign(if side == VocalSide::End { gtk::Align::End } else { gtk::Align::Start }).build();
        let drawn = Rc::clone(&state);
        area.set_draw_func(move |area, cr, _, height| {
            let s = drawn.borrow();
            let color = area.color();
            let animated = s.playing && area.settings().is_gtk_enable_animations();
            let clock = s.activated.elapsed().as_secs_f64() * 1000.0;
            let duration = (s.end_ms - s.start_ms).max(1) as f64;
            let fraction = ((s.position - s.start_ms) as f64 / duration).clamp(0.0, 1.0);
            let remaining = (s.end_ms - s.position) as f64;
            let mut group = if animated { 1.0 + 0.075 * (1.0 - (2.0 * std::f64::consts::PI * clock / 3000.0).cos()) } else { 1.0 };
            let mut group_alpha = 1.0;
            if animated && remaining < 1000.0 {
                // Перед следующей строкой точки раздуваются и лопаются.
                if remaining > 250.0 {
                    let p = ((1000.0 - remaining) / 750.0).clamp(0.0, 1.0);
                    group += (1.25 - group) * (1.0 - (1.0 - p) * (1.0 - p));
                } else {
                    let p = ((250.0 - remaining) / 250.0).clamp(0.0, 1.0);
                    group = 1.25 + (0.4 - 1.25) * p;
                    group_alpha = 1.0 - p;
                }
            }
            let (dot, gap) = (10.0 * s.scale, 6.0 * s.scale);
            let total = 3.0 * dot + 2.0 * gap;
            let (cx, cy) = (total / 2.0, f64::from(height) / 2.0);
            cr.translate(cx, cy);
            cr.scale(group, group);
            cr.translate(-cx, -cy);
            for k in 0..3 {
                let fill = (fraction * 3.0 - k as f64).clamp(0.0, 1.0);
                let appear = if animated { ease_out_back(((clock - k as f64 * 90.0) / 320.0).clamp(0.0, 1.0)) } else { 1.0 };
                let alpha = ((0.2 + 0.7 * fill) * group_alpha).clamp(0.0, 1.0) * f64::from(color.alpha());
                cr.set_source_rgba(f64::from(color.red()), f64::from(color.green()), f64::from(color.blue()), alpha);
                let x = k as f64 * (dot + gap) + dot / 2.0;
                cr.arc(x, cy, dot / 2.0 * appear.max(0.0), 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
            }
        });
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).visible(false).build();
        root.append(&area);
        root.add_css_class("lyric-line");
        root.update_property(&[gtk::accessible::Property::Label(tr("LyricsInstrumental"))]);
        RowView {
            root,
            labels: Vec::new(),
            words: Vec::new(),
            dots: Some((area, state)),
            start_ms,
            active: Cell::new(false),
            sung: Cell::new(-1),
            scale: Cell::new(1.0),
        }
    }

    fn apply_scale(&self, scale: f64) {
        self.scale.set(scale);
        for (index, (label, size, weight)) in self.labels.iter().enumerate() {
            let dim = (index == 0 && self.active.get() && !self.words.is_empty()).then(|| self.dim_from());
            label.set_attributes(Some(&attrs(size * scale, *weight, dim.flatten())));
        }
        let (x, y) = ((16.0 * scale) as i32, (10.0 * scale) as i32);
        self.root.set_margin_start(x);
        self.root.set_margin_end(x);
        self.root.set_margin_top(y);
        self.root.set_margin_bottom(y);
        if let Some((area, state)) = &self.dots {
            state.borrow_mut().scale = scale;
            area.set_content_width((46.0 * scale) as i32);
            area.set_content_height((40.0 * scale - 2.0 * f64::from(y)).max(1.0) as i32);
            area.set_margin_start((16.0 * scale) as i32);
            area.set_margin_end((16.0 * scale) as i32);
        }
    }

    /// Первое ещё не спетое слово: с него текст приглушён.
    fn dim_from(&self) -> Option<u32> {
        let sung = self.sung.get().max(0) as usize;
        if sung >= self.words.len() {
            return None;
        }
        Some(if sung == 0 { 0 } else { self.words[sung - 1].0 })
    }

    fn set_state(&self, active: bool, class: &str) {
        self.active.set(active);
        for other in ["past", "active", "future"] {
            if other == class {
                self.root.add_css_class(other);
            } else {
                self.root.remove_css_class(other);
            }
        }
        if let Some((_, state)) = &self.dots {
            // Проигрыш занимает место только пока идёт.
            self.root.set_visible(active);
            state.borrow_mut().activated = Instant::now();
        }
        if !self.words.is_empty() {
            self.sung.set(-1);
            // Вне строки слова одного цвета: приглушает прозрачность строки.
            if !active {
                let (label, size, weight) = &self.labels[0];
                label.set_attributes(Some(&attrs(size * self.scale.get(), *weight, None)));
            }
        }
    }

    /// Кадр текущей строки: слова загораются по времени, точки проигрыша живут.
    fn update(&self, position: i64, playing: bool) {
        if !self.active.get() {
            return;
        }
        if !self.words.is_empty() {
            let sung = self.words.iter().filter(|(_, start)| *start <= position).count() as i32;
            if sung != self.sung.get() {
                self.sung.set(sung);
                let (label, size, weight) = &self.labels[0];
                label.set_attributes(Some(&attrs(size * self.scale.get(), *weight, self.dim_from())));
            }
            return;
        }
        if let Some((area, state)) = &self.dots {
            {
                let mut state = state.borrow_mut();
                state.position = position;
                state.playing = playing;
            }
            area.queue_draw();
        }
    }

    fn body_rect(&self, canvas: &LyricsCanvas) -> Option<graphene::Rect> {
        if self.dots.is_some() {
            return None;
        }
        let mut union: Option<graphene::Rect> = None;
        for (label, _, _) in &self.labels {
            if let Some(rect) = text_rect(label, canvas) {
                union = Some(match union {
                    Some(u) => u.union(&rect),
                    None => rect,
                });
            }
        }
        union
    }
}

fn ease_out_back(t: f64) -> f64 {
    let (c1, c3) = (1.70158, 2.70158);
    let u = t - 1.0;
    1.0 + c3 * u * u * u + c1 * u * u
}

// ── синхронный текст ──

pub struct SyncedView {
    pub root: gtk::Overlay,
    scroller: gtk::ScrolledWindow,
    canvas: LyricsCanvas,
    lines: gtk::Box,
    source: gtk::Label,
    jump: gtk::Button,
    views: RefCell<Vec<RowView>>,
    rows: RefCell<Rc<Vec<LyricRow>>>,
    offset: Cell<i64>,
    active: Cell<i32>,
    follow: Cell<bool>,
    font: Cell<f64>,
    size: Cell<(i32, i32)>,
    /// Строка сменилась: подложка и прокрутка — на следующем такте, по новой раскладке.
    pending: Cell<Option<bool>>,
    pill_from: Cell<graphene::Rect>,
    pill_to: Cell<graphene::Rect>,
    spring: RefCell<Option<adw::SpringAnimation>>,
    fade: RefCell<Option<adw::TimedAnimation>>,
    scroll: RefCell<Option<adw::TimedAnimation>>,
    scroll_from: Cell<f64>,
    scroll_to: Cell<f64>,
    clock: RefCell<Option<glib::SourceId>>,
    resume: RefCell<Option<glib::SourceId>>,
    position: Box<dyn Fn() -> Option<Duration>>,
    playing: Box<dyn Fn() -> bool>,
    seek: Rc<dyn Fn(i64)>,
}

impl SyncedView {
    pub fn new(position: Box<dyn Fn() -> Option<Duration>>, playing: Box<dyn Fn() -> bool>, seek: Rc<dyn Fn(i64)>) -> Rc<SyncedView> {
        let lines = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).build();
        let source = gtk::Label::builder().xalign(0.0).wrap(true).margin_start(16).margin_top(12).build();
        source.add_css_class("dim-label");
        source.add_css_class("caption");
        let column = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        column.append(&lines);
        column.append(&source);
        let canvas = LyricsCanvas::new(&column);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .child(&canvas)
            .vexpand(true)
            .hexpand(true)
            .build();
        scroller.add_css_class("lyrics-scroller");
        let top = gtk::Box::builder().height_request(FADE_TOP as i32).valign(gtk::Align::Start).can_target(false).build();
        top.add_css_class("lyrics-fade-top");
        let bottom = gtk::Box::builder().height_request(72).valign(gtk::Align::End).can_target(false).build();
        bottom.add_css_class("lyrics-fade-bottom");
        let jump = gtk::Button::builder()
            .label(tr("LyricsJumpToLine"))
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .margin_bottom(16)
            .visible(false)
            .build();
        jump.add_css_class("pill");
        jump.add_css_class("osd");
        let root = gtk::Overlay::builder().child(&scroller).build();
        root.add_overlay(&top);
        root.add_overlay(&bottom);
        root.add_overlay(&jump);

        let view = Rc::new(SyncedView {
            root,
            scroller,
            canvas,
            lines,
            source,
            jump,
            views: RefCell::default(),
            rows: RefCell::default(),
            offset: Cell::new(0),
            active: Cell::new(-2),
            follow: Cell::new(true),
            font: Cell::new(BASE_FONT),
            size: Cell::new((0, 0)),
            pending: Cell::new(None),
            pill_from: Cell::new(graphene::Rect::zero()),
            pill_to: Cell::new(graphene::Rect::zero()),
            spring: RefCell::default(),
            fade: RefCell::default(),
            scroll: RefCell::default(),
            scroll_from: Cell::new(0.0),
            scroll_to: Cell::new(0.0),
            clock: RefCell::default(),
            resume: RefCell::default(),
            position,
            playing,
            seek,
        });
        view.connect();
        view
    }

    fn connect(self: &Rc<Self>) {
        // Пружина подложки (затухание 0.78, период ~70 мс) и затухание за 0,2 с.
        let weak = Rc::downgrade(self);
        let target = adw::CallbackAnimationTarget::new(move |t| {
            if let Some(view) = weak.upgrade() {
                view.canvas.set_pill(lerp_rect(&view.pill_from.get(), &view.pill_to.get(), t));
            }
        });
        let stiffness = (std::f64::consts::TAU / 0.070).powi(2);
        let spring = adw::SpringAnimation::new(&self.canvas, 0.0, 1.0, adw::SpringParams::new(0.78, 1.0, stiffness), target);
        spring.set_clamp(false);
        self.spring.replace(Some(spring));
        let weak = Rc::downgrade(self);
        let target = adw::CallbackAnimationTarget::new(move |alpha| {
            if let Some(view) = weak.upgrade() {
                view.canvas.set_alpha(alpha);
            }
        });
        self.fade.replace(Some(adw::TimedAnimation::new(&self.canvas, 0.0, 1.0, 200, target)));
        let weak = Rc::downgrade(self);
        let target = adw::CallbackAnimationTarget::new(move |t| {
            if let Some(view) = weak.upgrade() {
                let value = view.scroll_from.get() + (view.scroll_to.get() - view.scroll_from.get()) * t;
                view.scroller.vadjustment().set_value(value);
            }
        });
        let scroll = adw::TimedAnimation::new(&self.scroller, 0.0, 1.0, 450, target);
        scroll.set_easing(adw::Easing::EaseOutCubic);
        self.scroll.replace(Some(scroll));

        // Прокрутил человек — колесом, сенсорной панелью, касанием или клавишами.
        let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        wheel.connect_scroll(move |_, _, _| {
            if let Some(view) = weak.upgrade() {
                view.user_scrolled();
            }
            glib::Propagation::Proceed
        });
        self.scroller.add_controller(wheel);
        let touch = gtk::GestureDrag::builder().touch_only(true).propagation_phase(gtk::PropagationPhase::Capture).build();
        let weak = Rc::downgrade(self);
        touch.connect_drag_update(move |_, _, dy| {
            if dy.abs() > 8.0 {
                if let Some(view) = weak.upgrade() {
                    view.user_scrolled();
                }
            }
        });
        self.scroller.add_controller(touch);
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            use gdk::Key;
            if matches!(key, Key::Up | Key::Down | Key::Page_Up | Key::Page_Down | Key::Home | Key::End) {
                if let Some(view) = weak.upgrade() {
                    view.user_scrolled();
                }
            }
            glib::Propagation::Proceed
        });
        self.scroller.add_controller(keys);
        let weak = Rc::downgrade(self);
        self.jump.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.resume_follow();
            }
        });

        // Текущая строка — посередине области: сверху и снизу поля в половину высоты, чтобы
        // и первая, и последняя строка могли встать на середину.
        let weak = Rc::downgrade(self);
        self.scroller.vadjustment().connect_page_size_notify(move |adjustment| {
            if let Some(view) = weak.upgrade() {
                let half = (adjustment.page_size() / 2.0) as i32;
                view.lines.set_margin_top(half);
                view.lines.set_margin_bottom(half);
            }
        });

        // Часы — пока текст на экране.
        let weak = Rc::downgrade(self);
        self.canvas.connect_map(move |_| {
            if let Some(view) = weak.upgrade() {
                view.start_clock();
            }
        });
        let weak = Rc::downgrade(self);
        self.canvas.connect_unmap(move |_| {
            if let Some(view) = weak.upgrade() {
                if let Some(clock) = view.clock.take() {
                    clock.remove();
                }
            }
        });
    }

    fn start_clock(self: &Rc<Self>) {
        if let Some(clock) = self.clock.take() {
            clock.remove();
        }
        self.active.set(-2);
        self.follow.set(true);
        self.jump.set_visible(false);
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(Duration::from_millis(33), move || {
            let Some(view) = weak.upgrade() else { return glib::ControlFlow::Break };
            view.tick();
            glib::ControlFlow::Continue
        });
        self.clock.replace(Some(id));
        self.tick();
    }

    pub fn set_lyrics(self: &Rc<Self>, rows: &Rc<Vec<LyricRow>>, offset_ms: i64, source: Option<&str>) {
        self.offset.set(offset_ms);
        match source_text(source) {
            Some(text) => {
                self.source.set_label(text);
                self.source.set_visible(true);
            }
            None => self.source.set_visible(false),
        }
        if Rc::ptr_eq(&self.rows.borrow(), rows) {
            return;
        }
        self.rows.replace(Rc::clone(rows));
        while let Some(child) = self.lines.first_child() {
            self.lines.remove(&child);
        }
        let mut views = Vec::new();
        for row in rows.iter() {
            let view = match row {
                LyricRow::Sung(line) => RowView::sung(line),
                LyricRow::Interlude { start_ms, end_ms, side } => RowView::interlude(*start_ms, *end_ms, *side),
            };
            view.apply_scale(self.font.get() / BASE_FONT);
            let click = gtk::GestureClick::new();
            let (seek, start, weak) = (Rc::clone(&self.seek), view.start_ms, Rc::downgrade(self));
            click.connect_released(move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let offset = weak.upgrade().map(|v| v.offset.get()).unwrap_or(0);
                seek((start - offset).max(0));
            });
            view.root.add_controller(click);
            self.lines.append(&view.root);
            views.push(view);
        }
        self.views.replace(views);
        self.active.set(-2);
        self.follow.set(true);
        self.jump.set_visible(false);
        self.canvas.set_alpha(0.0);
        self.scroller.vadjustment().set_value(0.0);
        self.tick();
    }

    fn scale(&self) -> f64 {
        self.font.get() / BASE_FONT
    }

    /// Размер текста по размеру области: строки, поля, точки и подложка — в том же масштабе.
    fn apply_size(&self) {
        let size = (self.scroller.width(), self.scroller.height());
        if size == self.size.get() || size.0 <= 0 {
            return;
        }
        self.size.set(size);
        let font = font_size_for(f64::from(size.0), f64::from(size.1));
        if (font - self.font.get()).abs() >= 0.5 {
            self.font.set(font);
            let scale = self.scale();
            self.lines.set_spacing((4.0 * scale) as i32);
            for view in self.views.borrow().iter() {
                view.apply_scale(scale);
            }
            self.canvas.imp().radius.set((12.0 * scale) as f32);
        }
        self.pending.set(Some(true));
    }

    fn tick(&self) {
        self.apply_size();
        let views = self.views.borrow();
        if views.is_empty() {
            return;
        }
        let position = (self.position)().map(|p| p.as_millis() as i64).unwrap_or(0) + LEAD_MS + self.offset.get();
        let rows = self.rows.borrow();
        let index = rows::active_index_at(&rows, position).map(|i| i as i32).unwrap_or(-1);
        if index != self.active.get() {
            let previous = self.active.replace(index);
            for (i, view) in views.iter().enumerate() {
                let i = i as i32;
                view.set_state(
                    i == index,
                    if i < index {
                        "past"
                    } else if i == index {
                        "active"
                    } else {
                        "future"
                    },
                );
            }
            // Проигрыш занимает место только пока идёт: подложка и прокрутка считаются по новой
            // раскладке — на следующем такте.
            self.pending.set(Some(previous < -1 || self.pending.get() == Some(true)));
        } else if let Some(immediate) = self.pending.take() {
            self.move_pill(&views, immediate);
            if self.follow.get() {
                self.scroll_to_active(&views, !immediate);
            }
        } else if let Some(target) = self.pill_target(&views) {
            // Строка сдвинулась без смены (перенос при другом размере окна): подложка догоняет.
            if !close(&target, &self.pill_to.get()) {
                self.move_pill(&views, false);
            }
        }
        if index >= 0 {
            views[index as usize].update(position, (self.playing)());
        }
    }

    fn pill_target(&self, views: &[RowView]) -> Option<graphene::Rect> {
        let index = self.active.get();
        let view = views.get(usize::try_from(index).ok()?)?;
        let body = view.body_rect(&self.canvas)?;
        let (x, y) = ((16.0 * self.scale()) as f32, (10.0 * self.scale()) as f32);
        Some(graphene::Rect::new(body.x() - x, body.y() - y, body.width() + 2.0 * x, body.height() + 2.0 * y))
    }

    fn move_pill(&self, views: &[RowView], immediate: bool) {
        let Some(target) = self.pill_target(views) else {
            self.fade_to(0.0);
            return;
        };
        let spring = self.spring.borrow();
        let Some(spring) = spring.as_ref() else { return };
        let animations = self.canvas.settings().is_gtk_enable_animations();
        match self.canvas.pill() {
            Some(current) if !immediate && animations && self.canvas.alpha() > 0.01 => {
                self.pill_from.set(current);
                self.pill_to.set(target);
                spring.reset();
                spring.play();
            }
            _ => {
                spring.reset();
                self.pill_from.set(target);
                self.pill_to.set(target);
                self.canvas.set_pill(target);
            }
        }
        self.fade_to(1.0);
    }

    fn fade_to(&self, alpha: f64) {
        let fade = self.fade.borrow();
        let Some(fade) = fade.as_ref() else { return };
        let current = self.canvas.alpha();
        if (current - alpha).abs() < 0.01 {
            return;
        }
        fade.reset();
        fade.set_value_from(current);
        fade.set_value_to(alpha);
        fade.play();
    }

    /// Середина текущей строки — посередине области, как у Windows (пользователь, 2026-09-27,
    /// вместо «у верха» из задания 0007). До первой строки посередине стоит первая.
    fn scroll_to_active(&self, views: &[RowView], animated: bool) {
        let adjustment = self.scroller.vadjustment();
        let index = usize::try_from(self.active.get()).unwrap_or(0);
        let middle = views
            .get(index)
            .and_then(|view| view.root.compute_bounds(&self.canvas))
            .map(|b| f64::from(b.y()) + f64::from(b.height()) / 2.0);
        let target = middle.map(|y| y - adjustment.page_size() / 2.0).unwrap_or(0.0);
        let target = target.clamp(0.0, (adjustment.upper() - adjustment.page_size()).max(0.0));
        let scroll = self.scroll.borrow();
        let Some(scroll) = scroll.as_ref() else { return };
        if (target - adjustment.value()).abs() < 1.0 {
            return;
        }
        if animated {
            self.scroll_from.set(adjustment.value());
            self.scroll_to.set(target);
            scroll.reset();
            scroll.play();
        } else {
            scroll.reset();
            adjustment.set_value(target);
        }
    }

    /// Слежение — через 3 с после последнего движения; до тех пор — «К текущей строке».
    fn user_scrolled(self: &Rc<Self>) {
        if let Some(scroll) = self.scroll.borrow().as_ref() {
            scroll.pause();
        }
        self.follow.set(false);
        self.jump.set_visible(true);
        if let Some(resume) = self.resume.take() {
            resume.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(RESUME_FOLLOW, move || {
            if let Some(view) = weak.upgrade() {
                view.resume.take();
                view.resume_follow();
            }
        });
        self.resume.replace(Some(id));
    }

    fn resume_follow(&self) {
        if let Some(resume) = self.resume.take() {
            resume.remove();
        }
        self.follow.set(true);
        self.jump.set_visible(false);
        let views = self.views.borrow();
        self.scroll_to_active(&views, true);
    }
}

// ── область текста целиком ──

/// Область текста: синхронный, обычный или сообщение (загрузка, нет текста, нет связи).
pub struct LyricsPanel {
    pub root: gtk::Stack,
    synced: Rc<SyncedView>,
    plain: gtk::Label,
    plain_source: gtk::Label,
    plain_scroller: gtk::ScrolledWindow,
    status: adw::StatusPage,
    status_actions: gtk::Box,
    spinner: adw::Spinner,
}

impl LyricsPanel {
    pub fn new(synced: Rc<SyncedView>) -> Rc<LyricsPanel> {
        let plain = gtk::Label::builder().xalign(0.0).wrap(true).wrap_mode(pango::WrapMode::WordChar).selectable(false).build();
        plain.add_css_class("lyrics-plain");
        let plain_source = gtk::Label::builder().xalign(0.0).wrap(true).margin_top(16).build();
        plain_source.add_css_class("dim-label");
        plain_source.add_css_class("caption");
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(16)
            .margin_end(16)
            .margin_top(FADE_TOP as i32)
            .margin_bottom(48)
            .build();
        column.append(&plain);
        column.append(&plain_source);
        let plain_scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&column).vexpand(true).build();

        let status_actions = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).halign(gtk::Align::Center).build();
        let status = adw::StatusPage::builder().child(&status_actions).build();
        status.add_css_class("compact");
        let spinner = adw::Spinner::builder().width_request(32).height_request(32).build();
        let loading = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Center)
            .build();
        loading.append(&spinner);
        let loading_label = gtk::Label::new(Some(tr("LyricsLoading")));
        loading_label.add_css_class("dim-label");
        loading.append(&loading_label);

        let root = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).vexpand(true).hexpand(true).build();
        root.add_named(&synced.root, Some("synced"));
        root.add_named(&plain_scroller, Some("plain"));
        root.add_named(&status, Some("status"));
        root.add_named(&loading, Some("loading"));
        root.add_css_class("lyrics-panel");
        Rc::new(LyricsPanel { root, synced, plain, plain_source, plain_scroller, status, status_actions, spinner })
    }

    /// Показать состояние; `find`, `edit` и `retry` — действия кнопок сообщения.
    pub fn show(&self, service: &LyricsService) {
        let state = service.state();
        while let Some(child) = self.status_actions.first_child() {
            self.status_actions.remove(&child);
        }
        let button = |label: &str, action: &str| {
            let button = gtk::Button::builder().label(label).action_name(action).build();
            button.add_css_class("pill");
            button
        };
        match state {
            LyricsState::Synced { rows, offset_ms, source, .. } => {
                self.synced.set_lyrics(&rows, offset_ms, source.as_deref());
                self.root.set_visible_child_name("synced");
            }
            LyricsState::Plain { text, source } => {
                if self.plain.label() != text {
                    self.plain.set_label(&text);
                    self.plain_scroller.vadjustment().set_value(0.0);
                }
                match source_text(source.as_deref()) {
                    Some(text) => {
                        self.plain_source.set_label(text);
                        self.plain_source.set_visible(true);
                    }
                    None => self.plain_source.set_visible(false),
                }
                self.root.set_visible_child_name("plain");
            }
            LyricsState::Loading | LyricsState::Unknown => {
                self.root.set_visible_child_name("loading");
            }
            LyricsState::NotFound => {
                self.status.set_icon_name(Some("lyrics-symbolic"));
                self.status.set_title(tr("LyricsUnavailable"));
                let find = button(tr("LyricsFind"), "win.lyrics-find");
                find.add_css_class("suggested-action");
                self.status_actions.append(&find);
                self.status_actions.append(&button(tr("LyricsEdit"), "win.lyrics-edit"));
                self.root.set_visible_child_name("status");
            }
            LyricsState::Failed => {
                self.status.set_icon_name(Some("network-offline-symbolic"));
                self.status.set_title(tr("LyricsLoadFailed"));
                self.status_actions.append(&button(tr("Retry"), "win.lyrics-retry"));
                self.root.set_visible_child_name("status");
            }
        }
        let _ = &self.spinner;
    }
}
