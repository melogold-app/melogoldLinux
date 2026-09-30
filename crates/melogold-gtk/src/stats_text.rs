//! Подписи «Итогов» (задание 0009): месяцы и дни недели на языке интерфейса, длительности, названия
//! периодов и сравнения. Месяцы свои, а не из `strftime`: язык приложения может быть выбран не тем,
//! на котором настроена локаль системы.

use melogold_core::stats_window::{civil_from_days, weekday, Period, Window};
use melogold_data::stats::{Bar, DayPart, ListeningStats};

use crate::localization::{lang, tr, trf, Lang};

const MONTHS_RU: [&str; 12] =
    ["январь", "февраль", "март", "апрель", "май", "июнь", "июль", "август", "сентябрь", "октябрь", "ноябрь", "декабрь"];
const MONTHS_RU_OF: [&str; 12] =
    ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
const MONTHS_RU_TO: [&str; 12] =
    ["январю", "февралю", "марту", "апрелю", "маю", "июню", "июлю", "августу", "сентябрю", "октябрю", "ноябрю", "декабрю"];
const MONTHS_EN: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS_RU: [&str; 7] = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];
const WEEKDAYS_EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// «Сентябрь», «September».
pub fn month_name(month: u32) -> String {
    match lang() {
        Lang::Ru => capitalized(MONTHS_RU[month as usize - 1]),
        Lang::En => MONTHS_EN[month as usize - 1].to_owned(),
    }
}

/// «сентября» (28 сентября) и «September» (28 September).
fn month_in_date(month: u32) -> &'static str {
    match lang() {
        Lang::Ru => MONTHS_RU_OF[month as usize - 1],
        Lang::En => MONTHS_EN[month as usize - 1],
    }
}

/// «сентябрю» («к сентябрю»), «September» («vs September»).
fn month_after_to(month: u32) -> &'static str {
    match lang() {
        Lang::Ru => MONTHS_RU_TO[month as usize - 1],
        Lang::En => MONTHS_EN[month as usize - 1],
    }
}

/// Первая буква месяца — подпись столбика года.
pub fn month_initial(month: u32) -> String {
    match lang() {
        Lang::Ru => capitalized(MONTHS_RU[month as usize - 1]).chars().next().map(String::from).unwrap_or_default(),
        Lang::En => MONTHS_EN[month as usize - 1].chars().next().map(String::from).unwrap_or_default(),
    }
}

pub fn weekday_short(day: i64) -> &'static str {
    let index = weekday(day) as usize;
    match lang() {
        Lang::Ru => WEEKDAYS_RU[index],
        Lang::En => WEEKDAYS_EN[index],
    }
}

/// «38 ч 12 мин», «12 мин»; меньше минуты — «1 мин».
pub fn duration(ms: i64) -> String {
    let minutes = if ms > 0 { (ms / 60_000).max(1) } else { 0 };
    if minutes >= 60 {
        trf("DurationHoursMinutesFormat", &[&(minutes / 60), &(minutes % 60)])
    } else {
        trf("DurationMinutesFormat", &[&minutes])
    }
}

/// «28 сентября», «28 September».
fn date_text(day: i64) -> String {
    let (_, month, d) = civil_from_days(day);
    match lang() {
        Lang::Ru => format!("{d} {}", month_in_date(month)),
        Lang::En => format!("{} {d}", month_in_date(month)),
    }
}

/// Название периода над стрелками: «Сентябрь 2026», «2026», «21–27 сентября».
pub fn period_title(window: &Window, current_year: i64) -> String {
    let Some(first) = window.first_day else { return tr("AllTime").to_owned() };
    let (year, month, _) = civil_from_days(first);
    match window.period {
        Period::AllTime => tr("AllTime").to_owned(),
        Period::Year => year.to_string(),
        Period::Month => format!("{} {year}", month_name(month)),
        Period::Week => {
            let last = window.end_day.map_or(first, |end| end - 1);
            let ((_, m1, d1), (y2, m2, d2)) = (civil_from_days(first), civil_from_days(last));
            let span = match (lang(), m1 == m2) {
                (Lang::Ru, true) => format!("{d1}–{d2} {}", month_in_date(m2)),
                (Lang::En, true) => format!("{} {d1}–{d2}", month_in_date(m2)),
                _ => format!("{} – {}", date_text(first), date_text(last)),
            };
            if y2 == current_year {
                span
            } else {
                format!("{span} {y2}")
            }
        }
    }
}

/// «+12 %», «−5 %», «0 %» — с неразрывным пробелом.
pub fn percent_text(percent: i64) -> String {
    match percent {
        0 => "0\u{a0}%".to_owned(),
        p if p > 0 => format!("+{p}\u{a0}%"),
        p => format!("\u{2212}{}\u{a0}%", -p),
    }
}

/// «+12 % к августу»: сравнение с прошлым таким же периодом; `None` без него.
pub fn comparison(stats: &ListeningStats, current_year: i64) -> Option<String> {
    let percent = stats.change_percent()?;
    let first = stats.window.first_day?;
    let (year, month, _) = civil_from_days(first);
    let previous = match stats.window.period {
        Period::Week => tr("LinuxStatsVsLastWeek").to_owned(),
        Period::Month => {
            let (previous_year, previous_month) = if month == 1 { (year - 1, 12) } else { (year, month - 1) };
            let name = month_after_to(previous_month);
            if previous_year == current_year {
                name.to_owned()
            } else {
                format!("{name} {previous_year}")
            }
        }
        Period::Year => trf("LinuxStatsVsYear", &[&(year - 1)]),
        Period::AllTime => return None,
    };
    Some(trf("LinuxStatsChange", &[&percent_text(percent), &previous]))
}

/// Подпись под столбиком: у недели — день недели, у месяца — число раз в пять дней, у года — первая
/// буква месяца, у «Всё время» — год (не чаще, чем поместится).
pub fn bar_label(period: Period, index: usize, count: usize, bar: &Bar) -> Option<String> {
    let (year, month, day) = civil_from_days(bar.day);
    match period {
        Period::Week => Some(weekday_short(bar.day).to_owned()),
        Period::Month => (day == 1 || day % 5 == 0).then(|| day.to_string()),
        Period::Year => Some(month_initial(month)),
        Period::AllTime => {
            let step = if count > 12 {
                4
            } else if count > 6 {
                2
            } else {
                1
            };
            (index % step == 0).then(|| year.to_string())
        }
    }
}

/// «12 сентября», «Сентябрь», «2026» — про столбик в подсказке.
pub fn bar_name(period: Period, bar: &Bar) -> String {
    let (year, month, _) = civil_from_days(bar.day);
    match period {
        Period::Week | Period::Month => date_text(bar.day),
        Period::Year => month_name(month),
        Period::AllTime => year.to_string(),
    }
}

/// Название времени суток.
pub fn day_part_name(part: DayPart) -> &'static str {
    tr(match part {
        DayPart::Night => "LinuxDayNight",
        DayPart::Morning => "LinuxDayMorning",
        DayPart::Afternoon => "LinuxDayAfternoon",
        DayPart::Evening => "LinuxDayEvening",
    })
}

/// «14:00».
pub fn hour_text(hour: usize) -> String {
    format!("{hour:02}:00")
}
