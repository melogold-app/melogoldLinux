//! Столбики «Когда вы слушали» и «Время суток» (задание 0009): `GtkDrawingArea` и cairo. Цвет — акцент
//! темы (самый высокий — полный, остальные — приглушённые), верхушки скруглены, пустой день — тонкая
//! чёрточка, чтобы ряд не терял вид. Подпись столбика — в подсказке при наведении; для Orca — описание.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::cairo;

/// Один столбик: высота, подпись под ним (если есть) и подсказка.
#[derive(Clone)]
pub struct ChartBar {
    pub value: i64,
    pub label: Option<String>,
    pub tooltip: String,
}

const LABEL_HEIGHT: f64 = 20.0;
const BAR_SHARE: f64 = 0.66;
const MAX_RADIUS: f64 = 8.0;
const EMPTY_HEIGHT: f64 = 3.0;

/// Столбики высотой `height` и подписи под ними; `description` читает Orca вместо картинки.
pub fn bar_chart(bars: Vec<ChartBar>, height: i32, description: &str) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().hexpand(true).content_height(height + LABEL_HEIGHT as i32).build();
    area.add_css_class("stats-chart");
    area.set_has_tooltip(true);
    area.update_property(&[gtk::accessible::Property::Label(description)]);
    let bars = Rc::new(bars);
    let hover: Rc<RefCell<Option<usize>>> = Rc::default();

    let drawn = Rc::clone(&bars);
    let hovered = Rc::clone(&hover);
    area.set_draw_func(move |area, cr, width, total_height| {
        let (width, chart_height) = (f64::from(width), f64::from(total_height) - LABEL_HEIGHT);
        if drawn.is_empty() || width <= 0.0 || chart_height <= 0.0 {
            return;
        }
        let accent = adw::StyleManager::default().accent_color_rgba();
        let text = area.color();
        let peak = drawn.iter().map(|b| b.value).max().filter(|v| *v > 0);
        let peak_at = peak.and_then(|p| drawn.iter().position(|b| b.value == p));
        let slot = width / drawn.len() as f64;
        let bar_width = (slot * BAR_SHARE).max(2.0);
        let radius = (bar_width / 2.0).min(MAX_RADIUS);
        let hovered = *hovered.borrow();
        for (index, bar) in drawn.iter().enumerate() {
            let left = index as f64 * slot + (slot - bar_width) / 2.0;
            match peak {
                Some(peak) if bar.value > 0 => {
                    let bar_height = (chart_height * bar.value as f64 / peak as f64).max(EMPTY_HEIGHT * 2.0);
                    let alpha = if Some(index) == peak_at || hovered == Some(index) { 1.0 } else { 0.55 };
                    cr.set_source_rgba(f64::from(accent.red()), f64::from(accent.green()), f64::from(accent.blue()), alpha);
                    top_rounded(cr, left, chart_height - bar_height, bar_width, bar_height, radius);
                    let _ = cr.fill();
                }
                _ => {
                    cr.set_source_rgba(f64::from(text.red()), f64::from(text.green()), f64::from(text.blue()), 0.16);
                    top_rounded(cr, left, chart_height - EMPTY_HEIGHT, bar_width, EMPTY_HEIGHT, EMPTY_HEIGHT / 2.0);
                    let _ = cr.fill();
                }
            }
        }
        // Подписи: шрифт интерфейса, приглушённым цветом.
        let font = gtk::Settings::default().and_then(|s| s.gtk_font_name()).unwrap_or_default();
        let family = font.rsplit_once(' ').map_or(font.as_str(), |(name, _)| name).trim_end_matches(',');
        cr.select_font_face(if family.is_empty() { "sans-serif" } else { family }, cairo::FontSlant::Normal, cairo::FontWeight::Normal);
        cr.set_font_size(11.0);
        cr.set_source_rgba(f64::from(text.red()), f64::from(text.green()), f64::from(text.blue()), 0.62);
        for (index, bar) in drawn.iter().enumerate() {
            let Some(label) = &bar.label else { continue };
            let Ok(extents) = cr.text_extents(label) else { continue };
            let center = index as f64 * slot + slot / 2.0;
            let x = (center - extents.width() / 2.0 - extents.x_bearing()).clamp(0.0, (width - extents.width()).max(0.0));
            cr.move_to(x, chart_height + LABEL_HEIGHT - 5.0);
            let _ = cr.show_text(label);
        }
    });

    let shown = Rc::clone(&bars);
    let target = Rc::clone(&hover);
    area.connect_query_tooltip(move |area, x, _, _, tooltip| {
        let index = bar_at(area, f64::from(x), shown.len());
        if *target.borrow() != index {
            target.replace(index);
            area.queue_draw();
        }
        match index.and_then(|i| shown.get(i)) {
            Some(bar) => {
                tooltip.set_text(Some(&bar.tooltip));
                true
            }
            None => false,
        }
    });
    let motion = gtk::EventControllerMotion::new();
    let leave = Rc::clone(&hover);
    motion.connect_leave(move |controller| {
        if leave.borrow_mut().take().is_some() {
            if let Some(widget) = controller.widget() {
                widget.queue_draw();
            }
        }
    });
    area.add_controller(motion);
    // Акцент или тема сменились — перерисовать.
    let manager = adw::StyleManager::default();
    let weak = area.downgrade();
    let (accent_id, dark_id) = (
        manager.connect_accent_color_rgba_notify({
            let weak = weak.clone();
            move |_| {
                if let Some(area) = weak.upgrade() {
                    area.queue_draw();
                }
            }
        }),
        manager.connect_dark_notify(move |_| {
            if let Some(area) = weak.upgrade() {
                area.queue_draw();
            }
        }),
    );
    let ids = RefCell::new(Some((accent_id, dark_id)));
    area.connect_destroy(move |_| {
        if let Some((accent_id, dark_id)) = ids.take() {
            let manager = adw::StyleManager::default();
            manager.disconnect(accent_id);
            manager.disconnect(dark_id);
        }
    });
    area
}

fn bar_at(area: &gtk::DrawingArea, x: f64, count: usize) -> Option<usize> {
    let width = f64::from(area.width());
    (count > 0 && width > 0.0 && x >= 0.0 && x < width).then(|| ((x / width * count as f64) as usize).min(count - 1))
}

/// Прямоугольник со скруглёнными верхними углами.
fn top_rounded(cr: &cairo::Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let radius = radius.min(width / 2.0).min(height);
    cr.new_sub_path();
    cr.arc(x + radius, y + radius, radius, PI, 3.0 * FRAC_PI_2);
    cr.arc(x + width - radius, y + radius, radius, 3.0 * FRAC_PI_2, 2.0 * PI);
    cr.line_to(x + width, y + height);
    cr.line_to(x, y + height);
    cr.close_path();
}
