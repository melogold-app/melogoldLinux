//! Периоды «Итогов» (задание 0009, Android `StatsWindow`): неделя с понедельника, месяц, год и всё время,
//! по календарю и местному часовому поясу устройства. Без внешних библиотек времени: пояс приходит
//! функцией «смещение от UTC в миллисекундах на момент `utc_ms`» (в окне — `glib::TimeZone`), поэтому
//! границы верны и при переводе часов.

pub const DAY_MS: i64 = 86_400_000;
pub const HOUR_MS: i64 = 3_600_000;

/// Смещение местного времени от UTC (мс) на момент `utc_ms`.
pub type Zone<'a> = &'a dyn Fn(i64) -> i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Week,
    Month,
    Year,
    AllTime,
}

/// Дни с 1970-01-01 для даты по григорианскому календарю (Х. Хиннант, `days_from_civil`).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Дата (год, месяц 1–12, день) по дням с 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// День недели: 0 — понедельник … 6 — воскресенье.
pub fn weekday(days: i64) -> u32 {
    // 1970-01-01 — четверг.
    (days + 3).rem_euclid(7) as u32
}

pub fn days_in_month(year: i64, month: u32) -> u32 {
    (days_from_civil(if month == 12 { year + 1 } else { year }, month % 12 + 1, 1) - days_from_civil(year, month, 1)) as u32
}

/// Местное время момента, мс от местной полуночи 1970-01-01 (для разбора на дату и час).
pub fn local_ms(utc_ms: i64, zone: Zone) -> i64 {
    utc_ms + zone(utc_ms)
}

/// Местный день момента (дни с 1970-01-01).
pub fn local_day(utc_ms: i64, zone: Zone) -> i64 {
    local_ms(utc_ms, zone).div_euclid(DAY_MS)
}

/// Местный час момента, 0–23.
pub fn local_hour(utc_ms: i64, zone: Zone) -> usize {
    (local_ms(utc_ms, zone).rem_euclid(DAY_MS) / HOUR_MS) as usize
}

/// Момент (UTC, мс) местной полуночи `day`.
pub fn day_start(day: i64, zone: Zone) -> i64 {
    let local = day * DAY_MS;
    let guess = local - zone(local);
    local - zone(guess)
}

/// Один период и его место: `offset` 0 — текущий, -1 — прошлый и так далее. `[start_ms, end_ms)` — от
/// местной полуночи первого дня до полуночи после последнего. У «Всё время» дат и сдвига нет.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub period: Period,
    pub offset: i64,
    pub first_day: Option<i64>,
    pub end_day: Option<i64>,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Window {
    pub fn contains(&self, at_ms: i64) -> bool {
        at_ms >= self.start_ms && at_ms < self.end_ms
    }

    /// Дней в периоде: 7, 28–31, 365–366; у «Всё время» — 0.
    pub fn days(&self) -> i64 {
        match (self.first_day, self.end_day) {
            (Some(first), Some(end)) => end - first,
            _ => 0,
        }
    }

    /// Такой же период на шаг назад — для «к августу»; у «Всё время» его нет.
    pub fn previous(&self, zone: Zone) -> Option<Window> {
        let first = self.first_day?;
        Some(Window { offset: self.offset - 1, ..window(self.period, -1, first, zone) })
    }

    /// Год и месяц первого дня (для подписи периода).
    pub fn first_date(&self) -> Option<(i64, u32, u32)> {
        self.first_day.map(civil_from_days)
    }
}

/// Период `period` со сдвигом `offset` от того, в который попадает местный день `today`.
pub fn window(period: Period, offset: i64, today: i64, zone: Zone) -> Window {
    if period == Period::AllTime {
        return Window { period, offset: 0, first_day: None, end_day: None, start_ms: 0, end_ms: i64::MAX };
    }
    let (year, month, _) = civil_from_days(today);
    let months_from = |total: i64| days_from_civil(total.div_euclid(12), (total.rem_euclid(12) + 1) as u32, 1);
    let (first, end) = match period {
        Period::Week => {
            let first = today - weekday(today) as i64 + 7 * offset;
            (first, first + 7)
        }
        Period::Month => {
            let total = year * 12 + (month as i64 - 1) + offset;
            (months_from(total), months_from(total + 1))
        }
        Period::Year => (days_from_civil(year + offset, 1, 1), days_from_civil(year + offset + 1, 1, 1)),
        Period::AllTime => unreachable!(),
    };
    Window { period, offset, first_day: Some(first), end_day: Some(end), start_ms: day_start(first, zone), end_ms: day_start(end, zone) }
}

/// Год, о котором «Итоги готовы»: с 1 декабря по 31 января — заканчивающийся (или только что закончившийся).
pub fn wrapped_season_year(year: i64, month: u32) -> Option<i64> {
    match month {
        12 => Some(year),
        1 => Some(year - 1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(_: i64) -> i64 {
        0
    }
    fn moscow(_: i64) -> i64 {
        3 * HOUR_MS
    }
    /// Берлин: +1 ч, летом (с 2026-03-29 01:00 UTC по 2026-10-25 01:00 UTC) +2 ч.
    fn berlin(utc_ms: i64) -> i64 {
        let (from, to) = (days_from_civil(2026, 3, 29) * DAY_MS + HOUR_MS, days_from_civil(2026, 10, 25) * DAY_MS + HOUR_MS);
        if utc_ms >= from && utc_ms < to {
            2 * HOUR_MS
        } else {
            HOUR_MS
        }
    }
    fn day(y: i64, m: u32, d: u32) -> i64 {
        days_from_civil(y, m, d)
    }

    #[test]
    fn calendar_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2026, 9, 30)), (2026, 9, 30));
        assert_eq!(civil_from_days(days_from_civil(2024, 2, 29)), (2024, 2, 29));
        assert_eq!(weekday(day(2026, 9, 28)), 0, "28 сентября 2026 — понедельник");
        assert_eq!(weekday(day(2026, 9, 27)), 6);
        assert_eq!((days_in_month(2024, 2), days_in_month(2026, 2), days_in_month(2026, 12)), (29, 28, 31));
    }

    #[test]
    fn week_starts_on_monday() {
        // Среда 30 сентября 2026: неделя 28 сентября – 4 октября.
        let w = window(Period::Week, 0, day(2026, 9, 30), &utc);
        assert_eq!(w.first_day, Some(day(2026, 9, 28)));
        assert_eq!(w.end_day, Some(day(2026, 10, 5)));
        assert_eq!(w.days(), 7);
        // Воскресенье — ещё та же неделя; понедельник — уже следующая.
        assert_eq!(window(Period::Week, 0, day(2026, 10, 4), &utc).first_day, Some(day(2026, 9, 28)));
        assert_eq!(window(Period::Week, 0, day(2026, 10, 5), &utc).first_day, Some(day(2026, 10, 5)));
        assert_eq!(w.previous(&utc).unwrap().first_day, Some(day(2026, 9, 21)));
    }

    #[test]
    fn month_and_year_bounds() {
        let month = window(Period::Month, 0, day(2026, 9, 30), &utc);
        assert_eq!((month.first_day, month.end_day), (Some(day(2026, 9, 1)), Some(day(2026, 10, 1))));
        assert_eq!(month.days(), 30);
        // Назад через границу года: январь 2026 ← декабрь 2025.
        let january = window(Period::Month, 0, day(2026, 1, 15), &utc);
        assert_eq!(january.previous(&utc).unwrap().first_day, Some(day(2025, 12, 1)));
        assert_eq!(window(Period::Month, -12, day(2026, 9, 30), &utc).first_day, Some(day(2025, 9, 1)));
        let year = window(Period::Year, 0, day(2026, 9, 30), &utc);
        assert_eq!((year.first_day, year.end_day), (Some(day(2026, 1, 1)), Some(day(2027, 1, 1))));
        assert_eq!(year.days(), 365);
        assert_eq!(window(Period::Year, -2, day(2026, 9, 30), &utc).days(), 366, "2024 — високосный");
    }

    #[test]
    fn bounds_are_local_midnights() {
        // Москва: сентябрь начинается 31 августа в 21:00 UTC.
        let month = window(Period::Month, 0, day(2026, 9, 30), &moscow);
        assert_eq!(month.start_ms, day(2026, 9, 1) * DAY_MS - 3 * HOUR_MS);
        assert!(month.contains(month.start_ms) && !month.contains(month.start_ms - 1));
        assert!(!month.contains(month.end_ms) && month.contains(month.end_ms - 1));
        // 31 августа 22:00 UTC — уже 1 сентября в Москве.
        assert_eq!(local_day(day(2026, 8, 31) * DAY_MS + 22 * HOUR_MS, &moscow), day(2026, 9, 1));
        assert_eq!(local_hour(day(2026, 8, 31) * DAY_MS + 22 * HOUR_MS, &moscow), 1);
    }

    #[test]
    fn daylight_saving_keeps_days_whole() {
        // Октябрь 2026 в Берлине: начало по летнему времени (+2), конец — по зимнему (+1): месяц на час длиннее 31 суток.
        let october = window(Period::Month, 0, day(2026, 10, 10), &berlin);
        assert_eq!(october.start_ms, day(2026, 10, 1) * DAY_MS - 2 * HOUR_MS);
        assert_eq!(october.end_ms, day(2026, 11, 1) * DAY_MS - HOUR_MS);
        assert_eq!(october.end_ms - october.start_ms, 31 * DAY_MS + HOUR_MS);
    }

    #[test]
    fn wrapped_season() {
        assert_eq!(wrapped_season_year(2026, 12), Some(2026));
        assert_eq!(wrapped_season_year(2027, 1), Some(2026));
        assert_eq!(wrapped_season_year(2026, 2), None);
        assert_eq!(wrapped_season_year(2026, 11), None);
    }

    #[test]
    fn all_time_has_no_dates() {
        let all = window(Period::AllTime, 3, day(2026, 9, 30), &utc);
        assert_eq!((all.first_day, all.offset, all.days()), (None, 0, 0));
        assert!(all.contains(1) && all.previous(&utc).is_none());
    }
}
