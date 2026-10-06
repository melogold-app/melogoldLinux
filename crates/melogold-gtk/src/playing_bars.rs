//! Три столбика «играет» (docs/PROMPT.md §4, §5.3; Windows `PlayingBars.cs`): у играющего трека
//! вместо обложки — низы, середина, верх под настоящий звук ([`melogold_playback::levels`]). На
//! паузе и при выключенных анимациях столбики неподвижны. Все столбики окна обновляет один таймер,
//! пока хоть один на экране.

use std::cell::RefCell;

use gtk::glib;
use gtk::prelude::*;

type Source = Box<dyn Fn() -> ((f32, f32, f32), bool)>;

thread_local! {
    static SOURCE: RefCell<Option<Source>> = const { RefCell::new(None) };
    static BARS: RefCell<Vec<glib::WeakRef<gtk::DrawingArea>>> = const { RefCell::new(Vec::new()) };
    static VALUES: RefCell<[f32; 3]> = const { RefCell::new([0.3, 0.5, 0.35]) };
    static CLOCK: RefCell<Option<glib::SourceId>> = const { RefCell::new(None) };
}

/// Откуда брать уровни и играет ли плеер (окно ставит один раз).
pub fn set_source(source: impl Fn() -> ((f32, f32, f32), bool) + 'static) {
    SOURCE.with(|s| s.replace(Some(Box::new(source))));
}

/// Столбики 18 × 16 белым — поверх затемнённой обложки.
pub fn new() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(18)
        .content_height(16)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .can_target(false)
        .build();
    area.set_draw_func(|_, cr, width, height| {
        let values = VALUES.with(|v| *v.borrow());
        let (gap, count) = (2.0, 3.0);
        let bar = (f64::from(width) - gap * (count - 1.0)) / count;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        for (index, value) in values.iter().enumerate() {
            let h = (f64::from(height) * (0.15 + 0.85 * f64::from(value.clamp(0.0, 1.0)))).max(2.0);
            let x = index as f64 * (bar + gap);
            let y = f64::from(height) - h;
            let r = 1.0f64.min(bar / 2.0);
            cr.new_sub_path();
            cr.arc(x + bar - r, y + r, r, -std::f64::consts::FRAC_PI_2, 0.0);
            cr.arc(x + bar - r, y + h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
            cr.arc(x + r, y + h - r, r, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
            cr.arc(x + r, y + r, r, std::f64::consts::PI, 1.5 * std::f64::consts::PI);
            cr.close_path();
        }
        let _ = cr.fill();
    });
    area.connect_map(|area| {
        BARS.with(|bars| bars.borrow_mut().push(area.downgrade()));
        ensure_clock();
    });
    area
}

fn ensure_clock() {
    CLOCK.with(|clock| {
        if clock.borrow().is_some() {
            return;
        }
        let id = glib::timeout_add_local(std::time::Duration::from_millis(33), || {
            let alive: Vec<gtk::DrawingArea> = BARS.with(|bars| {
                let mut bars = bars.borrow_mut();
                bars.retain(|b| b.upgrade().is_some_and(|b| b.is_mapped()));
                bars.iter().filter_map(glib::WeakRef::upgrade).collect()
            });
            if alive.is_empty() {
                CLOCK.with(|c| c.replace(None));
                return glib::ControlFlow::Break;
            }
            // Выключены анимации («Уменьшение движения» в GNOME) — столбики стоят и при игре:
            // играющий трек и так отмечен ими, двигаться им незачем (доктрина §4.1).
            let animate = gtk::Settings::default().is_some_and(|settings| settings.is_gtk_enable_animations());
            let values = SOURCE.with(|s| {
                s.borrow().as_ref().map(|source| {
                    let ((low, mid, high), playing) = source();
                    if playing && animate {
                        [low, mid, high]
                    } else if playing {
                        [0.55, 0.9, 0.7]
                    } else {
                        [0.3, 0.5, 0.35]
                    }
                })
            });
            if let Some(values) = values {
                VALUES.with(|v| v.replace(values));
            }
            for bars in alive {
                bars.queue_draw();
            }
            glib::ControlFlow::Continue
        });
        clock.replace(Some(id));
    });
}
