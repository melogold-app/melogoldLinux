//! Устройства аккаунта (задание Windows 0006): вид по `platform` для значка и подписи, код входа
//! `XXXX-XXXX` (API §1.6 `UserCode`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Phone,
    Tablet,
    Computer,
    Watch,
    Headset,
    Other,
}

/// Вид по точному значению `platform` в нижнем регистре; незнакомое — «Устройство».
pub fn kind(platform: Option<&str>) -> DeviceKind {
    match platform.map(|p| p.trim().to_ascii_lowercase()).as_deref() {
        Some("android" | "ios") => DeviceKind::Phone,
        Some("ipados") => DeviceKind::Tablet,
        Some("macos" | "windows" | "linux") => DeviceKind::Computer,
        Some("watchos") => DeviceKind::Watch,
        Some("visionos") => DeviceKind::Headset,
        _ => DeviceKind::Other,
    }
}

/// Устройства для фильтра Истории и Итогов (задание 0012): чужие устройства, чьи прослушивания есть
/// здесь **и** которые есть в аккаунте (`names`: id → имя), по имени. Удалённое из аккаунта, устройство
/// без имени и событие без `deviceId` (пустая строка) не попадают: их прослушивания видны только в
/// «Все устройства». Фильтр показывают, когда список не пуст.
pub fn filter_devices<'a>(
    seen: impl IntoIterator<Item = &'a String>,
    own: &str,
    names: &std::collections::HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut others: Vec<(String, String)> = seen
        .into_iter()
        .filter(|id| !id.is_empty() && id.as_str() != own)
        .filter_map(|id| names.get(id).map(|name| (id.clone(), name.clone())))
        .collect();
    others.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()).then_with(|| a.0.cmp(&b.0)));
    others
}

/// Код входа: верхний регистр, без пробелов, `-` и `_`, `O→0`, `I,L→1`; ровно 8 знаков Crockford.
pub fn normalize_user_code(input: &str) -> Option<String> {
    const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let chars: Vec<char> = input
        .to_uppercase()
        .chars()
        .filter(|c| !matches!(c, '-' | '_') && !c.is_whitespace())
        .map(|c| match c {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect();
    if chars.len() != 8 || chars.iter().any(|c| !ALPHABET.contains(*c)) {
        return None;
    }
    let text: String = chars.into_iter().collect();
    Some(format!("{}-{}", &text[..4], &text[4..]))
}

/// Сколько ещё действует код: «4:32», не ниже «0:00» (задание 0008, Android `countdownText`).
pub fn countdown_text(remaining_ms: i64) -> String {
    let seconds = (remaining_ms.max(0) + 999) / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// `K7QX-M2PD` по знакам для чтения с экрана: «K, 7, Q, X, M, 2, P, D».
pub fn spoken_code(code: &str) -> String {
    code.chars().filter(|c| *c != '-').map(String::from).collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn removed_devices_are_not_in_the_filter() {
        let names = std::collections::HashMap::from([("me".to_owned(), "Ноутбук".to_owned()), ("tel".to_owned(), "Телефон".to_owned())]);
        // Три устройства, одно удалено из аккаунта: остаётся телефон (и «Это устройство» рядом).
        let seen = set(&["me", "tel", "gone"]);
        assert_eq!(filter_devices(&seen, "me", &names), [("tel".to_owned(), "Телефон".to_owned())]);
        // Все чужие удалены — фильтра нет.
        assert!(filter_devices(&set(&["me", "gone", "gone2"]), "me", &names).is_empty());
        // Событие без deviceId (пустая строка) не выбирается.
        assert!(filter_devices(&set(&["me", ""]), "me", &names).is_empty());
        // Без списка устройств имён нет — никого не показываем.
        assert!(filter_devices(&seen, "me", &Default::default()).is_empty());
    }

    #[test]
    fn kinds_by_platform() {
        assert_eq!(kind(Some("android")), DeviceKind::Phone);
        assert_eq!(kind(Some("ios")), DeviceKind::Phone);
        assert_eq!(kind(Some("ipados")), DeviceKind::Tablet);
        assert_eq!(kind(Some("Linux")), DeviceKind::Computer);
        assert_eq!(kind(Some("windows")), DeviceKind::Computer);
        assert_eq!(kind(Some("macos")), DeviceKind::Computer);
        assert_eq!(kind(Some("watchos")), DeviceKind::Watch);
        assert_eq!(kind(Some("visionos")), DeviceKind::Headset);
        assert_eq!(kind(Some("tvos")), DeviceKind::Other);
        assert_eq!(kind(None), DeviceKind::Other);
    }

    #[test]
    fn user_codes() {
        for input in ["k7qx m2pd", "K7QX-M2PD", "k7qxm2pd", "  k7qx_m2pd "] {
            assert_eq!(normalize_user_code(input).as_deref(), Some("K7QX-M2PD"), "{input}");
        }
        assert_eq!(normalize_user_code("OIL0-ABCD").as_deref(), Some("0110-ABCD"));
        assert_eq!(normalize_user_code("K7QX-M2P"), None);
        assert_eq!(normalize_user_code("K7QU-M2PD"), None, "U нет в алфавите Crockford");
    }

    #[test]
    fn countdown_never_goes_below_zero() {
        assert_eq!(countdown_text(272_000), "4:32");
        assert_eq!(countdown_text(300_000), "5:00");
        assert_eq!(countdown_text(59_001), "1:00", "секунда, которая ещё идёт, не округляется вниз");
        assert_eq!(countdown_text(1), "0:01");
        assert_eq!(countdown_text(0), "0:00");
        assert_eq!(countdown_text(-5_000), "0:00");
    }

    #[test]
    fn code_is_spoken_by_characters() {
        assert_eq!(spoken_code("K7QX-M2PD"), "K, 7, Q, X, M, 2, P, D");
    }
}
