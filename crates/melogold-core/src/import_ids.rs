//! id прослушиваний из копии (`melogoldAndroid/docs/spec/backup-format.md` §3, Android `ImportIds`,
//! Windows `ImportIds.cs`): UUIDv5 строки `"videoId|timestampMs|playTimeMs"` в пространстве
//! [`NAMESPACE`]. Одна копия, импортированная на двух устройствах, даёт те же id, и сервер не удваивает
//! историю. Векторы — `docs/spec/import-ids.vectors.json`.

use sha1::{Digest, Sha1};

/// `NS_MELOGOLD_IMPORT` = 4a3b8c8a-1d9c-48f2-938b-3077d54ab4fb.
pub const NAMESPACE: [u8; 16] = [0x4a, 0x3b, 0x8c, 0x8a, 0x1d, 0x9c, 0x48, 0xf2, 0x93, 0x8b, 0x30, 0x77, 0xd5, 0x4a, 0xb4, 0xfb];

/// id прослушивания; `play_time_ms` — уже зажатое в 1…86 400 000.
pub fn event_id(video_id: &str, timestamp_ms: i64, play_time_ms: i64) -> String {
    uuid5(&NAMESPACE, &format!("{video_id}|{timestamp_ms}|{play_time_ms}"))
}

/// RFC 9562, версия 5: SHA-1 от пространства (байты в сетевом порядке) и имени, в нижнем регистре.
pub fn uuid5(space: &[u8; 16], name: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(space);
    hasher.update(name.as_bytes());
    let mut bytes: [u8; 16] = hasher.finalize()[..16].try_into().expect("16 байт");
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Правило имён плана слияния сервера (API §4.7): NFKC, без краевых пробелов, пробелы схлопнуты,
/// нижний регистр — так импорт находит свой плейлист по имени.
pub fn norm(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let nfkc: String = name.nfkc().collect();
    nfkc.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vectors() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/import-ids.vectors.json")).unwrap();
        let vectors: serde_json::Value = serde_json::from_str(&text).unwrap();
        for case in vectors["cases"].as_array().unwrap() {
            let input = &case["input"];
            let id =
                event_id(input["videoId"].as_str().unwrap(), input["timestampMs"].as_i64().unwrap(), input["playTimeMs"].as_i64().unwrap());
            assert_eq!(id, case["expected"].as_str().unwrap(), "{}", case["id"]);
        }
    }

    #[test]
    fn names_compare_like_the_server() {
        assert_eq!(norm("  Дорога\u{00A0}\u{00A0}домой "), "дорога домой");
        assert_eq!(norm("ＭＩＸ"), "mix");
    }
}
