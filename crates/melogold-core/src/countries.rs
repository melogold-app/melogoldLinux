//! Название страны по коду из двух букв на языке интерфейса (задание 0001: «Недоступно в
//! стране «Россия»»). Таблица из CLDR — `scripts/sync-countries.py`.

use crate::countries_generated::COUNTRIES;

/// Неизвестный код — сам код.
pub fn name(code: &str, russian: bool) -> String {
    match COUNTRIES.binary_search_by(|(c, ..)| (*c).cmp(code)) {
        Ok(index) => {
            let (_, ru, en) = COUNTRIES[index];
            if russian { ru } else { en }.to_owned()
        }
        Err(_) => code.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_in_both_languages() {
        assert!(COUNTRIES.windows(2).all(|p| p[0].0 < p[1].0));
        assert_eq!(name("RU", true), "Россия");
        assert_eq!(name("RU", false), "Russia");
        assert_eq!(name("DE", true), "Германия");
        assert_eq!(name("R1", true), "R1");
        assert_eq!(name("QM", false), "QM");
    }
}
