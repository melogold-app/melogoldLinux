//! Текстовые правила: длительность «3:45» ↔ мс (Windows `TextRules.cs`), время API (§1.5).

/// «3:45», «1:02:10» → мс; всё остальное — `None`.
pub fn parse_duration(text: Option<&str>) -> Option<i64> {
    let parts: Vec<&str> = text?.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut total = 0i64;
    for part in parts {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        total = total * 60 + part.parse::<i64>().ok()?;
    }
    Some(total * 1000)
}

/// Мс → «3:45» или «1:02:10».
pub fn format_duration(ms: i64) -> String {
    let total = ms.max(0) / 1000;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Сейчас, мс Unix UTC.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration(Some("3:45")), Some(225_000));
        assert_eq!(parse_duration(Some(" 1:02:10 ")), Some(3_730_000));
        assert_eq!(parse_duration(Some("45")), None);
        assert_eq!(parse_duration(Some("1:2a")), None);
        assert_eq!(parse_duration(None), None);
        assert_eq!(format_duration(225_000), "3:45");
        assert_eq!(format_duration(3_730_000), "1:02:10");
        assert_eq!(format_duration(-5), "0:00");
    }
}
