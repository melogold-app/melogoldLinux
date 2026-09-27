//! Время API (§1.5): сервер отдаёт `YYYY-MM-DDTHH:mm:ss.sssZ`, принимает дробную часть до 9 знаков;
//! внутри клиента — миллисекунды эпохи UTC.

/// `YYYY-MM-DDTHH:mm:ss.sssZ` из миллисекунд эпохи.
pub fn format(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rest = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let (hours, minutes, seconds, millis) = (rest / 3_600_000, rest / 60_000 % 60, rest / 1000 % 60, rest % 1000);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis:03}Z")
}

/// Миллисекунды эпохи из времени API; смещения, кроме `Z`, не принимаются.
pub fn parse(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 20 || *b.last()? != b'Z' || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> { text.get(range)?.parse::<i64>().ok() };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hours, minutes, seconds) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    let millis = match &text[19..text.len() - 1] {
        "" => 0,
        fraction
            if fraction.starts_with('.') && (2..=10).contains(&fraction.len()) && fraction[1..].bytes().all(|c| c.is_ascii_digit()) =>
        {
            // Дробь усекается до миллисекунд (API §1.5).
            let digits = &fraction[1..];
            let padded = format!("{:0<3}", &digits[..digits.len().min(3)]);
            padded.parse::<i64>().ok()?
        }
        _ => return None,
    };
    Some(days_from_civil(year, month, day) * 86_400_000 + hours * 3_600_000 + minutes * 60_000 + seconds * 1000 + millis)
}

/// Дни от 1970-01-01 (алгоритм Говарда Хиннанта).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_server_forms() {
        assert_eq!(format(0), "1970-01-01T00:00:00.000Z");
        let ms = parse("2026-09-23T10:00:00.123Z").unwrap();
        assert_eq!(format(ms), "2026-09-23T10:00:00.123Z");
        assert_eq!(parse("2026-09-23T10:00:00.123456Z"), Some(ms), "дробь усекается до мс");
        assert_eq!(parse("2026-09-23T10:00:00Z"), Some(ms - 123));
        assert_eq!(parse("2024-02-29T23:59:59.999Z").map(format).as_deref(), Some("2024-02-29T23:59:59.999Z"));
        assert_eq!(parse("2026-09-23T10:00:00+03:00"), None);
        assert_eq!(parse("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse("garbage"), None);
        // Сверка с известной точкой: 2026-09-23T10:00:00Z.
        assert_eq!(parse("2026-09-23T10:00:00.000Z"), Some(1_790_157_600_000));
    }
}
