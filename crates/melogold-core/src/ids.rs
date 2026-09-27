//! Идентификаторы: UUID v4 для `opId`, `playlistId`, `eventId` (API §1.6 — только lowercase).

/// Новый UUID v4 из случайных байтов ядра.
pub fn new_uuid() -> String {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Случайные байты из `/dev/urandom` (он есть на любом Linux).
pub fn fill_random(buffer: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        if file.read_exact(buffer).is_ok() {
            return;
        }
    }
    // Без /dev/urandom (не бывает на живой системе) — время и адрес: хоть что-то неповторяющееся.
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = (seed >> ((index % 16) * 8)) as u8 ^ (index as u8).wrapping_mul(151);
    }
}

/// `Uuid` по API §1.6: 8-4-4-4-12, только строчные hex.
pub fn is_uuid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(len, part)| part.len() == *len)
        && parts.iter().all(|part| part.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuids_are_v4_lowercase_and_distinct() {
        let (a, b) = (new_uuid(), new_uuid());
        assert!(is_uuid(&a) && is_uuid(&b), "{a} {b}");
        assert_ne!(a, b);
        assert_eq!(&a[14..15], "4");
        assert!(!is_uuid("6F1C2C0E-8A3B-4F7E-9C1D-2B5E7A9F0C11"));
        assert!(is_uuid("6f1c2c0e-8a3b-4f7e-9c1d-2b5e7a9f0c11"));
    }
}
