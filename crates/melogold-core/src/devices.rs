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

#[cfg(test)]
mod tests {
    use super::*;

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
}
