//! Доказательство работы при регистрации (API §4.3, векторы `spec/pow.vectors.json`): первый
//! `nonce` ("0", "1", …), для которого `sha256(UTF-8(challenge + ":" + nonce))` начинается хотя бы
//! с `bits` нулевых битов.

use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

/// Решение; `None` — отменили (`cancel`).
pub fn solve(challenge: &str, bits: u32, cancel: &AtomicBool) -> Option<String> {
    let prefix = format!("{challenge}:");
    let mut base = Sha256::new();
    base.update(prefix.as_bytes());
    let mut nonce: u64 = 0;
    loop {
        if nonce & 0xFFFF == 0 && cancel.load(Ordering::Relaxed) {
            return None;
        }
        let text = nonce.to_string();
        let mut hasher = base.clone();
        hasher.update(text.as_bytes());
        if leading_zero_bits(&hasher.finalize()) >= bits {
            return Some(text);
        }
        nonce += 1;
    }
}

pub fn leading_zero_bits(hash: &[u8]) -> u32 {
    let mut count = 0;
    for byte in hash {
        if *byte == 0 {
            count += 8;
        } else {
            return count + byte.leading_zeros();
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vectors() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/pow.vectors.json")).unwrap();
        let spec: serde_json::Value = serde_json::from_str(&text).unwrap();
        let solutions = spec["solutions"].as_array().unwrap();
        assert!(!solutions.is_empty());
        let cancel = AtomicBool::new(false);
        for case in solutions {
            let challenge = case["challenge"].as_str().unwrap();
            let bits = case["bits"].as_u64().unwrap() as u32;
            let nonce = case["nonce"].as_str().unwrap();
            assert_eq!(solve(challenge, bits, &cancel).as_deref(), Some(nonce), "{case}");
            let digest = hex::encode(Sha256::digest(format!("{challenge}:{nonce}").as_bytes()));
            assert_eq!(digest, case["sha256"].as_str().unwrap());
        }
    }

    #[test]
    fn counts_bits_from_the_top() {
        assert_eq!(leading_zero_bits(&[0, 0, 0x80]), 16);
        assert_eq!(leading_zero_bits(&[0, 0x01]), 15);
        assert_eq!(leading_zero_bits(&[0x40]), 1);
        assert_eq!(leading_zero_bits(&[0, 0]), 16);
    }
}
